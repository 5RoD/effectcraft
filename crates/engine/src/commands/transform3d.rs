//! Atomic pose updates for 3D manipulation. Validate every target before committing;
//! consecutive pointer updates share one undo step through the normal merge key.

use std::collections::BTreeSet;

use effectcraft_keyframe::Value as KV;
use effectcraft_project::{LayerId, Uid};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use super::{CommandSpec, bad, has_comp, layer_mut, merge_p};
use crate::{EngineError, Result, Session, cmd};

const ID: &str = "layer.setTransforms3D";
const MAX_LAYERS: usize = 4096;
const MAX_VALUE: f64 = 1e12;

struct Change {
    layer: LayerId,
    time: Tick,
    values: Vec<(Uid, KV)>,
}

fn vector(value: &Value, name: &str) -> Result<[f64; 3]> {
    let values = value.as_array().filter(|values| values.len() == 3).ok_or_else(|| bad(ID, format!("{name} needs exactly three numbers")))?;
    let mut result = [0.0; 3];
    for (output, value) in result.iter_mut().zip(values) {
        *output = value
            .as_f64()
            .filter(|number| number.is_finite() && number.abs() <= MAX_VALUE)
            .ok_or_else(|| bad(ID, format!("{name} components must be finite and within +/-1e12")))?;
    }
    Ok(result)
}

fn set(s: &mut Session, params: &Value) -> Result<Value> {
    let changes = params
        .get("changes")
        .and_then(Value::as_array)
        .filter(|changes| !changes.is_empty() && changes.len() <= MAX_LAYERS)
        .ok_or_else(|| bad(ID, "changes must contain 1–4096 layer updates"))?;
    let cid = s.state.active_comp.ok_or(EngineError::NoComp)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let fps = comp.frame_rate.as_f64();
    if !fps.is_finite() || !(0.0..=1000.0).contains(&fps) || fps == 0.0 {
        return Err(bad(ID, "composition frame rate must be finite and within (0, 1000]"));
    }
    let time = match params.get("time") {
        Some(value) => {
            let seconds = value
                .as_f64()
                .filter(|value| value.is_finite() && *value >= Tick::MIN.seconds() && *value <= Tick::MAX.seconds())
                .ok_or_else(|| bad(ID, "time exceeds the supported finite Tick range"))?;
            comp.frame_rate.snap_nearest(Tick::from_seconds_f64(seconds))
        }
        None => s.time(),
    };
    if !(Tick::MIN..=Tick::MAX).contains(&time) {
        return Err(bad(ID, "composition time exceeds the supported Tick range"));
    }
    let mut seen = BTreeSet::new();
    let mut edits = Vec::with_capacity(changes.len());
    for change in changes {
        let object = change.as_object().ok_or_else(|| bad(ID, "each update must be an object"))?;
        if object.keys().any(|key| !["layer", "position", "scale", "orientation", "rotation"].contains(&key.as_str())) {
            return Err(bad(ID, "an update contains an unknown field"));
        }
        let lid = LayerId(change.get("layer").and_then(Value::as_u64).ok_or_else(|| bad(ID, "each update needs a layer ID"))?);
        if !seen.insert(lid) {
            return Err(bad(ID, "a layer occurs more than once"));
        }
        let layer = comp.layer(lid).ok_or_else(|| bad(ID, "a target layer is missing"))?;
        if layer.switches.locked || !layer.is_3d() {
            return Err(bad(ID, format!("“{}” must be an unlocked 3D layer", layer.name)));
        }
        if !layer.stretch.is_finite() || layer.stretch == 0.0 {
            return Err(bad(ID, "layer stretch must be finite and nonzero"));
        }
        let delta = time.0.checked_sub(layer.start_time.0).ok_or_else(|| bad(ID, "layer time subtraction overflows"))?;
        let local = if (layer.stretch - 100.0).abs() < 1e-9 {
            Tick(delta)
        } else if layer.stretch == -100.0 {
            Tick(delta.checked_neg().ok_or_else(|| bad(ID, "reverse layer time overflows"))?)
        } else {
            let value = delta as f64 * 100.0 / layer.stretch;
            if !value.is_finite() || value < i64::MIN as f64 || value >= i64::MAX as f64 {
                return Err(bad(ID, "layer stretch exceeds the supported Tick range"));
            }
            Tick(value as i64)
        };
        if !(Tick::MIN..=Tick::MAX).contains(&local) {
            return Err(bad(ID, "layer time exceeds the supported Tick range"));
        }
        let mut edit = Change { layer: lid, time: local, values: Vec::new() };
        for field in ["position", "scale", "orientation", "rotation"] {
            let Some(value) = change.get(field) else { continue };
            let value = vector(value, field)?;
            let separated = field == "position" && layer.props.prop("transform/positionX").is_some();
            let paths: Vec<(&str, KV)> = if separated {
                ["positionX", "positionY", "positionZ"].into_iter().zip(value).map(|(path, value)| (path, KV::Scalar(value))).collect()
            } else if field == "rotation" {
                ["rotationX", "rotationY", "rotation"].into_iter().zip(value).map(|(path, value)| (path, KV::Scalar(value))).collect()
            } else {
                vec![(field, KV::Vec3(value))]
            };
            for (path, value) in paths {
                let path = format!("transform/{path}");
                let property = layer.props.prop(&path).ok_or_else(|| bad(ID, format!("“{}” has no {path}", layer.name)))?;
                if property.static_only
                    || property.hold_only
                    || property.has_expression()
                    || property.value.kind_name() != value.kind_name()
                    || property.value_at(local).kind_name() != value.kind_name()
                {
                    return Err(bad(ID, format!("“{}” / {} must be an animatable transform without an enabled expression", layer.name, property.name)));
                }
                if property.value_at(local).components().iter().any(|number| !number.is_finite() || number.abs() > MAX_VALUE) {
                    return Err(bad(ID, "a target contains an invalid existing transform value"));
                }
                edit.values.push((property.uid, value));
            }
        }
        if edit.values.is_empty() {
            return Err(bad(ID, "an update has no transform fields"));
        }
        edits.push(edit);
    }
    let count = edits.len();
    s.edit("3D Transform", merge_p(params), |project, _| {
        for edit in edits {
            let layer = layer_mut(project, cid, edit.layer)?;
            for (uid, value) in edit.values {
                let property = layer.props.find_mut(uid).ok_or_else(|| bad(ID, "a transform target disappeared"))?;
                property.set_value_at(edit.time, value);
            }
        }
        Ok(json!({"layers": count, "time": time.seconds()}))
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "layer.setTransforms3D",
        "Set 3D Transforms",
        [],
        None,
        "{changes: [{layer, position?: [x,y,z], scale?: [x,y,z], orientation?: [x,y,z], rotation?: [x,y,z]}], time?: comp seconds, merge?} → {layers, time}; validates the whole batch, preserves animation, updates separated Position and merges continuous gestures",
        has_comp,
        set
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_project::Property;

    fn scene() -> (Session, [u64; 2]) {
        let mut session = Session::default();
        session.execute("comp.new", json!({"width":320,"height":180,"duration":3,"frameRate":120})).unwrap();
        let mut ids = [0; 2];
        for id in &mut ids {
            *id = session.execute("layer.newNull", json!({})).unwrap()["layer"].as_u64().unwrap();
            session.execute("layer.setSwitch", json!({"layer":*id,"switch":"threeD","value":true})).unwrap();
        }
        (session, ids)
    }

    fn property(session: &Session, id: u64, path: &str) -> Property {
        session.active_comp().unwrap().layer(LayerId(id)).unwrap().props.prop(path).unwrap().clone()
    }

    #[test]
    fn multi_layer_pose_updates_are_atomic_mergeable_and_preserve_redo_branches() {
        let (mut session, [a, b]) = scene();
        let before = session.project.to_file_json().unwrap();
        let depth = session.history.undo.len();
        for value in [10., 20., 30.] {
            session
                .execute(
                    ID,
                    json!({"merge":"test-drag","changes":[
                        {"layer":a,"position":[value,40,50],"scale":[110,120,130],"orientation":[10,20,30]},
                        {"layer":b,"rotation":[40,50,value]}
                    ]}),
                )
                .unwrap();
        }
        assert_eq!(session.history.undo.len(), depth + 1);
        assert_eq!(property(&session, a, "transform/position").value, KV::Vec3([30., 40., 50.]));
        assert_eq!(property(&session, b, "transform/rotationX").value, KV::Scalar(40.));
        assert_eq!(property(&session, b, "transform/rotationY").value, KV::Scalar(50.));
        assert_eq!(property(&session, b, "transform/rotation").value, KV::Scalar(30.));
        let final_state = session.project.to_file_json().unwrap();
        session.undo();
        assert_eq!(session.project.to_file_json().unwrap(), before);
        session.redo();
        assert_eq!(session.project.to_file_json().unwrap(), final_state);
        session.undo();
        session.execute(ID, json!({"changes":[{"layer":a,"position":[1,2,3]}]})).unwrap();
        assert_eq!(session.history.branches.len(), 1);
    }

    #[test]
    fn updates_preserve_animated_keys_and_use_stretched_layer_time_and_separated_axes() {
        let (mut session, [a, b]) = scene();
        session.execute("prop.separateDimensions", json!({"layer":a,"path":"transform/position"})).unwrap();
        for time in [0., 2.] {
            session.execute("prop.addKey", json!({"layer":a,"path":"transform/positionX","time":time,"value":time*10.})).unwrap();
            session.execute("prop.addKey", json!({"layer":b,"path":"transform/scale","time":time,"value":[100,100,100]})).unwrap();
        }
        let old_position = property(&session, a, "transform/positionX").into_data().keys;
        let old_scale = property(&session, b, "transform/scale").into_data().keys;
        session.execute("layer.timeStretch", json!({"layer":b,"percent":200})).unwrap();
        session.execute(ID, json!({"time":1.,"changes":[{"layer":a,"position":[11,22,33]},{"layer":b,"scale":[150,160,170]}]})).unwrap();
        let position = property(&session, a, "transform/positionX");
        assert_eq!(position.keys.len(), 3);
        assert_eq!(position.keys[0], old_position[0]);
        assert_eq!(position.keys[2], old_position[1]);
        assert_eq!(position.value_at(Tick::from_seconds_f64(1.0)), KV::Scalar(11.));
        assert_eq!(property(&session, a, "transform/positionY").value, KV::Scalar(22.));
        assert_eq!(property(&session, a, "transform/positionZ").value, KV::Scalar(33.));
        let scale = property(&session, b, "transform/scale");
        assert_eq!(scale.keys.len(), 3);
        assert_eq!(scale.keys[0], old_scale[0]);
        assert_eq!(scale.keys[2], old_scale[1]);
        assert_eq!(scale.value_at(Tick::from_seconds_f64(0.5)), KV::Vec3([150., 160., 170.]));
    }

    fn rejected(session: &mut Session, params: Value) {
        let before = session.project.to_file_json().unwrap();
        let undo = session.history.undo.len();
        let revision = session.revision;
        assert!(session.execute(ID, params).is_err());
        assert_eq!(session.project.to_file_json().unwrap(), before);
        assert_eq!(session.history.undo.len(), undo);
        assert_eq!(session.revision, revision);
    }

    #[test]
    fn locked_expression_missing_duplicate_and_hostile_batches_change_nothing() {
        let (mut session, [a, b]) = scene();
        for params in [
            json!({"changes":[]}),
            json!({"changes":[{"layer":a}]}),
            json!({"changes":[{"layer":a,"position":[1,2]}]}),
            json!({"changes":[{"layer":a,"position":[1,2,"bad"]}]}),
            json!({"changes":[{"layer":a,"position":[1,2,1e30]}]}),
            json!({"changes":[{"layer":a,"position":[1,2,3]},{"layer":a,"scale":[2,3,4]}]}),
            json!({"changes":[{"layer":a,"position":[1,2,3]},{"layer":u64::MAX,"position":[4,5,6]}]}),
            json!({"changes":[{"layer":a,"position":[1,2,3],"unknown":true}]}),
            json!({"time":1e30,"changes":[{"layer":a,"position":[1,2,3]}]}),
        ] {
            rejected(&mut session, params);
        }
        session.execute("layer.setSwitch", json!({"layer":b,"switch":"locked","value":true})).unwrap();
        rejected(&mut session, json!({"changes":[{"layer":a,"position":[1,2,3]},{"layer":b,"scale":[4,5,6]}]}));
        session.execute("layer.setSwitch", json!({"layer":b,"switch":"locked","value":false})).unwrap();
        session.execute("prop.setExpression", json!({"layer":b,"path":"transform/scale","expression":"[100,100,100]"})).unwrap();
        rejected(&mut session, json!({"changes":[{"layer":a,"position":[1,2,3]},{"layer":b,"scale":[4,5,6]}]}));
        session.execute("prop.setExpression", json!({"layer":b,"path":"transform/scale","expression":""})).unwrap();
        session.execute(ID, json!({"changes":[{"layer":a,"position":[1,2,3]}]})).unwrap();
    }

    #[test]
    fn invalid_layer_stretch_is_rejected_before_any_saturating_time_conversion() {
        for stretch in [0., f64::NAN, f64::INFINITY, f64::MIN_POSITIVE] {
            let (mut session, [a, b]) = scene();
            session
                .edit("Invalid stretch fixture", None, |project, state| {
                    let comp = project.comp_mut(state.active_comp.unwrap()).unwrap();
                    comp.layer_mut(LayerId(b)).unwrap().stretch = stretch;
                    Ok(())
                })
                .unwrap();
            // NaN/infinity cannot serialize, so inspect identity and history directly.
            let before = session.project.clone();
            let depth = session.history.undo.len();
            assert!(session.execute(ID, json!({"time":1.,"changes":[{"layer":a,"position":[1,2,3]},{"layer":b,"position":[4,5,6]}]})).is_err());
            assert!(std::sync::Arc::ptr_eq(&before, &session.project));
            assert_eq!(session.history.undo.len(), depth);
        }
    }
}

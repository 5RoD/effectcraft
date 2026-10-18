//! Install the original font-based counter rig on ordinary text layers. All formatting controls
//! are ordinary properties; Source Text keeps its styling and existing text animators.

use std::collections::BTreeSet;

use effectcraft_keyframe::{FigureWidth, Value as KV};
use effectcraft_project::build::Ids;
use effectcraft_project::{Expression, LayerSource};
use serde_json::{Value, json};

use super::{CommandSpec, bad, has_layers, layer_mut, layers_p, unlocked};
use crate::{EngineError, Result, Session, cmd};

const COMMAND: &str = "text.nativeCounter";
const EXPRESSION: &str = "ecTextCounter()";

fn number(p: &Value, key: &str, default: f64, min: f64, max: f64, integer: bool) -> Result<KV> {
    let value = match p.get(key) {
        None => default,
        Some(value) => value.as_f64().ok_or_else(|| bad(COMMAND, format!("`{key}` must be a number")))?,
    };
    if !value.is_finite() || !(min..=max).contains(&value) || (integer && value.fract() != 0.0) {
        return Err(bad(COMMAND, format!("`{key}` must be a finite {}between {min} and {max}", if integer { "integer " } else { "number " })));
    }
    Ok(KV::Scalar(value))
}

fn boolean(p: &Value, key: &str) -> Result<KV> {
    match p.get(key) {
        None => Ok(KV::Bool(false)),
        Some(value) => value.as_bool().map(KV::Bool).ok_or_else(|| bad(COMMAND, format!("`{key}` must be true or false"))),
    }
}

fn choice(p: &Value, key: &str, options: &[&str]) -> Result<KV> {
    let text = match p.get(key) {
        None => return Ok(KV::Enum(0)),
        Some(value) => value.as_str().ok_or_else(|| bad(COMMAND, format!("`{key}` must be one of {}", options.join(", "))))?,
    };
    options.iter().position(|option| *option == text).map(|index| KV::Enum(index as u32)).ok_or_else(|| bad(COMMAND, format!("unknown `{key}`: {text}")))
}

fn affix(p: &Value, key: &str) -> Result<KV> {
    let text = match p.get(key) {
        None => "",
        Some(value) => value.as_str().ok_or_else(|| bad(COMMAND, format!("`{key}` must be text")))?,
    };
    if text.chars().take(257).count() > 256 {
        return Err(bad(COMMAND, format!("`{key}` is limited to 256 characters")));
    }
    Ok(KV::Str(text.into()))
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    let figure_width = match choice(p, "figureWidth", &["preserve", "tabular", "proportional"])? {
        KV::Enum(1) => Some(FigureWidth::Tabular),
        KV::Enum(2) => Some(FigureWidth::Proportional),
        _ => None,
    };
    let settings = [
        ("value", number(p, "value", 0.0, -1e15, 1e15, false)?),
        ("type", choice(p, "format", &["number", "time"])?),
        ("decimals", number(p, "decimals", 0.0, 0.0, 10.0, true)?),
        ("padding", number(p, "padding", 0.0, 0.0, 32.0, true)?),
        ("grouping", choice(p, "grouping", &["none", "comma", "period", "space"])?),
        ("prefix", affix(p, "prefix")?),
        ("suffix", affix(p, "suffix")?),
        ("currentTime", boolean(p, "currentTime")?),
        ("countDown", boolean(p, "countDown")?),
        ("clockRate", number(p, "clockRate", 1.0, -100.0, 100.0, false)?),
        ("clockStart", number(p, "clockStart", 0.0, -1_000_000.0, 1_000_000.0, false)?),
        ("timeStyle", choice(p, "timeStyle", &["hms", "ms", "seconds"])?),
    ];
    let (cid, ids) = layers_p(s, p)?;
    let mut seen = BTreeSet::new();
    let ids: Vec<_> = unlocked(s, cid, ids, COMMAND)?.into_iter().filter(|id| seen.insert(*id)).collect();
    if ids.is_empty() {
        return Err(bad(COMMAND, "select an unlocked text layer"));
    }
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let size = [comp.width as f64, comp.height as f64];
    for id in &ids {
        let layer = comp.layer(*id).ok_or_else(|| bad(COMMAND, "a selected layer no longer exists"))?;
        let source = layer
            .props
            .prop("text/sourceText")
            .filter(|_| matches!(layer.source, LayerSource::Text))
            .ok_or_else(|| bad(COMMAND, "select only ordinary text layers"))?;
        if !matches!(source.value, KV::Text(_)) || source.keys.iter().any(|key| !matches!(key.value, KV::Text(_))) {
            return Err(bad(COMMAND, format!("“{}” has malformed Source Text values", layer.name)));
        }
        if source.expr.as_ref().is_some_and(|expr| !expr.text.trim().is_empty()) {
            return Err(bad(COMMAND, format!("“{}” already has a Source Text expression; remove it before installing a counter", layer.name)));
        }
        let effects = layer.effects().ok_or_else(|| bad(COMMAND, "this text layer has no Effects group"))?;
        if effects.groups().any(|effect| effect.match_id == effectcraft_effects::controls::TEXT_COUNTER) {
            return Err(bad(COMMAND, format!("“{}” already has Native Text Counter controls; edit those controls instead", layer.name)));
        }
    }
    // Check the allocator before instantiate's fixed sequence of additions. No amount or string
    // from the request can expand this effect's property count.
    let spec = effectcraft_effects::find(effectcraft_effects::controls::TEXT_COUNTER).ok_or_else(|| bad(COMMAND, "Native Text Counter is unavailable"))?;
    let allocations = spec
        .params
        .len()
        .checked_add(1)
        .and_then(|count| count.checked_mul(ids.len()))
        .and_then(|count| u64::try_from(count).ok())
        .ok_or_else(|| bad(COMMAND, "too many text layers"))?;
    s.project.next_id.checked_add(allocations).ok_or_else(|| bad(COMMAND, "project property id space is exhausted"))?;
    if let Some(check) = s.expr_check {
        check(EXPRESSION).map_err(|error| bad(COMMAND, format!("counter expression is unavailable: {error}")))?;
    }
    let result = s.edit("Native Text Counter", None, |project, state| {
        let mut selected = vec![];
        let mut result = vec![];
        for id in &ids {
            let mut next = project.next_id;
            let mut control = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, size);
            for (key, value) in &settings {
                control.get_mut(key).ok_or_else(|| bad(COMMAND, format!("counter control `{key}` is unavailable")))?.value = value.clone();
            }
            let uid = control.uid;
            let value_uid = control.get("value").map(|prop| prop.uid).ok_or_else(|| bad(COMMAND, "counter Value control is unavailable"))?;
            let layer = layer_mut(project, cid, *id)?;
            let source = layer.props.prop_mut("text/sourceText").ok_or_else(|| bad(COMMAND, "Source Text is unavailable"))?;
            if let Some(width) = figure_width {
                let source = &mut **source;
                // Styling may itself have Source Text keys. Set the font feature on every
                // saved document and run, retaining all other authored styling and times.
                for value in std::iter::once(&mut source.value).chain(source.keys.iter_mut().map(|key| &mut key.value)) {
                    let KV::Text(document) = value else { return Err(bad(COMMAND, "Source Text is malformed")) };
                    document.opentype.figure_width = width;
                    for run in &mut document.runs {
                        run.style.opentype.figure_width = width;
                    }
                }
            }
            source.expr = Some(Expression { text: EXPRESSION.into(), enabled: true });
            layer.props.sub_mut("effects").ok_or_else(|| bad(COMMAND, "Effects group is unavailable"))?.children.push(control.into());
            project.next_id = next;
            selected.push((*id, value_uid));
            result.push(json!({"layer":id.0,"effect":uid,"value":value_uid,"valuePath":format!("@{value_uid}")}));
        }
        state.selected_props = selected;
        state.selected_keys.clear();
        state.last_effect = Some(effectcraft_effects::controls::TEXT_COUNTER.into());
        Ok(result)
    })?;
    Ok(json!({"counters":result,"expression":EXPRESSION}))
}

fn has_text_layers(s: &Session) -> std::result::Result<(), String> {
    has_layers(s)?;
    if s.active_comp().is_some_and(|comp| {
        s.state.selected_layers.iter().any(|id| comp.layer(*id).is_some_and(|layer| !layer.switches.locked && matches!(layer.source, LayerSource::Text)))
    }) {
        Ok(())
    } else {
        Err("select an unlocked text layer".into())
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "text.nativeCounter",
        "Text Counter...",
        ["Animation", "Native Motion Tools"],
        None,
        "{layers?,layer?,comp?,value?:number,format?:number|time,decimals?:0..10,padding?:0..32,grouping?:none|comma|period|space,prefix?:text,suffix?:text,currentTime?:bool,countDown?:bool,clockRate?:number,clockStart?:layer seconds,timeStyle?:hms|ms|seconds,figureWidth?:preserve|tabular|proportional}",
        has_text_layers,
        install
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (Session, u64) {
        let mut session = Session::default();
        session.execute("comp.new", json!({"width":64,"height":64,"duration":5})).unwrap();
        let layer = session.execute("layer.newText", json!({"text":"Original","font":"Inter","size":40})).unwrap()["layer"].as_u64().unwrap();
        (session, layer)
    }

    #[test]
    fn native_text_counter_preserves_text_and_animators_and_one_undo() {
        let (mut s, layer) = session();
        s.execute("layer.addTextAnimator", json!({"layer":layer,"properties":["position","opacity"]})).unwrap();
        let before = s.project.clone();
        let count = s.history.undo.len();
        let result = s.execute(COMMAND, json!({"layers":[layer,layer],"value":1234.5,"decimals":2,"grouping":"comma","prefix":"$"})).unwrap();
        assert_eq!(result["counters"].as_array().unwrap().len(), 1);
        assert_eq!(s.history.undo.len(), count + 1);
        let original = before.comp(s.active_comp_id().unwrap()).unwrap().layer(effectcraft_project::LayerId(layer)).unwrap();
        let current = s.active_comp().unwrap().layer(effectcraft_project::LayerId(layer)).unwrap();
        assert_eq!(current.props.prop("text/sourceText").unwrap().value, original.props.prop("text/sourceText").unwrap().value);
        assert_eq!(current.props.group("text/animators"), original.props.group("text/animators"));
        assert_eq!(current.props.prop("text/sourceText").unwrap().expr.as_ref().unwrap().text, EXPRESSION);
        assert_eq!(s.state.selected_props[0].1, result["counters"][0]["value"].as_u64().unwrap());
        let saved = serde_json::to_string(&*s.project).unwrap();
        let reloaded: effectcraft_project::Project = serde_json::from_str(&saved).unwrap();
        assert!(reloaded.same_content(&s.project));
        s.undo();
        assert!(s.project.same_content(&before));
    }

    #[test]
    fn native_text_counter_rejects_conflicts_and_invalid_inputs_atomically() {
        let (mut s, first) = session();
        let second = s.execute("layer.newText", json!({"text":"Second"})).unwrap()["layer"].as_u64().unwrap();
        s.execute("prop.setExpression", json!({"layer":second,"path":"text/sourceText","expression":"'Keep me'","enabled":false})).unwrap();
        let before = s.project.clone();
        let count = s.history.undo.len();
        for params in [
            json!({"layers":[first,second]}),
            json!({"layer":first,"decimals":0.5}),
            json!({"layer":first,"value":1e300}),
            json!({"layer":first,"padding":33}),
            json!({"layer":first,"figureWidth":true}),
            json!({"layer":first,"figureWidth":"unknown"}),
            json!({"layer":first,"prefix":"😀".repeat(257)}),
        ] {
            assert!(s.execute(COMMAND, params).is_err());
            assert!(s.project.same_content(&before));
            assert_eq!(s.history.undo.len(), count);
        }
        // Fixed-count allocation must also reject a hostile deserialized project id counter.
        std::sync::Arc::make_mut(&mut s.project).next_id = u64::MAX;
        let before = s.project.clone();
        assert!(s.execute(COMMAND, json!({"layer":first})).is_err());
        assert!(s.project.same_content(&before));
    }

    #[test]
    fn native_text_counter_skips_locked_layers_and_rejects_duplicate_rigs() {
        let (mut s, first) = session();
        let second = s.execute("layer.newText", json!({"text":"Locked"})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.setSwitch", json!({"layer":second,"switch":"locked","value":true})).unwrap();
        let result = s.execute(COMMAND, json!({"layers":[first,second]})).unwrap();
        assert_eq!(result["counters"].as_array().unwrap().len(), 1);
        let before = s.project.clone();
        assert!(s.execute(COMMAND, json!({"layer":first})).is_err());
        assert!(s.project.same_content(&before));
        assert!(s.active_comp().unwrap().layer(effectcraft_project::LayerId(second)).unwrap().props.prop("text/sourceText").unwrap().expr.is_none());
    }

    #[test]
    fn native_counter_digit_width_keeps_styled_source_keys_and_roundtrips_in_one_undo() {
        use effectcraft_keyframe::Keyframe;
        use effectcraft_time::Tick;
        for (option, width) in [("preserve", None), ("tabular", Some(FigureWidth::Tabular)), ("proportional", Some(FigureWidth::Proportional))] {
            let (mut s, layer) = session();
            let cid = s.active_comp_id().unwrap();
            let source = std::sync::Arc::make_mut(&mut s.project)
                .comp_mut(cid)
                .unwrap()
                .layer_mut(effectcraft_project::LayerId(layer))
                .unwrap()
                .props
                .prop_mut("text/sourceText")
                .unwrap();
            let KV::Text(document) = &mut source.value else { panic!("expected text") };
            document.opentype.figure_width = FigureWidth::Proportional;
            document.apply_style(0..1, |style| {
                style.fill = [0.2, 0.6, 0.9, 1.0];
                style.opentype.figure_width = FigureWidth::Default;
            });
            let mut animated = document.clone();
            animated.size = 81.0;
            source.keys = vec![Keyframe::new(Tick::from_seconds_f64(1.25), KV::Text(animated))];
            let before = s.project.clone();
            let undo = s.history.undo.len();
            s.execute(COMMAND, json!({"layer":layer,"figureWidth":option})).unwrap();
            assert_eq!(s.history.undo.len(), undo + 1);
            let old = before.comp(cid).unwrap().layer(effectcraft_project::LayerId(layer)).unwrap().props.prop("text/sourceText").unwrap();
            let actual = s.active_comp().unwrap().layer(effectcraft_project::LayerId(layer)).unwrap().props.prop("text/sourceText").unwrap();
            let adapt = |value: &KV| {
                let KV::Text(document) = value else { panic!("expected text") };
                let mut document = document.clone();
                if let Some(width) = width {
                    document.opentype.figure_width = width;
                    for run in &mut document.runs {
                        run.style.opentype.figure_width = width;
                    }
                }
                KV::Text(document)
            };
            assert_eq!(actual.value, adapt(&old.value));
            let mut keys = old.keys.clone();
            for key in &mut keys {
                key.value = adapt(&key.value);
            }
            assert_eq!(actual.keys, keys);
            let bytes = serde_json::to_vec(&*s.project).unwrap();
            let loaded: effectcraft_project::Project = serde_json::from_slice(&bytes).unwrap();
            assert!(loaded.same_content(&s.project));
            s.undo();
            assert!(s.project.same_content(&before));
        }
    }
}

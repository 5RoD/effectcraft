//! Original animation and shake actions, stored as ordinary editable keyframes.

use effectcraft_keyframe::{EaseCurve, Keyframe, Value as KV, set_key};
use effectcraft_project::{Layer, Property, Uid};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, has_layers, layer_mut, layers_p, str_p, unlocked};
use crate::{EngineError, KeyRef, Result, Session, cmd};

const ACTION: &str = "anim.nativeAction";
const SHAKE: &str = "anim.nativeShake";
const MAX_KEYS: usize = 200_000;

fn number(p: &Value, key: &str, default: f64, min: f64, max: f64, command: &str) -> Result<f64> {
    let n = match p.get(key) {
        None => default,
        Some(v) => v.as_f64().ok_or_else(|| bad(command, format!("`{key}` must be a number")))?,
    };
    if !n.is_finite() || !(min..=max).contains(&n) {
        return Err(bad(command, format!("`{key}` must be finite and between {min} and {max}")));
    }
    Ok(n)
}

fn boolean(p: &Value, key: &str, command: &str) -> Result<bool> {
    match p.get(key) {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or_else(|| bad(command, format!("`{key}` must be true or false"))),
    }
}

fn position_paths(layer: &Layer, axis: usize) -> Vec<(String, Option<usize>)> {
    let names = ["positionX", "positionY", "positionZ"];
    if layer.props.prop("transform/positionX").is_some() {
        names.get(axis).map(|name| vec![(format!("transform/{name}"), None)]).unwrap_or_default()
    } else {
        vec![("transform/position".into(), Some(axis))]
    }
}

fn checked_property<'a>(layer: &'a Layer, path: &str, command: &str) -> Result<&'a Property> {
    let pr = layer.props.prop(path).ok_or_else(|| bad(command, format!("“{}” has no {path}", layer.name)))?;
    let numeric = if matches!(path, "transform/position" | "transform/scale") {
        matches!(pr.value, KV::Vec2(_) | KV::Vec3(_))
    } else {
        matches!(pr.value, KV::Scalar(_))
    };
    if !numeric {
        return Err(bad(command, format!("“{}” / {} has an invalid transform value", layer.name, pr.name)));
    }
    if pr.static_only || pr.hold_only || pr.has_expression() {
        return Err(bad(command, format!("“{}” / {} must be an animatable property without an enabled expression", layer.name, pr.name)));
    }
    Ok(pr)
}

fn validate_value(v: &KV, command: &str) -> Result<()> {
    if v.components().iter().any(|n| !n.is_finite() || n.abs() > 1e12) {
        return Err(bad(command, "an existing property has a non-finite or excessively large value"));
    }
    Ok(())
}

fn delta(v: &KV, axis: Option<usize>, amount: f64, command: &str) -> Result<KV> {
    let mut components = v.components();
    match axis {
        Some(axis) => {
            let n = components.get_mut(axis).ok_or_else(|| bad(command, "the chosen axis is unavailable"))?;
            *n += amount;
        }
        None => components.iter_mut().for_each(|n| *n += amount),
    }
    let result = v.with_components(&components);
    validate_value(&result, command)?;
    Ok(result)
}

struct Edit {
    layer: effectcraft_project::LayerId,
    uid: Uid,
    start: Tick,
    end: Tick,
    keys: Vec<Keyframe>,
}

fn commit(s: &mut Session, cid: effectcraft_project::ItemId, label: &str, edits: Vec<Edit>) -> Result<Value> {
    let count: usize = edits.iter().map(|e| e.keys.len()).sum();
    let properties = edits.len();
    s.edit(label, None, |project, state| {
        state.selected_keys.clear();
        state.selected_props.clear();
        for edit in edits {
            let layer = layer_mut(project, cid, edit.layer)?;
            let pr = layer.props.find_mut(edit.uid).ok_or_else(|| EngineError::Other("animation target disappeared".into()))?;
            // Replace the specified interval; animation outside it stays intact.
            pr.keys.retain(|key| key.time < edit.start || key.time > edit.end);
            for key in edit.keys {
                state.selected_keys.push(KeyRef { layer: edit.layer, prop: edit.uid, time: key.time });
                set_key(&mut pr.keys, key);
            }
            state.selected_props.push((edit.layer, edit.uid));
        }
        Ok(json!({"properties": properties, "keys": count}))
    })
}

struct ActionSettings<'a> {
    action: &'a str,
    path: &'static str,
    out: bool,
    axis: usize,
    duration: f64,
    stagger: f64,
    start: f64,
    distance: f64,
    amount: f64,
    overshoot: f64,
    reverse: bool,
    curve: EaseCurve,
}

// Read-only settings parsing is shared by execution and portable-preset validation.
fn action_settings<'a>(s: &Session, p: &'a Value) -> Result<ActionSettings<'a>> {
    let action = str_p(p, "action").unwrap_or("fadeIn");
    let (path, out) = match action {
        "fadeIn" => ("transform/opacity", false),
        "fadeOut" => ("transform/opacity", true),
        "slideIn" => ("transform/position", false),
        "slideOut" => ("transform/position", true),
        "scaleIn" => ("transform/scale", false),
        "scaleOut" => ("transform/scale", true),
        "rotateIn" => ("transform/rotation", false),
        "rotateOut" => ("transform/rotation", true),
        _ => return Err(bad(ACTION, "action: fadeIn|fadeOut|slideIn|slideOut|scaleIn|scaleOut|rotateIn|rotateOut")),
    };
    let axis = match str_p(p, "axis").unwrap_or("y") {
        "x" => 0,
        "y" => 1,
        "z" => 2,
        _ => return Err(bad(ACTION, "axis: x|y|z")),
    };
    let duration = number(p, "duration", 0.6, 0.001, 600.0, ACTION)?;
    let stagger = number(p, "stagger", 0.0, 0.0, 60.0, ACTION)?;
    let start = number(p, "time", s.time().seconds(), 0.0, Tick::MAX.seconds(), ACTION)?;
    let distance = number(p, "distance", 200.0, -100_000.0, 100_000.0, ACTION)?;
    let default_amount = if path.ends_with("rotation") {
        number(p, "angle", -90.0, -10_000.0, 10_000.0, ACTION)?
    } else {
        number(p, "scalePercent", 0.0, -10_000.0, 10_000.0, ACTION)?
    };
    let amount = number(p, "amount", default_amount, -10_000.0, 10_000.0, ACTION)?;
    let overshoot = number(p, "overshoot", 0.0, 0.0, 100.0, ACTION)?;
    if overshoot > 0.0 && action.starts_with("fade") {
        return Err(bad(ACTION, "overshoot is available for Slide, Scale, and Rotate actions"));
    }
    let reverse = boolean(p, "reverse", ACTION)?;
    let curve = if let Some(v) = p.get("curve") {
        serde_json::from_value::<EaseCurve>(v.clone()).ok().and_then(EaseCurve::sanitized).ok_or_else(|| bad(ACTION, "invalid easing curve"))?
    } else {
        crate::ease_presets::find(s, str_p(p, "ease").unwrap_or("Ease In-Out")).ok_or_else(|| bad(ACTION, "unknown easing preset"))?
    };
    Ok(ActionSettings { action, path, out, axis, duration, stagger, start, distance, amount, overshoot, reverse, curve })
}

fn action(s: &mut Session, p: &Value) -> Result<Value> {
    let ActionSettings { action, path, out, axis, duration, stagger, start, distance, amount, overshoot, reverse, curve } = action_settings(s, p)?;
    let (cid, ids) = layers_p(s, p)?;
    let selected: std::collections::BTreeSet<_> = unlocked(s, cid, ids, ACTION)?.into_iter().collect();
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let mut ids: Vec<_> = comp.layers.iter().filter(|layer| selected.contains(&layer.id)).map(|layer| layer.id).collect();
    if reverse {
        ids.reverse();
    }
    if ids.is_empty() {
        return Err(bad(ACTION, "select layers first"));
    }
    if ids.len().saturating_mul(3) > MAX_KEYS {
        return Err(bad(ACTION, "too many layers in one action"));
    }
    let mut edits = vec![];
    for (index, id) in ids.iter().enumerate() {
        let layer = comp.layer(*id).ok_or_else(|| bad(ACTION, "missing layer"))?;
        if !layer.stretch.is_finite() || layer.stretch == 0.0 {
            return Err(bad(ACTION, "invalid layer stretch"));
        }
        if path.ends_with("position") && axis == 2 && !layer.is_3d() {
            return Err(bad(ACTION, "Z slide requires a 3D layer"));
        }
        let ct0 = comp.frame_rate.snap_nearest(Tick::from_seconds_f64(start + index as f64 * stagger));
        let ct1 = comp.frame_rate.snap_nearest(ct0 + Tick::from_seconds_f64(duration));
        if ct1 <= ct0 || ct1 > comp.duration {
            return Err(bad(ACTION, "animation range must span at least one frame and fit in the composition"));
        }
        let lt0 = layer.layer_time(ct0);
        let lt1 = layer.layer_time(ct1);
        if [lt0, lt1].iter().any(|time| *time < Tick::MIN || *time > Tick::MAX) {
            return Err(bad(ACTION, "layer stretch exceeds the supported time range"));
        }
        if lt0 == lt1 {
            return Err(bad(ACTION, "layer stretch collapses the animation range"));
        }
        let targets = if path.ends_with("position") { position_paths(layer, axis) } else { vec![(path.into(), None)] };
        for (path, dimension) in targets {
            let pr = checked_property(layer, &path, ACTION)?;
            let base = pr.value_at(if out { lt0 } else { lt1 });
            if base.kind_name() != pr.value.kind_name() {
                return Err(bad(ACTION, "a keyframe has the wrong value type"));
            }
            validate_value(&base, ACTION)?;
            let other = if action.starts_with("fade") {
                KV::Scalar(0.0)
            } else if action.starts_with("slide") {
                delta(&base, dimension, distance, ACTION)?
            } else if action.starts_with("scale") {
                let values =
                    base.components().iter().enumerate().map(|(axis, n)| if axis == 2 && !layer.is_3d() { *n } else { n * amount / 100.0 }).collect::<Vec<_>>();
                base.with_components(&values)
            } else {
                delta(&base, None, amount, ACTION)?
            };
            validate_value(&other, ACTION)?;
            let (a, b) = if out { (base, other) } else { (other, base) };
            let mut keys = vec![Keyframe::new(lt0, a.clone())];
            if overshoot > 0.0 {
                let mid = comp.frame_rate.snap_nearest(ct0 + (ct1 - ct0).mul_ratio(7, 10));
                if mid > ct0 && mid < ct1 {
                    let ac = a.components();
                    let value = b.with_components(&b.components().iter().zip(ac).map(|(b, a)| b + (b - a) * overshoot / 100.0).collect::<Vec<_>>());
                    validate_value(&value, ACTION)?;
                    let time = layer.layer_time(mid);
                    if time <= lt0.min(lt1) || time >= lt0.max(lt1) {
                        return Err(bad(ACTION, "layer stretch collapses the overshoot keyframe; reduce the stretch"));
                    }
                    keys.push(Keyframe::new(time, value));
                }
            }
            keys.push(Keyframe::new(lt1, b));
            keys.sort_by_key(|key| key.time);
            let ease = if layer.stretch < 0.0 {
                EaseCurve { out_influence: curve.in_influence, out_speed: curve.in_speed, in_influence: curve.out_influence, in_speed: curve.out_speed }
            } else {
                curve
            };
            for i in 0..keys.len().saturating_sub(1) {
                ease.apply(&mut keys, i, pr.spatial);
            }
            edits.push(Edit { layer: *id, uid: pr.uid, start: lt0.min(lt1), end: lt0.max(lt1), keys });
        }
    }
    let label = match action {
        "fadeIn" => "Native Fade In",
        "fadeOut" => "Native Fade Out",
        "slideIn" => "Native Slide In",
        "slideOut" => "Native Slide Out",
        "scaleIn" => "Native Scale In",
        "scaleOut" => "Native Scale Out",
        "rotateIn" => "Native Rotate In",
        _ => "Native Rotate Out",
    };
    commit(s, cid, label, edits)
}

fn hash(mut n: u64) -> u64 {
    n = n.wrapping_add(0x9e37_79b9_7f4a_7c15);
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    n ^ (n >> 31)
}

fn noise(seed: u64, stream: u64, x: f64) -> f64 {
    let i = x.floor() as u64;
    let t = x - x.floor();
    let weight = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let sample = |i| (hash(seed ^ hash(stream) ^ hash(i)) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
    sample(i) * (1.0 - weight) + sample(i.wrapping_add(1)) * weight
}

struct ShakeSettings {
    duration: f64,
    start: f64,
    frequency: f64,
    decay: f64,
    rotation: f64,
    scale: f64,
    amplitudes: [f64; 3],
    seed: u64,
}

fn shake_settings(s: &Session, p: &Value) -> Result<ShakeSettings> {
    let duration = number(p, "duration", 1.0, 0.001, 600.0, SHAKE)?;
    let start = number(p, "time", s.time().seconds(), 0.0, Tick::MAX.seconds(), SHAKE)?;
    let frequency = number(p, "frequency", 8.0, 0.01, 120.0, SHAKE)?;
    let decay = number(p, "decay", 3.0, 0.0, 50.0, SHAKE)?;
    let rotation = number(p, "rotation", 2.0, 0.0, 360.0, SHAKE)?;
    let scale = number(p, "scale", 0.0, 0.0, 100.0, SHAKE)?;
    let amplitudes = [number(p, "x", 20.0, 0.0, 100_000.0, SHAKE)?, number(p, "y", 20.0, 0.0, 100_000.0, SHAKE)?, number(p, "z", 0.0, 0.0, 100_000.0, SHAKE)?];
    let seed = match p.get("seed") {
        None => 1,
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_f64().filter(|n| n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0).map(|n| n as u64))
            .ok_or_else(|| bad(SHAKE, "seed must be an unsigned integer"))?,
    };
    Ok(ShakeSettings { duration, start, frequency, decay, rotation, scale, amplitudes, seed })
}

fn shake(s: &mut Session, p: &Value) -> Result<Value> {
    let ShakeSettings { duration, start, frequency, decay, rotation, scale, amplitudes, seed } = shake_settings(s, p)?;
    let (cid, ids) = layers_p(s, p)?;
    let mut ids = unlocked(s, cid, ids, SHAKE)?;
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err(bad(SHAKE, "select layers first"));
    }
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let ct0 = comp.frame_rate.snap_nearest(Tick::from_seconds_f64(start));
    let ct1 = comp.frame_rate.snap_nearest(ct0 + Tick::from_seconds_f64(duration));
    if ct1 <= ct0 || ct1 > comp.duration {
        return Err(bad(SHAKE, "shake range must span at least one frame and fit in the composition"));
    }
    let first = comp.frame_rate.frame_at(ct0);
    let last = comp.frame_rate.frame_at(ct1);
    let frames = last.saturating_sub(first);
    let count = usize::try_from(frames).ok().and_then(|n| n.checked_add(1)).and_then(|n| n.checked_mul(ids.len())).and_then(|n| n.checked_mul(5));
    if frames <= 0 || count.is_none_or(|n| n > MAX_KEYS) {
        return Err(bad(SHAKE, "too many shake keyframes; shorten the range or select fewer layers"));
    }
    let mut edits = vec![];
    for id in ids {
        let layer = comp.layer(id).ok_or_else(|| bad(SHAKE, "missing layer"))?;
        if !layer.stretch.is_finite() || layer.stretch == 0.0 {
            return Err(bad(SHAKE, "invalid layer stretch"));
        }
        if amplitudes[2] > 0.0 && !layer.is_3d() {
            return Err(bad(SHAKE, "Z shake requires a 3D layer"));
        }
        let lt0 = layer.layer_time(ct0);
        let lt1 = layer.layer_time(ct1);
        if [lt0, lt1].iter().any(|time| *time < Tick::MIN || *time > Tick::MAX) {
            return Err(bad(SHAKE, "layer stretch exceeds the supported time range"));
        }
        let mut targets = vec![];
        for (axis, amplitude) in amplitudes.iter().enumerate().filter(|(_, amplitude)| **amplitude > 0.0) {
            targets.extend(position_paths(layer, axis).into_iter().map(|(path, dimension)| (path, dimension, *amplitude, axis as u64)));
        }
        if rotation > 0.0 {
            targets.push(("transform/rotation".into(), None, rotation, 3));
        }
        if scale > 0.0 {
            for axis in 0..if layer.is_3d() { 3 } else { 2 } {
                targets.push(("transform/scale".into(), Some(axis), scale, 4));
            }
        }
        let mut by_property: std::collections::BTreeMap<Uid, (String, Vec<(Option<usize>, f64, u64)>)> = Default::default();
        for (path, dimension, amplitude, stream) in targets {
            let pr = checked_property(layer, &path, SHAKE)?;
            by_property.entry(pr.uid).or_insert_with(|| (path, vec![])).1.push((dimension, amplitude, stream));
        }
        for (uid, (path, offsets)) in by_property {
            let pr = checked_property(layer, &path, SHAKE)?;
            let mut keys = vec![];
            for frame in first..=last {
                let ct = comp.frame_rate.tick_of(frame);
                let lt = layer.layer_time(ct);
                let u = (ct - ct0).seconds() / (ct1 - ct0).seconds();
                let fade = (u / 0.1).min(1.0) * ((1.0 - u) / 0.1).min(1.0);
                let envelope = if frame == first || frame == last { 0.0 } else { fade * (-decay * u).exp() };
                let mut value = pr.value_at(lt);
                if value.kind_name() != pr.value.kind_name() {
                    return Err(bad(SHAKE, "a keyframe has the wrong value type"));
                }
                validate_value(&value, SHAKE)?;
                for (axis, amplitude, stream) in &offsets {
                    value = delta(
                        &value,
                        *axis,
                        amplitude * envelope * noise(seed, id.0.wrapping_mul(17).wrapping_add(*stream), (ct - ct0).seconds() * frequency),
                        SHAKE,
                    )?;
                }
                keys.push(Keyframe::new(lt, value));
            }
            keys.sort_by_key(|key| key.time);
            if keys.windows(2).any(|pair| pair.first().map(|key| key.time) == pair.get(1).map(|key| key.time)) {
                return Err(bad(SHAKE, "layer stretch collapses composition frames; reduce the stretch"));
            }
            edits.push(Edit { layer: id, uid, start: lt0.min(lt1), end: lt0.max(lt1), keys });
        }
    }
    if edits.is_empty() {
        return Err(bad(SHAKE, "set at least one nonzero shake amplitude"));
    }
    commit(s, cid, "Native Seeded Shake", edits)
}

// Portable settings reuse the existing native handlers; this is not command replay.
const SAVE_SETTINGS: &str = "anim.saveNativePreset";
pub(super) const MAX_NATIVE_PRESET_BYTES: usize = 64 * 1024;
const MAX_NATIVE_PRESET_TARGETS: usize = 4096;
const ACTION_KEYS: &[&str] = &["action", "duration", "stagger", "reverse", "axis", "distance", "amount", "angle", "scalePercent", "overshoot", "ease", "curve"];
const SHAKE_KEYS: &[&str] = &["duration", "frequency", "decay", "x", "y", "z", "rotation", "scale", "seed"];

fn only_keys<'a>(value: &'a Value, allowed: &[&str], command: &str) -> Result<&'a serde_json::Map<String, Value>> {
    let object = value.as_object().ok_or_else(|| bad(command, "expected a settings object"))?;
    if object.len() > allowed.len() || object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(bad(command, "unsupported field in native motion preset"));
    }
    Ok(object)
}

fn supported_curve(value: &Value) -> Result<EaseCurve> {
    let error = || bad(SAVE_SETTINGS, "curve must contain supported finite Bézier influences and speeds");
    match value {
        Value::Array(values) if values.len() == 4 => {
            let handles =
                values.iter().map(Value::as_f64).collect::<Option<Vec<_>>>().and_then(|values| <[f64; 4]>::try_from(values).ok()).ok_or_else(error)?;
            if handles.iter().any(|number| !number.is_finite()) {
                return Err(error());
            }
            let curve = EaseCurve::from_handles(handles).ok_or_else(error)?;
            if curve.handles().iter().zip(handles).any(|(actual, requested)| (actual - requested).abs() > 16.0 * f64::EPSILON * requested.abs().max(1.0)) {
                return Err(error());
            }
            Ok(curve)
        }
        Value::Object(_) => {
            let object = only_keys(value, &["outInfluence", "outSpeed", "inInfluence", "inSpeed"], SAVE_SETTINGS)?;
            if object.len() != 4 {
                return Err(error());
            }
            if object.values().any(|value| value.as_f64().is_none_or(|number| !number.is_finite())) {
                return Err(error());
            }
            let curve: EaseCurve = serde_json::from_value(value.clone()).map_err(|_| error())?;
            if curve.sanitized() != Some(curve) {
                return Err(error());
            }
            Ok(curve)
        }
        _ => Err(error()),
    }
}

fn native_settings(s: &Session, command: &str, settings: &Value, portable: bool) -> Result<Value> {
    match command {
        ACTION => {
            let object = only_keys(settings, ACTION_KEYS, SAVE_SETTINGS)?;
            for key in ["action", "axis", "ease"] {
                if let Some(value) = object.get(key)
                    && value.as_str().is_none_or(|text| text.is_empty() || text.len() > 256)
                {
                    return Err(bad(SAVE_SETTINGS, "action, axis and ease must be bounded text"));
                }
            }
            if portable && (object.contains_key("ease") || !object.contains_key("curve")) {
                return Err(bad(SAVE_SETTINGS, "portable action settings require curve values, not a library name"));
            }
            if object.contains_key("ease") && object.contains_key("curve") {
                return Err(bad(SAVE_SETTINGS, "choose ease or curve, not both"));
            }
            for (key, value) in object {
                if matches!(key.as_str(), "action" | "axis" | "ease" | "curve") {
                    continue;
                }
                let valid = if key == "reverse" { value.is_boolean() } else { value.as_f64().is_some_and(f64::is_finite) };
                if !valid {
                    return Err(bad(SAVE_SETTINGS, "native action settings must use their documented scalar types"));
                }
            }
            // Validate the complete curve before cloning the bounded scalar settings.
            let curve = object.get("curve").map(supported_curve).transpose()?;
            let mut normalized = object.clone();
            if let Some(curve) = curve {
                normalized.insert("curve".into(), json!(curve));
            }
            let normalized = Value::Object(normalized);
            let a = action_settings(s, &normalized)?;
            let axis = ["x", "y", "z"].get(a.axis).ok_or_else(|| bad(SAVE_SETTINGS, "unsupported slide axis"))?;
            Ok(json!({"action":a.action,"duration":a.duration,"stagger":a.stagger,"reverse":a.reverse,
                "axis":axis,"distance":a.distance,"amount":a.amount,"overshoot":a.overshoot,"curve":a.curve}))
        }
        SHAKE => {
            only_keys(settings, SHAKE_KEYS, SAVE_SETTINGS)?;
            let a = shake_settings(s, settings)?;
            let [x, y, z] = a.amplitudes;
            Ok(json!({"duration":a.duration,"frequency":a.frequency,"decay":a.decay,
                "x":x,"y":y,"z":z,
                "rotation":a.rotation,"scale":a.scale,"seed":a.seed}))
        }
        _ => Err(bad(SAVE_SETTINGS, "native presets support only anim.nativeAction and anim.nativeShake")),
    }
}

fn preset_name<'a>(value: Option<&'a Value>, fallback: &'a str) -> Result<&'a str> {
    let name = match value {
        Some(value) => value.as_str().filter(|name| name.len() <= 512).ok_or_else(|| bad(SAVE_SETTINGS, "name must be bounded text"))?.trim(),
        None => fallback,
    };
    if name.is_empty() || name.chars().take(129).count() > 128 {
        return Err(bad(SAVE_SETTINGS, "name must contain 1–128 characters"));
    }
    Ok(name)
}

fn save_native_preset(s: &mut Session, p: &Value) -> Result<Value> {
    only_keys(p, &["path", "name", "command", "settings"], SAVE_SETTINGS)?;
    let path = str_p(p, "path")
        .filter(|path| !path.is_empty() && path.len() <= 32768 && !path.contains('\0'))
        .ok_or_else(|| bad(SAVE_SETTINGS, "provide a bounded preset path"))?;
    let command = str_p(p, "command").ok_or_else(|| bad(SAVE_SETTINGS, "provide a native command"))?;
    let settings = native_settings(s, command, p.get("settings").ok_or_else(|| bad(SAVE_SETTINGS, "provide settings"))?, false)?;
    let name = preset_name(p.get("name"), if command == ACTION { "Native Animation Action" } else { "Native Seeded Shake" })?;
    let tag = super::animation::PRESET_TAG;
    let document = json!({tag:1,"name":name,"nativeMotion":{"version":1,"command":command,"settings":settings}});
    let bytes = serde_json::to_vec_pretty(&document).map_err(|error| bad(SAVE_SETTINGS, error.to_string()))?;
    if bytes.len() > MAX_NATIVE_PRESET_BYTES {
        return Err(bad(SAVE_SETTINGS, "native preset exceeds 64 KiB"));
    }
    s.services.write_file(path, &bytes).map_err(|error| bad(SAVE_SETTINGS, format!("cannot save native settings: {error}")))?;
    Ok(json!({"path":path,"command":command,"settings":settings}))
}

pub(super) fn apply_native_preset(s: &mut Session, document: &Value, p: &Value) -> Result<Value> {
    const C: &str = "anim.applyPreset";
    only_keys(p, &["path", "preset", "layers", "layer", "comp"], C)?;
    let target_id = |value: &Value| value.as_u64().is_some() || value.as_str().is_some_and(|text| !text.is_empty() && text.len() <= 256);
    for key in ["layer", "comp"] {
        if p.get(key).is_some_and(|value| !target_id(value)) {
            return Err(bad(C, "native preset target ids must be numbers or bounded text"));
        }
    }
    if p.get("path").is_some_and(|value| value.as_str().is_none_or(|path| path.is_empty() || path.len() > 32768 || path.contains('\0'))) {
        return Err(bad(C, "provide a bounded preset path"));
    }
    only_keys(document, &[super::animation::PRESET_TAG, "name", "nativeMotion"], C)?;
    if document.get(super::animation::PRESET_TAG).and_then(Value::as_u64) != Some(1) {
        return Err(bad(C, "unsupported native animation preset version"));
    }
    preset_name(document.get("name"), "Native Motion")?;
    let native = document.get("nativeMotion").ok_or_else(|| bad(C, "native motion settings are missing"))?;
    let object = only_keys(native, &["version", "command", "settings"], C)?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(bad(C, "unsupported native motion settings version"));
    }
    let command = object.get("command").and_then(Value::as_str).ok_or_else(|| bad(C, "native motion command is missing"))?;
    let settings = native_settings(s, command, object.get("settings").ok_or_else(|| bad(C, "native motion settings are missing"))?, true)?;
    let mut params = settings.as_object().cloned().ok_or_else(|| bad(C, "native motion settings are invalid"))?;
    if p.get("layers")
        .is_some_and(|layers| layers.as_array().is_none_or(|layers| layers.len() > MAX_NATIVE_PRESET_TARGETS || layers.iter().any(|value| !target_id(value))))
        || (p.get("layers").is_none() && p.get("layer").is_none() && s.state.selected_layers.len() > MAX_NATIVE_PRESET_TARGETS)
    {
        return Err(bad(C, "select at most 4096 layers per native preset"));
    }
    for key in ["comp", "layer", "layers"] {
        if let Some(value) = p.get(key) {
            params.insert(key.into(), value.clone());
        }
    }
    let params = Value::Object(params);
    // No nested command dispatch: the only two routes call their checked planners,
    // then create exactly one native edit after all targets have been validated.
    match command {
        ACTION => action(s, &params),
        SHAKE => shake(s, &params),
        _ => Err(bad(C, "unsupported native motion command")),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "anim.saveNativePreset",
            "Save Native Motion Settings",
            [],
            None,
            "{path (.ecpreset),name?,command:anim.nativeAction|anim.nativeShake,settings:{...}} — saves portable parameters, not absolute keys or target ids",
            always,
            save_native_preset
        ),
        cmd!(
            "anim.nativeAction",
            "Animation Action...",
            ["Animation", "Native Motion Tools"],
            None,
            "{layers?, layer?, comp?, action?:fadeIn|fadeOut|slideIn|slideOut|scaleIn|scaleOut|rotateIn|rotateOut, time?:seconds, duration?:seconds, stagger?:seconds, reverse?:bool, axis?:x|y|z, distance?:pixels, amount?:number, angle?:degrees, scalePercent?:percent, overshoot?:percent, ease?:name, curve?:EaseCurve}",
            has_layers,
            action
        ),
        cmd!(
            "anim.nativeShake",
            "Seeded Shake...",
            ["Animation", "Native Motion Tools"],
            None,
            "{layers?, layer?, comp?, time?:seconds, duration?:seconds, frequency?:Hz, decay?:number, x?:pixels, y?:pixels, z?:pixels, rotation?:degrees, scale?:percent, seed?:integer}",
            has_layers,
            shake
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_timeline::{animate, prop, setup};

    #[test]
    fn batch_stagger_locked_reverse_and_undo() {
        let (mut s, a) = setup();
        let b = s.execute("layer.newNull", json!({})).unwrap()["layer"].as_u64().unwrap();
        let locked = s.execute("layer.newNull", json!({})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.setSwitch", json!({"layer": locked, "switch":"lock", "value":true})).unwrap();
        let before = s.project.clone();
        let undo = s.history.undo.len();
        let options = json!({"layers":[a,b,locked], "action":"fadeIn", "duration":1, "stagger":0.2, "reverse":true});
        s.execute_checked(ACTION, options.clone()).unwrap();
        assert_eq!(s.history.undo.len(), undo + 1);
        assert_eq!(prop(&s, a, "transform/opacity").keys[0].time, Tick::ZERO);
        assert_eq!(prop(&s, b, "transform/opacity").keys[0].time, Tick::from_seconds_f64(0.2));
        assert!(prop(&s, locked, "transform/opacity").keys.is_empty());
        assert_eq!(prop(&s, a, "transform/opacity").keys[1].value, KV::Scalar(100.0));
        s.undo();
        assert!(s.project.same_content(&before));
        s.redo();
        let after = s.project.clone();
        assert!(s.execute(ACTION, json!({"layers":[a,b],"duration":500})).is_err());
        assert!(s.project.same_content(&after));
    }

    #[test]
    fn actions_support_separated_position_and_negative_stretch() {
        let (mut s, layer) = setup();
        s.execute("prop.separateDimensions", json!({"layer":layer,"path":"transform/position"})).unwrap();
        s.execute(ACTION, json!({"layer":layer,"action":"slideIn","axis":"x","distance":300,"duration":1})).unwrap();
        assert_eq!(prop(&s, layer, "transform/positionX").keys[0].value, KV::Scalar(500.0));
        assert!(prop(&s, layer, "transform/positionY").keys.is_empty());
        s.execute("layer.timeStretch", json!({"layer":layer,"percent":-100})).unwrap();
        s.execute(ACTION, json!({"layer":layer,"action":"scaleIn","amount":10,"overshoot":15,"duration":1})).unwrap();
        let p = prop(&s, layer, "transform/scale");
        assert_eq!(p.keys.len(), 3);
        assert!(p.keys.windows(2).all(|pair| pair[0].time < pair[1].time));
        assert!(p.keys.iter().all(|key| key.value.as_vec3()[2] == 100.0));
    }

    #[test]
    fn shake_is_deterministic_restores_endpoints_and_preserves_existing_motion() {
        let (mut s, layer) = setup();
        animate(&mut s, layer, "transform/position", &[(0.0, json!([100, 100])), (2.0, json!([300, 100]))]);
        let before = prop(&s, layer, "transform/position");
        let options = json!({"layer":layer,"duration":2,"x":20,"y":0,"rotation":0,"decay":0,"seed":99});
        s.execute_checked(SHAKE, options.clone()).unwrap();
        let after = prop(&s, layer, "transform/position");
        assert_eq!(after.keys.len(), 61);
        assert_eq!(after.keys.first().unwrap().value, before.keys.first().unwrap().value);
        assert_eq!(after.keys.last().unwrap().value, before.keys.last().unwrap().value);
        assert!(after.keys.iter().all(|key| (key.value.as_vec3()[1] - 100.0).abs() < 1e-9));
        assert!(after.keys.iter().any(|key| (key.value.as_vec3()[0] - before.value_at(key.time).as_vec3()[0]).abs() > 1.0));
        s.undo();
        s.execute(SHAKE, options).unwrap();
        assert_eq!(prop(&s, layer, "transform/position").keys, after.keys);
        let text = s.project.to_file_json().unwrap();
        let reopened = effectcraft_project::Project::from_json(&text).unwrap();
        assert!(reopened.same_content(&s.project));
    }

    #[test]
    fn invalid_actions_and_shakes_are_atomic() {
        let (mut s, layer) = setup();
        let before = s.project.clone();
        let count = s.history.undo.len();
        for (command, p) in [
            (ACTION, json!({"duration":"bad"})),
            (ACTION, json!({"axis":"z","action":"slideIn"})),
            (SHAKE, json!({"seed":-1})),
            (SHAKE, json!({"frequency":1e300})),
            (SHAKE, json!({"duration":0})),
        ] {
            assert!(s.execute(command, p).is_err());
            assert!(s.project.same_content(&before));
            assert_eq!(s.history.undo.len(), count);
        }
        s.execute("prop.setExpression", json!({"layer":layer,"path":"transform/opacity","expression":"value"})).unwrap();
        let before = s.project.clone();
        assert!(s.execute(ACTION, json!({})).is_err());
        assert!(s.project.same_content(&before));
        let cid = s.active_comp_id().unwrap();
        s.edit("Hostile stretch", None, |project, _| {
            project.comp_mut(cid).unwrap().layer_mut(effectcraft_project::LayerId(layer)).unwrap().stretch = 1e300;
            Ok(())
        })
        .unwrap();
        let before = s.project.clone();
        assert!(s.execute(SHAKE, json!({})).is_err());
        assert!(s.project.same_content(&before));
        for (stretch, overshoot) in [(2e13, 10), (1e-300, 0)] {
            s.edit("Hostile stretch", None, |project, _| {
                project.comp_mut(cid).unwrap().layer_mut(effectcraft_project::LayerId(layer)).unwrap().stretch = stretch;
                Ok(())
            })
            .unwrap();
            let before = s.project.clone();
            assert!(s.execute(SHAKE, json!({"duration":1})).is_err());
            assert!(s.execute(ACTION, json!({"action":"scaleIn","duration":1,"overshoot":overshoot})).is_err());
            assert!(s.project.same_content(&before));
        }
    }
}

#[cfg(test)]
mod native_preset_tests {
    use super::*;
    use effectcraft_project::LayerId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture(label: &str) -> (Session, u64, std::path::PathBuf) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let mut s = Session::default();
        s.execute("comp.new", json!({"width":160,"height":90,"duration":6,"frameRate":120})).unwrap();
        let target = s.execute("layer.newNull", json!({"name":"Target"})).unwrap()["layer"].as_u64().unwrap();
        for (time, value) in [(0.0, [800.0, 400.0, 0.0]), (4.0, [1200.0, 800.0, 0.0])] {
            s.execute("prop.addKey", json!({"layer":target,"path":"transform/position","time":time,"value":value})).unwrap();
        }
        s.execute("time.set", json!({"time":1})).unwrap();
        let path = std::env::temp_dir().join(format!("ec-native-settings-{label}-{}-{}.ecpreset", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        (s, target, path)
    }

    fn property(s: &Session, target: u64, name: &str) -> Property {
        s.active_comp().unwrap().layer(LayerId(target)).unwrap().props.prop(name).unwrap().clone()
    }

    fn load(s: &Session, path: &std::path::Path) -> Value {
        serde_json::from_slice(&s.services.read_file(path.to_str().unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn action_settings_keep_actual_curve_and_regenerate_at_target_cti_with_locks_and_one_undo() {
        let (mut s, target, path) = fixture("action");
        let locked = s.execute("layer.newNull", json!({"name":"Locked"})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.setSwitch", json!({"layer":locked,"switch":"locked","value":true})).unwrap();
        s.execute("keys.easePreset.save", json!({"name":"Portable custom","curve":[0.4,3,0.6,-2]})).unwrap();
        let before = s.project.clone();
        let undo = s.history.undo.len();
        s.execute(
            SAVE_SETTINGS,
            json!({"path":path,"command":ACTION,"settings":{"action":"slideIn","axis":"x","distance":-240,"duration":0.6,"ease":"Portable custom"}}),
        )
        .unwrap();
        assert!(s.project.same_content(&before));
        assert_eq!(s.history.undo.len(), undo);
        let document = load(&s, &path);
        let settings = &document["nativeMotion"]["settings"];
        assert!(settings.get("ease").is_none());
        assert_eq!(settings["curve"]["outSpeed"], json!(7.5));
        for forbidden in ["time", "layer", "layers", "comp"] {
            assert!(settings.get(forbidden).is_none());
        }
        s.ease_presets.clear(); // The destination need not have the source's curve library.
        s.execute("time.set", json!({"time":2})).unwrap();
        let original = property(&s, target, "transform/position");
        s.execute("anim.applyPreset", json!({"path":path,"layers":[locked,target]})).unwrap();
        assert_eq!(s.history.undo.len(), undo + 1);
        let result = property(&s, target, "transform/position");
        for key in &original.keys {
            assert!(result.keys.contains(key), "keys outside the regenerated interval are retained");
        }
        let end = Tick::from_seconds_f64(2.6);
        assert_eq!(result.value_at(end), original.value_at(end));
        assert_eq!(result.value_at(Tick::from_seconds_f64(2.0)).as_vec3()[0], original.value_at(end).as_vec3()[0] - 240.0);
        let cid = s.active_comp_id().unwrap();
        assert_eq!(s.project.comp(cid).unwrap().layer(LayerId(locked)).unwrap().props, before.comp(cid).unwrap().layer(LayerId(locked)).unwrap().props);
        let after = s.project.clone();
        s.undo();
        assert!(s.project.same_content(&before));
        let mut direct = settings.as_object().unwrap().clone();
        direct.insert("layers".into(), json!([locked, target]));
        s.execute(ACTION, Value::Object(direct)).unwrap();
        assert!(s.project.same_content(&after), "preset regenerates exactly the same native action, not an absolute source snapshot");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn shake_settings_add_to_target_motion_and_seed_survives_file_roundtrip() {
        let (mut s, target, path) = fixture("shake");
        let settings = json!({"duration":1,"frequency":9,"decay":2,"x":20,"y":12,"rotation":3,"scale":5,"seed":u64::MAX});
        let before = s.project.clone();
        s.execute(SAVE_SETTINGS, json!({"path":path,"command":SHAKE,"settings":settings})).unwrap();
        let document = load(&s, &path);
        assert_eq!(document["nativeMotion"]["settings"]["seed"].as_u64(), Some(u64::MAX));
        let undo = s.history.undo.len();
        s.execute("anim.applyPreset", json!({"preset":document,"layer":target})).unwrap();
        assert_eq!(s.history.undo.len(), undo + 1);
        let result = property(&s, target, "transform/position");
        let cid = s.active_comp_id().unwrap();
        let original = before.comp(cid).unwrap().layer(LayerId(target)).unwrap().props.prop("transform/position").unwrap();
        for key in &original.keys {
            assert!(result.keys.contains(key));
        }
        for time in [1.0, 2.0] {
            let time = Tick::from_seconds_f64(time);
            assert_eq!(result.value_at(time), original.value_at(time), "shake endpoints retain target animation");
        }
        assert!(
            result
                .keys
                .iter()
                .filter(|key| key.time > Tick::from_seconds_f64(1.0) && key.time < Tick::from_seconds_f64(2.0))
                .any(|key| key.value != original.value_at(key.time))
        );
        let after = s.project.clone();
        s.undo();
        assert!(s.project.same_content(&before));
        let mut direct = settings.as_object().unwrap().clone();
        direct.insert("layer".into(), json!(target));
        s.execute(SHAKE, Value::Object(direct)).unwrap();
        assert!(s.project.same_content(&after), "seeded preset and direct settings use the identical checked handler");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_native_documents_and_failed_plans_leave_project_history_and_recent_preferences_unchanged() {
        let (mut s, target, path) = fixture("invalid");
        s.execute(SAVE_SETTINGS, json!({"path":path,"command":SHAKE,"settings":{"seed":7}})).unwrap();
        let valid = load(&s, &path);
        let mut invalid = Vec::new();
        for (pointer, value) in [
            ("/effectcraftAnimationPreset", json!(2)),
            ("/nativeMotion/version", json!(2)),
            ("/nativeMotion/command", json!("script.run")),
            ("/nativeMotion/settings", json!([])),
            ("/nativeMotion/settings/seed", json!(1.5)),
            ("/nativeMotion/settings/duration", json!(600)),
        ] {
            let mut doc = valid.clone();
            *doc.pointer_mut(pointer).unwrap() = value;
            invalid.push(doc);
        }
        for key in ["time", "comp", "layer", "layers", "script", "command", "path", "curve"] {
            let mut doc = valid.clone();
            doc["nativeMotion"]["settings"][key] = json!(1);
            invalid.push(doc);
        }
        let mut doc = valid.clone();
        doc["effects"] = json!([]); // A native file cannot also replay absolute/effect payloads.
        invalid.push(doc);
        let mut doc = valid.clone();
        doc["name"] = json!("x".repeat(129));
        invalid.push(doc);
        let before = s.project.clone();
        let undo = s.history.undo.len();
        let prefs = serde_json::to_value(&s.prefs).unwrap();
        let revision = s.prefs_revision;
        for document in invalid {
            assert!(s.execute("anim.applyPreset", json!({"path":path,"preset":document,"layer":target})).is_err());
            assert!(s.project.same_content(&before));
            assert_eq!(s.history.undo.len(), undo);
            assert_eq!(serde_json::to_value(&s.prefs).unwrap(), prefs);
            assert_eq!(s.prefs_revision, revision);
        }
        s.execute("layer.setSwitch", json!({"layer":target,"switch":"locked","value":true})).unwrap();
        let locked = s.project.clone();
        assert!(s.execute("anim.applyPreset", json!({"path":path,"preset":valid,"layer":target})).is_err());
        assert!(s.project.same_content(&locked));
        assert_eq!(serde_json::to_value(&s.prefs).unwrap(), prefs);
        assert_eq!(s.prefs_revision, revision);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn native_file_and_target_budgets_fail_before_editing_or_updating_recent_presets() {
        let (mut s, target, path) = fixture("budget");
        s.execute(SAVE_SETTINGS, json!({"path":path,"command":SHAKE,"settings":{"seed":7}})).unwrap();
        let document = load(&s, &path);
        let before = s.project.clone();
        let undo = s.history.undo.len();
        let prefs = serde_json::to_value(&s.prefs).unwrap();
        let revision = s.prefs_revision;
        // Valid JSON with excessive trailing whitespace still exceeds the file budget.
        let mut bytes = serde_json::to_vec(&document).unwrap();
        bytes.resize(MAX_NATIVE_PRESET_BYTES + 1, b' ');
        s.services.write_file(path.to_str().unwrap(), &bytes).unwrap();
        let error = s.execute("anim.applyPreset", json!({"path":path,"layer":target})).unwrap_err();
        assert!(error.to_string().contains("64 KiB"));
        for params in [
            json!({"preset":document,"layers":vec![target; MAX_NATIVE_PRESET_TARGETS + 1]}),
            json!({"preset":document,"layers":[{"nested":"not an id"}]}),
            json!({"preset":document,"layer":target,"time":0}),
        ] {
            assert!(s.execute("anim.applyPreset", params).is_err());
        }
        assert!(s.project.same_content(&before));
        assert_eq!(s.history.undo.len(), undo);
        assert_eq!(serde_json::to_value(&s.prefs).unwrap(), prefs);
        assert_eq!(s.prefs_revision, revision);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn saving_rejects_context_command_replay_bad_types_and_unsupported_curves_without_writing() {
        let (mut s, _, path) = fixture("save-invalid");
        let before = s.project.clone();
        let undo = s.history.undo.len();
        for (command, settings) in [
            ("script.run", json!({})),
            (SHAKE, json!({"time":1})),
            (SHAKE, json!({"layers":[1]})),
            (SHAKE, json!({"duration":"1"})),
            (SHAKE, json!({"seed":-1})),
            (ACTION, json!({"curve":[0,0,1,1]})),
            (ACTION, json!({"curve":{"outInfluence":40,"outSpeed":7.5,"inInfluence":40,"inSpeed":7.5,"script":"bad"}})),
        ] {
            assert!(s.execute(SAVE_SETTINGS, json!({"path":path,"command":command,"settings":settings})).is_err());
            assert!(!path.exists());
            assert!(s.project.same_content(&before));
            assert_eq!(s.history.undo.len(), undo);
        }
    }
}

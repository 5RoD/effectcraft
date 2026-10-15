//! Parameter forms for the original native animation tools.

use super::forms::{Field, form};
use crate::EffectcraftApp;
use serde_json::{Value, json};

pub fn open_form(app: &mut EffectcraftApp, id: &str, p: &Value) -> bool {
    if !matches!(id, "anim.nativeAction" | "anim.nativeShake")
        || !p.as_object().is_none_or(|o| o.keys().all(|key| matches!(key.as_str(), "comp" | "layer" | "layers")))
    {
        return false;
    }
    let start = app.session.time().seconds();
    let remaining = app.session.active_comp().map(|c| (c.duration.seconds() - start).max(c.frame_duration().seconds())).unwrap_or(1.0);
    let (title, fields) = if id == "anim.nativeAction" {
        let curves: Vec<(String, Value)> =
            effectcraft_engine::ease_presets::presets(&app.session).map(|(name, _, _)| (name.to_string(), json!(name))).collect();
        let names: Vec<(&str, Value)> = curves.iter().map(|(name, value)| (name.as_str(), value.clone())).collect();
        let selected = curves.iter().position(|(name, _)| name == "Ease In-Out").unwrap_or(0);
        (
            "Native Animation Action",
            vec![
                Field::choice(
                    "action",
                    "Action",
                    &[
                        ("Fade In", json!("fadeIn")),
                        ("Fade Out", json!("fadeOut")),
                        ("Slide In", json!("slideIn")),
                        ("Slide Out", json!("slideOut")),
                        ("Scale In", json!("scaleIn")),
                        ("Scale Out", json!("scaleOut")),
                        ("Rotate In", json!("rotateIn")),
                        ("Rotate Out", json!("rotateOut")),
                    ],
                    0,
                ),
                Field::num("time", "Start (comp seconds)", start),
                Field::num("duration", "Duration (seconds)", 0.6_f64.min(remaining)),
                Field::num("stagger", "Layer stagger (seconds)", 0.0),
                Field::bool("reverse", "Stagger bottom to top", false),
                Field::choice("axis", "Slide axis", &[("X", json!("x")), ("Y", json!("y")), ("Z (3D)", json!("z"))], 1),
                Field::num("distance", "Slide offset (pixels)", 200.0),
                Field::num("scalePercent", "Scale endpoint (%)", 0.0),
                Field::num("angle", "Rotation offset (degrees)", -90.0),
                Field::num("overshoot", "Slide/Scale/Rotate overshoot (%)", 0.0),
                Field::choice("ease", "Easing curve", &names, selected),
                Field::note(
                    "help",
                    "Selected unlocked layers; one undo step. Replaces keys inside the interval. Save Animation Preset stores an absolute keyframe snapshot.",
                ),
            ],
        )
    } else {
        (
            "Native Seeded Shake",
            vec![
                Field::num("time", "Start (comp seconds)", start),
                Field::num("duration", "Duration (seconds)", 1.0_f64.min(remaining)),
                Field::num("frequency", "Frequency (Hz)", 8.0),
                Field::num("decay", "Decay (0 = sustained)", 3.0),
                Field::num("x", "X amplitude (pixels)", 20.0),
                Field::num("y", "Y amplitude (pixels)", 20.0),
                Field::num("z", "Z amplitude (3D pixels)", 0.0),
                Field::num("rotation", "Rotation amplitude (degrees)", 2.0),
                Field::num("scale", "Scale amplitude (percentage points)", 0.0),
                Field::num("seed", "Random seed (nonnegative integer)", 1.0),
                Field::note(
                    "help",
                    "Adds reproducible motion. Endpoints unchanged; one undo step. Rerun these settings to shake another animation; Save Animation Preset stores absolute keys.",
                ),
            ],
        )
    };
    form(app, title, id, p.clone(), fields);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dialog, menus};
    use effectcraft_engine::Session;
    use egui_kittest::Harness;

    #[test]
    fn native_motion_dialogs_register_controls_and_apply_defaults() {
        for size in [egui::vec2(1600.0, 1000.0), egui::vec2(640.0, 480.0)] {
            let mut session = Session::default();
            session.execute("comp.new", json!({"width":64,"height":64,"duration":3})).unwrap();
            session.execute("layer.newNull", json!({})).unwrap();
            let mut h = Harness::builder().with_size(size).build_eframe(|_| EffectcraftApp::new(session));
            h.run_steps(3);
            for command in ["anim.nativeAction", "anim.nativeShake"] {
                let ctx = h.ctx.clone();
                menus::invoke(h.state_mut(), &ctx, command, json!({})).unwrap();
                assert!(matches!(h.state().dialog, Some(Dialog::Form)));
                h.run_steps(3);
                let ids: Vec<&str> = h.state().auto.previous.iter().chain(h.state().auto.elements.iter()).map(|e| e.id.as_str()).collect();
                for id in ["form.field.time", "form.field.duration", "form.ok", "form.cancel"] {
                    assert!(ids.contains(&id), "{command} missing {id}");
                }
                let rect = |r: [f32; 4]| egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]));
                let dialog = rect(h.state().auto.find("form.dialog").unwrap().rect);
                for id in ["form.ok", "form.cancel"] {
                    let button = rect(h.state().auto.find(id).unwrap().rect);
                    assert!(dialog.contains_rect(button), "{command} clips {id} at {size:?}: {dialog:?} {button:?}");
                    assert!(h.ctx.content_rect().contains_rect(button), "{command} footer leaves the viewport at {size:?}");
                }
                if command == "anim.nativeAction" {
                    let fields = &mut h.state_mut().dialog_state.form.fields;
                    if let Some(Field { kind: super::super::forms::FieldKind::Number { value, .. }, .. }) = fields.iter_mut().find(|f| f.key == "overshoot") {
                        *value = 20.0;
                    }
                    // Switching back to Fade must ignore a retained Scale/Slide overshoot value.
                    assert!(h.state().dialog_state.form.params().get("overshoot").is_none());
                }
                let form = h.state().dialog_state.form.clone();
                h.state_mut().dialog = None;
                let count = h.state().session.history.undo.len();
                h.state_mut().session.execute_checked(&form.command, form.params()).unwrap();
                assert_eq!(h.state().session.history.undo.len(), count + 1);
                h.state_mut().session.undo();
            }
        }
    }
}

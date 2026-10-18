//! Install the original counter rig; font and character styling use the ordinary Character panel.

use serde_json::{Value, json};

use super::forms::{Field, form};
use crate::EffectcraftApp;

pub fn open_form(app: &mut EffectcraftApp, id: &str, params: &Value) -> bool {
    if id != "text.nativeCounter" || !params.as_object().is_none_or(|params| params.keys().all(|key| matches!(key.as_str(), "comp" | "layer" | "layers"))) {
        return false;
    }
    form(
        app,
        "Native Text Counter",
        id,
        params.clone(),
        vec![
            Field::num("value", "Value / clock offset", 0.0),
            Field::choice("format", "Format", &[("Number", json!("number")), ("Time", json!("time"))], 0),
            Field::num("decimals", "Decimal places (0–10; time ≤6)", 0.0),
            Field::num("padding", "Minimum integer digits (0–32)", 0.0),
            Field::choice(
                "grouping",
                "Digit grouping",
                &[("None", json!("none")), ("1,234.56", json!("comma")), ("1.234,56", json!("period")), ("1 234.56", json!("space"))],
                0,
            ),
            Field::text("prefix", "Prefix", ""),
            Field::text("suffix", "Suffix", ""),
            Field::choice(
                "figureWidth",
                "Digit width",
                &[
                    ("Keep current styling", json!("preserve")),
                    ("Tabular digits (font support required)", json!("tabular")),
                    ("Proportional digits", json!("proportional")),
                ],
                0,
            ),
            Field::bool("currentTime", "Use layer clock", false),
            Field::bool("countDown", "Count down (layer clock)", false),
            Field::num("clockRate", "Clock rate", 1.0),
            Field::num("clockStart", "Clock start (layer seconds)", 0.0),
            Field::choice("timeStyle", "Time style", &[("HH:MM:SS", json!("hms")), ("MM:SS", json!("ms")), ("Seconds", json!("seconds"))], 0),
            Field::note(
                "help",
                "Select ordinary text layers. Choose fonts and styling in Character; animate Value in Effect Controls. One undo step; existing Source Text expressions are protected.",
            ),
        ],
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dialog, menus};
    use effectcraft_engine::Session;
    use egui_kittest::Harness;

    #[test]
    fn native_text_counter_dialog_has_controls_and_installs_defaults() {
        let mut session = Session::default();
        session.execute("comp.new", json!({"width":64,"height":64,"duration":3})).unwrap();
        session.execute("layer.newText", json!({"text":"Counter","size":40})).unwrap();
        let mut harness = Harness::builder().with_size(egui::vec2(1680.0, 1020.0)).build_eframe(|_| EffectcraftApp::new(session));
        harness.run_steps(3);
        let ctx = harness.ctx.clone();
        menus::invoke(harness.state_mut(), &ctx, "text.nativeCounter", json!({})).unwrap();
        assert!(matches!(harness.state().dialog, Some(Dialog::Form)));
        harness.run_steps(3);
        let ids: Vec<&str> = harness.state().auto.previous.iter().chain(harness.state().auto.elements.iter()).map(|element| element.id.as_str()).collect();
        for id in ["form.field.value", "form.field.format", "form.field.prefix", "form.field.suffix", "form.field.figureWidth", "form.ok", "form.cancel"] {
            assert!(ids.contains(&id), "missing {id}");
        }
        let form = harness.state().dialog_state.form.clone();
        assert_eq!(form.params()["figureWidth"], "preserve");
        let count = harness.state().session.history.undo.len();
        harness.state_mut().dialog = None;
        harness.state_mut().session.execute_checked(&form.command, form.params()).unwrap();
        assert_eq!(harness.state().session.history.undo.len(), count + 1);
        harness.state_mut().session.undo();
    }
}

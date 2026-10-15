//! Native forms save parameter presets through the normal .ecpreset file dialog.

use std::cell::RefCell;
use std::rc::Rc;

use effectcraft_engine::Session;
use effectcraft_ui_egui::{Dialog, EffectcraftApp, menus};
use egui::{Event, Modifiers, PointerButton, Rect, pos2, vec2};
use egui_kittest::Harness;
use serde_json::{Value, json};

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> Rect {
    let [x, y, w, height] = h.state().auto.find(id).unwrap_or_else(|| panic!("missing {id}")).rect;
    Rect::from_min_size(pos2(x, y), vec2(w, height))
}

#[test]
fn saving_native_settings_from_short_forms_uses_ecpreset_and_never_applies_to_the_source() {
    for command in ["anim.nativeAction", "anim.nativeShake"] {
        let mut session = Session::default();
        session.execute("comp.new", json!({"width":320,"height":180,"duration":6,"frameRate":120})).unwrap();
        let source = session.execute("layer.newNull", json!({})).unwrap()["layer"].as_u64().unwrap();
        session.execute("time.set", json!({"time":2})).unwrap();
        let path = std::env::temp_dir().join(format!("ec-native-settings-form-{command}-{}.ecpreset", std::process::id()));
        let selected = path.to_str().unwrap().to_string();
        let asked: Rc<RefCell<Vec<String>>> = Rc::default();
        let mut app = EffectcraftApp::new(session);
        let requests = asked.clone();
        app.hooks.pick_save_file = Some(Box::new(move |name, extension| {
            requests.borrow_mut().push(format!("{name}:{extension}"));
            Some(selected.clone())
        }));
        let mut h = Harness::builder().with_size(vec2(640.0, 480.0)).build_eframe(|_| app);
        h.run_steps(3);
        let context = h.ctx.clone();
        menus::invoke(h.state_mut(), &context, command, json!({"layers":[source]})).unwrap();
        h.run_steps(3);
        let project = h.state().session.project.clone();
        let undo = h.state().session.history.undo.len();
        let button = rect(&h, "form.saveSettings");
        assert!(rect(&h, "form.dialog").contains_rect(button));
        assert!(h.ctx.content_rect().contains_rect(button));
        let pos = button.center();
        for pressed in [true, false] {
            h.input_mut()
                .events
                .extend([Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
            h.step();
        }
        h.run_steps(3);
        assert_eq!(&*asked.borrow(), &["Native Motion.ecpreset:ecpreset"]);
        assert!(matches!(h.state().dialog, Some(Dialog::Form)), "saving does not apply or close the source form");
        assert!(h.state().session.project.same_content(&project));
        assert_eq!(h.state().session.history.undo.len(), undo);
        let document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["nativeMotion"]["command"], json!(command));
        let settings = document["nativeMotion"]["settings"].as_object().unwrap();
        for key in ["time", "layers", "layer", "comp"] {
            assert!(!settings.contains_key(key), "source context must not travel in a parameter preset");
        }
        assert!(document.get("properties").is_none() && document.get("effects").is_none());
        let _ = std::fs::remove_file(path);
    }
}

//! Precise native easing entry, clipboard, overshoot handles and short-panel access.

use effectcraft_engine::{Session, keyframe::EaseCurve};
use effectcraft_ui_egui::{EffectcraftApp, dock::PanelKind};
use egui::{Event, Key, Modifiers, OutputCommand, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn harness(width: f32, height: f32) -> Harness<'static, EffectcraftApp> {
    let mut session = Session::default();
    session.execute("comp.new", json!({"width":64,"height":64,"duration":3})).unwrap();
    let layer = session.execute("layer.newNull", json!({})).unwrap()["layer"].as_u64().unwrap();
    for (time, value) in [(0.0, 10.0), (1.0, 90.0)] {
        session.execute("prop.addKey", json!({"layer":layer,"path":"transform/opacity","time":time,"value":value})).unwrap();
    }
    session.execute("prop.select", json!({"layer":layer,"path":"transform/opacity"})).unwrap();
    session.execute("keys.selectAll", json!({})).unwrap();
    let mut app = EffectcraftApp::new(session);
    app.ui.start_screen = false;
    app.show_panel(PanelKind::EasePresets);
    app.ui.maximized = Some(PanelKind::EasePresets);
    let mut harness = Harness::builder().with_size(vec2(width, height)).build_eframe(|_| app);
    harness.run_steps(3);
    harness
}

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> Rect {
    let [x, y, w, height] = h.state().auto.find(id).unwrap_or_else(|| panic!("missing {id}")).rect;
    Rect::from_min_size(pos2(x, y), vec2(w, height))
}

fn click_at(h: &mut Harness<'_, EffectcraftApp>, point: Pos2) -> Vec<String> {
    let mut copied = Vec::new();
    for pressed in [true, false] {
        h.input_mut()
            .events
            .extend([Event::PointerMoved(point), Event::PointerButton { pos: point, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        h.step();
        for command in &h.output().platform_output.commands {
            if let OutputCommand::CopyText(text) = command {
                copied.push(text.clone());
            }
        }
    }
    h.run_steps(2);
    copied
}

fn click(h: &mut Harness<'_, EffectcraftApp>, id: &str) -> Vec<String> {
    let point = rect(h, id).center();
    click_at(h, point)
}

fn enter_text(h: &mut Harness<'_, EffectcraftApp>, id: &str, text: &str) {
    click(h, id);
    let command = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
    h.input_mut().events.extend([
        Event::ModifiersChanged(command),
        Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: command },
        Event::Key { key: Key::A, physical_key: None, pressed: false, repeat: false, modifiers: command },
        Event::ModifiersChanged(Modifiers::NONE),
        Event::Paste(text.into()),
    ]);
    h.step();
    for pressed in [true, false] {
        h.input_mut().events.push(Event::Key { key: Key::Enter, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(2);
}

#[test]
fn numeric_handles_clipboard_apply_and_undo_preserve_the_exact_curve() {
    let mut h = harness(1200.0, 900.0);
    let project = h.state().session.project.clone();
    let undo = h.state().session.history.undo.len();
    for (id, value) in [("x1", "0.4"), ("y1", "3"), ("x2", "0.6"), ("y2", "-2")] {
        enter_text(&mut h, &format!("easePresets.handle.{id}"), value);
    }
    let curve = h.state().ui.ease_presets.curve;
    assert_eq!(curve.handles(), [0.4, 3.0, 0.6, -2.0]);
    assert!(h.state().session.project.same_content(&project));
    assert_eq!(h.state().session.history.undo.len(), undo, "editing a working curve does not edit project keys");
    let text = click(&mut h, "easePresets.copy").pop().unwrap();
    assert_eq!(text, "cubic-bezier(0.4, 3, 0.6, -2)");
    h.state_mut().ui.ease_presets.curve = EaseCurve::LINEAR;
    h.state_mut().hooks.clipboard_text = Some(Box::new(move || Some(text.clone())));
    h.run_steps(2);
    click(&mut h, "easePresets.paste");
    assert_eq!(h.state().ui.ease_presets.curve, curve);
    click(&mut h, "easePresets.apply");
    assert_eq!(h.state().session.history.undo.len(), undo + 1);
    let captured: EaseCurve = serde_json::from_value(h.state_mut().session.execute("keys.easePreset.capture", json!({})).unwrap()).unwrap();
    assert_eq!(captured.out_influence, curve.out_influence);
    assert_eq!(captured.in_influence, curve.in_influence);
    // Apply multiplies normalized speed by the property's segment slope to store
    // units/second; capture divides by that slope. This dimensional roundtrip can
    // round by one ULP. The direct numeric/text/clipboard equalities above stay exact.
    for (actual, expected) in [(captured.out_speed, curve.out_speed), (captured.in_speed, curve.in_speed)] {
        assert!((expected.next_down()..=expected.next_up()).contains(&actual), "captured speed {actual} differs from {expected} by more than one ULP");
    }
    h.state_mut().session.undo();
    assert!(h.state().session.project.same_content(&project));
}

#[test]
fn invalid_text_and_clipboard_keep_the_last_valid_curve_and_project() {
    let mut h = harness(1200.0, 900.0);
    enter_text(&mut h, "easePresets.bezierText", "cubic-bezier(0.4,3,0.6,-2)");
    let curve = h.state().ui.ease_presets.curve;
    let project = h.state().session.project.clone();
    let undo = h.state().session.history.undo.len();
    for invalid in ["0.4,3,0.6", "0.4,NaN,0.6,-2", "0.4,3,1,-2", "cubic-bezier(0.4,41,0.6,-2)"] {
        enter_text(&mut h, "easePresets.bezierText", invalid);
        assert!(h.state().ui.ease_presets.curve_error.is_some(), "{invalid}");
        assert!(h.state().auto.find("easePresets.curveError").is_some());
        assert_eq!(h.state().ui.ease_presets.curve, curve);
    }
    h.state_mut().hooks.clipboard_text = Some(Box::new(|| Some("not a curve".into())));
    click(&mut h, "easePresets.paste");
    assert_eq!(h.state().ui.ease_presets.curve, curve);
    assert!(h.state().ui.ease_presets.curve_error.is_some());
    assert!(h.state().session.project.same_content(&project));
    assert_eq!(h.state().session.history.undo.len(), undo);
}

#[test]
fn extreme_handles_fit_and_pointer_drag_keeps_one_scale_until_release() {
    let mut h = harness(1200.0, 900.0);
    h.state_mut().ui.ease_presets.curve = EaseCurve::from_handles([0.4, 3.0, 0.6, -2.0]).unwrap();
    h.run_steps(2);
    let graph = rect(&h, "easePresets.graph");
    for id in ["easePresets.handle.out", "easePresets.handle.in"] {
        assert!(graph.contains_rect(rect(&h, id)), "{id} must be fully reachable");
    }
    let from = rect(&h, "easePresets.handle.out").center();
    h.input_mut()
        .events
        .extend([Event::PointerMoved(from), Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerMoved(from - vec2(0.0, 8.0)));
    h.step();
    let first = h.state().ui.ease_presets.curve.handles()[1];
    h.input_mut().events.push(Event::PointerMoved(from - vec2(0.0, 16.0)));
    h.step();
    let second = h.state().ui.ease_presets.curve.handles()[1];
    assert!(first > 3.0);
    assert!(((second - first) - (first - 3.0)).abs() < 1e-5, "equal pointer deltas keep equal units throughout the drag");
    h.input_mut().events.push(Event::PointerButton { pos: from - vec2(0.0, 16.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    for id in ["easePresets.handle.out", "easePresets.handle.in"] {
        assert!(rect(&h, "easePresets.graph").contains_rect(rect(&h, id)), "the graph fits again after release");
    }
}

#[test]
fn short_docks_scroll_to_every_precise_editor_and_keep_extreme_handles_visible() {
    for (width, height) in [(420.0, 480.0), (1200.0, 240.0)] {
        let mut h = harness(width, height);
        h.state_mut().ui.ease_presets.curve = EaseCurve::from_handles([0.4, 3.0, 0.6, -2.0]).unwrap();
        h.run_steps(2);
        let panel = rect(&h, "panel.EasePresets");
        for id in ["easePresets.handle.out", "easePresets.handle.in"] {
            assert!(panel.contains_rect(rect(&h, id)));
        }
        let ids = ["handle.x1", "handle.y1", "handle.x2", "handle.y2", "bezierText", "copy", "paste", "useCurve", "apply", "export", "import"];
        let mut reachable = [false; 11];
        for _ in 0..30 {
            for (visible, id) in reachable.iter_mut().zip(ids) {
                *visible |= panel.contains_rect(rect(&h, &format!("easePresets.{id}")));
            }
            h.input_mut().events.extend([
                Event::PointerMoved(panel.center()),
                Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -30.0), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move },
            ]);
            h.run_steps(12);
        }
        assert!(reachable.into_iter().all(|visible| visible), "every editor must be reachable at {width}×{height}: {reachable:?}");
    }
}

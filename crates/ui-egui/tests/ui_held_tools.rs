//! Real input events exercise temporary tool ownership, including the control-channel phases.

use effectcraft_engine::Session;
use effectcraft_ui_egui::{ControlRequest, EffectcraftApp, dock::PanelKind, state::Tool};
use egui::{Event, Key, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::Harness;
use serde_json::{Value, json};

fn app() -> EffectcraftApp {
    let mut session = Session::default();
    session.execute("comp.new", json!({"width": 320, "height": 180, "duration": 2})).unwrap();
    let layer = session.execute("layer.newSolid", json!({"width": 80, "height": 40, "color": "#38d8e8"})).unwrap()["layer"].as_u64().unwrap();
    session.execute("layer.setSwitch", json!({"layer": layer, "switch": "threeD", "value": true})).unwrap();
    let mut app = EffectcraftApp::new(session);
    app.ui.start_screen = false;
    app.ui.focused = PanelKind::Composition;
    app.ui.viewer.zoom = Some(1.0);
    app
}

fn harness() -> Harness<'static, EffectcraftApp> {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app());
    h.run_steps(3);
    h
}

fn key(h: &mut Harness<'_, EffectcraftApp>, key: Key, pressed: bool) {
    h.input_mut().events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
    h.step();
}

fn elapsed(h: &mut Harness<'_, EffectcraftApp>, seconds: f64) {
    h.input_mut().time = Some(h.ctx.input(|i| i.time) + seconds);
    h.step();
}

fn drag(h: &mut Harness<'_, EffectcraftApp>) {
    let [x, y, w, hh] = h.state().auto.find("viewer.area").unwrap().rect;
    let from = pos2(x + w / 2.0, y + hh / 2.0);
    h.input_mut()
        .events
        .extend([Event::PointerMoved(from), Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerMoved(from + vec2(24.0, 12.0)));
    h.step();
    h.input_mut().events.push(Event::PointerButton {
        pos: from + vec2(24.0, 12.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(2);
}

#[test]
fn taps_latch_and_cycle_tools_without_dispatching_key_releases_again() {
    let mut h = harness();
    for (k, tool) in [
        (Key::H, Tool::Hand),
        (Key::Z, Tool::Zoom),
        (Key::W, Tool::Rotate),
        (Key::Y, Tool::PanBehind),
        (Key::Q, Tool::Rectangle),
        (Key::Q, Tool::RoundedRect),
        (Key::G, Tool::Pen),
        (Key::C, Tool::UnifiedCamera),
        (Key::V, Tool::Selection),
    ] {
        key(&mut h, k, true);
        assert_eq!(h.state().ui.tool, tool);
        key(&mut h, k, false);
        assert_eq!(h.state().ui.tool, tool, "release of {k:?} must not cycle a second time");
    }
}

#[test]
fn held_hand_drag_restores_the_previous_tool_without_a_document_edit() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Rotate;
    let project = serde_json::to_value(&*h.state().session.project).unwrap();
    let undo = h.state().session.history.undo.len();
    let pan = h.state().ui.viewer.pan;
    key(&mut h, Key::H, true);
    drag(&mut h);
    key(&mut h, Key::H, false);
    assert_eq!(h.state().ui.tool, Tool::Rotate);
    assert_ne!(h.state().ui.viewer.pan, pan);
    assert_eq!(h.state().session.history.undo.len(), undo);
    assert_eq!(serde_json::to_value(&*h.state().session.project).unwrap(), project);
}

#[test]
fn held_camera_restores_toolbar_slot_memory_and_keeps_the_navigation() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Rectangle;
    let slots = h.state().ui.slot_tools.clone();
    let camera = h.state_mut().session.execute("layer.newCamera", json!({"name":"Original camera"})).unwrap()["layer"].as_u64().unwrap();
    let before = h.state_mut().session.execute("prop.get", json!({"layer":camera,"path":"transform/position"})).unwrap()["value"].clone();
    let undo = h.state().session.history.undo.len();
    key(&mut h, Key::C, true);
    assert_eq!(h.state().ui.tool, Tool::UnifiedCamera);
    drag(&mut h);
    key(&mut h, Key::C, false);
    assert_eq!(h.state().ui.tool, Tool::Rectangle);
    assert_eq!(h.state().ui.slot_tools, slots);
    let after = h.state_mut().session.execute("prop.get", json!({"layer":camera,"path":"transform/position"})).unwrap()["value"].clone();
    assert_ne!(after, before);
    assert_eq!(h.state().session.history.undo.len(), undo + 1);
    h.state_mut().session.undo();
    assert_eq!(h.state_mut().session.execute("prop.get", json!({"layer":camera,"path":"transform/position"})).unwrap()["value"], before);
}

#[test]
fn a_hold_without_mouse_input_restores_the_shape_tool_and_its_slot() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Ellipse;
    h.state_mut().ui.slot_tools[8] = Tool::Ellipse;
    let slots = h.state().ui.slot_tools.clone();
    key(&mut h, Key::Q, true);
    assert_eq!(h.state().ui.tool, Tool::Polygon);
    elapsed(&mut h, 0.5);
    key(&mut h, Key::Q, false);
    assert_eq!(h.state().ui.tool, Tool::Ellipse);
    assert_eq!(h.state().ui.slot_tools, slots);
}

#[test]
fn nested_held_tools_restore_correctly_in_both_release_orders() {
    for parent_first in [false, true] {
        let mut h = harness();
        h.state_mut().ui.tool = Tool::Zoom;
        let slots = h.state().ui.slot_tools.clone();
        key(&mut h, Key::C, true);
        key(&mut h, Key::H, true);
        elapsed(&mut h, 0.5);
        key(&mut h, if parent_first { Key::C } else { Key::H }, false);
        assert_eq!(h.state().ui.tool, if parent_first { Tool::Hand } else { Tool::UnifiedCamera });
        key(&mut h, if parent_first { Key::H } else { Key::C }, false);
        assert_eq!(h.state().ui.tool, Tool::Zoom);
        assert_eq!(h.state().ui.slot_tools, slots);
    }
}

#[test]
fn explicit_tool_choices_and_focus_loss_do_not_leave_a_stuck_temporary_tool() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Zoom;
    let slots = h.state().ui.slot_tools.clone();
    key(&mut h, Key::C, true);
    h.state_mut().ui.tool = Tool::Selection;
    h.step();
    key(&mut h, Key::C, false);
    assert_eq!(h.state().ui.tool, Tool::Selection);
    assert_eq!(h.state().ui.slot_tools, slots);
    key(&mut h, Key::C, true);
    h.input_mut().events.push(Event::WindowFocused(false));
    h.step();
    assert_eq!(h.state().ui.tool, Tool::Selection);
    assert_eq!(h.state().ui.slot_tools, slots);
    h.input_mut().events.push(Event::WindowFocused(true));
    h.step();
    key(&mut h, Key::C, false);
    assert_eq!(h.state().ui.tool, Tool::Selection);
    key(&mut h, Key::C, true);
    let [x, y, w, hh] = h.state().auto.find("tools.UnifiedCamera").unwrap().rect;
    let at = pos2(x + w / 2.0, y + hh / 2.0);
    h.input_mut()
        .events
        .extend([Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    key(&mut h, Key::C, false);
    assert_eq!(h.state().ui.tool, Tool::UnifiedCamera, "clicking the same visible temporary tool makes an explicit selection");
    assert_eq!(h.state().ui.slot_tools[3], Tool::UnifiedCamera);
}

#[test]
fn remapped_letter_keys_use_their_binding_and_typing_owns_the_input() {
    let mut h = harness();
    // J normally belongs to Timeline key navigation, which also owns the Composition viewer.
    h.state_mut().session.execute("shortcuts.set", json!({"command": "time.previousKey", "keys": []})).unwrap();
    h.state_mut().session.execute("shortcuts.set", json!({"command": "tool.camera", "keys": "J"})).unwrap();
    h.state_mut().ui.tool = Tool::Zoom;
    key(&mut h, Key::J, true);
    assert_eq!(h.state().ui.tool, Tool::UnifiedCamera);
    elapsed(&mut h, 0.5);
    key(&mut h, Key::J, false);
    assert_eq!(h.state().ui.tool, Tool::Zoom);
    h.state_mut().ui.focused = PanelKind::Timeline;
    key(&mut h, Key::H, true);
    h.input_mut().events.push(Event::Key { key: Key::F, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND });
    h.step();
    h.run_steps(2);
    assert!(h.ctx.egui_wants_keyboard_input());
    assert_eq!(h.state().ui.tool, Tool::Zoom);
    h.input_mut().events.push(Event::Text("h".into()));
    key(&mut h, Key::H, false);
    assert_eq!(h.state().ui.timeline.search, "h");
    assert_eq!(h.state().ui.tool, Tool::Zoom);
}

#[test]
fn dialogs_restore_the_previous_tool_even_without_key_up() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Zoom;
    key(&mut h, Key::C, true);
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "app.settings", json!({"page":"3d"})).unwrap();
    h.step();
    assert_eq!(h.state().ui.tool, Tool::Zoom);
    key(&mut h, Key::C, false);
    assert_eq!(h.state().ui.tool, Tool::Zoom);
}

#[test]
fn an_open_menu_restores_the_tool_and_owns_its_keys() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Zoom;
    key(&mut h, Key::H, true);
    let [x, y, w, hh] = h.state().auto.find("viewer.area").unwrap().rect;
    let at = pos2(x + w / 2.0, y + hh / 2.0);
    h.input_mut()
        .events
        .extend([Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    assert!(egui::Popup::is_any_open(&h.ctx));
    assert_eq!(h.state().ui.tool, Tool::Zoom);
    key(&mut h, Key::H, false);
    key(&mut h, Key::C, true);
    key(&mut h, Key::C, false);
    assert_eq!(h.state().ui.tool, Tool::Zoom);
}

#[test]
fn focus_loss_cancels_the_spacebar_preview_tap() {
    let mut h = harness();
    key(&mut h, Key::Space, true);
    h.input_mut().events.push(Event::WindowFocused(false));
    h.step();
    h.input_mut().events.push(Event::WindowFocused(true));
    h.step();
    key(&mut h, Key::Space, false);
    assert!(!h.state().playback.playing);
}

#[test]
fn control_key_phases_hold_repeat_release_and_reject_invalid_input() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app().with_control(rx));
    h.run_steps(3);
    let call = |h: &mut Harness<'_, EffectcraftApp>, params: Value| -> Value {
        let (req, reply) = ControlRequest::new("ui.key", params);
        tx.send(req).unwrap();
        for _ in 0..40 {
            let ctx = h.ctx.clone();
            let mut raw = std::mem::take(h.input_mut());
            eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
            *h.input_mut() = raw;
            h.step();
            if let Ok(result) = reply.try_recv() {
                return result;
            }
        }
        panic!("control request did not complete");
    };
    assert_eq!(call(&mut h, json!({"key":"C"}))["ok"], true);
    assert_eq!(h.state().ui.tool, Tool::UnifiedCamera);
    assert_eq!(call(&mut h, json!({"key":"C","phase":"down"}))["ok"], true);
    assert_eq!(h.state().ui.tool, Tool::Orbit);
    assert_eq!(call(&mut h, json!({"key":"C","phase":"down"}))["ok"], true);
    assert_eq!(h.state().ui.tool, Tool::Orbit, "repeated down must not cycle again");
    for invalid in [json!("unknown"), json!(false), Value::Null] {
        let result = call(&mut h, json!({"key":"C","phase":invalid}));
        assert_eq!(result["ok"], false);
        assert_eq!(h.state().ui.tool, Tool::Orbit);
    }
    elapsed(&mut h, 0.5);
    assert_eq!(call(&mut h, json!({"key":"C","phase":"up"}))["ok"], true);
    assert_eq!(h.state().ui.tool, Tool::UnifiedCamera);
}

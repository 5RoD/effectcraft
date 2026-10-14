//! Original small scenes exercise real keyboard and pointer events, without a media fixture.

use effectcraft_engine::Session;
use effectcraft_ui_egui::{EffectcraftApp, dock::PanelKind, state::Tool};
use egui::{Event, Key, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn app(camera: bool) -> EffectcraftApp {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Camera controls", "width": 320, "height": 180, "duration": 2})).unwrap();
    let solid = s.execute("layer.newSolid", json!({"name": "Card", "width": 80, "height": 40, "color": "#38d8e8"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.setSwitch", json!({"layer": solid, "switch": "threeD", "value": true})).unwrap();
    if camera {
        s.execute("layer.newCamera", json!({"name": "Camera"})).unwrap();
    }
    s.execute("layer.select", json!({"layers": [solid]})).unwrap();
    let mut app = EffectcraftApp::new(s);
    app.ui.focused = PanelKind::Composition;
    app.ui.viewer.zoom = Some(1.0);
    app
}

fn camera_prop(h: &Harness<'_, EffectcraftApp>, path: &str) -> [f64; 3] {
    h.state().session.active_comp().unwrap().layers.iter().find(|l| l.is_camera()).unwrap().props.prop(path).unwrap().value.as_vec3()
}

fn key(h: &mut Harness<'_, EffectcraftApp>, key: Key, modifiers: Modifiers) {
    h.input_mut().events.push(Event::ModifiersChanged(modifiers));
    for pressed in [true, false] {
        h.input_mut().events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
        h.step();
    }
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.step();
}

fn drag(h: &mut Harness<'_, EffectcraftApp>, button: PointerButton, modifiers: Modifiers) {
    let [x, y, w, hh] = h.state().auto.find("viewer.area").unwrap().rect;
    let from = pos2(x + w * 0.5, y + hh * 0.5);
    h.input_mut().events.push(Event::ModifiersChanged(modifiers));
    h.input_mut().events.push(Event::PointerMoved(from));
    h.input_mut().events.push(Event::PointerButton { pos: from, button, pressed: true, modifiers });
    h.step();
    for i in 1..=3 {
        h.input_mut().events.push(Event::PointerMoved(from + vec2(i as f32 * 12.0, i as f32 * 6.0)));
        h.step();
    }
    h.input_mut().events.push(Event::PointerButton { pos: from + vec2(36.0, 18.0), button, pressed: false, modifiers });
    h.run_steps(2);
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.step();
}

#[test]
fn camera_shortcuts_cycle_and_frame_in_the_viewer_preserving_timeline_feather() {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app(true));
    h.run_steps(3);
    for expected in [Tool::UnifiedCamera, Tool::Orbit, Tool::PanCamera, Tool::Dolly, Tool::UnifiedCamera] {
        key(&mut h, Key::C, Modifiers::NONE);
        assert_eq!(h.state().ui.tool, expected);
    }
    assert!(h.state().auto.find("tools.UnifiedCamera").is_some());
    for (k, expected) in [(Key::Num1, Tool::Orbit), (Key::Num2, Tool::PanCamera), (Key::Num3, Tool::Dolly)] {
        key(&mut h, k, Modifiers::NONE);
        assert_eq!(h.state().ui.tool, expected);
    }
    let solid = h.state().session.state.selected_layers[0];
    h.state_mut().session.execute("prop.set", json!({"layer": solid.0, "path": "transform/position", "value": [70, 45, 15]})).unwrap();
    let n = h.state().session.history.undo.len();
    let before = camera_prop(&h, "transform/poi");
    key(&mut h, Key::F, Modifiers::NONE);
    assert_ne!(camera_prop(&h, "transform/poi"), before);
    assert_eq!(camera_prop(&h, "transform/poi"), [70.0, 45.0, 15.0]);
    assert_eq!(h.state().session.history.undo.len(), n + 1);
    assert!(h.state().ui.timeline.reveal.is_empty());
    let second = h.state_mut().session.execute("layer.newSolid", json!({"width": 80, "height": 40})).unwrap()["layer"].as_u64().unwrap();
    h.state_mut().session.execute("layer.setSwitch", json!({"layer": second, "switch": "threeD", "value": true})).unwrap();
    h.state_mut().session.execute("prop.set", json!({"layer": second, "path": "transform/position", "value": [230, 135, -15]})).unwrap();
    let flat = h.state_mut().session.execute("layer.newSolid", json!({"width": 80, "height": 40})).unwrap()["layer"].as_u64().unwrap();
    h.state_mut().session.execute("prop.set", json!({"layer": flat, "path": "transform/position", "value": [9000, 8000, 0]})).unwrap();
    h.state_mut().session.execute("layer.select", json!({"layers": [solid.0]})).unwrap();
    key(&mut h, Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(camera_prop(&h, "transform/poi"), [150.0, 90.0, 0.0], "Ctrl+Shift+F includes the other 3D card and excludes the far-off 2D card");
    assert!(h.state().dialog.is_none());
    h.state_mut().ui.focused = PanelKind::Timeline;
    let before = camera_prop(&h, "transform/position");
    key(&mut h, Key::F, Modifiers::NONE);
    assert_eq!(camera_prop(&h, "transform/position"), before);
    assert!(h.state().ui.timeline.reveal.iter().any(|k| k == "feather"));
    key(&mut h, Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(h.state().dialog, Some(effectcraft_ui_egui::Dialog::Form), "Ctrl+Shift+F retains the Mask Feather dialog outside camera-tool context");
    assert_eq!(camera_prop(&h, "transform/position"), before);
    key(&mut h, Key::Escape, Modifiers::NONE);
    assert!(h.state().dialog.is_none());
    h.state_mut().ui.timeline.reveal.clear();
    h.state_mut().ui.focused = PanelKind::Composition;
    h.state_mut().ui.tool = Tool::Selection;
    key(&mut h, Key::F, Modifiers::NONE);
    assert_eq!(camera_prop(&h, "transform/position"), before);
    assert!(h.state().ui.timeline.reveal.iter().any(|k| k == "feather"));
}

#[test]
fn unified_camera_uses_all_three_buttons_and_merges_each_drag_into_one_undo() {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app(true));
    h.state_mut().ui.tool = Tool::UnifiedCamera;
    h.run_steps(3);
    for button in [PointerButton::Primary, PointerButton::Middle, PointerButton::Secondary] {
        let position = camera_prop(&h, "transform/position");
        let poi = camera_prop(&h, "transform/poi");
        let selection = h.state().session.state.selected_layers.clone();
        let pan = h.state().ui.viewer.pan;
        let undo = h.state().session.history.undo.len();
        drag(&mut h, button, Modifiers::NONE);
        assert_ne!(camera_prop(&h, "transform/position"), position, "{button:?} moves the camera");
        if button == PointerButton::Primary {
            assert_eq!(camera_prop(&h, "transform/poi"), poi);
        } else {
            let after_poi = camera_prop(&h, "transform/poi");
            let after_pos = camera_prop(&h, "transform/position");
            assert_ne!(after_poi, poi);
            for i in 0..3 {
                assert!(((after_pos[i] - position[i]) - (after_poi[i] - poi[i])).abs() < 1e-8, "pan/dolly translate the two-node rig together");
            }
        }
        assert_eq!(h.state().ui.viewer.pan, pan, "middle camera dragging must not pan the canvas");
        assert_eq!(h.state().session.state.selected_layers, selection, "secondary dragging must not pick a layer");
        assert_eq!(h.state().session.history.undo.len(), undo + 1);
        h.state_mut().session.undo();
        h.run_steps(2);
        assert_eq!(camera_prop(&h, "transform/position"), position);
        assert_eq!(camera_prop(&h, "transform/poi"), poi);
    }
    let position = camera_prop(&h, "transform/position");
    drag(&mut h, PointerButton::Secondary, Modifiers::SHIFT);
    let after = camera_prop(&h, "transform/position");
    assert_ne!(after[0], position[0], "Shift changes Unified secondary drag to orbit");
    assert_eq!(after[1], position[1], "dominant horizontal Shift-orbit preserves camera Y");
    h.state_mut().session.undo();
    h.state_mut().ui.tool = Tool::Selection;
    h.run_steps(2);
    let pan = h.state().ui.viewer.pan;
    let undo = h.state().session.history.undo.len();
    drag(&mut h, PointerButton::Middle, Modifiers::NONE);
    assert_ne!(h.state().ui.viewer.pan, pan, "other tools keep middle-button canvas panning");
    assert_eq!(h.state().session.history.undo.len(), undo);
}

#[test]
fn unified_camera_navigates_a_default_view_without_a_camera_layer() {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app(false));
    h.state_mut().ui.tool = Tool::UnifiedCamera;
    h.run_steps(3);
    let cid = h.state().session.active_comp_id().unwrap();
    let before = serde_json::to_value(&*h.state().session.project).unwrap();
    let undo = h.state().session.history.undo.len();
    drag(&mut h, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.state().session.state.views3d[&cid].current, effectcraft_render::three_d::View3D::Default);
    let orbit = h.state().session.view_camera(cid).unwrap().eye;
    drag(&mut h, PointerButton::Middle, Modifiers::NONE);
    assert_ne!(h.state().session.view_camera(cid).unwrap().eye, orbit);
    key(&mut h, Key::F, Modifiers::NONE);
    assert_eq!(h.state().session.history.undo.len(), undo);
    assert_eq!(serde_json::to_value(&*h.state().session.project).unwrap(), before);
    assert!(h.state().session.active_comp().unwrap().layers.iter().all(|l| !l.is_camera()));
}

#[test]
fn alt_camera_navigation_preserves_the_toolbar_and_merges_each_camera_drag() {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app(true));
    h.run_steps(3);
    let slots = h.state().ui.slot_tools.clone();
    let selection = h.state().session.state.selected_layers.clone();
    let pan = h.state().ui.viewer.pan;
    for button in [PointerButton::Primary, PointerButton::Middle, PointerButton::Secondary] {
        let position = camera_prop(&h, "transform/position");
        let undo = h.state().session.history.undo.len();
        drag(&mut h, button, Modifiers::ALT);
        assert_ne!(camera_prop(&h, "transform/position"), position, "Alt+{button:?} navigates the camera");
        assert_eq!(h.state().session.history.undo.len(), undo + 1);
        assert_eq!(h.state().ui.tool, Tool::Selection);
        assert_eq!(h.state().ui.slot_tools, slots);
        assert_eq!(h.state().ui.viewer.pan, pan);
        assert_eq!(h.state().session.state.selected_layers, selection);
        assert!(!egui::Popup::is_any_open(&h.ctx));
        h.state_mut().session.undo();
        h.run_steps(2);
        assert_eq!(camera_prop(&h, "transform/position"), position);
    }
    h.state_mut().session.execute("prefs.set", json!({"key": "threeD.altCameraNavigation", "value": false})).unwrap();
    let saved = serde_json::to_value(&h.state().session.prefs).unwrap();
    let reopened: effectcraft_engine::prefs::Prefs = serde_json::from_value(saved).unwrap();
    assert!(!reopened.three_d.alt_camera_navigation);
    assert!(effectcraft_engine::prefs::Prefs::from_json("{}").three_d.alt_camera_navigation);
    let position = camera_prop(&h, "transform/position");
    drag(&mut h, PointerButton::Middle, Modifiers::ALT);
    assert_eq!(camera_prop(&h, "transform/position"), position);
    assert_ne!(h.state().ui.viewer.pan, pan, "disabling the preference restores canvas panning");
}

#[test]
fn alt_navigates_the_default_3d_view_but_preserves_flat_composition_controls() {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app(false));
    h.run_steps(3);
    let cid = h.state().session.active_comp_id().unwrap();
    let project = serde_json::to_value(&*h.state().session.project).unwrap();
    let undo = h.state().session.history.undo.len();
    drag(&mut h, PointerButton::Primary, Modifiers::ALT);
    let orbit = h.state().session.view_camera(cid).unwrap().eye;
    drag(&mut h, PointerButton::Middle, Modifiers::ALT);
    assert_ne!(h.state().session.view_camera(cid).unwrap().eye, orbit);
    assert_eq!(h.state().session.history.undo.len(), undo);
    assert_eq!(serde_json::to_value(&*h.state().session.project).unwrap(), project);
    assert_eq!(h.state().ui.tool, Tool::Selection);
    let solid = h.state().session.state.selected_layers[0];
    h.state_mut().session.execute("layer.setSwitch", json!({"layer": solid.0, "switch": "threeD", "value": false})).unwrap();
    let view = serde_json::to_value(&h.state().session.state.views3d).unwrap();
    let pan = h.state().ui.viewer.pan;
    drag(&mut h, PointerButton::Middle, Modifiers::ALT);
    assert_eq!(serde_json::to_value(&h.state().session.state.views3d).unwrap(), view);
    assert_ne!(h.state().ui.viewer.pan, pan);
}

#[test]
fn alt_camera_release_and_other_modifier_controls_do_not_leak_into_selection() {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(|_| app(true));
    h.run_steps(3);
    let selection = h.state().session.state.selected_layers.clone();
    let [x, y, w, hh] = h.state().auto.find("viewer.area").unwrap().rect;
    let from = pos2(x + w * 0.5, y + hh * 0.5);
    // Release Alt before the right button, for both a click and a camera drag.
    for delta in [vec2(0.0, 0.0), vec2(36.0, 18.0)] {
        h.input_mut().events.extend([
            Event::ModifiersChanged(Modifiers::ALT),
            Event::PointerMoved(from),
            Event::PointerButton { pos: from, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::ALT },
        ]);
        h.step();
        h.input_mut().events.push(Event::PointerMoved(from + delta));
        h.step();
        h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
        h.step();
        h.input_mut().events.push(Event::PointerButton { pos: from + delta, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(2);
        assert_eq!(h.state().session.state.selected_layers, selection);
        assert_eq!(h.state().ui.tool, Tool::Selection);
        assert!(!egui::Popup::is_any_open(&h.ctx));
    }
    let blank = from + vec2(140.0, 75.0);
    h.input_mut().events.extend([
        Event::ModifiersChanged(Modifiers::ALT),
        Event::PointerMoved(blank),
        Event::PointerButton { pos: blank, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::ALT },
    ]);
    h.step();
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: blank, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    assert_eq!(h.state().session.state.selected_layers, selection, "a short Alt-click on empty space does not deselect");
    let position = camera_prop(&h, "transform/position");
    let pan = h.state().ui.viewer.pan;
    h.input_mut().events.push(Event::Key { key: Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    drag(&mut h, PointerButton::Primary, Modifiers::ALT);
    h.input_mut().events.push(Event::Key { key: Key::Space, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    assert_eq!(camera_prop(&h, "transform/position"), position);
    assert_ne!(h.state().ui.viewer.pan, pan, "Spacebar panning takes priority");
    drag(&mut h, PointerButton::Middle, Modifiers::ALT | Modifiers::COMMAND);
    assert_eq!(camera_prop(&h, "transform/position"), position, "Ctrl/Cmd+Alt retains its alternate tool actions");
    h.state_mut().ui.tool = Tool::Zoom;
    h.run_steps(2);
    let zoom = h.state().ui.viewer.zoom.unwrap();
    h.input_mut().events.extend([
        Event::ModifiersChanged(Modifiers::ALT),
        Event::PointerMoved(from),
        Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::ALT },
    ]);
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::ALT });
    h.step();
    assert_eq!(h.state().ui.viewer.zoom, Some(zoom * 0.5), "Zoom's Alt-click still zooms out");
    assert_eq!(camera_prop(&h, "transform/position"), position);
}

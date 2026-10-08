use effectcraft_engine::Session;
use effectcraft_ui_egui::{EffectcraftApp, dock::PanelKind};
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use serde_json::json;

fn harness() -> Harness<'static, EffectcraftApp> {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Shortcuts", "width": 64, "height": 64, "duration": 3, "frameRate": 30})).unwrap();
    s.execute("layer.newNull", json!({"name": "Find this"})).unwrap();
    Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(s))
}

fn key(h: &mut Harness<'_, EffectcraftApp>, key: Key, modifiers: Modifiers) {
    h.input_mut().events.push(Event::ModifiersChanged(modifiers));
    h.input_mut().events.push(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    h.step();
    h.input_mut().events.push(Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
    h.run_steps(2);
}

#[test]
fn find_focuses_the_active_panel_and_accepts_typing() {
    for panel in [PanelKind::Timeline, PanelKind::Project] {
        let mut h = harness();
        h.state_mut().show_panel(panel);
        h.run_steps(3);
        h.state_mut().ui.focused = panel;
        key(&mut h, Key::F, Modifiers::COMMAND);
        h.input_mut().events.push(Event::Text("Find".into()));
        h.run_steps(3);
        if panel == PanelKind::Timeline {
            assert_eq!(h.state().ui.timeline.search, "Find");
            assert!(h.state().ui.project_search.is_empty());
        } else {
            assert_eq!(h.state().ui.project_search, "Find");
            assert!(h.state().ui.timeline.search.is_empty());
        }
    }
}

#[test]
fn new_folder_shortcut_does_not_create_a_shape_layer() {
    let mut h = harness();
    h.run_steps(3);
    h.state_mut().ui.focused = PanelKind::Project;
    let before = h.state().session.active_comp().unwrap().layers.len();
    key(&mut h, Key::N, Modifiers { command: true, alt: true, shift: true, ..Modifiers::NONE });
    assert_eq!(h.state().session.active_comp().unwrap().layers.len(), before);
    assert_eq!(h.state().session.shortcuts().shortcut_of("project.newFolder", &json!(null)), Some("Cmd+Alt+Shift+N"));
    assert_eq!(h.state().session.shortcuts().shortcut_of("layer.newShape", &json!(null)), None);
}

#[test]
fn selected_layer_work_area_shortcut_uses_layer_bounds_and_full_comp_fallback() {
    let mut h = harness();
    h.state_mut().session.execute("layer.timing", json!({"in": 0.5, "out": 2.0})).unwrap();
    h.run_steps(3);
    h.state_mut().ui.focused = PanelKind::Timeline;
    key(&mut h, Key::B, Modifiers { command: true, alt: true, ..Modifiers::NONE });
    let (start, end) = h.state().session.active_comp().unwrap().work_area;
    assert_eq!((start.seconds(), end.seconds()), (0.5, 2.0));
    h.state_mut().session.state.selected_layers.clear();
    key(&mut h, Key::B, Modifiers { command: true, alt: true, ..Modifiers::NONE });
    let (start, end) = h.state().session.active_comp().unwrap().work_area;
    assert_eq!((start.seconds(), end.seconds()), (0.0, 3.0));
}

#[test]
fn timeline_panel_shortcuts_do_not_fire_from_the_project_panel() {
    let mut h = harness();
    h.run_steps(3);
    h.state_mut().ui.focused = PanelKind::Project;
    key(&mut h, Key::F4, Modifiers::NONE);
    assert!(!h.state().ui.timeline.show_modes);
    h.state_mut().ui.focused = PanelKind::Timeline;
    key(&mut h, Key::F4, Modifiers::NONE);
    assert!(h.state().ui.timeline.show_modes);
}

#[test]
fn focused_panel_shortcut_overrides_a_custom_application_shortcut() {
    let mut h = harness();
    h.state_mut().session.execute("shortcuts.set", json!({"command": "edit.deselectAll", "keys": "F4"})).unwrap();
    h.run_steps(3);
    h.state_mut().ui.focused = PanelKind::Timeline;
    key(&mut h, Key::F4, Modifiers::NONE);
    assert!(h.state().ui.timeline.show_modes);
    assert!(!h.state().session.state.selected_layers.is_empty(), "the focused panel owns F4");
    assert!(!h.state().session.state.project_selection.is_empty());
    h.state_mut().ui.focused = PanelKind::Project;
    key(&mut h, Key::F4, Modifiers::NONE);
    assert!(h.state().session.state.project_selection.is_empty(), "the application binding runs on the focused Project panel");
    assert!(!h.state().session.state.selected_layers.is_empty(), "Project deselection keeps Timeline selection");
}

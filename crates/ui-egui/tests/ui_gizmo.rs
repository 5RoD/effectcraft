//! Synthetic scenes exercise the real 3D handle hit tests and pointer lifecycle.

use effectcraft_engine::{Session, project::LayerId};
use effectcraft_ui_egui::{
    EffectcraftApp,
    dock::PanelKind,
    state::{GizmoMode, GizmoSpace, RotationTarget, Tool},
};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Vec2, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn app() -> EffectcraftApp {
    app_size(80, 40)
}

fn app_size(width: u32, height: u32) -> EffectcraftApp {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name":"3D handles", "width":320, "height":180, "duration":2})).unwrap();
    let layer = s.execute("layer.newSolid", json!({"name":"Card", "width":width, "height":height, "color":"#38d8e8"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.setSwitch", json!({"layer":layer,"switch":"threeD","value":true})).unwrap();
    s.execute("layer.select", json!({"layers":[layer]})).unwrap();
    let mut app = EffectcraftApp::new(s);
    app.ui.start_screen = false;
    app.ui.focused = PanelKind::Composition;
    app.ui.viewer.zoom = Some(1.);
    app.ui.viewer.gizmo = GizmoMode::Position;
    app
}

fn harness() -> Harness<'static, EffectcraftApp> {
    let mut h = Harness::builder().with_size(vec2(1280., 800.)).build_eframe(|_| app());
    h.run_steps(3);
    h
}
fn layer(h: &Harness<'_, EffectcraftApp>) -> LayerId {
    h.state().session.state.selected_layers[0]
}
fn prop(h: &Harness<'_, EffectcraftApp>, id: LayerId, name: &str) -> [f64; 3] {
    h.state().session.active_comp().unwrap().layer(id).unwrap().props.prop(&format!("transform/{name}")).unwrap().value.as_vec3()
}
fn center(h: &Harness<'_, EffectcraftApp>, id: &str) -> Pos2 {
    let [x, y, w, hh] = h.state().auto.find(id).unwrap_or_else(|| panic!("missing {id}")).rect;
    pos2(x + w / 2., y + hh / 2.)
}
fn key(h: &mut Harness<'_, EffectcraftApp>, key: Key) {
    for pressed in [true, false] {
        h.input_mut().events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
        h.step();
    }
}
fn drag(h: &mut Harness<'_, EffectcraftApp>, from: Pos2, delta: Vec2, modifiers: Modifiers) {
    h.input_mut().events.extend([
        Event::ModifiersChanged(modifiers),
        Event::PointerMoved(from),
        Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers },
    ]);
    h.step();
    for i in 1..=3 {
        h.input_mut().events.push(Event::PointerMoved(from + delta * (i as f32 / 3.)));
        h.step();
    }
    h.input_mut().events.push(Event::PointerButton { pos: from + delta, button: PointerButton::Primary, pressed: false, modifiers });
    h.run_steps(2);
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.step();
}
fn close(a: [f64; 3], b: [f64; 3]) {
    for (a, b) in a.into_iter().zip(b) {
        assert!((a - b).abs() < 1e-4, "{a} != {b}");
    }
}

fn rotations(h: &Harness<'_, EffectcraftApp>, id: LayerId) -> [f64; 3] {
    ["rotationX", "rotationY", "rotation"]
        .map(|name| h.state().session.active_comp().unwrap().layer(id).unwrap().props.prop(&format!("transform/{name}")).unwrap().value.as_f64())
}
fn click(h: &mut Harness<'_, EffectcraftApp>, identifier: &str) {
    let pos = center(h, identifier);
    h.input_mut()
        .events
        .extend([Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
}

#[test]
fn rotation_tool_exposes_xyz_rings_and_shifts_orientation_in_fifteen_degree_steps() {
    let mut h = harness();
    let id = layer(&h);
    h.state_mut().session.execute("prop.set", json!({"layer":id.0,"path":"transform/rotation","value":17})).unwrap();
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::World;
    key(&mut h, Key::W);
    assert_eq!(h.state().ui.tool, Tool::Rotate);
    assert!(h.state().auto.find("tools.rotation.target").is_some());
    for (axis, index, delta) in [("x", 0, vec2(4., -12.)), ("y", 1, vec2(-12., -4.)), ("z", 2, vec2(-22., -5.))] {
        let from = center(&h, &format!("viewer.gizmo.rotation.{axis}.0"));
        let undo = h.state().session.history.undo.len();
        drag(&mut h, from, delta, Modifiers::SHIFT);
        let angles = prop(&h, id, "orientation");
        assert!(angles[index].abs() >= 15. - 1e-9, "{axis}: {angles:?}; {}", h.state().ui.status);
        assert!((angles[index] / 15. - (angles[index] / 15.).round()).abs() < 1e-6);
        close(rotations(&h, id), [0., 0., 17.]);
        close(prop(&h, id, "position"), [160., 90., 0.]);
        assert_eq!(h.state().session.history.undo.len(), undo + 1);
        h.state_mut().session.undo();
        h.run_steps(2);
        close(prop(&h, id, "orientation"), [0.; 3]);
    }
}

#[test]
fn rotation_target_keeps_orientation_and_preserves_multiple_turns_in_one_drag() {
    let mut h = harness();
    let id = layer(&h);
    h.state_mut().session.execute("prop.set", json!({"layer":id.0,"path":"transform/rotation","value":20})).unwrap();
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::World;
    h.state_mut().ui.viewer.rotation_target = RotationTarget::Rotation;
    key(&mut h, Key::W);
    let from = center(&h, "viewer.gizmo.rotation.z.0");
    let pivot = effectcraft_ui_egui::panels::viewer::comp_to_screen(&h.ctx, [160., 90.]).unwrap();
    let radial = from - pivot;
    let angle = radial.y.atan2(radial.x);
    let radius = radial.length();
    let undo = h.state().session.history.undo.len();
    h.input_mut()
        .events
        .extend([Event::PointerMoved(from), Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    let mut point = from;
    for n in 1..=60 {
        let radians = angle + (n as f32 * 7.5).to_radians();
        point = pivot + vec2(radians.cos(), radians.sin()) * radius;
        h.input_mut().events.push(Event::PointerMoved(point));
        h.step();
    }
    h.input_mut().events.push(Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    close(rotations(&h, id), [0., 0., 470.]);
    close(prop(&h, id, "orientation"), [0.; 3]);
    assert_eq!(h.state().session.history.undo.len(), undo + 1);
    h.state_mut().session.undo();
    h.run_steps(2);
    close(rotations(&h, id), [0., 0., 20.]);
    h.state_mut().session.redo();
    h.run_steps(2);
    close(rotations(&h, id), [0., 0., 470.]);
}

#[test]
fn rotation_tool_body_drag_picks_3d_content_and_works_with_layer_controls_hidden() {
    let mut h = harness();
    let id = layer(&h);
    key(&mut h, Key::W);
    h.state_mut().session.state.selected_layers.clear();
    h.state_mut().ui.viewer.show_layer_controls = false;
    h.run_steps(2);
    let from = effectcraft_ui_egui::panels::viewer::comp_to_screen(&h.ctx, [185., 90.]).unwrap();
    drag(&mut h, from, vec2(-2., 9.), Modifiers::SHIFT);
    assert_eq!(layer(&h), id);
    close(prop(&h, id, "orientation"), [0., 0., 15.]);
    close(rotations(&h, id), [0.; 3]);
    h.state_mut().session.undo();
    h.run_steps(2);
    close(prop(&h, id, "orientation"), [0.; 3]);
}

#[test]
fn flat_rotation_tool_uses_press_origin_and_fifteen_degree_shift_snapping() {
    let mut h = harness();
    let id = layer(&h);
    h.state_mut().session.execute("layer.setSwitch", json!({"layer":id.0,"switch":"threeD","value":false})).unwrap();
    key(&mut h, Key::W);
    let from = effectcraft_ui_egui::panels::viewer::comp_to_screen(&h.ctx, [185., 90.]).unwrap();
    drag(&mut h, from, vec2(-2., 9.), Modifiers::SHIFT);
    close(rotations(&h, id), [0., 0., 15.]);
    assert_eq!(layer(&h), id, "Shift constrains rotation without toggling selection");
    assert!(h.state().auto.find("viewer.gizmo.rotation.z.0").is_none());
}

#[test]
fn rotation_tool_layer_handles_map_corners_to_z_and_side_midpoints_to_xy() {
    // Keep the bounding-box handles outside the rotation rings so this exercises the box hit.
    let mut h = Harness::builder().with_size(vec2(1280., 800.)).build_eframe(|_| app_size(200, 140));
    h.run_steps(3);
    let id = layer(&h);
    key(&mut h, Key::W);
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::World;
    for (handle, axis, delta) in [(4, 0, vec2(0., -16.)), (5, 1, vec2(-16., 0.)), (0, 2, vec2(15., -15.))] {
        let from = center(&h, &format!("viewer.handle.{}.{handle}", id.0));
        let before = h.state().session.history.undo.len();
        drag(&mut h, from, delta, Modifiers::SHIFT);
        let actual = prop(&h, id, "orientation");
        assert!(actual[axis].abs() >= 15. - 1e-9, "handle {handle}, axis {axis}: {actual:?}; {}", h.state().ui.status);
        for (index, value) in actual.into_iter().enumerate() {
            if index != axis {
                assert!(value.abs() < 1e-6, "wrong rotation axis: {actual:?}");
            }
        }
        assert_eq!(layer(&h), id);
        assert_eq!(h.state().session.history.undo.len(), before + 1);
        h.state_mut().session.undo();
        h.run_steps(2);
        close(prop(&h, id, "orientation"), [0.; 3]);
    }
}

#[test]
fn authored_rotation_mode_retains_nonzero_orientation_and_target_changes_end_a_drag() {
    let mut h = harness();
    let id = layer(&h);
    h.state_mut().session.execute("prop.set", json!({"layer":id.0,"path":"transform/orientation","value":[10,20,30]})).unwrap();
    h.state_mut().ui.viewer.rotation_target = RotationTarget::Rotation;
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::World;
    key(&mut h, Key::W);
    let from = center(&h, "viewer.gizmo.rotation.z.0");
    h.input_mut()
        .events
        .extend([Event::PointerMoved(from), Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerMoved(from + vec2(-12., -3.)));
    h.step();
    let reached = rotations(&h, id);
    assert!(reached.iter().any(|v| v.abs() > 1.));
    close(prop(&h, id, "orientation"), [10., 20., 30.]);
    h.state_mut().ui.viewer.rotation_target = RotationTarget::Orientation;
    h.step();
    h.input_mut().events.push(Event::PointerMoved(from + vec2(-28., -6.)));
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: from + vec2(-28., -6.), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    close(rotations(&h, id), reached);
    close(prop(&h, id, "orientation"), [10., 20., 30.]);
}

#[test]
fn rotation_target_choices_validate_before_change_and_restore_after_held_w() {
    let mut h = harness();
    key(&mut h, Key::W);
    click(&mut h, "tools.rotation.target");
    click(&mut h, "tools.rotation.rotation");
    assert_eq!(h.state().ui.viewer.rotation_target, RotationTarget::Rotation);
    let before = serde_json::to_value(&h.state().ui).unwrap();
    let ctx = h.ctx.clone();
    assert!(effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "view.gizmo.rotationTarget", json!({"target":"bad"})).is_err());
    assert_eq!(serde_json::to_value(&h.state().ui).unwrap(), before);
    let saved: effectcraft_ui_egui::state::ViewerState = serde_json::from_value(serde_json::to_value(&h.state().ui.viewer).unwrap()).unwrap();
    assert_eq!(saved.rotation_target, RotationTarget::Rotation);
    h.set_size(vec2(960., 600.));
    h.run_steps(3);
    click(&mut h, "tools.gizmo.compact");
    click(&mut h, "tools.rotation.orientation");
    assert_eq!(h.state().ui.viewer.rotation_target, RotationTarget::Orientation);
    key(&mut h, Key::Num5);
    h.input_mut().events.push(Event::Key { key: Key::W, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    let from = center(&h, "viewer.gizmo.rotation.z.0");
    drag(&mut h, from, vec2(-12., -3.), Modifiers::NONE);
    h.input_mut().events.push(Event::Key { key: Key::W, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    assert_eq!(h.state().ui.tool, Tool::Selection);
    assert_eq!(h.state().ui.viewer.gizmo, GizmoMode::Scale);
}

#[test]
fn position_handles_move_xyz_and_each_drag_is_one_undo() {
    let mut h = harness();
    let id = layer(&h);
    for (axis, delta, expected) in [("x", vec2(30., 0.), [190., 90., 0.]), ("y", vec2(0., 30.), [160., 120., 0.]), ("z", vec2(0., -30.), [160., 90., 30.])] {
        let before = prop(&h, id, "position");
        let undo = h.state().session.history.undo.len();
        let from = center(&h, &format!("viewer.gizmo.position.{axis}"));
        drag(&mut h, from, delta, Modifiers::NONE);
        close(prop(&h, id, "position"), expected);
        assert_eq!(h.state().session.history.undo.len(), undo + 1);
        assert_eq!(layer(&h), id);
        h.state_mut().session.undo();
        h.run_steps(2);
        close(prop(&h, id, "position"), before);
        h.state_mut().session.redo();
        h.run_steps(2);
        close(prop(&h, id, "position"), expected);
        h.state_mut().session.undo();
        h.run_steps(2);
    }
}

#[test]
fn scaling_is_axis_only_or_uniform_with_shift_and_zero_scale_remains_editable() {
    let mut h = harness();
    let id = layer(&h);
    key(&mut h, Key::Num5);
    assert_eq!(h.state().ui.viewer.gizmo, GizmoMode::Scale);
    for mods in [Modifiers::NONE, Modifiers::SHIFT] {
        let from = center(&h, "viewer.gizmo.scale.x");
        let undo = h.state().session.history.undo.len();
        drag(&mut h, from, vec2(27., 0.), mods);
        close(prop(&h, id, "scale"), if mods.shift { [150.; 3] } else { [150., 100., 100.] });
        close(prop(&h, id, "position"), [160., 90., 0.]);
        assert_eq!(h.state().session.history.undo.len(), undo + 1);
        h.state_mut().session.undo();
        h.run_steps(2);
    }
    h.state_mut().session.execute("prop.set", json!({"layer":id.0,"path":"transform/scale","value":[0,0,0]})).unwrap();
    key(&mut h, Key::Num4);
    let from = center(&h, "viewer.gizmo.position.x");
    drag(&mut h, from, vec2(24., 0.), Modifiers::NONE);
    close(prop(&h, id, "position"), [184., 90., 0.]);
}

#[test]
fn rotation_ring_changes_orientation_preserves_authored_rotation_and_undoes() {
    let mut h = harness();
    let id = layer(&h);
    h.state_mut().session.execute("prop.set", json!({"layer":id.0,"path":"transform/rotation","value":17})).unwrap();
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::World;
    key(&mut h, Key::Num6);
    // Z ring segment 0 is near the downward radius; horizontal motion is tangential.
    let from = center(&h, "viewer.gizmo.rotation.z.0");
    let undo = h.state().session.history.undo.len();
    drag(&mut h, from, vec2(-22., -5.), Modifiers::SHIFT);
    let orientation = prop(&h, id, "orientation");
    assert!(orientation[2].abs() >= 5., "rotation changed: {orientation:?}; {}", h.state().ui.status);
    assert!((orientation[2] / 5. - (orientation[2] / 5.).round()).abs() < 1e-6);
    close(prop(&h, id, "position"), [160., 90., 0.]);
    assert_eq!(h.state().session.active_comp().unwrap().layer(id).unwrap().props.prop("transform/rotation").unwrap().value.as_f64(), 17.);
    assert_eq!(h.state().session.history.undo.len(), undo + 1);
    h.state_mut().session.undo();
    h.run_steps(2);
    close(prop(&h, id, "orientation"), [0.; 3]);
}

#[test]
fn edge_on_rotation_rings_keep_small_drags_small_instead_of_jumping_half_a_turn() {
    let mut h = harness();
    let id = layer(&h);
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::World;
    key(&mut h, Key::Num6);
    for (axis, index, delta) in [("x", 0, vec2(4., -12.)), ("y", 1, vec2(-12., -4.))] {
        let from = center(&h, &format!("viewer.gizmo.rotation.{axis}.0"));
        drag(&mut h, from, delta, Modifiers::NONE);
        let angles = prop(&h, id, "orientation");
        assert!(angles[index].abs() > 5. && angles[index].abs() < 60., "small {axis} drag jumped to {angles:?}; {}", h.state().ui.status);
        h.state_mut().session.undo();
        h.run_steps(2);
        close(prop(&h, id, "orientation"), [0.; 3]);
    }
}

#[test]
fn selected_parent_and_child_move_once_and_world_drag_respects_rotated_parent() {
    let mut h = harness();
    let parent = layer(&h);
    let child = h.state_mut().session.execute("layer.newSolid", json!({"width":30,"height":30})).unwrap()["layer"].as_u64().unwrap();
    for (id, path, value) in [(parent.0, "transform/rotation", json!(90)), (parent.0, "transform/scale", json!([200, 200, 200]))] {
        h.state_mut().session.execute("prop.set", json!({"layer":id,"path":path,"value":value})).unwrap();
    }
    h.state_mut().session.execute("layer.setSwitch", json!({"layer":child,"switch":"threeD","value":true})).unwrap();
    h.state_mut().session.execute("layer.setParent", json!({"layers":[child],"parent":parent.0,"compensate":false})).unwrap();
    h.state_mut().session.execute("prop.set", json!({"layer":child,"path":"transform/position","value":[40,20,0]})).unwrap();
    h.state_mut().session.execute("layer.select", json!({"layers":[child]})).unwrap();
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::World;
    h.run_steps(3);
    let from = center(&h, "viewer.gizmo.position.x");
    drag(&mut h, from, vec2(24., 0.), Modifiers::NONE);
    close(prop(&h, LayerId(child), "position"), [40., 8., 0.]);
    h.state_mut().session.undo();
    h.run_steps(2);
    h.state_mut().session.execute("layer.select", json!({"layers":[parent.0,child]})).unwrap();
    h.run_steps(2);
    let before = prop(&h, LayerId(child), "position");
    let old = prop(&h, parent, "position");
    let from = center(&h, "viewer.gizmo.position.x");
    drag(&mut h, from, vec2(24., 0.), Modifiers::NONE);
    close(prop(&h, parent, "position"), [old[0] + 24., old[1], old[2]]);
    close(prop(&h, LayerId(child), "position"), before);
}

#[test]
fn expression_errors_are_atomic_and_single_layer_rotation_keeps_driven_position() {
    let mut h = harness();
    let id = layer(&h);
    h.state_mut().session.execute("prop.setExpression", json!({"layer":id.0,"path":"transform/position","expression":"[160,90,0]"})).unwrap();
    h.run_steps(2);
    let project = h.state().session.project.to_file_json().unwrap();
    let undo = h.state().session.history.undo.len();
    let from = center(&h, "viewer.gizmo.position.x");
    drag(&mut h, from, vec2(24., 0.), Modifiers::NONE);
    assert_eq!(h.state().session.project.to_file_json().unwrap(), project);
    assert_eq!(h.state().session.history.undo.len(), undo);
    assert!(!h.state().ui.status.is_empty());
    key(&mut h, Key::Num6);
    let from = center(&h, "viewer.gizmo.rotation.z.0");
    drag(&mut h, from, vec2(-22., -5.), Modifiers::NONE);
    assert!(prop(&h, id, "orientation")[2].abs() > 1.);
}

#[test]
fn shortcuts_held_selection_and_saved_ui_preserve_gizmo_modes() {
    let mut h = harness();
    for (keycode, mode) in [(Key::Num5, GizmoMode::Scale), (Key::Num6, GizmoMode::Rotation), (Key::Num4, GizmoMode::Position), (Key::V, GizmoMode::Universal)] {
        key(&mut h, keycode);
        assert_eq!(h.state().ui.viewer.gizmo, mode);
        assert_eq!(h.state().ui.tool, Tool::Selection);
    }
    key(&mut h, Key::Num6);
    h.input_mut().events.push(Event::Key { key: Key::V, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    assert_eq!(h.state().ui.viewer.gizmo, GizmoMode::Universal);
    h.input_mut().time = Some(h.ctx.input(|i| i.time) + 0.5);
    h.step();
    h.input_mut().events.push(Event::Key { key: Key::V, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    assert_eq!(h.state().ui.viewer.gizmo, GizmoMode::Rotation);
    h.state_mut().ui.viewer.gizmo_space = GizmoSpace::View;
    let reopened: effectcraft_ui_egui::state::ViewerState = serde_json::from_value(serde_json::to_value(&h.state().ui.viewer).unwrap()).unwrap();
    assert_eq!(reopened.gizmo, GizmoMode::Rotation);
    assert_eq!(reopened.gizmo_space, GizmoSpace::View);
    assert!(h.state().auto.find("tools.gizmo.mode").is_some());
    assert!(h.state().auto.find("tools.gizmo.space").is_some());
    h.state_mut().ui.focused = PanelKind::Timeline;
    key(&mut h, Key::Num4);
    assert_eq!(h.state().ui.viewer.gizmo, GizmoMode::Rotation, "composition shortcut respects other panels");
}

#[test]
fn changing_mode_or_selection_and_losing_focus_end_the_active_drag() {
    for cancel in ["mode", "selection", "focus", "view"] {
        let mut h = harness();
        let id = layer(&h);
        let from = center(&h, "viewer.gizmo.position.x");
        h.input_mut()
            .events
            .extend([Event::PointerMoved(from), Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        h.step();
        h.input_mut().events.push(Event::PointerMoved(from + vec2(12., 0.)));
        h.step();
        close(prop(&h, id, "position"), [172., 90., 0.]);
        match cancel {
            "mode" => h.state_mut().ui.viewer.gizmo = GizmoMode::Scale,
            "selection" => h.state_mut().session.state.selected_layers.clear(),
            "view" => h.state_mut().ui.viewer.pan = [20., 0.],
            _ => h.input_mut().events.push(Event::WindowFocused(false)),
        }
        h.step();
        h.input_mut().events.push(Event::PointerMoved(from + vec2(36., 0.)));
        h.step();
        h.input_mut().events.push(Event::PointerButton {
            pos: from + vec2(36., 0.),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        h.step();
        let after = prop(&h, id, "position");
        assert!((after[0] - 172.).abs() < 1e-4, "{cancel}: {after:?}; {}", h.state().ui.status);
        close(after, [172., 90., 0.]);
    }
}

#[test]
fn narrow_toolbar_choices_remain_clickable_without_overlapping_community_buttons() {
    let mut h = harness();
    h.set_size(vec2(960., 600.));
    h.run_steps(3);
    let [x, y, w, hh] = h.state().auto.find("tools.gizmo.compact").unwrap().rect;
    assert!(x >= 0. && x + w <= 960. && y + hh <= 600.);
    let point = center(&h, "tools.gizmo.compact");
    h.input_mut()
        .events
        .extend([Event::PointerMoved(point), Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    let point = center(&h, "tools.gizmo.rotation");
    h.input_mut()
        .events
        .extend([Event::PointerMoved(point), Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    assert_eq!(h.state().ui.viewer.gizmo, GizmoMode::Rotation);
    let [snap_x, _, snap_w, _] = h.state().auto.find("header.snappingOptions").unwrap().rect;
    let [community_x, _, _, _] = h.state().auto.find("header.appearance").unwrap().rect;
    assert!(snap_x + snap_w <= community_x, "controls overlap: snapping {snap_x}+{snap_w}, appearance {community_x}");
}

#[test]
#[ignore = "functional layout snapshots; run explicitly with GIZMO_SNAPSHOT_DIR"]
fn gizmo_layout_snapshots() {
    let folder = std::path::PathBuf::from(std::env::var_os("GIZMO_SNAPSHOT_DIR").unwrap());
    let mut h = Harness::builder().with_size(vec2(1280., 800.)).with_pixels_per_point(1.).wgpu().build_eframe(|_| app());
    h.run_steps(5);
    for (name, mode) in [("position", GizmoMode::Position), ("scale", GizmoMode::Scale), ("rotation", GizmoMode::Rotation), ("universal", GizmoMode::Universal)]
    {
        h.state_mut().ui.viewer.gizmo = mode;
        h.run_steps(3);
        h.render().unwrap().save(folder.join(format!("{name}.png"))).unwrap();
    }
    h.set_size(vec2(960., 600.));
    h.run_steps(3);
    h.render().unwrap().save(folder.join("narrow.png")).unwrap();
}

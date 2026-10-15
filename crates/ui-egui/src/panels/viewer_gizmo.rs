//! Original geometric 3D handles. Geometry stays on the CPU; manipulating a batch uses
//! the engine's checked atomic command, never per-layer partial edits or GPU readbacks.

use super::viewer::{ViewerMap, project_segment};
use crate::{
    EffectcraftApp,
    state::{GizmoMode, GizmoSpace, RotationTarget, Tool},
};
use effectcraft_engine::geom::{Mat4, Vec3, vec3};
use effectcraft_engine::project::{AutoOrient, LayerId};
use effectcraft_engine::render::{EvalCtx, three_d::CameraState};
use effectcraft_engine::time::Tick;
use egui::{Color32, Pos2, Rect, Stroke, Vec2, vec2};
use serde_json::json;
use std::collections::BTreeSet;

const RING_STEPS: usize = 48;
const MAX_LAYERS: usize = 4096;
const EPS: f64 = 1e-9;

pub(crate) fn mode(app: &EffectcraftApp) -> Option<GizmoMode> {
    match app.ui.tool {
        Tool::Selection => Some(app.ui.viewer.gizmo),
        Tool::Rotate => Some(GizmoMode::Rotation),
        _ => None,
    }
}
fn target(app: &EffectcraftApp) -> RotationTarget {
    if app.ui.tool == Tool::Rotate { app.ui.viewer.rotation_target } else { RotationTarget::Orientation }
}

fn finite(v: Vec3) -> bool {
    [v.x, v.y, v.z].iter().all(|v| v.is_finite() && v.abs() <= 1e12)
}
fn unit(v: Vec3) -> Option<Vec3> {
    let length = v.length();
    (finite(v) && length.is_finite() && length > EPS).then(|| v / length)
}
fn matrix_finite(m: Mat4) -> bool {
    m.0.iter().flatten().all(|v| v.is_finite() && v.abs() <= 1e12)
}
fn array(v: Vec3) -> [f64; 3] {
    [v.x, v.y, v.z]
}
fn axis(i: usize) -> Vec3 {
    match i {
        0 => vec3(1., 0., 0.),
        1 => vec3(0., 1., 0.),
        _ => vec3(0., 0., 1.),
    }
}
fn color(i: usize) -> Color32 {
    match i {
        0 => Color32::from_rgb(224, 80, 80),
        1 => Color32::from_rgb(96, 208, 96),
        _ => Color32::from_rgb(80, 140, 240),
    }
}
fn name(i: usize) -> &'static str {
    match i {
        0 => "x",
        1 => "y",
        _ => "z",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    Position,
    Scale,
    Rotation,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Position => "position",
            Self::Scale => "scale",
            Self::Rotation => "rotation",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Hit {
    kind: Kind,
    axis: usize,
    from: Pos2,
    to: Pos2,
    angle: f64,
}

fn distance(hit: &Hit, p: Pos2) -> f32 {
    let d = hit.to - hit.from;
    let t = if d.length_sq() > 1e-6 { ((p - hit.from).dot(d) / d.length_sq()).clamp(0., 1.) } else { 0. };
    p.distance(hit.from + d * t)
}
pub(crate) fn hit(hits: &[Hit], p: Pos2) -> Option<Hit> {
    hits.iter().filter(|h| distance(h, p) <= 6.).min_by(|a, b| distance(a, p).total_cmp(&distance(b, p))).cloned()
}

#[derive(Clone, Debug)]
struct Pose {
    id: LayerId,
    anchor: Vec3,
    parent: Mat4,
    inv: Mat4,
    scale: [f64; 3],
    orientation: [f64; 3],
    rotations: [f64; 3],
    rotation_reference: [f64; 3],
    rotation: Mat4,
    auto: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Geometry {
    ids: Vec<LayerId>,
    pivot: Vec3,
    basis: [Vec3; 3],
    local: [Vec3; 3],
    camera: CameraState,
    map: ViewerMap,
    radius: f64,
    space: GizmoSpace,
}

impl Geometry {
    pub(crate) fn new(ctx: &EvalCtx, ids: &[LayerId], space: GizmoSpace, camera: CameraState, map: ViewerMap) -> Option<Self> {
        if ids.len() > MAX_LAYERS {
            return None;
        }
        let ids: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| ctx.comp.layer(*id).is_some_and(|l| l.is_3d() && !l.is_camera() && !l.is_light() && !l.switches.locked && l.is_active_at(ctx.time)))
            .take(MAX_LAYERS + 1)
            .collect();
        if ids.is_empty() || ids.len() > MAX_LAYERS || !map.zoom.is_finite() || map.zoom <= 0. || !camera.zoom.is_finite() || camera.zoom <= EPS {
            return None;
        }
        let mut pivot = Vec3::ZERO;
        for id in &ids {
            let l = ctx.comp.layer(*id)?;
            let a = ctx.v3(l, l.transform()?, "anchor", [0.; 3]);
            let point = ctx.world_matrix(l).apply(Vec3::from(a));
            if !finite(point) {
                return None;
            }
            pivot += point / ids.len() as f64;
        }
        let first = ctx.comp.layer(*ids.first()?)?;
        let parent = first.parent.and_then(|id| ctx.comp.layer(id)).map(|l| ctx.world_matrix(l)).unwrap_or(Mat4::IDENTITY);
        // Derive axes before the child's Scale, so a collapsed or mirrored layer remains editable.
        let tr = first.transform()?;
        let orient = effectcraft_engine::render::three_d::camera::auto_orient_3d(ctx, first, Vec3::from(ctx.position(first, tr)))
            .unwrap_or_else(|| Mat4::orientation(Vec3::from(ctx.v3(first, tr, "orientation", [0.; 3]))));
        let rotation = parent
            * orient
            * Mat4::rotate_z(ctx.f(first, tr, "rotation", 0.))
            * Mat4::rotate_y(ctx.f(first, tr, "rotationY", 0.))
            * Mat4::rotate_x(ctx.f(first, tr, "rotationX", 0.));
        let local = [0, 1, 2].map(|i| unit(rotation.apply_vec(axis(i))));
        let local = [local[0]?, local[1]?, local[2]?];
        let basis = match space {
            GizmoSpace::Local => local,
            GizmoSpace::World => [axis(0), axis(1), axis(2)],
            GizmoSpace::View => [unit(camera.right())?, unit(camera.down())?, unit(camera.forward())?],
        };
        camera.project(map.comp[0] as f64, map.comp[1] as f64, pivot)?;
        let world_per_point = if camera.ortho { 1. / camera.zoom } else { camera.depth(pivot) / camera.zoom } / map.zoom as f64;
        let radius = 54. * world_per_point;
        if !radius.is_finite() || !(EPS..=1e10).contains(&radius) {
            return None;
        }
        Some(Self { ids, pivot, basis, local, camera, map, radius, space })
    }
    fn point(&self, p: Vec3) -> Option<Pos2> {
        let p = self.camera.project(self.map.comp[0] as f64, self.map.comp[1] as f64, p)?;
        let screen = self.map.to_screen([p.x, p.y]);
        (screen.x.is_finite() && screen.y.is_finite() && screen.x.abs() < 1e6 && screen.y.abs() < 1e6).then_some(screen)
    }
    fn ring(&self, i: usize) -> Option<(Vec3, Vec3)> {
        let normal = *self.basis.get(i)?;
        let hint = if normal.dot(axis(0)).abs() < 0.8 { axis(0) } else { axis(1) };
        let u = unit(normal.cross(hint))?;
        Some((u, unit(normal.cross(u))?))
    }
    pub(crate) fn draw(&self, app: &mut EffectcraftApp, painter: &egui::Painter, mode: GizmoMode) -> Vec<Hit> {
        let mut hits = Vec::with_capacity(3 * (RING_STEPS + 2));
        let Some(center) = self.point(self.pivot) else { return hits };
        let pc = self.camera.projection(self.map.comp[0] as f64, self.map.comp[1] as f64);
        for i in 0..3 {
            let col = color(i);
            if matches!(mode, GizmoMode::Universal | GizmoMode::Position) {
                let direction = self.basis[i];
                let endpoint = self.point(self.pivot + direction * self.radius);
                if let Some(endpoint) = endpoint {
                    let end = if center.distance(endpoint) < 8. { center + vec2(10. + i as f32 * 9., -14.) } else { endpoint };
                    painter.arrow(center, end - center, Stroke::new(2., col));
                    let from = end + (center - end) * 0.25;
                    self.register(app, &mut hits, Hit { kind: Kind::Position, axis: i, from, to: end, angle: 0. }, None);
                }
            }
            if matches!(mode, GizmoMode::Universal | GizmoMode::Scale) {
                let endpoint = self.point(self.pivot + self.local[i] * self.radius * 0.64);
                if let Some(endpoint) = endpoint {
                    let end = if center.distance(endpoint) < 8. { center + vec2(-10. - i as f32 * 9., 14.) } else { endpoint };
                    painter.line_segment([center, end], Stroke::new(1., col));
                    painter.rect_filled(Rect::from_center_size(end, vec2(7., 7.)), 0., col);
                    self.register(app, &mut hits, Hit { kind: Kind::Scale, axis: i, from: end, to: end, angle: 0. }, None);
                }
            }
            if matches!(mode, GizmoMode::Universal | GizmoMode::Rotation)
                && let Some((u, v)) = self.ring(i)
            {
                let radius = self.radius * 0.86;
                for n in 0..RING_STEPS {
                    let angle = n as f64 * std::f64::consts::TAU / RING_STEPS as f64;
                    let next = (n + 1) as f64 * std::f64::consts::TAU / RING_STEPS as f64;
                    let a = self.pivot + (u * angle.cos() + v * angle.sin()) * radius;
                    let b = self.pivot + (u * next.cos() + v * next.sin()) * radius;
                    if let Some([a, b]) = project_segment(&pc, a, b) {
                        let (a, b) = (self.map.to_screen(a), self.map.to_screen(b));
                        if a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite() && a.distance(b) < 1e5 {
                            painter.line_segment([a, b], Stroke::new(1.5, col));
                            self.register(app, &mut hits, Hit { kind: Kind::Rotation, axis: i, from: a, to: b, angle: (angle + next) * 0.5 }, Some(n));
                        }
                    }
                }
            }
        }
        painter.circle_filled(center, 2.5, Color32::WHITE);
        hits
    }
    fn register(&self, app: &mut EffectcraftApp, hits: &mut Vec<Hit>, hit: Hit, segment: Option<usize>) {
        let rect = Rect::from_two_pos(hit.from, hit.to).expand(5.);
        if !self.map.area.intersects(rect) {
            return;
        }
        let base = format!("viewer.gizmo.{}.{}", hit.kind.name(), name(hit.axis));
        let id = segment.map_or(base.clone(), |n| format!("{base}.{n}"));
        app.auto.add(&id, rect, &format!("3D {} {}", hit.kind.name(), name(hit.axis)));
        hits.push(hit);
    }
    fn poses(&self, ctx: &EvalCtx) -> Result<Vec<Pose>, String> {
        let selected: BTreeSet<_> = self.ids.iter().copied().collect();
        let mut poses = Vec::with_capacity(self.ids.len());
        for id in &self.ids {
            let l = ctx.comp.layer(*id).ok_or("3D transform layer disappeared")?;
            let mut parent = l.parent;
            let mut seen = BTreeSet::from([*id]);
            let mut descendant = false;
            while let Some(id) = parent {
                if seen.len() > 64 || !seen.insert(id) {
                    return Err("3D parent chain is cyclic or too deep".into());
                }
                descendant |= selected.contains(&id);
                parent = ctx.comp.layer(id).ok_or("3D parent is missing")?.parent;
            }
            if descendant {
                continue;
            }
            let tr = l.transform().ok_or("3D transform properties are unavailable")?;
            let parent = l.parent.and_then(|id| ctx.comp.layer(id)).map(|l| ctx.world_matrix(l)).unwrap_or(Mat4::IDENTITY);
            let inv = parent.inverse().filter(|m| matrix_finite(*m)).ok_or("3D parent has a singular or invalid transform")?;
            let orientation = ctx.v3(l, tr, "orientation", [0.; 3]);
            let rotations = [ctx.f(l, tr, "rotationX", 0.), ctx.f(l, tr, "rotationY", 0.), ctx.f(l, tr, "rotation", 0.)];
            let scale = ctx.v3(l, tr, "scale", [100.; 3]);
            let anchor = ctx.world_matrix(l).apply(Vec3::from(ctx.v3(l, tr, "anchor", [0.; 3])));
            if !finite(anchor) || orientation.iter().chain(&rotations).chain(&scale).any(|v| !v.is_finite() || v.abs() > 1e12) || !matrix_finite(parent) {
                return Err("3D transform values are non-finite or unsupported".into());
            }
            let rotation =
                Mat4::orientation(Vec3::from(orientation)) * Mat4::rotate_z(rotations[2]) * Mat4::rotate_y(rotations[1]) * Mat4::rotate_x(rotations[0]);
            poses.push(Pose {
                id: *id,
                anchor,
                parent,
                inv,
                scale,
                orientation,
                rotations,
                rotation_reference: rotations,
                rotation,
                auto: l.auto_orient != AutoOrient::Off,
            });
        }
        Ok(poses)
    }
    pub(crate) fn begin(&self, ctx: &EvalCtx, hit: Hit, press: Pos2) -> Result<Drag, String> {
        let direction = if hit.kind == Kind::Scale {
            *self.local.get(hit.axis).ok_or("3D axis is unavailable")?
        } else {
            *self.basis.get(hit.axis).ok_or("3D axis is unavailable")?
        };
        let poses = self.poses(ctx)?;
        if hit.kind == Kind::Rotation && poses.iter().any(|pose| pose.auto) {
            return Err("Disable Auto-Orient before rotating with the 3D gizmo".into());
        }
        let center = self.point(self.pivot).ok_or("3D pivot is behind the camera")?;
        let end = self.point(self.pivot + direction * self.radius).ok_or("3D axis is behind the camera")?;
        let projected = end - center;
        let linear = if projected.length_sq() > 4. { projected / (self.radius as f32) } else { vec2(0., -54. / self.radius as f32) };
        let (ring_u, ring_v) = self.ring(hit.axis).ok_or("3D rotation basis is invalid")?;
        let radial = ring_u * hit.angle.cos() + ring_v * hit.angle.sin();
        let p = self.pivot + radial * self.radius * 0.86;
        let tangent = direction.cross(radial);
        let screen = self.point(p).ok_or("3D handle is behind the camera")?;
        let tangent = self.point(p + tangent * self.radius * 0.0086).map(|next| (next - screen) * 100.).unwrap_or(vec2(0., 0.));
        let tangent = if tangent.length_sq() > 4. { tangent } else { vec2(54., 0.) };
        let plane_angle = self.plane_angle(press, direction, ring_u, ring_v);
        let start_angle = plane_angle.unwrap_or(hit.angle);
        Ok(Drag {
            geometry: self.clone(),
            hit,
            poses,
            direction,
            press,
            linear,
            ring_u,
            ring_v,
            tangent,
            start_angle,
            use_plane: plane_angle.is_some(),
            last_angle: start_angle,
            total_angle: 0.,
            last_value: None,
            comp: ctx.comp_id,
            time: ctx.time,
            context: None,
            selection: Vec::new(),
        })
    }
    pub(crate) fn begin_free(&self, ctx: &EvalCtx, press: Pos2) -> Result<Drag, String> {
        let mut geometry = self.clone();
        geometry.basis = [
            unit(self.camera.right()).ok_or("Invalid camera right axis")?,
            unit(self.camera.down()).ok_or("Invalid camera down axis")?,
            unit(self.camera.forward()).ok_or("Invalid camera forward axis")?,
        ];
        geometry.begin(ctx, Hit { kind: Kind::Rotation, axis: 2, from: press, to: press, angle: 0. }, press)
    }
    pub(crate) fn begin_axis(&self, ctx: &EvalCtx, axis: usize, press: Pos2) -> Result<Drag, String> {
        self.begin(ctx, Hit { kind: Kind::Rotation, axis, from: press, to: press, angle: 0. }, press)
    }
    fn plane_angle(&self, screen: Pos2, normal: Vec3, u: Vec3, v: Vec3) -> Option<f64> {
        let p = self.map.to_comp(screen);
        let x = (p[0] - self.map.comp[0] as f64 * 0.5) / self.camera.zoom;
        let y = (p[1] - self.map.comp[1] as f64 * 0.5) / self.camera.zoom;
        let origin = if self.camera.ortho { self.camera.eye + self.camera.right() * x + self.camera.down() * y } else { self.camera.eye };
        let ray = if self.camera.ortho { self.camera.forward() } else { unit(self.camera.right() * x + self.camera.down() * y + self.camera.forward())? };
        let denominator = ray.dot(normal);
        if !denominator.is_finite() || denominator.abs() < 1e-5 {
            return None;
        }
        let t = (self.pivot - origin).dot(normal) / denominator;
        // A plane through the eye has no unique projected angle. Keep using the screen
        // tangent for that entire gesture rather than jumping to the eye's radial angle.
        if !t.is_finite() || t <= EPS {
            return None;
        }
        let radial = origin + ray * t - self.pivot;
        if !finite(radial) || radial.length() < EPS {
            return None;
        }
        Some(radial.dot(v).atan2(radial.dot(u)))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Drag {
    geometry: Geometry,
    hit: Hit,
    poses: Vec<Pose>,
    direction: Vec3,
    press: Pos2,
    linear: Vec2,
    ring_u: Vec3,
    ring_v: Vec3,
    tangent: Vec2,
    start_angle: f64,
    use_plane: bool,
    last_angle: f64,
    total_angle: f64,
    last_value: Option<(f64, bool)>,
    comp: effectcraft_engine::project::ItemId,
    time: Tick,
    context: Option<(Tool, GizmoMode, RotationTarget)>,
    selection: Vec<LayerId>,
}

fn rotate(axis: Vec3, degrees: f64) -> Mat4 {
    let (s, c) = degrees.to_radians().sin_cos();
    let k = 1. - c;
    let [x, y, z] = array(axis);
    Mat4([
        [c + x * x * k, x * y * k - z * s, x * z * k + y * s, 0.],
        [y * x * k + z * s, c + y * y * k, y * z * k - x * s, 0.],
        [z * x * k - y * s, z * y * k + x * s, c + z * z * k, 0.],
        [0., 0., 0., 1.],
    ])
}
fn orientation(matrix: Mat4, reference: [f64; 3]) -> Result<[f64; 3], String> {
    let columns = [0, 1, 2].map(|i| matrix.apply_vec(axis(i)));
    if !matrix_finite(matrix)
        || columns.iter().any(|c| (c.dot(*c) - 1.).abs() > 1e-6)
        || columns[0].dot(columns[1]).abs() > 1e-6
        || columns[0].dot(columns[2]).abs() > 1e-6
        || columns[1].dot(columns[2]).abs() > 1e-6
        || columns[0].cross(columns[1]).dot(columns[2]) < 0.999999
    {
        return Err("This rotation would require shear in the parent space; use a uniformly scaled parent".into());
    }
    let m = matrix.0;
    let sy = (-m[2][0]).clamp(-1., 1.);
    let y = sy.asin();
    let (x, z) = if (1. - sy * sy).sqrt() > 1e-7 {
        (m[2][1].atan2(m[2][2]), m[1][0].atan2(m[0][0]))
    } else {
        let z = reference[2].to_radians();
        let combined = (-m[1][2]).atan2(m[1][1]);
        (combined + sy.signum() * z, z)
    };
    let a = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
    let nearest = |candidate: [f64; 3]| std::array::from_fn(|i| candidate[i] + 360. * ((reference[i] - candidate[i]) / 360.).round());
    let a: [f64; 3] = nearest(a);
    let b: [f64; 3] = nearest([a[0] + 180., 180. - a[1], a[2] + 180.]);
    let distance = |v: [f64; 3]| v.into_iter().zip(reference).map(|(x, y)| (x - y).powi(2)).sum::<f64>();
    Ok(if distance(a) <= distance(b) { a } else { b })
}
impl Drag {
    pub(crate) fn apply(&mut self, app: &mut EffectcraftApp, current: Option<&Geometry>, pos: Pos2, mods: egui::Modifiers, merge: &str) -> Result<(), String> {
        let original = &self.geometry;
        let context = mode(app).map(|mode| (app.ui.tool, mode, target(app)));
        let same_view = current.is_some_and(|current| {
            current.ids == original.ids
                && current.map.origin == original.map.origin
                && current.map.zoom == original.map.zoom
                && current.map.comp == original.map.comp
                && current.camera.view.0 == original.camera.view.0
                && current.camera.eye == original.camera.eye
                && current.camera.zoom == original.camera.zoom
                && current.camera.ortho == original.camera.ortho
        });
        if app.session.state.active_comp != Some(self.comp)
            || app.session.time() != self.time
            || context.is_none()
            || app.ui.viewer.gizmo_space != self.geometry.space
            || self.context.is_some_and(|previous| Some(previous) != context)
            || (self.context.is_some() && app.session.state.selected_layers != self.selection)
            || !same_view
        {
            return Err("3D gesture ended because its composition, time, tool, selection, view or axis space changed".into());
        }
        self.context = context;
        if self.selection.is_empty() {
            self.selection = app.session.state.selected_layers.clone();
        }
        let delta = pos - self.press;
        let value = match self.hit.kind {
            Kind::Position => delta.dot(self.linear) as f64 / self.linear.length_sq() as f64,
            Kind::Scale => 1. + delta.dot(self.linear) as f64 / self.linear.length_sq() as f64 / self.geometry.radius,
            Kind::Rotation => {
                let angle = if self.use_plane {
                    self.geometry.plane_angle(pos, self.direction, self.ring_u, self.ring_v).unwrap_or(self.last_angle)
                } else {
                    self.start_angle + delta.dot(self.tangent) as f64 / self.tangent.length_sq() as f64
                };
                self.total_angle += (angle - self.last_angle + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
                self.last_angle = angle;
                let degrees = self.total_angle.to_degrees();
                if mods.command {
                    degrees
                } else {
                    let step = if mods.shift { if app.ui.tool == Tool::Rotate { 15. } else { 5. } } else { 1. };
                    (degrees / step).round() * step
                }
            }
        };
        if !value.is_finite() || value.abs() > 1e10 {
            return Err("3D drag exceeds the supported finite range".into());
        }
        let uniform = self.hit.kind == Kind::Scale && mods.shift;
        if self.last_value == Some((value, uniform)) || (self.last_value.is_none() && value == if self.hit.kind == Kind::Scale { 1. } else { 0. }) {
            return Ok(());
        }
        let rotation = rotate(self.direction, value);
        let mut changes = Vec::with_capacity(self.poses.len());
        for pose in &mut self.poses {
            let mut change = json!({"layer":pose.id.0});
            let anchor = match self.hit.kind {
                Kind::Position => pose.anchor + self.direction * value,
                Kind::Rotation => self.geometry.pivot + rotation.apply_vec(pose.anchor - self.geometry.pivot),
                Kind::Scale => {
                    let d = pose.anchor - self.geometry.pivot;
                    self.geometry.pivot + if uniform { d * value } else { d + self.direction * (d.dot(self.direction) * (value - 1.)) }
                }
            };
            if self.hit.kind == Kind::Position || (anchor - pose.anchor).length() > EPS {
                change["position"] = json!(array(pose.inv.apply(anchor)));
            }
            match self.hit.kind {
                Kind::Position => {}
                Kind::Scale => {
                    let mut scale = pose.scale;
                    for (i, v) in scale.iter_mut().enumerate() {
                        if uniform || i == self.hit.axis {
                            *v *= value;
                        }
                    }
                    change["scale"] = json!(scale);
                }
                Kind::Rotation => {
                    let local = pose.inv * rotation * pose.parent * pose.rotation;
                    if target(app) == RotationTarget::Rotation {
                        let authored_orientation = Mat4::orientation(Vec3::from(pose.orientation));
                        let angles = orientation(authored_orientation.transpose() * local, pose.rotation_reference)?;
                        pose.rotation_reference = angles;
                        change["rotation"] = json!(angles);
                    } else {
                        let authored = Mat4::rotate_z(pose.rotations[2]) * Mat4::rotate_y(pose.rotations[1]) * Mat4::rotate_x(pose.rotations[0]);
                        change["orientation"] = json!(orientation(local * authored.transpose(), pose.orientation)?);
                    }
                }
            }
            changes.push(change);
        }
        app.session.execute("layer.setTransforms3D", json!({"changes":changes,"merge":merge})).map_err(|e| e.to_string())?;
        self.last_value = Some((value, uniform));
        Ok(())
    }
}

fn mode_choices(app: &mut EffectcraftApp, ui: &mut egui::Ui) -> Option<&'static str> {
    let mut selected = None;
    for (mode, id) in [(GizmoMode::Universal, "universal"), (GizmoMode::Position, "position"), (GizmoMode::Scale, "scale"), (GizmoMode::Rotation, "rotation")] {
        let response = ui.selectable_label(app.ui.viewer.gizmo == mode, format!("{mode:?}"));
        app.auto.add_response(&format!("tools.gizmo.{id}"), &response, &format!("{mode:?} gizmo"));
        if response.clicked() {
            selected = Some(id);
            ui.close();
        }
    }
    selected
}

fn space_choices(app: &mut EffectcraftApp, ui: &mut egui::Ui) -> Option<&'static str> {
    let mut selected = None;
    for (space, id) in [(GizmoSpace::Local, "local"), (GizmoSpace::World, "world"), (GizmoSpace::View, "view")] {
        let response = ui.selectable_label(app.ui.viewer.gizmo_space == space, format!("{space:?}"));
        app.auto.add_response(&format!("tools.gizmo.space.{id}"), &response, &format!("{space:?} axis space"));
        if response.clicked() {
            selected = Some(id);
            ui.close();
        }
    }
    selected
}

fn target_choices(app: &mut EffectcraftApp, ui: &mut egui::Ui) -> Option<&'static str> {
    let mut selected = None;
    for (target, id) in [(RotationTarget::Orientation, "orientation"), (RotationTarget::Rotation, "rotation")] {
        let response = ui.selectable_label(app.ui.viewer.rotation_target == target, format!("{target:?}"));
        app.auto.add_response(&format!("tools.rotation.{id}"), &response, &format!("Set {target:?}"));
        if response.clicked() {
            selected = Some(id);
            ui.close();
        }
    }
    selected
}

pub(crate) fn toolbar(app: &mut EffectcraftApp, ui: &mut egui::Ui, rect: Rect) -> bool {
    if mode(app).is_none()
        || !app.session.active_comp().is_some_and(|c| app.session.state.selected_layers.iter().any(|id| c.layer(*id).is_some_and(|l| l.is_3d())))
    {
        return false;
    }
    let mut selected = None;
    let mut space = None;
    let mut rotation_target = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        if rect.width() < 100. {
            let response = egui::ComboBox::from_id_salt("gizmo-compact")
                .selected_text("3D")
                .width(44.)
                .show_ui(ui, |ui| {
                    if app.ui.tool == Tool::Rotate {
                        ui.label("Set");
                        rotation_target = target_choices(app, ui);
                    } else {
                        ui.label("Transform gizmo");
                        selected = mode_choices(app, ui);
                    }
                    ui.separator();
                    ui.label("Axis space");
                    space = space_choices(app, ui);
                })
                .response
                .on_hover_text("3D transform gizmo and axis space (4/5/6; V for Universal)");
            app.auto.add_response("tools.gizmo.compact", &response, "3D transform gizmo and axis space");
            return;
        }
        ui.horizontal(|ui| {
            if app.ui.tool == Tool::Rotate {
                let response = egui::ComboBox::from_id_salt("rotation-target")
                    .selected_text(format!("Set: {:?}", app.ui.viewer.rotation_target))
                    .width(116.)
                    .show_ui(ui, |ui| target_choices(app, ui));
                rotation_target = response.inner.flatten();
                app.auto.add_response("tools.rotation.target", &response.response, "Set Orientation or Rotation");
            } else {
                let mode = app.ui.viewer.gizmo;
                let response = egui::ComboBox::from_id_salt("gizmo-mode").selected_text(format!("{mode:?}")).width(92.).show_ui(ui, |ui| mode_choices(app, ui));
                selected = response.inner.flatten();
                app.auto.add_response("tools.gizmo.mode", &response.response, "3D transform gizmo");
                response.response.on_hover_text("3D transform gizmo (4/5/6; V for Universal)");
            }
            let current = app.ui.viewer.gizmo_space;
            let response =
                egui::ComboBox::from_id_salt("gizmo-space").selected_text(format!("{current:?}")).width(70.).show_ui(ui, |ui| space_choices(app, ui));
            space = response.inner.flatten();
            app.auto.add_response("tools.gizmo.space", &response.response, "3D axis space");
        });
    });
    if let Some(mode) = selected
        && let Err(error) = crate::menus::invoke(app, ui.ctx(), &format!("view.gizmo.{mode}"), json!({}))
    {
        app.ui.status = error;
    }
    if let Some(space) = space
        && let Err(error) = crate::menus::invoke(app, ui.ctx(), "view.gizmo.space", json!({"space":space}))
    {
        app.ui.status = error;
    }
    if let Some(target) = rotation_target
        && let Err(error) = crate::menus::invoke(app, ui.ctx(), "view.gizmo.rotationTarget", json!({"target":target}))
    {
        app.ui.status = error;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_euler_roundtrips_axes_gimbals_and_multiple_turns_without_accepting_shear() {
        for angles in [[0., 0., 0.], [17., 38., -53.], [31., 90., -27.], [31., -90., -27.], [730., -215., 1090.]] {
            let matrix = Mat4::orientation(Vec3::from(angles));
            let recovered = orientation(matrix, angles).unwrap();
            let actual = Mat4::orientation(Vec3::from(recovered));
            for (a, b) in matrix.0.iter().flatten().zip(actual.0.iter().flatten()) {
                assert!((a - b).abs() < 1e-6, "{angles:?} => {recovered:?}");
            }
        }
        for i in 0..3 {
            let actual = rotate(axis(i), 37.);
            let expected = match i {
                0 => Mat4::rotate_x(37.),
                1 => Mat4::rotate_y(37.),
                _ => Mat4::rotate_z(37.),
            };
            for (a, b) in actual.0.iter().flatten().zip(expected.0.iter().flatten()) {
                assert!((a - b).abs() < 1e-9);
            }
        }
        assert!(orientation(Mat4::scale(vec3(2., 1., 1.)), [0.; 3]).is_err());
        assert!(orientation(Mat4::scale(vec3(-1., 1., 1.)), [0.; 3]).is_err());
        assert!(orientation(Mat4::rotate_x(f64::NAN), [0.; 3]).is_err());
    }

    #[test]
    fn screen_ray_recovers_ring_angles_in_perspective_and_orthographic_views() {
        let camera = effectcraft_engine::render::three_d::default_camera(320., 180.);
        let map = ViewerMap { origin: egui::pos2(100., 50.), zoom: 1.7, comp: [320., 180.], area: Rect::from_min_size(egui::Pos2::ZERO, vec2(1280., 800.)) };
        for camera in [camera, CameraState { ortho: true, zoom: 1.3, ..camera }] {
            let geometry = Geometry {
                ids: vec![],
                pivot: vec3(160., 90., 0.),
                basis: [axis(0), axis(1), axis(2)],
                local: [axis(0), axis(1), axis(2)],
                camera,
                map,
                radius: 24.,
                space: GizmoSpace::World,
            };
            let (u, v) = geometry.ring(2).unwrap();
            for angle in [-2.8_f64, -1.2, 0.3, 2.4] {
                let point = geometry.point(geometry.pivot + (u * angle.cos() + v * angle.sin()) * 24.).unwrap();
                let recovered = geometry.plane_angle(point, axis(2), u, v).unwrap();
                assert!((angle - recovered).abs() < 1e-5, "{angle} != {recovered}");
            }
            // End-on planes intentionally use a bounded screen tangent, not a division by zero.
            if camera.ortho {
                assert!(geometry.plane_angle(map.to_screen([160., 90.]), axis(0), axis(1), axis(2)).is_none());
            }
        }
    }
}

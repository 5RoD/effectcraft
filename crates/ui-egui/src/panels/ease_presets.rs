//! Ease Presets panel (Window ▸ Ease Presets): easing curves kept by name and applied to pairs of
//! keyframes (our own design and presets). The value graph at the top edits a working curve by
//! dragging its two handles, and Apply eases the selected keyframes with it; From Keys reads the
//! curve between the first selected pair. Clicking a preset's thumbnail loads it and applies it.
//! Save Current keeps the working curve under the name in the field; Rename and Delete act on the
//! selected user preset. Every action is a `keys.easePreset.*` command.
//!
//! Automation ids: `easePresets.graph`, `easePresets.handle.out` / `.in`, `easePresets.apply`,
//! `easePresets.fromKeys`, `easePresets.name`, `easePresets.save`, `easePresets.rename`,
//! `easePresets.delete`, `easePresets.preset.<n>` (1-based, labelled with the preset's name).
//! Precise editors: `easePresets.handle.x1` / `.y1` / `.x2` / `.y2`, `.bezierText`,
//! `.copy`, `.paste`, `.useCurve`, and `.curveError` when validation fails.

use effectcraft_engine::ease_presets;
use effectcraft_engine::keyframe::EaseCurve;
use egui::epaint::CubicBezierShape;
use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::panel_kit as kit;
use crate::theme::Tokens;
use crate::{EffectcraftApp, widgets};

/// Panel state (serde in the UI state).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EasePanelState {
    /// The curve the graph shows and Apply uses.
    pub curve: EaseCurve,
    /// The preset clicked last (Rename and Delete act on it).
    pub selected: Option<String>,
    /// Name for Save Current and Rename.
    pub name: String,
    /// Pending edits stay separate from the last valid curve until committed.
    pub handle_text: [String; 4],
    pub bezier_text: String,
    pub curve_error: Option<String>,
    synced_curve: Option<EaseCurve>,
    numbers_dirty: bool,
}

impl Default for EasePanelState {
    fn default() -> Self {
        let mut state = EasePanelState {
            curve: EaseCurve::LINEAR,
            selected: None,
            name: String::new(),
            handle_text: Default::default(),
            bezier_text: String::new(),
            curve_error: None,
            synced_curve: None,
            numbers_dirty: false,
        };
        state.sync_fields();
        state
    }
}

impl EasePanelState {
    fn sync_fields(&mut self) {
        self.handle_text = self.curve.handles().map(|value| value.to_string());
        self.bezier_text = bezier_text(&self.curve);
        self.synced_curve = Some(self.curve);
        self.numbers_dirty = false;
    }

    fn accept(&mut self, curve: Result<EaseCurve, String>) {
        match curve {
            Ok(curve) => {
                self.curve = curve;
                self.selected = None;
                self.curve_error = None;
                self.sync_fields();
            }
            Err(error) => self.curve_error = Some(error),
        }
    }
}

fn bezier_text(curve: &EaseCurve) -> String {
    let [x1, y1, x2, y2] = curve.handles();
    // Display uses round-trippable f64 text, rather than the summary's rounded values.
    format!("cubic-bezier({x1}, {y1}, {x2}, {y2})")
}

fn checked_handles(handles: [f64; 4]) -> Result<EaseCurve, String> {
    if handles.iter().any(|value| !value.is_finite()) {
        return Err("Use four finite numbers.".into());
    }
    let [x1, _, x2, _] = handles;
    if !(0.001..=1.0).contains(&x1) || !(0.0..=0.999).contains(&x2) {
        return Err("X1 must be 0.001–1 and X2 0–0.999.".into());
    }
    let curve = EaseCurve::from_handles(handles).ok_or("Use four finite numbers.")?;
    // The engine sanitizes unsupported slopes. Entry reports that bound rather than
    // silently changing a pasted curve; ordinary floating-point roundoff is allowed.
    if curve.handles().iter().zip(handles).any(|(actual, requested)| (actual - requested).abs() > 16.0 * f64::EPSILON * requested.abs().max(1.0)) {
        return Err("Y handles exceed relative speeds −100–100.".into());
    }
    Ok(curve)
}

fn parse_numbers<'a>(mut parts: impl Iterator<Item = &'a str>) -> Result<EaseCurve, String> {
    let mut handles = [0.0; 4];
    for value in &mut handles {
        *value = parts.next().ok_or("Enter exactly four numbers.")?.trim().parse::<f64>().map_err(|_| "Use four finite numbers.")?;
    }
    if parts.next().is_some() {
        return Err("Enter exactly four numbers.".into());
    }
    checked_handles(handles)
}

fn parse_bezier(text: &str) -> Result<EaseCurve, String> {
    if text.len() > 512 {
        return Err("Curve text is limited to 512 bytes.".into());
    }
    let text = text.trim().strip_suffix(';').unwrap_or(text.trim()).trim();
    let body = if let Some(rest) = text.strip_prefix("cubic-bezier") {
        rest.trim().strip_prefix('(').and_then(|rest| rest.strip_suffix(')')).ok_or("Use cubic-bezier(x1, y1, x2, y2).")?
    } else if let Some(rest) = text.strip_prefix('[') {
        rest.strip_suffix(']').ok_or("Close the four-number array with ].")?
    } else {
        text
    };
    parse_numbers(body.split(','))
}

/// Include both keys and handles, with room for the complete pointer hit targets.
fn graph_range(curve: &EaseCurve) -> [f64; 2] {
    let [_, y1, _, y2] = curve.handles();
    let lo = y1.min(y2).min(0.0);
    let hi = y1.max(y2).max(1.0);
    let pad = (hi - lo) * 0.15;
    [lo - pad, hi + pad]
}

#[derive(Clone)]
struct GraphDrag {
    range: [f64; 2],
    plot: Rect,
    origin: Pos2,
    handles: [f64; 4],
}
const TILE: egui::Vec2 = vec2(84.0, 64.0);
const GAP: f32 = 6.0;
const ROW: f32 = 30.0;
/// Height of the controls under the graph (numbers, three button rows, status, "Presets").
const CONTROLS: f32 = 342.0;
/// Panels at least this wide put the presets beside the graph and controls.
const SIDE_BY_SIDE: f32 = 560.0;

/// Screen point of normalised time `x` and value `v` in `r`, values `lo..hi` bottom to top.
fn to_screen(r: Rect, lo: f64, hi: f64, x: f64, v: f64) -> Pos2 {
    pos2(r.min.x + r.width() * x as f32, r.max.y - r.height() * ((v - lo) / (hi - lo)) as f32)
}

/// The curve from (0, 0) to (1, 1) through its handles.
fn curve_shape(c: &EaseCurve, at: impl Fn(f64, f64) -> Pos2, stroke: Stroke) -> CubicBezierShape {
    let [x1, y1, x2, y2] = c.handles();
    CubicBezierShape::from_points_stroke([at(0.0, 0.0), at(x1, y1), at(x2, y2), at(1.0, 1.0)], false, Color32::TRANSPARENT, stroke)
}

pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let p = ui.painter().with_clip_rect(rect);
    let mut st = app.ui.ease_presets.clone();
    st.curve = st.curve.sanitized().unwrap_or(EaseCurve::LINEAR);
    if st.synced_curve != Some(st.curve) {
        st.sync_fields();
    }
    let pairs = ease_presets::selected_pairs(&app.session).len();
    let x0 = rect.min.x + 10.0;
    // Wide panels: graph and controls on the left, presets on the right; else presets below,
    // with room for at least one row of them.
    let side = rect.width() >= SIDE_BY_SIDE;
    let w = if side { 300.0 } else { (rect.width() - 20.0).max(210.0) };
    let room = rect.height() - 16.0 - CONTROLS - if side { 0.0 } else { TILE.y + GAP };
    let graph_height = (w * 0.6).min(room).clamp(90.0, 220.0);
    let list_width = rect.width() - if side { w + 36.0 } else { 20.0 };
    let rows = ease_presets::presets(&app.session).count().div_ceil(columns(list_width));
    let list_height = rows as f32 * (TILE.y + GAP);
    let content_height = if side { (16.0 + graph_height + CONTROLS).max(34.0 + list_height) } else { 16.0 + graph_height + CONTROLS + list_height };
    // The graph, controls and full preset grid share one scroll owner, so short docks keep
    // library controls reachable and a wheel over the grid does not scroll two regions.
    let scroll = widgets::PanelScroll::begin(ui, egui::Id::new("ease-presets-scroll"), rect);
    let content = Rect::from_min_size(rect.min - vec2(0.0, scroll.offset), vec2(rect.width(), rect.height().max(content_height)));
    let g = Rect::from_min_size(pos2(x0, content.min.y + 10.0), vec2(w, graph_height));
    let before = st.curve;
    graph(app, ui, g, &mut st.curve);
    if st.curve != before {
        st.selected = None;
        st.curve_error = None;
        st.sync_fields();
    }
    let mut y = g.max.y + 4.0;
    let c = st.curve;
    let numbers = format!("Out {:.1} % · {:.2}      In {:.1} % · {:.2}", c.out_influence, c.out_speed, c.in_influence, c.in_speed);
    p.text(pos2(x0, y + 8.0), Align2::LEFT_CENTER, numbers, Tokens::mono(11.0), t.text_faint);
    y += 22.0;
    y = precise_controls(app, ui, &mut st, x0, y, w);
    let c = st.curve;
    let at = |x: f32, y: f32, width: f32| Rect::from_min_size(pos2(x, y), vec2(width, 24.0));
    if kit::button(app, ui, at(x0, y, 90.0), "easePresets.apply", "Apply", pairs > 0) {
        apply(app, json!({"curve": c}), pairs, "the curve");
    }
    if kit::button(app, ui, at(x0 + 96.0, y, 100.0), "easePresets.fromKeys", "From Keys", false)
        && let Some(v) = kit::exec(app, "keys.easePreset.capture", json!({}))
        && let Ok(curve) = serde_json::from_value(v)
    {
        st.curve = curve;
        st.selected = None;
        st.curve_error = None;
        st.sync_fields();
    }
    y += ROW;
    let field = at(x0, y, (w - 116.0).max(80.0));
    widgets::text_field(ui, field, &mut st.name, "Preset name", &t);
    app.auto.add("easePresets.name", field, "Preset name");
    let name = st.name.trim().to_string();
    if kit::button(app, ui, at(field.max.x + 6.0, y, 110.0), "easePresets.save", "Save Current", false) {
        if name.is_empty() {
            app.ui.status = "Type a name for the preset first".into();
        } else if let Some(v) = kit::exec(app, "keys.easePreset.save", json!({"name": name, "curve": c})) {
            st.selected = v["name"].as_str().map(str::to_string);
            app.ui.status = format!("Saved the ease preset “{}”", st.selected.as_deref().unwrap_or(&name));
        }
    }
    y += ROW;
    // Rename and Delete act on the selected user preset.
    let user = st.selected.clone().filter(|n| app.session.ease_presets.iter().any(|q| &q.name == n));
    let rename = kit::button(app, ui, at(x0, y, 90.0), "easePresets.rename", "Rename", false);
    let delete = kit::button(app, ui, at(x0 + 96.0, y, 90.0), "easePresets.delete", "Delete", false);
    match (&user, rename, delete) {
        (None, true, _) | (None, _, true) => app.ui.status = "Select one of your own presets first".into(),
        (Some(old), true, _) if name.is_empty() => app.ui.status = format!("Type a new name for “{old}” first"),
        (Some(old), true, _) => {
            if let Some(v) = kit::exec(app, "keys.easePreset.rename", json!({"name": old, "newName": name})) {
                st.selected = v["name"].as_str().map(str::to_string);
            }
        }
        (Some(old), _, true) => {
            let deleted = kit::exec(app, "keys.easePreset.delete", json!({"name": old})).is_some();
            if deleted {
                st.selected = None;
            }
        }
        _ => {}
    }
    y += ROW;
    for (x, id, label) in [(x0, "export", "Export Library"), (x0 + 126.0, "import", "Import Library")] {
        if kit::button(app, ui, at(x, y, 120.0), &format!("easePresets.{id}"), label, false) {
            let ctx = ui.ctx().clone();
            if let Err(e) = crate::menus::invoke(app, &ctx, &format!("keys.easePreset.{id}"), json!({})) {
                app.ui.status = e;
            }
        }
    }
    y += ROW;
    let msg = if pairs > 0 {
        format!("{pairs} keyframe pair{} selected", if pairs == 1 { "" } else { "s" })
    } else {
        "Select two or more neighbouring keyframes".into()
    };
    p.text(pos2(x0, y + 4.0), Align2::LEFT_CENTER, msg, Tokens::ui(11.0), t.text_faint);
    y += 16.0;
    let head = if side { pos2(x0 + w + 16.0, content.min.y + 6.0) } else { pos2(x0, y) };
    kit::label(ui, head, "Presets", &t);
    let list = Rect::from_min_max(head + vec2(0.0, 22.0), pos2(content.max.x - 10.0, content.max.y - 6.0));
    if let Some((name, curve)) = grid(app, ui, list, st.selected.as_deref()) {
        st.curve = curve;
        st.curve_error = None;
        st.sync_fields();
        if pairs > 0 {
            apply(app, json!({"preset": name}), pairs, &format!("“{name}”"));
        }
        st.selected = Some(name);
    }
    if st != app.ui.ease_presets {
        app.ui.ease_presets = st;
    }
    scroll.end(ui, &mut app.auto, "easePresets.scroll", content_height, &t);
}

fn precise_controls(app: &mut EffectcraftApp, ui: &mut egui::Ui, st: &mut EasePanelState, x: f32, mut y: f32, width: f32) -> f32 {
    let t = app.tokens;
    let column = (width - GAP) * 0.5;
    let mut commit = false;
    for (index, (name, text)) in ["x1", "y1", "x2", "y2"].into_iter().zip(st.handle_text.iter_mut()).enumerate() {
        let at = pos2(x + (index % 2) as f32 * (column + GAP), y + (index / 2) as f32 * ROW);
        kit::label(ui, at, name, &t);
        let rect = Rect::from_min_size(at + vec2(22.0, 0.0), vec2(column - 22.0, 24.0));
        let response = ui.push_id(("easePresets.number", name), |ui| widgets::text_field(ui, rect, text, name, &t)).inner;
        app.auto.add(&format!("easePresets.handle.{name}"), rect, text);
        st.numbers_dirty |= response.changed();
        commit |= response.lost_focus() || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)));
    }
    if commit && st.numbers_dirty {
        st.accept(parse_numbers(st.handle_text.iter().map(String::as_str)));
    }
    y += 2.0 * ROW;
    let rect = Rect::from_min_size(pos2(x, y), vec2(width, 24.0));
    let field = ui.push_id("easePresets.bezierText", |ui| widgets::text_field(ui, rect, &mut st.bezier_text, "cubic-bezier(x1, y1, x2, y2)", &t)).inner;
    app.auto.add("easePresets.bezierText", rect, &st.bezier_text);
    if (field.has_focus() || field.lost_focus()) && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
        st.accept(parse_bezier(&st.bezier_text));
    }
    y += ROW;
    let button_width = (width - 2.0 * GAP) / 3.0;
    let at = |column: f32| Rect::from_min_size(pos2(x + column * (button_width + GAP), y), vec2(button_width, 24.0));
    if kit::button(app, ui, at(0.0), "easePresets.copy", "Copy Curve", false) {
        ui.ctx().copy_text(bezier_text(&st.curve));
        app.ui.status = "Copied the working cubic-bezier curve".into();
    }
    if kit::button(app, ui, at(1.0), "easePresets.paste", "Paste Curve", false) {
        let clipboard = app.hooks.clipboard_text.as_ref().and_then(|read| read());
        match clipboard {
            Some(text) => {
                st.accept(parse_bezier(&text));
            }
            None => {
                field.request_focus();
                app.ui.status = "Paste into the curve field, then press Enter or Use Curve".into();
            }
        }
    }
    if kit::button(app, ui, at(2.0), "easePresets.useCurve", "Use Curve", false) {
        st.accept(parse_bezier(&st.bezier_text));
    }
    y += ROW;
    if let Some(error) = &st.curve_error {
        let rect = Rect::from_min_size(pos2(x, y), vec2(width, 38.0));
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let galley = painter.layout(error.clone(), Tokens::ui(11.0), t.danger, width);
        painter.galley(rect.min, galley, t.danger);
        app.auto.add("easePresets.curveError", rect, error);
    }
    y + 38.0
}

fn apply(app: &mut EffectcraftApp, params: serde_json::Value, pairs: usize, what: &str) {
    if pairs == 0 {
        app.ui.status = "Select two or more neighbouring keyframes of a property".into();
    } else if let Some(r) = kit::exec(app, "keys.easePreset.apply", params) {
        app.ui.status = format!("Eased {} keyframe pair(s) with {what}", r["pairs"]);
    }
}

/// The working curve's value graph: keys at (0, 0) and (1, 1), the linear ease for reference, and
/// the two handles, dragged to edit the curve.
fn graph(app: &mut EffectcraftApp, ui: &mut egui::Ui, g: Rect, curve: &mut EaseCurve) {
    let t = app.tokens;
    let p = ui.painter().with_clip_rect(g.intersect(ui.clip_rect()));
    p.rect_filled(g, 3.0, t.field_bg);
    p.rect_stroke(g, 3.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    app.auto.add("easePresets.graph", g, "Ease curve");
    let plot = g.shrink2(vec2(16.0, 6.0));
    let drag_id = egui::Id::new("easePresets.graph.drag");
    if !ui.input(|input| input.pointer.primary_down()) {
        ui.data_mut(|data| data.remove::<GraphDrag>(drag_id));
    }
    let dragging = ui.data(|data| data.get_temp::<GraphDrag>(drag_id));
    let [lo, hi] = dragging.as_ref().map(|drag| drag.range).unwrap_or_else(|| graph_range(curve));
    let at = |x: f64, v: f64| to_screen(plot, lo, hi, x, v);
    for v in [0.0, 1.0] {
        p.line_segment([at(0.0, v), at(1.0, v)], Stroke::new(1.0, t.separator));
    }
    p.add(egui::Shape::dashed_line(&[at(0.0, 0.0), at(1.0, 1.0)], Stroke::new(1.0, t.separator), 4.0, 4.0));
    p.add(curve_shape(curve, at, Stroke::new(2.0, t.accent)));
    let [x1, y1, x2, y2] = curve.handles();
    let mut next = None;
    for (out, key, h) in [(true, at(0.0, 0.0), at(x1, y1)), (false, at(1.0, 1.0), at(x2, y2))] {
        p.line_segment([key, h], Stroke::new(1.0, t.text_dim));
        p.rect_filled(Rect::from_center_size(key, vec2(7.0, 7.0)), 1.0, t.keyframe);
        let r = Rect::from_center_size(h, vec2(14.0, 14.0));
        let resp = ui.interact(r, egui::Id::new(("easePresets.handle", out)), Sense::drag());
        app.auto.add(if out { "easePresets.handle.out" } else { "easePresets.handle.in" }, r, if out { "Out handle" } else { "In handle" });
        let hot = resp.hovered() || resp.dragged();
        if hot {
            ui.ctx().set_cursor_icon(if resp.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
        }
        p.circle_filled(h, if hot { 5.5 } else { 4.5 }, if hot { t.keyframe_selected } else { t.text });
        if resp.drag_started()
            && let Some(origin) = ui.input(|input| input.pointer.press_origin())
        {
            ui.data_mut(|data| data.insert_temp(drag_id, GraphDrag { range: [lo, hi], plot, origin, handles: curve.handles() }));
        }
        if resp.dragged_by(egui::PointerButton::Primary)
            && let Some(pointer) = resp.interact_pointer_pos()
            && let Some(drag) = ui.data(|data| data.get_temp::<GraphDrag>(drag_id))
        {
            // The starting range and pointer offset are fixed for the entire gesture.
            // Re-fitting on every update would change the units per pixel mid-drag.
            let delta = pointer - drag.origin;
            let mut handles = drag.handles;
            let (xi, yi) = if out { (0, 1) } else { (2, 3) };
            if let Some(x) = handles.get_mut(xi) {
                *x = (*x + f64::from(delta.x / drag.plot.width())).clamp(if out { 0.001 } else { 0.0 }, if out { 1.0 } else { 0.999 });
            }
            if let Some(y) = handles.get_mut(yi) {
                *y -= f64::from(delta.y / drag.plot.height()) * (drag.range[1] - drag.range[0]);
            }
            next = EaseCurve::from_handles(handles);
        }
    }
    if let Some(c) = next {
        *curve = c;
    }
}

fn columns(width: f32) -> usize {
    (((width + GAP) / (TILE.x + GAP)).floor() as usize).max(1)
}

/// Preset thumbnails in the panel's scrolling content; returns the clicked one.
fn grid(app: &mut EffectcraftApp, ui: &mut egui::Ui, list: Rect, selected: Option<&str>) -> Option<(String, EaseCurve)> {
    let t = app.tokens;
    let visible = list.intersect(ui.clip_rect());
    let p = ui.painter().with_clip_rect(visible);
    let cols = columns(list.width());
    let mut clicked = None;
    for (n, (name, curve, built_in)) in ease_presets::presets(&app.session).enumerate() {
        let r = Rect::from_min_size(list.min + vec2((n % cols) as f32 * (TILE.x + GAP), (n / cols) as f32 * (TILE.y + GAP)), TILE);
        if !r.intersects(visible) {
            continue;
        }
        let resp = ui.interact(r.intersect(visible), egui::Id::new(("easePresets.preset", n)), Sense::click());
        app.auto.add(&format!("easePresets.preset.{}", n + 1), r, name);
        let on = selected == Some(name);
        p.rect_filled(
            r,
            4.0,
            if on {
                t.row_selected
            } else if resp.hovered() {
                t.hover
            } else {
                t.field_bg
            },
        );
        let thumb = Rect::from_min_size(r.min, vec2(TILE.x, TILE.y - 16.0)).shrink2(vec2(12.0, 7.0));
        let [_, y1, _, y2] = curve.handles();
        let (lo, hi) = (y1.min(y2).min(0.0), y1.max(y2).max(1.0));
        p.add(curve_shape(&curve, |x, v| to_screen(thumb, lo, hi, x, v), Stroke::new(1.5, if on { t.text } else { t.text_dim })));
        widgets::text_fit(
            &p,
            pos2(r.center().x, r.max.y - 9.0),
            Align2::CENTER_CENTER,
            name,
            Tokens::ui(11.0),
            TILE.x - 6.0,
            if built_in { t.text_dim } else { t.text },
        );
        if resp.on_hover_text(format!("{name}: click to apply it to the selected keyframes")).clicked() {
            clicked = Some((name.to_string(), curve));
        }
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precise_curve_text_round_trips_overshoot_and_rejects_unrepresentable_input() {
        let handles = [0.4, 3.0, 0.6, -2.0];
        let expected = checked_handles(handles).unwrap();
        assert_eq!(expected.handles(), handles);
        for input in ["cubic-bezier(0.4, 3, 0.6, -2)", " cubic-bezier (4e-1, 3, 6e-1, -2); ", "[0.4, 3, 0.6, -2]", "0.4,3,0.6,-2"] {
            let curve = parse_bezier(input).unwrap();
            assert_eq!(curve, expected, "{input}");
            assert_eq!(parse_bezier(&bezier_text(&curve)).unwrap(), curve);
        }
        let mut state = EasePanelState::default();
        state.accept(Ok(expected));
        let valid_text = state.bezier_text.clone();
        for input in [
            "",
            "0.4,3,0.6",
            "0.4,3,0.6,-2,7",
            "cubic-bezier(0.4,NaN,0.6,-2)",
            "0.4,inf,0.6,-2",
            "0.4,1e309,0.6,-2",
            "cubic-bezier(0.4,3,0.6,-2",
            "cubic-bezier(0.4,3,0.6,-2) junk",
            "0,0,0.6,-2",
            "0.4,3,1,1",
            "0.4,41,0.6,-2",
        ] {
            state.accept(parse_bezier(input));
            assert!(state.curve_error.is_some(), "{input}");
            assert_eq!(state.curve, expected, "invalid text changed the working curve: {input}");
            assert_eq!(state.bezier_text, valid_text, "rejected clipboard input must not replace usable curve text");
        }
        state.accept(parse_bezier(&" ".repeat(513)));
        assert!(state.curve_error.is_some());
        assert_eq!(state.curve, expected);
    }

    #[test]
    fn graph_range_includes_extreme_valid_handles_and_full_hit_targets() {
        for handles in [[0.4, 3.0, 0.6, -2.0], [1.0, 100.0, 0.0, -99.0], [0.001, -0.1, 0.999, 1.1]] {
            let curve = checked_handles(handles).unwrap();
            let [lo, hi] = graph_range(&curve);
            let graph = Rect::from_min_size(Pos2::ZERO, vec2(210.0, 90.0));
            let plot = graph.shrink2(vec2(16.0, 6.0));
            for [x, y] in [[handles[0], handles[1]], [handles[2], handles[3]]] {
                assert!(graph.contains_rect(Rect::from_center_size(to_screen(plot, lo, hi, x, y), vec2(14.0, 14.0))));
            }
        }
    }
}

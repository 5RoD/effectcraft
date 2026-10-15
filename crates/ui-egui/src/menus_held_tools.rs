//! Spring-loaded, unmodified letter shortcuts. State is transient and bounded by the 26 keys.

use crate::{EffectcraftApp, state::Tool};
use egui::{Context, Key, Modifiers};

const TAP_SECONDS: f64 = 0.3;
const SLOT_COUNT: usize = Tool::SLOTS.len();

#[derive(Clone, Copy)]
pub(super) struct Snapshot {
    tool: Tool,
    slots: [Tool; SLOT_COUNT],
    gizmo: crate::state::GizmoMode,
}

impl Snapshot {
    pub(super) fn capture(app: &EffectcraftApp) -> Self {
        Self { tool: app.ui.tool, slots: std::array::from_fn(|i| app.ui.slot_tools.get(i).copied().unwrap_or(Tool::Selection)), gizmo: app.ui.viewer.gizmo }
    }

    fn restore(self, app: &mut EffectcraftApp, applied: Self) {
        if app.ui.tool == applied.tool {
            app.ui.tool = self.tool;
            if app.ui.viewer.gizmo == applied.gizmo {
                app.ui.viewer.gizmo = self.gizmo;
            }
        }
        self.restore_slots(app, applied);
    }

    fn restore_slots(self, app: &mut EffectcraftApp, applied: Self) {
        for ((slot, before), after) in app.ui.slot_tools.iter_mut().zip(self.slots).zip(applied.slots) {
            if *slot == after {
                *slot = before;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Hold {
    key: Key,
    since: f64,
    used: bool,
    before: Snapshot,
    applied: Snapshot,
}

impl Hold {
    fn temporary(self, now: f64) -> bool {
        self.used || now.is_finite() && self.since.is_finite() && now - self.since >= TAP_SECONDS
    }
}

#[derive(Clone, Default)]
struct Holds(Vec<Hold>);

fn id() -> egui::Id {
    egui::Id::new("held-tool-shortcuts")
}

pub(super) fn eligible(key: Key, modifiers: Modifiers, command: &str) -> bool {
    let name = key.name().as_bytes();
    !modifiers.any() && command.starts_with("tool.") && name.len() == 1 && name.first().is_some_and(u8::is_ascii_alphabetic)
}

/// Run even when there are no key events, before typing/dialog/menu early returns.
pub(super) fn update(app: &mut EffectcraftApp, ctx: &Context) {
    let blocked = app.dialog.is_some() || app.session.state.text_edit.is_some() || ctx.egui_wants_keyboard_input() || crate::widgets::any_menu_open(ctx);
    let (lost, used) = ctx.input(|i| {
        (i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false))), i.pointer.any_down() || i.pointer.any_pressed() || i.pointer.any_released())
    });
    ctx.data_mut(|d| {
        let state = d.get_temp_mut_or_default::<Holds>(id());
        if blocked || lost || state.0.last().is_some_and(|last| last.applied.tool != app.ui.tool) {
            // Restore only unchanged temporary state; an explicit tool choice owns its tool.
            if let Some((first, last)) = state.0.first().zip(state.0.last()) {
                first.before.restore(app, last.applied);
            }
            state.0.clear();
        } else if used {
            for held in &mut state.0 {
                held.used = true;
            }
        }
    });
}

pub(super) fn begin(app: &EffectcraftApp, ctx: &Context, key: Key, before: Snapshot) {
    let (since, used) = ctx.input(|i| (i.time, i.pointer.any_down() || i.pointer.any_pressed()));
    ctx.data_mut(|d| {
        let state = d.get_temp_mut_or_default::<Holds>(id());
        if state.0.len() < 26 && !state.0.iter().any(|held| held.key == key) {
            state.0.push(Hold { key, since, used, before, applied: Snapshot::capture(app) });
        }
    });
}

pub(super) fn release(app: &mut EffectcraftApp, ctx: &Context, key: Key) {
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| {
        let state = d.get_temp_mut_or_default::<Holds>(id());
        let Some(index) = state.0.iter().position(|held| held.key == key) else { return };
        let held = state.0.remove(index);
        if held.temporary(now) {
            if let Some(child) = state.0.get_mut(index) {
                // A parent was released first: the child must return past that temporary tool.
                child.before = held.before;
            } else {
                held.before.restore(app, held.applied);
            }
        } else if index == state.0.len() {
            // A quick tap is an explicit choice; older held keys cannot undo that choice.
            state.0.clear();
        }
    });
}

fn dispatch_id() -> egui::Id {
    egui::Id::new("held-tool-key-dispatch")
}

/// The marker is removed during unwinding too; a failed command cannot claim later inputs.
pub(super) struct KeyDispatch(Context);

impl KeyDispatch {
    pub(super) fn new(ctx: &Context) -> Self {
        ctx.data_mut(|d| d.insert_temp(dispatch_id(), true));
        Self(ctx.clone())
    }
}

impl Drop for KeyDispatch {
    fn drop(&mut self) {
        self.0.data_mut(|d| d.remove::<bool>(dispatch_id()));
    }
}

pub(super) fn explicit_choice(app: &mut EffectcraftApp, ctx: &Context) {
    if ctx.data(|d| d.get_temp::<bool>(dispatch_id())).unwrap_or(false) {
        return;
    }
    if let Some(state) = ctx.data_mut(|d| d.remove_temp::<Holds>(id()))
        && let Some((first, last)) = state.0.first().zip(state.0.last())
    {
        first.before.restore_slots(app, last.applied);
    }
}

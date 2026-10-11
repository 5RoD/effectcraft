use std::sync::Arc;

use effectcraft_engine::{Session, autosave::Recovery, config::MemoryConfig};
use effectcraft_ui_egui::{
    Dialog, EffectcraftApp,
    control::{self, ControlRequest, Outcome},
};
use egui_kittest::Harness;
use serde_json::json;

fn offer(name: &str) -> Recovery {
    Recovery {
        project: Some(format!("C:/Projects/{name}.ecproj")),
        autosave: Some(format!("C:/Recovery/{name}/Recovered.ecproj")),
        dirty: true,
        completed_edits: true,
        warning: None,
    }
}

fn harness(size: egui::Vec2, recovery: Recovery) -> Harness<'static, EffectcraftApp> {
    let mut session = Session { config: Some(Arc::new(MemoryConfig::default())), ..Default::default() };
    session.begin_recovery();
    session.autosave.run.offered = Some(recovery.clone());
    Harness::builder().with_size(size).build_eframe(|_| {
        let mut app = EffectcraftApp::new(session);
        app.offer_recovery(recovery);
        app
    })
}

#[test]
fn resolving_recovery_through_the_command_channel_advances_and_closes_the_dialog() {
    let mut h = harness(egui::vec2(1000.0, 700.0), offer("First"));
    let second = offer("Second");
    h.state_mut().session.autosave.run.pending.push_back(second.clone());
    h.run_steps(3);
    let ctx = h.ctx.clone();
    for expected in [Some(second), None] {
        let (request, _) = ControlRequest::new("engine.execute", json!({"command": "file.resolveRecovery", "params": {"dismiss": true}}));
        let Outcome::Done(reply) = control::handle(h.state_mut(), &ctx, &request) else { panic!("expected immediate recovery reply") };
        assert_eq!(reply["ok"], true, "{reply}");
        h.run_steps(3);
        assert_eq!(h.state().recovery, expected);
        assert_eq!(h.state().dialog, expected.as_ref().map(|_| Dialog::Recovery));
    }
}

#[test]
fn an_unopened_recovery_keeps_its_choice_after_a_failed_resolution() {
    let recovery = offer("Preserved");
    let mut h = harness(egui::vec2(1000.0, 700.0), recovery.clone());
    h.run_steps(3);
    assert!(h.state_mut().session.execute("file.resolveRecovery", json!({})).is_err());
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::Recovery));
    assert_eq!(h.state().recovery, Some(recovery));
}

#[test]
fn long_recovery_details_keep_all_choices_visible_in_a_small_window() {
    for size in [egui::vec2(640.0, 480.0), egui::vec2(640.0, 320.0), egui::vec2(1000.0, 700.0)] {
        let mut recovery = offer(&"Project Ω 🚀 — long directory name /".repeat(10));
        recovery.warning = Some("The last complete edit was recovered. A damaged record was preserved for inspection. ".repeat(12));
        let mut h = harness(size, recovery);
        h.run_steps(5);
        for id in ["recovery.openAutoSave", "recovery.openProject", "recovery.dismiss"] {
            let element = h.state().auto.find(id).unwrap_or_else(|| panic!("missing {id}"));
            let rect = egui::Rect::from_min_size(egui::pos2(element.rect[0], element.rect[1]), egui::vec2(element.rect[2], element.rect[3]));
            assert!(h.ctx.content_rect().contains_rect(rect), "{id} leaves {size:?}: {rect:?}");
        }
        if let Ok(dir) = std::env::var("UI_RECOVERY_REVIEW_DIR") {
            std::fs::create_dir_all(&dir).unwrap();
            h.render().unwrap().save(std::path::Path::new(&dir).join(format!("recovery-{}-{}.png", size.x, size.y))).unwrap();
        }
    }
}

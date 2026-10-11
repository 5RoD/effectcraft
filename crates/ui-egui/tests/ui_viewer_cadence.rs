//! The control-channel observer samples visible final UI passes without inventing playback.

use effectcraft_engine::Session;
use effectcraft_ui_egui::{
    EffectcraftApp,
    control::{self, ControlRequest, Outcome},
};
use egui_kittest::Harness;
use serde_json::{Value, json};

fn observe(h: &mut Harness<'_, EffectcraftApp>, params: Value) -> Value {
    let ctx = h.ctx.clone();
    let (req, _reply) = ControlRequest::new("ui.viewer.observe", params);
    let Outcome::Done(reply) = control::handle(h.state_mut(), &ctx, &req) else { panic!("observation replies immediately") };
    reply
}

#[test]
fn observation_control_preserves_samples_on_invalid_requests_and_ignores_hidden_viewers() {
    let mut session = Session::default();
    session.execute("comp.new", json!({"name": "Cadence", "width": 64, "height": 36, "duration": 1, "frameRate": 120})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(|_| EffectcraftApp::new(session));
    h.run_steps(3);
    assert_eq!(observe(&mut h, json!({}))["result"]["samples"], 0);
    assert_eq!(observe(&mut h, json!({"action": "start", "maxSamples": 64, "seconds": 10}))["ok"], true);
    h.run_steps(3);
    let before = observe(&mut h, json!({"action": "read", "count": 64}));
    let samples = before["result"]["samples"].as_u64().unwrap();
    assert!(samples >= 3, "a visible composition produces samples: {before}");
    for params in [json!({"action": "start", "count": 2049}), json!({"action": "start", "maxSamples": 120001}), json!({"action": "start", "seconds": "ten"})] {
        assert_eq!(observe(&mut h, params)["ok"], false);
        assert_eq!(observe(&mut h, json!({}))["result"]["samples"], samples);
    }
    h.input_mut().viewports.get_mut(&egui::ViewportId::ROOT).unwrap().minimized = Some(true);
    h.run_steps(3);
    let hidden = observe(&mut h, json!({"action": "stop", "count": 64}));
    assert_eq!(hidden["result"]["samples"], samples, "a minimized viewer produces no presentation samples");
    assert_eq!(hidden["result"]["recording"], false);
    assert_eq!(observe(&mut h, json!({"action": "reset"}))["result"]["samples"], 0);
}

//! Effect Controls operations: copy/paste, duplicate, reorder, remove (Delete), reset, and the
//! Edit menu routing to them when effects are selected, all with undo.

use effectcraft_keyframe::Value as KV;
use effectcraft_project::{Layer, LayerId};
use serde_json::json;

use crate::Session;

fn comp_with_two_solids() -> (Session, u64, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 640, "height": 360, "frameRate": 30, "duration": 10})).unwrap();
    let a = s.execute("layer.newSolid", json!({"color": "#ff0000", "width": 200, "height": 100})).unwrap()["layer"].as_u64().unwrap();
    let b = s.execute("layer.newSolid", json!({"color": "#00ff00", "width": 400, "height": 300})).unwrap()["layer"].as_u64().unwrap();
    (s, a, b)
}

fn layer(s: &Session, id: u64) -> Layer {
    s.active_comp().unwrap().layer(LayerId(id)).unwrap().clone()
}

fn fx_names(s: &Session, id: u64) -> Vec<String> {
    layer(s, id).effects().map(|f| f.groups().map(|g| g.name.clone()).collect()).unwrap_or_default()
}

fn apply(s: &mut Session, lid: u64, effect: &str) -> u64 {
    s.execute("effect.apply", json!({"layers": [lid], "effect": effect})).unwrap()["effects"][0].as_u64().unwrap()
}

fn select_effect(s: &mut Session, lid: u64, uid: u64) {
    s.state.selected_layers = vec![LayerId(lid)];
    s.state.selected_props = vec![(LayerId(lid), uid)];
}

#[test]
fn copy_paste_effects_between_layers_with_undo() {
    let (mut s, a, b) = comp_with_two_solids();
    let blur = apply(&mut s, a, "Gaussian Blur");
    let gid = layer(&s, a).effects().unwrap().groups().next().unwrap().get("blurriness").unwrap().uid;
    s.execute("prop.set", json!({"layer": a, "prop": gid, "value": 12.5})).unwrap();
    select_effect(&mut s, a, blur);
    // Edit ▸ Copy with an effect selected copies the effect, not the layer.
    s.execute("edit.copy", json!({})).unwrap();
    assert_eq!(s.state.effect_clipboard.len(), 1);
    assert!(s.state.clipboard.is_empty());
    // Paste onto the other layer.
    s.state.selected_layers = vec![LayerId(b)];
    s.state.selected_props.clear();
    let n_layers = s.active_comp().unwrap().layers.len();
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(s.active_comp().unwrap().layers.len(), n_layers, "no layer pasted");
    assert_eq!(fx_names(&s, b), vec!["Gaussian Blur"]);
    let pasted = layer(&s, b).effects().unwrap().groups().next().unwrap().clone();
    assert_ne!(pasted.uid, blur, "fresh uids");
    assert_eq!(pasted.get("blurriness").unwrap().value, KV::Scalar(12.5));
    // Pasting again names the second instance uniquely.
    s.execute("effect.paste", json!({"layers": [b]})).unwrap();
    assert_eq!(fx_names(&s, b), vec!["Gaussian Blur", "Gaussian Blur 2"]);
    s.undo();
    s.undo();
    assert!(fx_names(&s, b).is_empty());
    assert_eq!(fx_names(&s, a), vec!["Gaussian Blur"]);
}

#[test]
fn duplicate_reorder_and_delete_selected_effect() {
    let (mut s, a, _) = comp_with_two_solids();
    let blur = apply(&mut s, a, "Gaussian Blur");
    apply(&mut s, a, "Invert");
    select_effect(&mut s, a, blur);
    // Edit ▸ Duplicate (Cmd+D) duplicates the selected effect right after it and selects the copy.
    s.execute("edit.duplicate", json!({})).unwrap();
    assert_eq!(fx_names(&s, a), vec!["Gaussian Blur", "Gaussian Blur 2", "Invert"]);
    let dup = s.state.selected_props[0].1;
    assert_ne!(dup, blur);
    // Reorder: move Invert to the top.
    s.execute("effect.reorder", json!({"layer": a, "effect": "Invert", "index": 1})).unwrap();
    assert_eq!(fx_names(&s, a), vec!["Invert", "Gaussian Blur", "Gaussian Blur 2"]);
    // Delete with the duplicate selected removes just that effect (not the layer).
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(fx_names(&s, a), vec!["Invert", "Gaussian Blur"]);
    assert!(s.active_comp().unwrap().layer(LayerId(a)).is_some());
    s.undo();
    assert_eq!(fx_names(&s, a), vec!["Invert", "Gaussian Blur", "Gaussian Blur 2"]);
    s.undo();
    assert_eq!(fx_names(&s, a), vec!["Gaussian Blur", "Gaussian Blur 2", "Invert"]);
    s.undo();
    assert_eq!(fx_names(&s, a), vec!["Gaussian Blur", "Invert"]);
}

#[test]
fn reset_effect_restores_defaults_in_one_undo_step() {
    let (mut s, _, b) = comp_with_two_solids();
    let ramp = apply(&mut s, b, "Gradient Ramp");
    let g = layer(&s, b).effects().unwrap().groups().next().unwrap().clone();
    let start = g.get("start").unwrap().uid;
    let shape = g.get("shape").unwrap().uid;
    // Point defaults are fractions of the layer (400 × 300).
    assert_eq!(g.get("start").unwrap().value, KV::Vec2([200.0, 0.0]));
    s.execute("prop.set", json!({"layer": b, "prop": start, "value": [10.0, 20.0]})).unwrap();
    s.execute("prop.set", json!({"layer": b, "prop": shape, "value": 1})).unwrap();
    let undo_before = s.history.undo.len();
    s.execute("effect.reset", json!({"layer": b, "effect": ramp})).unwrap();
    assert_eq!(s.history.undo.len(), undo_before + 1);
    let g = layer(&s, b).effects().unwrap().groups().next().unwrap().clone();
    assert_eq!(g.get("start").unwrap().value, KV::Vec2([200.0, 0.0]));
    assert_eq!(g.get("shape").unwrap().value, KV::Enum(0));
    s.undo();
    let g = layer(&s, b).effects().unwrap().groups().next().unwrap().clone();
    assert_eq!(g.get("start").unwrap().value, KV::Vec2([10.0, 20.0]));
}

#[test]
fn reset_restores_parameters_inside_twirl_down_groups() {
    let (mut s, a, _) = comp_with_two_solids();
    // Fractal and Scribble keep most parameters in groups (`mandelbrot/…`, `edgeOptions/…`).
    for (effect, changes) in [
        ("Fractal", [("setChoice", json!(1)), ("mandelbrot/mandelbrotX", json!(1.25)), ("fractalColor/hue", json!(90.0))]),
        ("Scribble", [("scribble", json!(2)), ("edgeOptions/edgeWidth", json!(33.0)), ("strokeOptions/spacing", json!(9.0))]),
    ] {
        let fx = apply(&mut s, a, effect);
        let group = |s: &Session| layer(s, a).effects().unwrap().groups().find(|g| g.uid == fx).unwrap().clone();
        let defaults: Vec<_> = changes.iter().map(|(id, _)| group(&s).prop(id).unwrap().value.clone()).collect();
        for (id, v) in &changes {
            let uid = group(&s).prop(id).unwrap().uid;
            s.execute("prop.set", json!({"layer": a, "prop": uid, "value": v})).unwrap();
            assert_ne!(group(&s).prop(id).unwrap().value, defaults[changes.iter().position(|(i, _)| i == id).unwrap()], "{effect} {id} changed");
        }
        s.execute("effect.reset", json!({"layer": a, "effect": fx})).unwrap();
        for ((id, _), d) in changes.iter().zip(&defaults) {
            assert_eq!(&group(&s).prop(id).unwrap().value, d, "{effect} {id} back to its default");
        }
    }
}

#[test]
fn reset_keys_animated_params_at_the_cti() {
    let (mut s, a, _) = comp_with_two_solids();
    let blur = apply(&mut s, a, "Gaussian Blur");
    let uid = layer(&s, a).effects().unwrap().groups().next().unwrap().get("blurriness").unwrap().uid;
    s.execute("prop.toggleAnimation", json!({"layer": a, "prop": uid})).unwrap();
    s.execute("prop.set", json!({"layer": a, "prop": uid, "value": 30.0})).unwrap();
    s.execute("time.set", json!({"time": 2.0})).unwrap();
    s.execute("effect.reset", json!({"layer": a, "effect": blur})).unwrap();
    let pr = layer(&s, a).effects().unwrap().groups().next().unwrap().get("blurriness").unwrap().clone();
    assert_eq!(pr.keys.len(), 2, "a key at the CTI with the default value");
    assert_eq!(pr.keys[1].value, KV::Scalar(0.0));
}

#[test]
fn copy_layers_clears_effect_clipboard() {
    let (mut s, a, b) = comp_with_two_solids();
    let blur = apply(&mut s, a, "Gaussian Blur");
    select_effect(&mut s, a, blur);
    s.execute("effect.copy", json!({})).unwrap();
    s.state.selected_props.clear();
    s.state.selected_layers = vec![LayerId(b)];
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.state.effect_clipboard.is_empty());
    let n = s.active_comp().unwrap().layers.len();
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(s.active_comp().unwrap().layers.len(), n + 1, "layer pasted");
}

#[test]
fn layer_params_get_a_source_companion() {
    let (mut s, a, _) = comp_with_two_solids();
    apply(&mut s, a, "ec.channel.blend");
    let g = layer(&s, a).effects().unwrap().groups().next().unwrap().clone();
    let layer_params: Vec<_> = g.props().filter(|p| matches!(p.ui, effectcraft_project::ParamUi::Layer)).map(|p| p.match_id.clone()).collect();
    assert!(!layer_params.is_empty());
    for id in layer_params {
        let src = g.get(&effectcraft_effects::layer_source_id(&id)).expect("companion");
        assert_eq!(src.value, KV::Enum(2), "Effects & Masks by default");
        assert!(matches!(src.ui, effectcraft_project::ParamUi::Hidden));
    }
}

/// Projects saved before parameters were renamed, regrouped, added or had their popups
/// reordered open with their instances brought up to date (`effectcraft_effects::migrate`).
#[test]
fn opening_an_old_project_upgrades_effect_instances() {
    use effectcraft_effects::migrate::PARAM_ID_ALIASES;
    use effectcraft_project::{Node, ParamUi, PropGroup};
    let (mut s, a, _) = comp_with_two_solids();
    apply(&mut s, a, "ec.blur.gaussian");
    // One instance of every effect with renamed parameter ids.
    let mut aliased: Vec<&str> = PARAM_ID_ALIASES.iter().map(|(e, _, _)| *e).collect();
    aliased.dedup();
    for e in &aliased {
        apply(&mut s, a, e);
    }
    // Rewrite the saved instances the way an older version wrote them.
    fn take(g: &mut PropGroup, path: &str) -> Option<effectcraft_project::Property> {
        match path.split_once('/') {
            None => {
                let i = g.children.iter().position(|c| matches!(c, Node::Prop(p) if p.match_id == path))?;
                match g.children.remove(i) {
                    Node::Prop(p) => Some(p),
                    Node::Group(_) => None,
                }
            }
            Some((first, rest)) => take(g.sub_mut(first)?, rest),
        }
    }
    let mut old = (*s.project).clone();
    let cid = s.active_comp_id().unwrap();
    {
        let comp = std::sync::Arc::make_mut(match &mut old.items.get_mut(&cid).unwrap().kind {
            effectcraft_project::ItemKind::Comp(c) => c,
            _ => panic!("not a composition"),
        });
        let l = comp.layers.iter_mut().find(|l| l.id == LayerId(a)).unwrap();
        for n in &mut l.props.sub_mut("effects").unwrap().children {
            let Node::Group(g) = n else { continue };
            if g.match_id == "ec.blur.gaussian" {
                // Repeat Edge Pixels did not exist yet, Blurriness had another name, the popup
                // listed its options in reverse.
                take(g, "repeatEdge").unwrap();
                g.get_mut("blurriness").unwrap().name = "Old Blurriness".into();
                let d = g.get_mut("dimensions").unwrap();
                let ParamUi::Popup { options } = &d.ui else { panic!("dimensions is a popup") };
                d.ui = ParamUi::Popup { options: options.iter().rev().cloned().collect() };
                d.value = KV::Enum(0);
            }
            for (e, old_id, new_id) in PARAM_ID_ALIASES {
                if g.match_id == *e {
                    let mut p = take(g, new_id).unwrap_or_else(|| panic!("{e}: no {new_id}"));
                    // Put it back at its old path (which may be inside a group).
                    let (path, leaf) = old_id.rsplit_once('/').map(|(p, l)| (Some(p), l)).unwrap_or((None, old_id));
                    p.match_id = leaf.to_string();
                    let mut at: &mut PropGroup = g;
                    for seg in path.into_iter().flat_map(|p| p.split('/')) {
                        if at.sub(seg).is_none() {
                            at.children.push(PropGroup::new(1_000_000 + at.children.len() as u64, seg, seg).into());
                        }
                        at = at.sub_mut(seg).unwrap();
                    }
                    at.children.push(p.into());
                }
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("ec-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.ecproj").to_string_lossy().to_string();
    std::fs::write(&path, old.to_json()).unwrap();
    let mut s2 = Session::default();
    s2.execute("file.open", json!({"path": path})).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    let l = s2.project.comp(cid).unwrap().layer(LayerId(a)).unwrap().clone();
    let fx = l.effects().unwrap();
    let spec = effectcraft_effects::find("ec.blur.gaussian").unwrap();
    let sp = |id: &str| spec.params.iter().find(|p| p.id == id).unwrap();
    let blur = fx.groups().find(|g| g.match_id == "ec.blur.gaussian").unwrap();
    assert_eq!(blur.get("blurriness").unwrap().name, sp("blurriness").name);
    assert!(blur.get("repeatEdge").is_some(), "missing parameter added");
    let dims = blur.get("dimensions").unwrap();
    let ParamUi::Popup { options } = &sp("dimensions").ui else { panic!() };
    assert_eq!(dims.value, KV::Enum(options.len() as u32 - 1), "popup value remapped by label");
    assert_eq!(dims.ui, sp("dimensions").ui);
    for (e, old_id, new_id) in PARAM_ID_ALIASES {
        let g = fx.groups().find(|g| g.match_id == *e).unwrap();
        let at = |id: &str| {
            let (path, leaf) = id.rsplit_once('/').map(|(p, l)| (Some(p), l)).unwrap_or((None, id));
            let mut node = Some(g);
            for seg in path.into_iter().flat_map(|p| p.split('/')) {
                node = node.and_then(|n| n.sub(seg));
            }
            node.and_then(|n| n.get(leaf)).is_some()
        };
        assert!(!at(old_id), "{e}: {old_id} still present");
        let (path, leaf) = new_id.rsplit_once('/').map(|(p, l)| (Some(p), l)).unwrap_or((None, new_id));
        let mut node = Some(g);
        for seg in path.into_iter().flat_map(|p| p.split('/')) {
            node = node.and_then(|n| n.sub(seg));
        }
        assert!(node.and_then(|n| n.get(leaf)).is_some(), "{e}: {old_id} not moved to {new_id}");
    }
    // The upgraded project renders.
    let _ = s2.render(cid, s2.time(), Default::default());
}

#[test]
fn checked_bloom_session_frame_apis_and_remote_worker_reject_failed_pixels() {
    use crate::remote::{FrameMsg, FrameReply, FrameServer, Mirror};
    use crate::render::RenderOpts;
    use crate::time::Tick;
    use std::sync::Arc;
    let (mut session, layer_id, _) = comp_with_two_solids();
    let comp = session.active_comp_id().unwrap();
    apply(&mut session, layer_id, "Native Bloom");
    Arc::make_mut(&mut session.project).comp_mut(comp).unwrap().layer_mut(LayerId(layer_id)).unwrap().props.prop_mut("effects/#1/radius").unwrap().value =
        KV::Scalar(f64::MAX);
    for transparent in [false, true] {
        let error = session.render_rgba8_alpha(comp, Tick::ZERO, 32, transparent).unwrap_err();
        assert!(error.to_string().contains("ec.stylize.bloom"));
    }
    assert!(crate::commands::frame_export::layered_psd(&session, comp, Tick::ZERO, 0.05).is_err());
    assert!(crate::commands::frame_export::layered_exr(&session, comp, Tick::ZERO, 0.05).is_err());
    assert!(session.execute("view.takeSnapshot", json!({"scale": 0.05})).is_err());
    assert!(session.snapshot.is_none());
    let mut worker = FrameServer::new(session.footage.clone(), session.expr.clone());
    let mut mirror = Mirror::default();
    assert!(worker.handle(mirror.sync(session.revision, &session.project).unwrap()).is_empty());
    let replies = worker.handle(FrameMsg::Render {
        id: 17,
        revision: session.revision,
        comp,
        time: Tick::ZERO,
        opts: Box::new(RenderOpts::default()),
        disk: None,
        layers: false,
        prefetch: vec![],
    });
    assert_eq!(replies.len(), 1);
    let (FrameReply::Failed { id, error, stale }, pixels) = &replies[0] else { panic!("expected checked render failure") };
    assert_eq!(*id, 17);
    assert!(!stale);
    assert!(error.contains("ec.stylize.bloom"));
    assert!(pixels.is_none());
    assert_eq!(worker.cache.stats().entries, 0);
}

#[test]
fn checked_bloom_remote_cpu_retry_finishes_without_obsolete_deferred_passes() {
    use crate::remote::{FrameMsg, FrameReply, FrameServer, Mirror};
    use crate::render::{Accelerator, Backend, FrameFailure, FxStep, Image, RenderOpts, Renderer};
    use crate::time::Tick;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct Declines {
        missed: Arc<AtomicBool>,
        calls: AtomicUsize,
    }
    impl Accelerator for Declines {
        fn name(&self) -> String {
            "synthetic checked retry".into()
        }
        fn comp_frame(&self, renderer: &Renderer, comp: crate::project::ItemId, time: Tick) -> Option<Image> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.missed.store(true, Ordering::Release);
            renderer.diagnostics.unwrap().record(FrameFailure {
                comp,
                layer: LayerId(1),
                effect_uid: 1,
                effect_id: "ec.stylize.bloom".into(),
                time,
                message: "synthetic GPU transfer refusal".into(),
            });
            Some(Image::new(1, 1))
        }
        fn supports_effect(&self, _: &str) -> bool {
            false
        }
        fn effects(&self, _: &[FxStep], _: &crate::effects::Buf, _: Option<f32>) -> Option<crate::effects::Buf> {
            None
        }
        fn pass_begin(&self) {
            self.missed.store(false, Ordering::Release);
        }
        fn pass_missed(&self) -> bool {
            self.missed.load(Ordering::Acquire)
        }
        fn miss_gate(&self) -> Option<Arc<AtomicBool>> {
            Some(self.missed.clone())
        }
    }
    let mut session = Session::default();
    let comp = crate::project::ItemId(session.execute("comp.new", json!({"width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
    let layer = session.execute("layer.newSolid", json!({"color": "#ffffff"})).unwrap()["layer"].as_u64().unwrap();
    apply(&mut session, layer, "Native Bloom");
    let expected = crate::remote::rgba8_premultiplied(&session.render_checked(comp, Tick::ZERO, RenderOpts::default()).unwrap());
    let declines = Arc::new(Declines { missed: Arc::default(), calls: AtomicUsize::new(0) });
    let mut worker = FrameServer::new(session.footage.clone(), session.expr.clone());
    worker.set_accel(Some(declines.clone()));
    let mut mirror = Mirror::default();
    worker.handle(mirror.sync(session.revision, &session.project).unwrap());
    let replies = worker.handle(FrameMsg::Render {
        id: 18,
        revision: session.revision,
        comp,
        time: Tick::ZERO,
        opts: Box::new(RenderOpts { backend: Backend::Gpu, ..Default::default() }),
        disk: None,
        layers: false,
        prefetch: vec![],
    });
    let (FrameReply::Frame { passes, gpu, .. }, pixels) = &replies[0] else { panic!("CPU retry must complete immediately") };
    assert_eq!(*passes, 1);
    assert!(!gpu);
    assert_eq!(pixels.as_ref().unwrap(), &expected);
    assert_eq!(declines.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn opening_a_schema_1_project_moves_shape_layer_effect_points_into_effect_space() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 200, "height": 100, "frameRate": 30, "duration": 2})).unwrap();
    let shape = s.execute("layer.newShape", json!({"kind": "rect", "size": [180, 80]})).unwrap()["layer"].as_u64().unwrap();
    let solid = s.execute("layer.newSolid", json!({"color": "#0000ff", "width": 200, "height": 100})).unwrap()["layer"].as_u64().unwrap();
    let point = |s: &Session, lid: u64, fx: u64, id: &str| -> KV {
        let l = layer(s, lid);
        let g = l.effects().unwrap().groups().find(|g| g.uid == fx).unwrap();
        g.props().find(|p| p.match_id == id || (id.is_empty() && p.ui == effectcraft_project::ParamUi::Point)).unwrap().value.clone()
    };
    let set = |s: &mut Session, lid: u64, fx: u64, id: &str, v: [f64; 2]| {
        let l = layer(s, lid);
        let g = l.effects().unwrap().groups().find(|g| g.uid == fx).unwrap();
        let uid = g.props().find(|p| p.match_id == id || (id.is_empty() && p.ui == effectcraft_project::ParamUi::Point)).unwrap().uid;
        s.execute("prop.set", json!({"layer": lid, "prop": uid, "value": v})).unwrap();
    };
    let twirl = apply(&mut s, shape, "Twirl");
    set(&mut s, shape, twirl, "center", [-30.0, -10.0]);
    let bulge = apply(&mut s, shape, "Bulge");
    let control = apply(&mut s, shape, "Point Control");
    set(&mut s, shape, control, "", [5.0, 5.0]);
    let on_solid = apply(&mut s, solid, "Twirl");
    set(&mut s, solid, on_solid, "center", [30.0, 20.0]);
    let adjust = s.execute("layer.newShape", json!({"kind": "rect", "size": [50, 50]})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.setSwitch", json!({"layers": [adjust], "switch": "adjustment", "value": true})).unwrap();
    let on_adjust = apply(&mut s, adjust, "Twirl");
    set(&mut s, adjust, on_adjust, "center", [40.0, 30.0]);
    assert_eq!(point(&s, shape, bulge, "center"), KV::Vec2([100.0, 50.0]), "Bulge's default centre");

    let old = s.project.to_file_json().unwrap().replacen("\"schema\": 2", "\"schema\": 1", 1);
    assert!(old.contains("\"schema\": 1"));
    let dir = std::env::temp_dir().join(format!("ec-effect-space-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.ecproj").to_string_lossy().to_string();
    std::fs::write(&path, old).unwrap();
    let mut s2 = Session::default();
    s2.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(s2.project.schema, effectcraft_project::SCHEMA_VERSION);
    let check = |s2: &Session| {
        assert_eq!(point(s2, shape, twirl, "center"), KV::Vec2([70.0, 40.0]), "a set point keeps its place in the comp");
        assert_eq!(point(s2, shape, bulge, "center"), KV::Vec2([100.0, 50.0]), "a default point becomes the layer centre");
        assert_eq!(point(s2, shape, control, ""), KV::Vec2([5.0, 5.0]), "expression controls keep their values");
        assert_eq!(point(s2, solid, on_solid, "center"), KV::Vec2([30.0, 20.0]), "a solid's effect space is its layer space");
        assert_eq!(point(s2, adjust, on_adjust, "center"), KV::Vec2([40.0, 30.0]), "an adjustment layer's effects see the comp");
    };
    check(&s2);
    // Saved as schema 2 and opened again: nothing moves twice.
    s2.execute("file.save", json!({"path": path})).unwrap();
    let mut s3 = Session::default();
    s3.execute("file.open", json!({"path": path})).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    check(&s3);
}

#[test]
fn ocio_effect_warns_about_unsupported_config_transforms() {
    let (mut s, a, _) = comp_with_two_solids();
    let fx = apply(&mut s, a, "OCIO Color Space Transform");
    let warning = |s: &mut Session| s.execute("effect.warning", json!({"layer": a, "effect": fx})).unwrap()["warning"].clone();
    assert!(warning(&mut s).is_null(), "built-in config");
    let set = |s: &mut Session, param: &str, value: serde_json::Value| {
        let uid = layer(s, a).effects().unwrap().groups().next().unwrap().get(param).unwrap().uid;
        s.execute("prop.set", json!({"layer": a, "prop": uid, "value": value})).unwrap();
    };
    let config = "ocio_profile_version: 2\ncolorspaces:\n  - !<ColorSpace>\n    name: lin\n  - !<ColorSpace>\n    name: graded\n    from_scene_reference: !<GradingToneTransform> {}\n";
    set(&mut s, "config", json!(1));
    set(&mut s, "configFile", json!(config));
    set(&mut s, "sourceName", json!("lin"));
    set(&mut s, "destinationName", json!("graded"));
    let w = warning(&mut s);
    assert!(w.as_str().is_some_and(|w| w.contains("GradingToneTransform")), "{w}");
    set(&mut s, "destinationName", json!("lin"));
    assert!(warning(&mut s).is_null());
    set(&mut s, "destinationName", json!("nowhere"));
    assert!(warning(&mut s).as_str().is_some_and(|w| w.contains("no color space `nowhere`")));
    set(&mut s, "configFile", json!("/no/such/config.ocio"));
    assert!(warning(&mut s).as_str().is_some_and(|w| w.starts_with("Can't read the OCIO config")));
}

#[test]
fn effect_upgrade_cache_keeps_shared_states_of_different_schemas_distinct() {
    let mut session = Session::default();
    let comp = crate::project::ItemId(session.execute("comp.new", json!({"width": 200, "height": 100})).unwrap()["comp"].as_u64().unwrap());
    let shape = session.execute("layer.newShape", json!({"kind": "rect", "size": [180, 80]})).unwrap()["layer"].as_u64().unwrap();
    let effect = apply(&mut session, shape, "Twirl");
    let prop = layer(&session, shape).effects().unwrap().groups().find(|g| g.uid == effect).unwrap().get("center").unwrap().uid;
    session.execute("prop.set", json!({"layer": shape, "prop": prop, "value": [-30.0, -10.0]})).unwrap();
    let mut old = (*session.project).clone();
    old.schema = 1;
    let mut current = old.clone();
    current.schema = crate::project::SCHEMA_EFFECT_SPACE;
    let shared = current.comp_arc(comp).unwrap();
    assert!(std::sync::Arc::ptr_eq(&shared, &old.comp_arc(comp).unwrap()));
    let point = |project: &crate::project::Project| {
        project
            .comp(comp)
            .unwrap()
            .layer(LayerId(shape))
            .unwrap()
            .effects()
            .unwrap()
            .groups()
            .find(|g| g.uid == effect)
            .unwrap()
            .get("center")
            .unwrap()
            .value
            .clone()
    };
    let mut cache = Default::default();
    crate::upgrade_effects_cached(&mut old, &mut cache);
    assert_eq!(point(&old), KV::Vec2([70.0, 40.0]));
    crate::upgrade_effects_cached(&mut current, &mut cache);
    assert_eq!(point(&current), KV::Vec2([-30.0, -10.0]), "schema-2 states must not reuse schema-1 point migration");
    assert!(std::sync::Arc::ptr_eq(&shared, &current.comp_arc(comp).unwrap()), "an unchanged current-schema composition remains shared");
    assert_eq!(old.schema, crate::project::SCHEMA_VERSION);
}

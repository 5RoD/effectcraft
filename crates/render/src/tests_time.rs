//! Time effects through the renderer: neighbouring frames via `EffectHost::self_at`, and the
//! layer cache staying correct while scrubbing.

use effectcraft_color::Label;
use effectcraft_keyframe::{Keyframe, ShapePath, Value};
use effectcraft_project::build::{self, Ids};
use effectcraft_project::{Comp, ItemId, ItemKind, LayerSource, MaskMode, Project, Solid};
use effectcraft_time::{FrameRate, Tick};

/// The native effect host must retain exact temporal identity, even when seconds cannot
/// distinguish a boundary from one Tick before it. No codecs or large fixtures are needed.
#[test]
fn exact_effect_host_preserves_boundaries_neighbour_ticks_and_float_ulp_inputs() {
    use effectcraft_effects::EffectHost;
    use effectcraft_project::{BitDepth, Footage};
    use std::sync::{Arc, Mutex, atomic::AtomicUsize};
    struct Source(Mutex<Vec<Tick>>);
    impl crate::FootageSource for Source {
        fn frame(&self, _: ItemId, _: &Footage, time: Tick) -> Option<Arc<crate::Image>> {
            self.0.lock().unwrap().push(time);
            Some(Arc::new(crate::Image::filled(1, 1, [0.2, 0.3, 0.4, 1.0])))
        }
    }
    for rate in FrameRate::COMMON {
        let mut p = Project::default();
        p.settings.bit_depth = BitDepth::Bpc32;
        let comp = Comp::new(1, 1, rate, Tick::MAX);
        let cid = p.add_item("Exact host", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
        let fid = p.add_item(
            "Generated frame",
            Label::Aqua,
            None,
            ItemKind::Footage(Footage { width: 1, height: 1, duration: Tick::MAX, has_video: true, frame_rate: rate, ..Default::default() }),
        );
        for name in ["self", "auxiliary"] {
            let mut layer = build::layer(&mut p, &comp, name, LayerSource::Footage { item: fid }, (1, 1), None);
            layer.start_time = rate.tick_of(-37);
            p.comp_mut(cid).unwrap().layers.push(layer);
        }
        let source = Source(Mutex::new(Vec::new()));
        let renderer = crate::Renderer::new(&p, &source, crate::RenderOpts::default());
        let comp = p.comp(cid).unwrap();
        for frame in [-31_000_003, -37, -1, 0, 1, 37, 31_000_003] {
            let boundary = rate.tick_of(frame);
            let mut times: Vec<_> = [-7, -1, 0, 1, 7].into_iter().map(|delta| Tick(boundary.0 + delta)).collect();
            let mut seconds = boundary.seconds();
            for _ in 0..4 {
                seconds = seconds.next_down();
            }
            for _ in 0..9 {
                times.push(Tick::from_seconds_f64(seconds));
                seconds = seconds.next_up();
            }
            for time in times {
                let ctx = crate::EvalCtx { project: &p, comp_id: cid, comp, time: comp.layers[0].comp_time(time), expr: None, footage: None };
                let host = crate::FxHost { r: &renderer, ctx: &ctx, layer: &comp.layers[0], origin: [0.0; 2], index: AtomicUsize::new(0), original: None };
                assert_eq!(host.time_ticks(), Some((time, ctx.time)));
                source.0.lock().unwrap().clear();
                assert!(host.self_at_tick(time, 0).is_some());
                assert!(host.layer_at_tick(comp.layers[1].id.0, comp.layers[1].comp_time(time), false).is_some());
                assert_eq!(*source.0.lock().unwrap(), vec![time, time], "rate={rate:?}, frame={frame}, time={time:?}");
                assert_eq!(rate.frame_at(source.0.lock().unwrap()[0]), rate.frame_at(time));
            }
        }
    }
}

fn render(p: &Project, cid: ItemId, t: f64, cache: Option<&crate::LayerCache>) -> crate::Image {
    let mut r = crate::Renderer::new(p, &crate::NoFootage, crate::RenderOpts::default());
    r.cache = cache;
    r.comp_frame(cid, Tick::from_seconds_f64(t))
}

/// A white 200×100 solid whose full-frame mask fades in (opacity 0 → 100 over 0..1 s), with
/// effect `fx` (and parameter overrides) applied.
fn scene(fx: Option<(&str, &[(&str, Value)])>) -> (Project, ItemId) {
    let mut p = Project::default();
    let comp = Comp::new(200, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
    let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color: [1.0, 1.0, 1.0], width: 200, height: 100, pixel_aspect: 1.0 }));
    let mut l = build::layer(&mut p, &comp, "Solid", LayerSource::Solid { item: sid }, (200, 100), None);
    let mut next = p.next_id;
    let mut m = build::mask(&mut Ids(&mut next), "Mask 1", ShapePath::rect([100.0, 50.0], 400.0, 200.0), MaskMode::Add, [255, 255, 0]);
    if let Some(o) = m.prop_mut("opacity") {
        o.keys = vec![Keyframe::new(Tick::ZERO, Value::Scalar(0.0)), Keyframe::new(Tick::from_seconds_f64(1.0), Value::Scalar(100.0))];
    }
    l.props.sub_mut("masks").unwrap().children.push(m.into());
    if let Some((id, vals)) = fx {
        let spec = effectcraft_effects::find(id).unwrap();
        let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [200.0, 100.0]);
        for (k, v) in vals {
            g.prop_mut(k).unwrap().value = v.clone();
        }
        l.props.sub_mut("effects").unwrap().children.push(g.into());
    }
    p.next_id = next;
    p.comp_mut(cid).unwrap().layers.push(l);
    (p, cid)
}

fn alpha(img: &crate::Image) -> f32 {
    img.get(100, 50)[3]
}

#[test]
fn posterize_time_holds_frames() {
    let (p, cid) = scene(Some(("ec.time.posterizetime", &[("frameRate", Value::Scalar(4.0))])));
    for (t, want) in [(0.3, 0.25), (0.45, 0.25), (0.5, 0.5), (0.74, 0.5), (0.8, 0.75)] {
        let a = alpha(&render(&p, cid, t, None));
        assert!((a - want).abs() < 0.01, "t={t}: {a}");
    }
    let (plain, cid) = scene(None);
    assert!((alpha(&render(&plain, cid, 0.3, None)) - 0.3).abs() < 0.01);
}

#[test]
fn echo_blends_neighbouring_frames() {
    let vals = [("echoTime", Value::Scalar(-0.1)), ("numberOfEchoes", Value::Scalar(2.0)), ("decay", Value::Scalar(0.5))];
    let (p, cid) = scene(Some(("ec.time.echo", &vals)));
    let a = alpha(&render(&p, cid, 0.3, None));
    let want = 0.3 + 0.5 * 0.2 + 0.25 * 0.1;
    assert!((a - want).abs() < 0.01, "{a} vs {want}");
}

#[test]
fn time_difference_of_static_layer_is_black() {
    let mut p = Project::default();
    let comp = Comp::new(64, 32, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
    let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color: [0.8, 0.4, 0.2], width: 64, height: 32, pixel_aspect: 1.0 }));
    let mut l = build::layer(&mut p, &comp, "Solid", LayerSource::Solid { item: sid }, (64, 32), None);
    let spec = effectcraft_effects::find("ec.time.timedifference").unwrap();
    let mut next = p.next_id;
    let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [64.0, 32.0]);
    g.prop_mut("timeOffset").unwrap().value = Value::Scalar(-0.5);
    p.next_id = next;
    l.props.sub_mut("effects").unwrap().children.push(g.into());
    p.comp_mut(cid).unwrap().layers.push(l);
    let img = render(&p, cid, 1.0, None);
    assert!(img.data.iter().all(|px| px[0] == 0.0 && px[1] == 0.0 && px[2] == 0.0 && px[3] == 1.0), "{:?}", img.get(10, 10));
}

/// Scrubbing back and forth with the layer cache gives exactly the uncached frames (Echo's
/// output depends on neighbouring frames, so its key must include the layer time), and an edit
/// to the animation is picked up.
#[test]
fn cache_correct_while_scrubbing() {
    let vals = [("echoTime", Value::Scalar(-0.2)), ("numberOfEchoes", Value::Scalar(3.0)), ("decay", Value::Scalar(0.6)), ("echoOperator", Value::Enum(6))];
    let (mut p, cid) = scene(Some(("ec.time.echo", &vals)));
    let cache = crate::LayerCache::default();
    for t in [0.3, 0.5, 0.3, 1.2, 0.8, 0.5, 0.3, 1.5] {
        let a = render(&p, cid, t, Some(&cache));
        let b = render(&p, cid, t, None);
        assert_eq!(a.data, b.data, "t={t}");
    }
    let st = cache.stats();
    assert!(st.hits > 0, "{st:?}");
    // Re-key the fade (0 → 100 over 0..0.5 s): cached frames must not be served.
    p.comp_mut(cid).unwrap().layers[0].props.prop_mut("masks/#1/opacity").unwrap().keys[1].time = Tick::from_seconds_f64(0.5);
    for t in [0.3, 0.5, 0.8] {
        assert_eq!(render(&p, cid, t, Some(&cache)).data, render(&p, cid, t, None).data, "edited t={t}");
    }
}

/// A layer's input up to an effect keys only the effects before it: editing a later effect (a
/// Puppet pin being dragged, #212) keeps the input cached; editing an earlier one doesn't.
#[test]
fn input_keys_ignore_later_effects() {
    let (mut p, cid) = scene(Some(("ec.blur.gaussian", &[("blurriness", Value::Scalar(4.0))])));
    {
        let spec = effectcraft_effects::find("ec.stylize.mosaic").unwrap();
        let mut next = p.next_id;
        let g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [200.0, 100.0]);
        p.next_id = next;
        p.comp_mut(cid).unwrap().layers[0].props.sub_mut("effects").unwrap().children.push(g.into());
    }
    let key = |p: &Project| {
        let comp = p.comp(cid).unwrap();
        let ctx = crate::EvalCtx { project: p, comp_id: cid, comp, time: Tick::from_seconds_f64(0.5), expr: None, footage: None };
        let l = &comp.layers[0];
        (crate::cache::input_key(&ctx, l, 1.0, false, false, 1), crate::cache::input_key(&ctx, l, 1.0, false, false, 2))
    };
    let (one, two) = key(&p);
    assert!(one.is_some() && one != two);
    let set = |p: &mut Project, path: &str, v: f64| p.comp_mut(cid).unwrap().layers[0].props.prop_mut(path).unwrap().value = Value::Scalar(v);
    set(&mut p, "effects/#2/horizontal", 30.0);
    assert_eq!(key(&p).0, one, "the second effect doesn't change the input to it");
    assert_ne!(key(&p).1, two);
    set(&mut p, "effects/#1/blurriness", 8.0);
    assert_ne!(key(&p).0, one);
}

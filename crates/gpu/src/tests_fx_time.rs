//! GPU time effects (Time Difference, Time Displacement, CC Force Motion Blur, CC Wide Time,
//! Pixel Motion Blur, Timewarp) vs the CPU effects (the oracle). They fetch other frames through
//! the effect host: a precomp whose content moves, at a nonzero time, composited at 8 and
//! 32 bpc (the direct cases have no host and pass the layer through).

use effectcraft_color::Label;
use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_project::{BitDepth, Comp, ItemKind, LayerSource, build};
use effectcraft_render::RenderOpts;
use effectcraft_time::{FrameRate, Tick};

use crate::tests::{Scene, check, compare_at, effect_case, n, opts, set, v3};

fn e(v: u32) -> Value {
    Value::Enum(v)
}

fn on() -> Value {
    Value::Bool(true)
}

struct Clock;
impl effectcraft_effects::EffectHost for Clock {
    fn layer(&self, _: u64, _: bool) -> Option<effectcraft_effects::LayerPixels> {
        None
    }
    fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
        None
    }
    fn self_at(&self, time: f64, _: usize) -> Option<effectcraft_effects::Buf> {
        let mut image = effectcraft_raster::Image::new(31, 19);
        for (index, pixel) in image.data.iter_mut().enumerate() {
            let value = (0.5 + 0.4 * (index as f64 * 0.1 + time * 3.0).sin()) as f32;
            *pixel = [value * 0.4, value * 0.5, value * 0.2, 0.5];
        }
        Some(effectcraft_effects::Buf { img: image, offset: [1.0, 2.0], scale: 1.0 })
    }
}

#[test]
fn exact_timewarp_source_boundaries_reach_gpu_without_float_host_roundtrips() {
    use effectcraft_effects::{Buf, EffectHost, LayerPixels};
    use effectcraft_raster::Image;
    struct Identity {
        rate: FrameRate,
        time: Tick,
    }
    impl EffectHost for Identity {
        fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
            None
        }
        fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
            None
        }
        fn self_at(&self, _: f64, _: usize) -> Option<Buf> {
            panic!("native Timewarp must use exact Tick transport")
        }
        fn self_at_tick(&self, time: Tick, _: usize) -> Option<Buf> {
            let frame = self.rate.frame_at(time);
            Some(Buf { img: Image::filled(1, 1, [frame.rem_euclid(65_536) as f32, frame.div_euclid(65_536) as f32, 0.0, 1.0]), offset: [0.0; 2], scale: 1.0 })
        }
        fn time_ticks(&self) -> Option<(Tick, Tick)> {
            Some((self.time, self.time))
        }
    }
    let gpu = crate::tests::gpu();
    assert!(std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1") || gpu.is_some());
    let Some(gpu) = gpu else { return };
    let spec = effectcraft_effects::find(effectcraft_effects::timewarp::ID).unwrap();
    let input = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1.0 };
    for rate in [FrameRate::FPS_60, FrameRate::FPS_120, FrameRate::FPS_59_94, FrameRate::FPS_119_88] {
        for frame in [-31_000_003, -5, -1, 0, 1, 5, 31_000_003] {
            let time = rate.tick_of(frame);
            let host = Identity { rate, time };
            for method in 0..=2 {
                let mut parameters = timewarp_parameters(method, 1);
                parameters.values.insert("adjustTimeBy".into(), e(0));
                parameters.values.insert("speed".into(), n(100.0));
                parameters.values.insert("motionBlur/enableMotionBlur".into(), Value::Bool(false));
                let ctx = effectcraft_effects::EffectCtx {
                    params: &parameters,
                    time: time.seconds(),
                    layer_size: [1.0; 2],
                    seed: 1,
                    adjustment: false,
                    env: effectcraft_effects::EffectEnv { host: Some(&host), frame_rate: rate.as_f64(), comp_time: time.seconds(), ..Default::default() },
                };
                let expected = host.self_at_tick(time, 0).unwrap();
                let cpu = effectcraft_effects::apply(spec, &ctx, input.clone()).unwrap();
                let step = effectcraft_render::FxStep { spec, ctx };
                let output = effectcraft_render::Accelerator::effect_checked(gpu, &step, &input, None).unwrap().unwrap();
                assert_eq!(cpu.img.data, expected.img.data);
                assert_eq!(output.img.data, expected.img.data, "rate={rate:?}, frame={frame}, method={method}");
            }
        }
    }
}

fn timewarp_parameters(method: u32, samples: usize) -> effectcraft_effects::Params {
    let spec = effectcraft_effects::find(effectcraft_effects::timewarp::ID).unwrap();
    let mut parameters = effectcraft_effects::Params {
        values: spec.params.iter().map(|parameter| (parameter.id.into(), effectcraft_effects::default_value(parameter, [31.0, 19.0]))).collect(),
    };
    for (key, value) in [
        ("method", e(method)),
        ("adjustTimeBy", e(1)),
        ("sourceFrame", n(4.4)),
        ("motionBlur/enableMotionBlur", on()),
        ("motionBlur/shutterControl", e(1)),
        ("motionBlur/shutterAngle", n(0.0)),
        ("motionBlur/shutterSamples", n(samples as f64)),
    ] {
        parameters.values.insert(key.into(), value);
    }
    parameters
}

fn timewarp_context(parameters: &effectcraft_effects::Params) -> effectcraft_effects::EffectCtx<'_> {
    effectcraft_effects::EffectCtx {
        params: parameters,
        time: 0.7,
        layer_size: [31.0, 19.0],
        seed: 1,
        adjustment: false,
        env: effectcraft_effects::EffectEnv { host: Some(&Clock), frame_rate: 10.0, comp_time: 0.7, ..Default::default() },
    }
}

#[test]
fn checked_timewarp_streaming_keeps_gpu_uploads_and_scratch_bounded() {
    let gpu = crate::tests::gpu();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "Timewarp acceptance requires a working GPU");
    let Some(gpu) = gpu else { return };
    let spec = effectcraft_effects::find(effectcraft_effects::timewarp::ID).unwrap();
    let input = effectcraft_effects::Buf { img: effectcraft_raster::Image::new(31, 19), offset: [1.0, 2.0], scale: 1.0 };
    for method in 0..=2 {
        for samples in [1, 8, 64] {
            let parameters = timewarp_parameters(method, samples);
            let cpu = effectcraft_effects::apply(spec, &timewarp_context(&parameters), input.clone()).unwrap();
            let before = gpu.context().transfer_stats();
            let step = effectcraft_render::FxStep { spec, ctx: timewarp_context(&parameters) };
            let output = effectcraft_render::Accelerator::effect_checked(gpu, &step, &input, None).unwrap().expect("checked GPU Timewarp must run");
            let after = gpu.context().transfer_stats();
            assert!(after.uploads - before.uploads <= 3, "only input and two unique sources upload, independent of samples");
            assert_eq!(after.readbacks - before.readbacks, 1);
            assert_eq!((output.offset, output.scale), (cpu.offset, cpu.scale));
            assert_eq!(crate::tests::diff(&output.img, &cpu.img, 1e-3).over, 0, "method {method}, samples {samples}");
        }
    }
    let mut parameters = timewarp_parameters(2, 64);
    parameters.values.insert("speed".into(), n(f64::INFINITY));
    let before = gpu.context().transfer_stats();
    let step = effectcraft_render::FxStep { spec, ctx: timewarp_context(&parameters) };
    assert!(effectcraft_render::Accelerator::effect_checked(gpu, &step, &input, None).unwrap_err().contains("Speed"));
    let after = gpu.context().transfer_stats();
    assert_eq!((after.uploads, after.readbacks), (before.uploads, before.readbacks), "hostile controls fail before GPU transfers");
}

#[test]
fn checked_timewarp_missing_kernel_is_an_error_then_a_valid_whole_frame_works() {
    crate::tests::hold_gpu_lock();
    let gpu = crate::Gpu::headless();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "missing-kernel acceptance requires a working GPU");
    let Some(mut gpu) = gpu else { return };
    std::sync::Arc::get_mut(&mut gpu.ctx).unwrap().drop_kernel("ftm_mix");
    let spec = effectcraft_effects::find(effectcraft_effects::timewarp::ID).unwrap();
    let input = effectcraft_effects::Buf { img: effectcraft_raster::Image::new(31, 19), offset: [1.0, 2.0], scale: 1.0 };
    let mut parameters = timewarp_parameters(1, 8);
    let step = effectcraft_render::FxStep { spec, ctx: timewarp_context(&parameters) };
    let error = effectcraft_render::Accelerator::effect_checked(&gpu, &step, &input, None).unwrap_err();
    assert!(error.contains("ftm_mix"));
    parameters.values.insert("method".into(), e(0));
    let step = effectcraft_render::FxStep { spec, ctx: timewarp_context(&parameters) };
    let actual = effectcraft_render::Accelerator::effect_checked(&gpu, &step, &input, None).unwrap().unwrap();
    let expected = effectcraft_effects::apply(spec, &timewarp_context(&parameters), input).unwrap();
    assert_eq!(crate::tests::diff(&actual.img, &expected.img, 1e-3).over, 0);
}

/// `id` with `vals` on a moving precomp; `map` adds a hidden footage layer and sets parameter
/// `map` to it.
fn moving(id: &str, vals: &[(&str, Value)], map: Option<&str>) {
    for depth in [BitDepth::Bpc8, BitDepth::Bpc32] {
        let mut s = Scene::new(depth);
        let bg = s.solid([0.15, 0.1, 0.2], 97, 61);
        s.push(bg);
        let mut vals = vals.to_vec();
        if let Some(param) = map {
            let mut m = s.footage(50, 36);
            m.switches.video = false;
            let mid = s.push(m);
            vals.push((param, Value::Layer(Some(mid.0))));
        }
        let inner = Comp::new(60, 40, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
        let inner_id = s.p.add_item("Inner", Label::Sandstone, None, ItemKind::Comp(inner.into()));
        let mut a = s.footage(30, 24);
        let pos = a.props.prop_mut("transform/position").unwrap();
        pos.keys = vec![Keyframe::new(Tick::ZERO, v3(10.0, 12.0)), Keyframe::new(Tick::from_seconds_f64(1.0), v3(50.0, 30.0))];
        let rot = a.props.prop_mut("transform/rotation").unwrap();
        rot.keys = vec![Keyframe::new(Tick::ZERO, Value::Scalar(0.0)), Keyframe::new(Tick::from_seconds_f64(1.0), Value::Scalar(90.0))];
        s.p.comp_mut(inner_id).unwrap().layers = vec![a];
        let mut l = build::layer(&mut s.p, &s.comp, "Inner", LayerSource::Comp { item: inner_id }, (60, 40), None);
        s.effect(&mut l, id, &vals);
        set(&mut l, "transform/rotation", Value::Scalar(8.0));
        s.push(l);
        let t = Tick::from_seconds_f64(0.4);
        let label = format!("{id} {vals:?} {depth:?}");
        check(&label, compare_at(&s, opts(), t), 0.0);
        check(&format!("{label} half"), compare_at(&s, RenderOpts { scale: 0.5, ..opts() }, t), 0.0);
    }
}

#[test]
fn without_a_host() {
    for id in
        ["ec.time.timedifference", "ec.time.timedisplacement", "ec.time.ccforcemotionblur", "ec.time.ccwidetime", "ec.time.pixelmotionblur", "ec.time.timewarp"]
    {
        effect_case(id, &[]);
    }
}

#[test]
fn frame_averages() {
    moving("ec.time.ccforcemotionblur", &[], None);
    moving("ec.time.ccforcemotionblur", &[("motionBlurLevels", n(5.0)), ("shutterAngle", n(400.0))], None);
    moving("ec.time.ccwidetime", &[], None);
    moving("ec.time.ccwidetime", &[("forwardSteps", n(1.0)), ("backwardSteps", n(4.0))], None);
    moving("ec.time.pixelmotionblur", &[], None);
    moving("ec.time.pixelmotionblur", &[("shutterControl", e(1)), ("shutterAngle", n(300.0)), ("shutterSamples", n(5.0))], None);
}

#[test]
fn time_difference() {
    moving("ec.time.timedifference", &[("timeOffset", n(-0.1))], None);
    for alpha in 0..9 {
        moving(
            "ec.time.timedifference",
            &[("timeOffset", n(0.2)), ("contrast", n(40.0)), ("absoluteDifference", Value::Bool(alpha % 2 == 0)), ("alphaChannel", e(alpha))],
            None,
        );
    }
    moving("ec.time.timedifference", &[("timeOffset", n(0.1)), ("alphaChannel", e(2))], Some("targetLayer"));
}

#[test]
fn time_displacement() {
    moving("ec.time.timedisplacement", &[("maxDisplacementTime", n(0.3)), ("timeResolution", n(10.0))], None);
    moving("ec.time.timedisplacement", &[("maxDisplacementTime", n(0.5)), ("timeResolution", n(30.0))], Some("displacementMapLayer"));
    moving("ec.time.timedisplacement", &[("maxDisplacementTime", n(2.0)), ("stretchMap", Value::Bool(false))], Some("displacementMapLayer"));
}

#[test]
fn timewarp() {
    for method in 0..3 {
        moving("ec.time.timewarp", &[("method", e(method)), ("speed", n(37.0))], None);
    }
    moving("ec.time.timewarp", &[("method", e(1)), ("adjustTimeBy", e(1)), ("sourceFrame", n(7.4))], None);
    moving("ec.time.timewarp", &[("method", e(2)), ("speed", n(43.0)), ("motionBlur/enableMotionBlur", on()), ("motionBlur/shutterSamples", n(3.0))], None);
    moving(
        "ec.time.timewarp",
        &[
            ("speed", n(61.0)),
            ("tuning/filtering", e(1)),
            ("tuning/errorThreshold", n(30.0)),
            ("sourceCrops/leftCrop", n(4.0)),
            ("sourceCrops/topCrop", n(3.0)),
        ],
        None,
    );
    moving("ec.time.timewarp", &[("speed", n(55.0)), ("tuning/buildFromOneImage", on())], None);
    moving("ec.time.timewarp", &[("speed", n(47.0))], Some("warpLayer"));
    // A Matte Layer: every method and Show, the luminance channels, motion blur.
    for method in 0..3 {
        for show in 0..4 {
            moving("ec.time.timewarp", &[("method", e(method)), ("speed", n(47.0)), ("show", e(show))], Some("matteLayer"));
        }
    }
    moving("ec.time.timewarp", &[("speed", n(39.0)), ("matteChannel", e(2))], Some("matteLayer"));
    moving("ec.time.timewarp", &[("speed", n(43.0)), ("matteChannel", e(1)), ("motionBlur/enableMotionBlur", on())], Some("matteLayer"));
    moving("ec.time.timewarp", &[("method", e(1)), ("speed", n(52.0)), ("tuning/buildFromOneImage", on()), ("show", e(1))], Some("matteLayer"));
}

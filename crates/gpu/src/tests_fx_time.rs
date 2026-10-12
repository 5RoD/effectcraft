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

/// This five-pixel fixture originally switched CPU/GPU fallback branches by one ULP,
/// producing a 0.0585 channel error. Keep the strict tolerance through the new transition.
#[test]
fn pixel_motion_threshold_neighbours_are_continuous_on_cpu_and_gpu() {
    use effectcraft_effects::{Buf, EffectHost, LayerPixels};
    use effectcraft_raster::Image;
    struct Samples;
    impl EffectHost for Samples {
        fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
            None
        }
        fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
            None
        }
        fn self_at(&self, time: f64, _: usize) -> Option<Buf> {
            let values = if time < 0.05 { [0.0, 0.25, 0.5, 0.75, 1.0] } else { [0.75, 0.5, 0.25, 0.0, 0.0] };
            Some(Buf { img: Image { width: 5, height: 1, data: values.into_iter().map(|r| [r, 0.0, 0.0, 1.0]).collect() }, offset: [0.0; 2], scale: 1.0 })
        }
    }
    let gpu = crate::tests::gpu();
    assert!(std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1") || gpu.is_some());
    let Some(gpu) = gpu else { return };
    let mut parameters = timewarp_parameters(2, 1);
    parameters.values.insert("sourceFrame".into(), n(0.9));
    parameters.values.insert("motionBlur/enableMotionBlur".into(), Value::Bool(false));
    let mut ctx = timewarp_context(&parameters);
    ctx.layer_size = [5.0, 1.0];
    ctx.env.host = Some(&Samples);
    let mut plan = effectcraft_effects::timewarp_plan(&ctx).unwrap().unwrap();
    let effectcraft_effects::TwStep::Motion { a, b, w, flow } = plan.steps[0].0 else { panic!("fractional frame must use motion") };
    plan.flows[flow].v.fill([1.3, 0.0]);
    let f = plan.flows[flow].at(2.5, 0.5);
    let pa = plan.frames[a].sample_bilinear_clamped(2.5 - w as f64 * f[0] as f64, 0.5);
    let pb = plan.frames[b].sample_bilinear_clamped(2.5 + (1.0 - w as f64) * f[0] as f64, 0.5);
    let error = (pa[0] - pb[0]).abs();
    assert!(error > 0.0 && error < 1.0);
    for threshold in [error.next_down(), error, error.next_up()] {
        plan.pm.error_threshold = threshold;
        let cpu = plan.render_cpu().unwrap();
        let mut encoder = crate::context::Enc::new(gpu.context());
        let input = crate::effects::GBuf { img: gpu.context().upload_image(&Image::new(5, 1)).unwrap(), offset: [0.0; 2], scale: 1.0 };
        let output = crate::fx_time::prepared(&mut encoder, input, Some(&plan)).unwrap();
        let output = encoder.download(&output.img).unwrap();
        eprintln!("motion threshold={threshold:?}, CPU error={error:?}, warped={pa:?}/{pb:?}, CPU={:?}, GPU={:?}", cpu.img.data[2], output.data[2]);
        assert!(output.data.iter().flatten().zip(cpu.img.data.iter().flatten()).all(|(a, b)| a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-3));
    }
}

fn motion_plan(a: effectcraft_raster::Image, b: effectcraft_raster::Image, fraction: f32, extreme: bool) -> effectcraft_effects::TimewarpPlan {
    struct Frames(effectcraft_raster::Image, effectcraft_raster::Image);
    impl effectcraft_effects::EffectHost for Frames {
        fn layer(&self, _: u64, _: bool) -> Option<effectcraft_effects::LayerPixels> {
            None
        }
        fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
            None
        }
        fn self_at(&self, time: f64, _: usize) -> Option<effectcraft_effects::Buf> {
            Some(effectcraft_effects::Buf { img: if time < 0.05 { self.0.clone() } else { self.1.clone() }, offset: [0.0; 2], scale: 1.0 })
        }
    }
    let size = [a.width as f64, a.height as f64];
    let host = Frames(a, b);
    let mut parameters = timewarp_parameters(2, 1);
    parameters.values.insert("sourceFrame".into(), n(fraction as f64));
    parameters.values.insert("motionBlur/enableMotionBlur".into(), Value::Bool(false));
    parameters.values.insert("tuning/filtering".into(), e(u32::from(extreme)));
    let mut context = timewarp_context(&parameters);
    context.layer_size = size;
    context.env.host = Some(&host);
    let mut plan = effectcraft_effects::timewarp_plan(&context).unwrap().unwrap();
    for flow in &mut plan.flows {
        flow.v.fill([1.3, -0.17]);
    }
    plan
}

fn assert_motion_plan_parity(gpu: &crate::Gpu, plan: &effectcraft_effects::TimewarpPlan) {
    let cpu = plan.render_cpu().unwrap();
    let mut encoder = crate::context::Enc::new(gpu.context());
    let input = crate::effects::GBuf { img: gpu.context().upload_image(&effectcraft_raster::Image::new(1, 1)).unwrap(), offset: [0.0; 2], scale: 1.0 };
    let output = crate::fx_time::prepared(&mut encoder, input, Some(plan)).unwrap();
    let output = encoder.download(&output.img).unwrap();
    assert_eq!((output.width, output.height), (cpu.img.width, cpu.img.height));
    let mut maximum = 0.0f32;
    for (index, (a, b)) in output.data.iter().flatten().zip(cpu.img.data.iter().flatten()).enumerate() {
        assert!(a.is_finite() && b.is_finite(), "non-finite output channel {index}");
        maximum = maximum.max((a - b).abs());
    }
    eprintln!(
        "motion {}x{}, extreme={}, threshold={}, maximum absolute error={maximum}",
        output.width, output.height, plan.pm.extreme, plan.pm.error_threshold
    );
    assert!(maximum <= 1e-3, "strict absolute error={maximum}, extreme={}, threshold={}", plan.pm.extreme, plan.pm.error_threshold);
}

#[test]
fn pixel_motion_transition_covers_hdr_negative_alpha_extreme_and_local_coordinates() {
    use effectcraft_raster::Image;
    let gpu = crate::tests::gpu();
    assert!(std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1") || gpu.is_some());
    let Some(gpu) = gpu else { return };
    for (gain, shift, vary_alpha) in [(1.0, 0.0, false), (1.0, -0.75, false), (8.0, -3.0, true), (0.0, 0.0, true)] {
        let frame = |values: [f32; 5]| Image {
            width: 5,
            height: 1,
            data: values.map(|r| [r * gain + shift, r * gain * 0.4 + shift, -r * gain * 0.25, if vary_alpha { 0.1 + r * 0.7 } else { 1.0 }]).to_vec(),
        };
        for extreme in [false, true] {
            let mut plan = motion_plan(frame([0.0, 0.25, 0.5, 0.75, 1.0]), frame([0.75, 0.5, 0.25, 0.0, 0.0]), 0.9, extreme);
            let effectcraft_effects::TwStep::Motion { a, b, w, flow } = plan.steps[0].0 else { panic!("expected fractional motion") };
            let f = plan.flows[flow].at(2.5, 0.5);
            let sample = |image: &Image, x, y| if extreme { image.sample_bicubic(x, y) } else { image.sample_bilinear_clamped(x, y) };
            let pa = sample(&plan.frames[a], 2.5 - w as f64 * f[0] as f64, 0.5 - w as f64 * f[1] as f64);
            let pb = sample(&plan.frames[b], 2.5 + (1.0 - w as f64) * f[0] as f64, 0.5 + (1.0 - w as f64) * f[1] as f64);
            let error = pa.iter().zip(pb).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            let magnitude = pa.iter().chain(pb.iter()).map(|v| v.abs()).fold(1.0f32, f32::max);
            let band = (0.00015 * magnitude).min(0.001).min(error * 0.5);
            for threshold in
                [0.0, 1.0, error.next_down(), error, error.next_up(), error - band * 0.5, error + band * 0.5, error - band * 1.01, error + band * 1.01]
            {
                if !(0.0..=1.0).contains(&threshold) {
                    continue;
                }
                plan.pm.error_threshold = threshold;
                assert_motion_plan_parity(gpu, &plan);
            }
        }
    }
    // Odd dimensions and a far-away coordinate expose loss from adding displacement
    // to a full f32 pixel coordinate. Large *bounded* displacements exercise safe taps.
    let frame = |phase: f32| {
        let mut image = Image::new(1291, 17);
        for (index, pixel) in image.data.iter_mut().enumerate() {
            let x = (index % 1291) as f32;
            let y = (index / 1291) as f32;
            let value = 0.5 + 0.25 * (x * 0.7 + y * 0.13 + phase).sin();
            *pixel = [value, value * 2.0, -value, 0.6];
        }
        image
    };
    for extreme in [false, true] {
        let mut plan = motion_plan(frame(0.0), frame(0.3), 0.9, extreme);
        for threshold in [0.0, 0.1, 1.0] {
            plan.pm.error_threshold = threshold;
            assert_motion_plan_parity(gpu, &plan);
        }
        for flow in &mut plan.flows {
            flow.v.fill([65_536.0, -65_536.0]);
        }
        assert_motion_plan_parity(gpu, &plan);
    }
}

#[test]
fn pixel_motion_invalid_public_plan_fails_before_transfers_then_recovers() {
    use effectcraft_raster::Image;
    let gpu = crate::tests::gpu();
    assert!(std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1") || gpu.is_some());
    let Some(gpu) = gpu else { return };
    let mut plan = motion_plan(Image::filled(3, 1, [0.1; 4]), Image::filled(3, 1, [0.2; 4]), 0.9, false);
    let mut encoder = crate::context::Enc::new(gpu.context());
    let image = gpu.context().upload_image(&Image::new(1, 1)).unwrap();
    let input = || crate::effects::GBuf { img: image.clone(), offset: [0.0; 2], scale: 1.0 };
    let before = gpu.context().transfer_stats();
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        -f32::MAX,
        effectcraft_effects::timewarp::MAX_CHANNEL_MAGNITUDE.next_up(),
        -effectcraft_effects::timewarp::MAX_CHANNEL_MAGNITUDE.next_up(),
    ] {
        for channel in 0..4 {
            let saved = plan.frames[0].data[0][channel];
            plan.frames[0].data[0][channel] = value;
            assert!(crate::fx_time::prepared(&mut encoder, input(), Some(&plan)).err().unwrap().contains("non-finite RGBA"));
            plan.frames[0].data[0][channel] = saved;
        }
        for field in 0..3 {
            let (threshold, weight, vector) = (plan.pm.error_threshold, plan.pm.weights[0], plan.flows[0].v[0][0]);
            match field {
                0 => plan.pm.error_threshold = value,
                1 => plan.pm.weights[0] = value,
                _ => plan.flows[0].v[0][0] = value,
            }
            assert!(crate::fx_time::prepared(&mut encoder, input(), Some(&plan)).is_err());
            (plan.pm.error_threshold, plan.pm.weights[0], plan.flows[0].v[0][0]) = (threshold, weight, vector);
        }
    }
    let pixel = plan.frames[0].data.pop().unwrap();
    assert!(crate::fx_time::prepared(&mut encoder, input(), Some(&plan)).err().unwrap().contains("frame storage"));
    plan.frames[0].data.push(pixel);
    let after = gpu.context().transfer_stats();
    assert_eq!((before.uploads, before.readbacks), (after.uploads, after.readbacks));
    assert_motion_plan_parity(gpu, &plan);
    // The inclusive support limit is not a clamp: exact whole-frame transport retains
    // positive/negative channels at the limit on both backends.
    plan.steps[0].0 = effectcraft_effects::TwStep::Whole(0);
    plan.frames[0].data.fill([effectcraft_effects::timewarp::MAX_CHANNEL_MAGNITUDE, -effectcraft_effects::timewarp::MAX_CHANNEL_MAGNITUDE, 0.0, 1.0]);
    assert_motion_plan_parity(gpu, &plan);
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

//! One-pixel temporal identity oracle. Native media chooses floor(FrameRate, Tick), so
//! rounding a source boundary to microseconds can fetch the preceding decoded frame.

use effectcraft_effects::{Buf, EffectCtx, EffectEnv, EffectHost, LayerPixels, Params};
use effectcraft_keyframe::Value;
use effectcraft_raster::Image;
use effectcraft_time::{FrameRate, Tick};
use std::sync::Mutex;

struct FrameIdentity {
    rate: FrameRate,
    shift: Tick,
    time: Tick,
}

fn encoded(frame: i64) -> [f32; 4] {
    // Preserve an off-by-one at long times without f32 integer precision hiding it.
    [frame.rem_euclid(65_536) as f32, frame.div_euclid(65_536) as f32, 0.0, 1.0]
}

impl EffectHost for FrameIdentity {
    fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
        None
    }
    fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
        None
    }
    fn self_at(&self, time: f64, effects: usize) -> Option<Buf> {
        self.self_at_tick(Tick::from_seconds_f64(time), effects)
    }
    fn self_at_tick(&self, time: Tick, effects: usize) -> Option<Buf> {
        assert_eq!(effects, 0);
        let frame = self.rate.frame_at(time + self.shift);
        Some(Buf { img: Image::filled(1, 1, encoded(frame)), offset: [0.0; 2], scale: 1.0 })
    }
    fn time_ticks(&self) -> Option<(Tick, Tick)> {
        Some((self.time, self.time))
    }
}

fn identity(comp: FrameRate, source: FrameRate, shift: Tick, frame: i64, by_frame: bool, method: u32) {
    let time = comp.tick_of(frame);
    let host = FrameIdentity { rate: source, shift, time };
    let spec = effectcraft_effects::find("ec.time.timewarp").unwrap();
    let mut params = Params { values: spec.params.iter().map(|p| (p.id.into(), effectcraft_effects::default_value(p, [1.0; 2]))).collect() };
    for (id, value) in [
        ("method", Value::Enum(method)),
        ("adjustTimeBy", Value::Enum(by_frame as u32)),
        ("sourceFrame", Value::Scalar(if by_frame { frame as f64 } else { 0.0 })),
        ("speed", Value::Scalar(100.0)),
        ("motionBlur/enableMotionBlur", Value::Bool(false)),
    ] {
        params.values.insert(id.into(), value);
    }
    let ctx = EffectCtx {
        params: &params,
        time: time.seconds(),
        layer_size: [1.0; 2],
        seed: 1,
        adjustment: false,
        env: EffectEnv { host: Some(&host), frame_rate: comp.as_f64(), comp_time: time.seconds(), ..Default::default() },
    };
    let plan = effectcraft_effects::timewarp_plan(&ctx).unwrap().unwrap();
    assert!(matches!(plan.steps.as_slice(), [(effectcraft_effects::TwStep::Whole(_), _)]), "zero fraction must not estimate/interpolate motion");
    let output = plan.render_cpu().unwrap();
    assert_eq!(
        output.img.data[0],
        encoded(source.frame_at(time + shift)),
        "comp={comp:?}, source={source:?}, shift={shift:?}, frame={frame}, by_frame={by_frame}, method={method}"
    );
}

#[test]
fn frame_identity_at_exact_sixty_and_one_twenty_boundaries_in_all_methods() {
    for rate in [FrameRate::FPS_60, FrameRate::FPS_120] {
        for frame in [-121, -120, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 37, 120, 121] {
            for by_frame in [false, true] {
                for method in 0..=2 {
                    identity(rate, rate, Tick::ZERO, frame, by_frame, method);
                }
            }
        }
    }
}

#[test]
fn exact_current_time_and_reverse_speed_keep_the_tick_before_a_boundary() {
    for rate in [FrameRate::FPS_60, FrameRate::FPS_120, FrameRate::FPS_29_97, FrameRate::FPS_119_88] {
        for frame in [-31_000_003, -37, -1, 0, 1, 37, 31_000_003] {
            for delta in [-7, -1, 0, 1, 7] {
                let time = Tick(rate.tick_of(frame).0 + delta);
                let host = FrameIdentity { rate, shift: Tick::ZERO, time };
                let spec = effectcraft_effects::find("ec.time.timewarp").unwrap();
                for speed in [-100.0, 100.0] {
                    for method in 0..=if delta == 0 { 2 } else { 0 } {
                        let mut params =
                            Params { values: spec.params.iter().map(|p| (p.id.into(), effectcraft_effects::default_value(p, [1.0; 2]))).collect() };
                        params.values.insert("method".into(), Value::Enum(method));
                        params.values.insert("speed".into(), Value::Scalar(speed));
                        let ctx = EffectCtx {
                            params: &params,
                            time: time.seconds(),
                            layer_size: [1.0; 2],
                            seed: 1,
                            adjustment: false,
                            env: EffectEnv { host: Some(&host), frame_rate: rate.as_f64(), comp_time: time.seconds(), ..Default::default() },
                        };
                        let plan = effectcraft_effects::timewarp_plan(&ctx).unwrap().unwrap();
                        let output = plan.render_cpu().unwrap();
                        let expected = if speed < 0.0 { -time } else { time };
                        assert_eq!(
                            output.img.data[0],
                            encoded(rate.frame_at(expected)),
                            "rate={rate:?}, frame={frame}, delta={delta}, speed={speed}, method={method}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn legacy_host_defaults_delegate_without_nudging_and_expose_the_float_limit() {
    struct Legacy(Mutex<Vec<f64>>);
    impl EffectHost for Legacy {
        fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
            None
        }
        fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
            None
        }
        fn self_at(&self, time: f64, _: usize) -> Option<Buf> {
            self.0.lock().unwrap().push(time);
            None
        }
        fn layer_at(&self, _: u64, time: f64, _: bool) -> Option<LayerPixels> {
            self.0.lock().unwrap().push(time);
            None
        }
    }
    let host = Legacy(Mutex::new(Vec::new()));
    let boundary = FrameRate::FPS_120.tick_of(31_000_003);
    let before = Tick(boundary.0 - 1);
    assert_eq!(before.seconds(), boundary.seconds(), "seconds cannot encode every neighbouring Tick at this duration");
    for time in [before, boundary, Tick(boundary.0 + 1)] {
        host.self_at_tick(time, 0);
        host.layer_at_tick(7, time, false);
    }
    assert_eq!(
        *host.0.lock().unwrap(),
        vec![before.seconds(), before.seconds(), boundary.seconds(), boundary.seconds(), Tick(boundary.0 + 1).seconds(), Tick(boundary.0 + 1).seconds()]
    );
    assert!(host.time_ticks().is_none());
}

#[test]
fn frame_identity_at_rational_broadcast_boundaries_in_all_methods() {
    for rate in [FrameRate::FPS_23_976, FrameRate::FPS_29_97, FrameRate::FPS_59_94, FrameRate::FPS_119_88] {
        for frame in [-1001, -5, -2, -1, 0, 1, 2, 5, 1001, 10001] {
            for by_frame in [false, true] {
                for method in 0..=2 {
                    identity(rate, rate, Tick::ZERO, frame, by_frame, method);
                }
            }
        }
    }
}

#[test]
fn frame_identity_preserves_interpretation_and_shifted_source_time() {
    for (comp, source) in [
        (FrameRate::FPS_60, FrameRate::FPS_120),
        (FrameRate::FPS_120, FrameRate::FPS_60),
        (FrameRate::FPS_59_94, FrameRate::FPS_119_88),
        (FrameRate::FPS_119_88, FrameRate::FPS_59_94),
    ] {
        for shift in [Tick::ZERO, source.tick_of(37), source.tick_of(-37)] {
            for frame in [-5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 120] {
                for by_frame in [false, true] {
                    for method in 0..=2 {
                        identity(comp, source, shift, frame, by_frame, method);
                    }
                }
            }
        }
    }
}

#[test]
fn frame_identity_remains_exact_in_hour_day_and_three_day_projects() {
    for rate in [FrameRate::FPS_60, FrameRate::FPS_120, FrameRate::FPS_29_97, FrameRate::FPS_119_88] {
        for seconds in [3600, 8 * 3600, 24 * 3600, 72 * 3600] {
            let base = rate.frame_at(Tick::from_seconds_f64(seconds as f64));
            for frame in [base - 1, base, base + 1, -base - 1, -base, -base + 1] {
                for shift in [Tick::ZERO, rate.tick_of(37), rate.tick_of(-37)] {
                    for method in 0..=2 {
                        identity(rate, rate, shift, frame, false, method);
                    }
                }
            }
        }
    }
}

#[test]
fn auxiliary_matte_and_warp_layers_fetch_the_same_shifted_exact_boundaries() {
    struct Auxiliary {
        rate: FrameRate,
        local: Tick,
        comp: Tick,
        reads: Mutex<Vec<(u64, i64)>>,
    }
    impl EffectHost for Auxiliary {
        fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
            None
        }
        fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
            None
        }
        fn self_at(&self, _: f64, _: usize) -> Option<Buf> {
            Some(Buf { img: Image::filled(1, 1, [0.5; 4]), offset: [0.0; 2], scale: 1.0 })
        }
        fn layer_at(&self, id: u64, time: f64, _: bool) -> Option<LayerPixels> {
            self.layer_at_tick(id, Tick::from_seconds_f64(time), false)
        }
        fn layer_at_tick(&self, id: u64, time: Tick, _: bool) -> Option<LayerPixels> {
            self.reads.lock().unwrap().push((id, self.rate.frame_at(time)));
            Some(LayerPixels { buf: self.self_at(time.seconds(), 0).unwrap(), size: [1.0; 2] })
        }
        fn time_ticks(&self) -> Option<(Tick, Tick)> {
            Some((self.local, self.comp))
        }
    }
    for rate in [FrameRate::FPS_60, FrameRate::FPS_120, FrameRate::FPS_59_94, FrameRate::FPS_119_88] {
        for base in [-31_000_003, -37, 37, 31_000_003] {
            let host = Auxiliary { rate, local: rate.tick_of(2), comp: rate.tick_of(base + 2), reads: Mutex::new(Vec::new()) };
            let spec = effectcraft_effects::find("ec.time.timewarp").unwrap();
            let mut params = Params { values: spec.params.iter().map(|p| (p.id.into(), effectcraft_effects::default_value(p, [1.0; 2]))).collect() };
            for (id, value) in [
                ("method", Value::Enum(0)),
                ("adjustTimeBy", Value::Enum(1)),
                ("sourceFrame", Value::Scalar(-5.0)),
                ("matteLayer", Value::Layer(Some(7))),
                ("warpLayer", Value::Layer(Some(8))),
            ] {
                params.values.insert(id.into(), value);
            }
            let ctx = EffectCtx {
                params: &params,
                time: rate.tick_of(2).seconds(),
                layer_size: [1.0; 2],
                seed: 1,
                adjustment: false,
                env: EffectEnv { host: Some(&host), frame_rate: rate.as_f64(), comp_time: rate.tick_of(base + 2).seconds(), ..Default::default() },
            };
            effectcraft_effects::timewarp_plan(&ctx).unwrap().unwrap();
            assert_eq!(*host.reads.lock().unwrap(), vec![(7, base - 5), (7, base - 4), (8, base - 5), (8, base - 4)], "rate={rate:?}, base={base}");
        }
    }
}

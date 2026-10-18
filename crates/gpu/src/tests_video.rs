//! Native video conversion, pipeline selection and checked failure regressions.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::{
    context::Enc,
    tests::{Scene, diff, gpu},
};
use effectcraft_project::{BitDepth, Footage, FootageKind, ItemId, ItemKind};
use effectcraft_raster::Image;
use effectcraft_render::{Accelerator, FootageSource, RenderOpts, Renderer, video::Yuv420Frame};
use effectcraft_time::Tick;

fn native_gpu() -> Option<&'static crate::Gpu> {
    let gpu = gpu();
    if std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1") {
        assert!(gpu.is_some(), "native video effect acceptance requires a working GPU");
    }
    gpu
}

fn frame(w: u32, h: u32, kr: f32, kb: f32, full: bool) -> Arc<Yuv420Frame> {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let planes = [
        Arc::new((0..w * h).map(|i| (i.wrapping_mul(37).wrapping_add(11) % 256) as u8).collect()),
        Arc::new((0..cw * ch).map(|i| (i.wrapping_mul(71).wrapping_add(3) % 256) as u8).collect()),
        Arc::new((0..cw * ch).map(|i| (i.wrapping_mul(53).wrapping_add(251) % 256) as u8).collect()),
    ];
    let kg = 1.0 - kr - kb;
    let cr = 2.0 * (1.0 - kr);
    let cb = 2.0 * (1.0 - kb);
    let range = if full { [1.0 / 255.0, 0.0, 1.0 / 255.0, 128.0] } else { [1.0 / 219.0, 16.0, 1.0 / 224.0, 128.0] };
    Arc::new(Yuv420Frame::new(w, h, planes, range, [cr, cr * kr / kg, cb * kb / kg, cb]).unwrap())
}

/// Scalar CPU reference, including edge clamping and vertical-before-horizontal interpolation.
fn reference(frame: &Yuv420Frame) -> Image {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let r = frame.range();
    let m = frame.matrix();
    let p = frame.planes();
    let neighbour = |i: usize, n: usize| {
        let c = (i / 2).min(n - 1);
        (c, if i.is_multiple_of(2) { c.saturating_sub(1) } else { (c + 1).min(n - 1) })
    };
    let data = (0..w * h)
        .map(|i| {
            let (x0, x1) = neighbour(i % w, cw);
            let (y0, y1) = neighbour(i / w, ch);
            let chroma = |plane: &[u8]| {
                let a = plane[y0 * cw + x0] as f32 * 0.75 + plane[y1 * cw + x0] as f32 * 0.25;
                let b = plane[y0 * cw + x1] as f32 * 0.75 + plane[y1 * cw + x1] as f32 * 0.25;
                ((a - r[3]) * r[2]) * 0.75 + ((b - r[3]) * r[2]) * 0.25
            };
            let u = chroma(&p[1]);
            let v = chroma(&p[2]);
            let y = (p[0][i] as f32 - r[1]) * r[0];
            [(y + m[0] * v).clamp(0.0, 1.0), (y - m[1] * v - m[2] * u).clamp(0.0, 1.0), (y + m[3] * u).clamp(0.0, 1.0), 1.0]
        })
        .collect();
    Image { width: frame.width(), height: frame.height(), data }
}

#[test]
fn native_video_pixels_cover_odd_edges_ranges_matrices_and_project_depths() {
    let Some(g) = gpu() else { return };
    assert!(g.ctx.has_kernel("video_yuv420"));
    for (w, h) in [(1, 1), (1, 7), (9, 1), (5, 3), (32, 18), (97, 61)] {
        for (kr, kb) in [(0.299, 0.114), (0.2126, 0.0722), (0.2627, 0.0593)] {
            for full in [false, true] {
                let frame = frame(w, h, kr, kb, full);
                for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
                    let mut cpu = reference(&frame);
                    let settings = effectcraft_project::ProjectSettings { bit_depth: depth, ..Default::default() };
                    effectcraft_render::color::Pipe::of(&settings).quantize(&mut cpu);
                    let mut e = Enc::new(&g.ctx);
                    let src = crate::video::upload(&mut e, &frame, depth.levels()).unwrap().unwrap();
                    let actual = e.download(&src).unwrap();
                    let tolerance = depth.levels().map_or(2e-6, |l| 1.0 / l + 2e-6);
                    assert_eq!(diff(&cpu, &actual, tolerance).over, 0, "{w}x{h} {kr}/{kb} full={full} depth={depth:?}");
                    assert!(actual.data.iter().all(|px| px[3] == 1.0));
                }
            }
        }
    }
}

#[test]
fn staged_video_uploads_keep_each_frames_planes_until_submission() {
    let Some(g) = native_gpu() else { return };
    let a = frame(5, 3, 0.2126, 0.0722, false);
    let b = frame(9, 1, 0.299, 0.114, true);
    let mut encoder = Enc::new(&g.ctx);
    // Both staging views have been dropped before the shared command buffer is submitted.
    // Odd plane lengths also exercise the zero padding between storage-buffer planes.
    let first = crate::video::upload(&mut encoder, &a, None).unwrap().unwrap();
    let second = crate::video::upload(&mut encoder, &b, None).unwrap().unwrap();
    assert_eq!(diff(&reference(&a), &encoder.download(&first).unwrap(), 2e-6).over, 0);
    assert_eq!(diff(&reference(&b), &encoder.download(&second).unwrap(), 2e-6).over, 0);
}

struct Source {
    raw_calls: AtomicUsize,
    image_calls: AtomicUsize,
    fail: bool,
}
impl FootageSource for Source {
    fn yuv420_half_size(&self) -> bool {
        true
    }
    fn frame(&self, _: ItemId, f: &Footage, t: Tick) -> Option<Arc<Image>> {
        self.image_calls.fetch_add(1, Ordering::Relaxed);
        Some(Arc::new(reference(&frame(f.width, f.height, 0.2126, 0.0722, t != Tick::ZERO))))
    }
    fn frame_at_size(&self, item: ItemId, f: &Footage, t: Tick, w: u32, h: u32) -> Option<Arc<Image>> {
        if f.width.is_multiple_of(2) && f.height.is_multiple_of(2) && w <= f.width / 2 && h <= f.height / 2 {
            self.image_calls.fetch_add(1, Ordering::Relaxed);
            let half = reference_half(&frame(f.width, f.height, 0.2126, 0.0722, t != Tick::ZERO));
            return Some(Arc::new(effectcraft_raster::resample(&half, w, h)));
        }
        self.frame(item, f, t)
    }
    fn yuv420_frame_checked(&self, _: ItemId, f: &Footage, t: Tick) -> Result<Option<Arc<Yuv420Frame>>, String> {
        self.raw_calls.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err("synthetic truncated video plane".into());
        }
        Ok(Some(frame(f.width, f.height, 0.2126, 0.0722, t != Tick::ZERO)))
    }
}

fn scene(depth: BitDepth) -> Scene {
    scene_size(depth, 97, 61)
}

fn scene_size(depth: BitDepth, width: u32, height: u32) -> Scene {
    let mut s = Scene::new(depth);
    let layer = s.footage(width, height);
    let effectcraft_project::LayerSource::Footage { item } = layer.source else { panic!("footage") };
    let ItemKind::Footage(f) = &mut s.p.item_mut(item).unwrap().kind else { panic!("footage item") };
    f.kind = FootageKind::Video;
    f.duration = Tick::from_seconds_f64(2.0);
    s.push(layer);
    s
}

/// Independent scalar form of media's half-420 filter: average luma, filter centred
/// chroma vertically then horizontally, normalize before the horizontal filter, then clip.
fn reference_half(frame: &Yuv420Frame) -> Image {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    assert!(w.is_multiple_of(2) && h.is_multiple_of(2));
    let (cw, ch) = (w / 2, h / 2);
    let planes = frame.planes();
    let range = frame.range();
    let matrix = frame.matrix();
    let mut out = Image::new(cw as u32, ch as u32);
    for y in 0..ch {
        for x in 0..cw {
            let chroma = |plane: &[u8]| {
                let vertical = |xx: usize| {
                    let a = plane[y.saturating_sub(1) * cw + xx] as f32;
                    let b = plane[y * cw + xx] as f32;
                    let c = plane[(y + 1).min(ch - 1) * cw + xx] as f32;
                    ((a + c) * 0.125 + b * 0.75 - range[3]) * range[2]
                };
                (vertical(x.saturating_sub(1)) + vertical((x + 1).min(cw - 1))) * 0.125 + vertical(x) * 0.75
            };
            let (u, v) = (chroma(&planes[1]), chroma(&planes[2]));
            let i = 2 * y * w + 2 * x;
            let yy = ((planes[0][i] as f32 + planes[0][i + 1] as f32 + planes[0][i + w] as f32 + planes[0][i + w + 1] as f32) * 0.25 - range[1]) * range[0];
            out.data[y * cw + x] =
                [(yy + matrix[0] * v).clamp(0.0, 1.0), (yy - matrix[1] * v - matrix[2] * u).clamp(0.0, 1.0), (yy + matrix[3] * u).clamp(0.0, 1.0), 1.0];
        }
    }
    out
}

#[test]
fn native_reduced_video_matches_half_filter_resize_and_depth_quantization() {
    let Some(g) = native_gpu() else { return };
    assert!(g.ctx.has_kernel("video_yuv420_half"));
    for (w, h) in [(2, 2), (32, 18), (98, 62)] {
        for (kr, kb) in [(0.299, 0.114), (0.2126, 0.0722), (0.2627, 0.0593)] {
            for full in [false, true] {
                let frame = frame(w, h, kr, kb, full);
                let half = reference_half(&frame);
                for factor in [0.5, 1.0 / 3.0, 0.25, 0.125, 0.1] {
                    let size = (((w as f64 * factor).round() as u32).max(1), ((h as f64 * factor).round() as u32).max(1));
                    for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
                        let mut expected = effectcraft_raster::resample(&half, size.0, size.1);
                        let settings = effectcraft_project::ProjectSettings { bit_depth: depth, ..Default::default() };
                        effectcraft_render::color::Pipe::of(&settings).quantize(&mut expected);
                        let mut encoder = Enc::new(&g.ctx);
                        let image = crate::video::upload_reduced(&mut encoder, &frame, depth.levels(), size).unwrap().unwrap();
                        let actual = encoder.download(&image).unwrap();
                        let tolerance = depth.levels().map_or(3e-6, |levels| 1.0 / levels + 3e-6);
                        let difference = diff(&expected, &actual, tolerance);
                        assert_eq!(difference.over, 0, "{w}x{h}->{size:?} {kr}/{kb} full={full} depth={depth:?}: {difference:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn native_reduced_video_rejects_invalid_sizes_before_transfers_and_declines_odd_sources() {
    let Some(g) = native_gpu() else { return };
    let input = frame(32, 18, 0.2126, 0.0722, false);
    for size in [(0, 1), (1, 0), (17, 9), (16, 10), (u32::MAX, 1)] {
        let before = g.context().transfer_stats();
        let mut encoder = Enc::new(&g.ctx);
        assert!(crate::video::upload_reduced(&mut encoder, &input, None, size).is_err());
        assert_eq!(g.context().transfer_stats(), before);
    }
    let odd = frame(33, 19, 0.2126, 0.0722, false);
    let before = g.context().transfer_stats();
    assert!(crate::video::upload_reduced(&mut Enc::new(&g.ctx), &odd, None, (16, 9)).unwrap().is_none());
    assert_eq!(g.context().transfer_stats(), before);
}

#[test]
fn native_reduced_video_preserves_custom_source_resampling_without_opt_in() {
    struct Custom(Source);
    impl FootageSource for Custom {
        fn frame(&self, item: ItemId, f: &Footage, t: Tick) -> Option<Arc<Image>> {
            self.0.frame(item, f, t)
        }
        fn yuv420_frame_checked(&self, item: ItemId, f: &Footage, t: Tick) -> Result<Option<Arc<Yuv420Frame>>, String> {
            self.0.yuv420_frame_checked(item, f, t)
        }
    }
    let Some(g) = native_gpu() else { return };
    let s = scene_size(BitDepth::Bpc32, 100, 64);
    let source = Custom(Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false });
    let renderer = Renderer::new(&s.p, &source, RenderOpts { scale: 1.0 / 3.0, ..Default::default() });
    let expected = renderer.comp_frame_cpu_checked(s.cid, Tick::ZERO).unwrap();
    let actual = g.render(&renderer, s.cid, Tick::ZERO).unwrap();
    assert_eq!(source.0.raw_calls.load(Ordering::Relaxed), 0);
    assert_eq!(diff(&expected, &actual, 1e-3).over, 0);
}

#[test]
fn native_reduced_video_keeps_compact_source_through_transform_and_bloom() {
    use effectcraft_keyframe::Value;
    use effectcraft_render::Backend;
    let Some(g) = native_gpu() else { return };
    for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
        for scale in [0.5, 1.0 / 3.0, 0.25, 0.125] {
            for bloom in [false, true] {
                let mut s = scene_size(depth, 100, 64);
                let mut layer = s.p.comp(s.cid).unwrap().layers[0].clone();
                crate::tests::set(&mut layer, "transform/position", Value::Vec3([44.0, 28.0, 0.0]));
                crate::tests::set(&mut layer, "transform/rotation", Value::Scalar(7.0));
                crate::tests::set(&mut layer, "transform/opacity", Value::Scalar(63.0));
                if bloom {
                    s.effect(&mut layer, effectcraft_effects::bloom::ID, &[("radius", Value::Scalar(8.0)), ("threshold", Value::Scalar(0.2))]);
                }
                s.p.comp_mut(s.cid).unwrap().layers[0] = layer;
                let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
                let cpu_opts = RenderOpts { scale, backend: Backend::Cpu, ..Default::default() };
                let expected = Renderer::new(&s.p, &source, cpu_opts).comp_frame_cpu_checked(s.cid, Tick::ZERO).unwrap();
                let images = source.image_calls.load(Ordering::Relaxed);
                let transfer = g.context().transfer_stats();
                let renderer = Renderer::new(&s.p, &source, RenderOpts { backend: Backend::Gpu, ..cpu_opts }).with_accel(Some(g));
                let actual = g.render(&renderer, s.cid, Tick::ZERO).unwrap();
                assert_eq!(source.image_calls.load(Ordering::Relaxed), images, "reduced native preview must not convert source pixels on CPU");
                assert_eq!(source.raw_calls.load(Ordering::Relaxed), 1);
                let after = g.context().transfer_stats();
                assert_eq!(after.uploads - transfer.uploads, 1);
                assert_eq!(after.readbacks - transfer.readbacks, 1, "only the final test frame reads back");
                let difference = diff(&expected, &actual, crate::tests::tolerance(depth));
                assert_eq!(difference.over, 0, "scale={scale} {depth:?} bloom={bloom}: {difference:?}");
            }
        }
    }
}

#[test]
fn native_video_composites_without_float_source_copies_and_falls_back_for_interpretation() {
    let Some(g) = gpu() else { return };
    for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
        let mut s = scene(depth);
        let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
        for t in [Tick::ZERO, Tick::from_seconds_f64(1.0), Tick::ZERO] {
            let opts = RenderOpts::default();
            let cpu = Renderer::new(&s.p, &source, opts).comp_frame_cpu(s.cid, t);
            let before = source.image_calls.load(Ordering::Relaxed);
            let actual = g.render(&Renderer::new(&s.p, &source, opts), s.cid, t).unwrap();
            assert_eq!(source.image_calls.load(Ordering::Relaxed), before, "GPU must avoid CPU float source");
            assert_eq!(diff(&cpu, &actual, depth.levels().map_or(2e-6, |l| 1.0 / l + 2e-6)).over, 0);
        }
        assert_eq!(source.raw_calls.load(Ordering::Relaxed), 3);
        let item = match s.p.comp(s.cid).unwrap().layers[0].source {
            effectcraft_project::LayerSource::Footage { item } => item,
            _ => panic!("footage"),
        };
        let ItemKind::Footage(f) = &mut s.p.item_mut(item).unwrap().kind else { panic!("footage item") };
        f.invert_alpha = true;
        let before = source.raw_calls.load(Ordering::Relaxed);
        let renderer = Renderer::new(&s.p, &source, RenderOpts::default());
        let cpu = renderer.comp_frame_cpu(s.cid, Tick::ZERO);
        let actual = g.render(&renderer, s.cid, Tick::ZERO).unwrap();
        assert_eq!(source.raw_calls.load(Ordering::Relaxed), before);
        assert_eq!(diff(&cpu, &actual, 1e-3).over, 0);
    }
}

#[test]
fn failed_native_video_never_becomes_a_successful_frame() {
    let Some(g) = gpu() else { return };
    let s = scene(BitDepth::Bpc32);
    let before = s.p.clone();
    let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: true };
    let diagnostics = effectcraft_render::FrameDiagnostics::default();
    let renderer = Renderer::new(&s.p, &source, RenderOpts::default());
    let renderer = renderer.with_diagnostics(&diagnostics);
    assert!(diagnostics.result(g.render(&renderer, s.cid, Tick::ZERO)).is_err());
    assert_eq!(source.image_calls.load(Ordering::Relaxed), 0, "failed source must not silently substitute pixels");
    assert_eq!(s.p, before, "media failure must preserve the document");
    let repaired = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
    assert!(g.render(&Renderer::new(&s.p, &repaired, RenderOpts::default()), s.cid, Tick::ZERO).is_some());
}

#[test]
fn native_video_preserves_transforms_opacity_and_linear_blending() {
    let Some(g) = gpu() else { return };
    for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
        let mut s = scene(depth);
        s.p.settings.blend_linear = true;
        let layer = &mut s.p.comp_mut(s.cid).unwrap().layers[0];
        crate::tests::set(layer, "transform/position", effectcraft_keyframe::Value::Vec3([38.0, 27.0, 0.0]));
        crate::tests::set(layer, "transform/scale", effectcraft_keyframe::Value::Vec3([71.0, 83.0, 100.0]));
        crate::tests::set(layer, "transform/rotation", effectcraft_keyframe::Value::Scalar(17.0));
        crate::tests::set(layer, "transform/opacity", effectcraft_keyframe::Value::Scalar(63.0));
        let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
        let renderer = Renderer::new(&s.p, &source, RenderOpts::default());
        let cpu = renderer.comp_frame_cpu(s.cid, Tick::ZERO);
        let before = source.image_calls.load(Ordering::Relaxed);
        let actual = g.render(&renderer, s.cid, Tick::ZERO).unwrap();
        assert_eq!(source.image_calls.load(Ordering::Relaxed), before);
        assert_eq!(diff(&cpu, &actual, crate::tests::tolerance(depth)).over, 0);
    }
}

#[test]
fn native_video_defers_fields_colour_management_and_reduced_resolution() {
    let Some(g) = gpu() else { return };
    for case in 0..4 {
        let mut s = scene(BitDepth::Bpc32);
        let mut opts = RenderOpts::default();
        match case {
            0 => {
                for item in s.p.items.values_mut() {
                    if let ItemKind::Footage(f) = &mut item.kind {
                        f.fields = effectcraft_project::FieldOrder::UpperFirst;
                    }
                }
            }
            1 => {
                s.p.settings.working_space = Some(effectcraft_color::ColorSpace::Rec709);
                s.p.settings.linearize = true;
            }
            2 => opts.scale = 0.5,
            _ => s.p.comp_mut(s.cid).unwrap().layers[0].stretch = 200.0,
        }
        let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
        let renderer = Renderer::new(&s.p, &source, opts);
        let cpu = renderer.comp_frame_cpu(s.cid, Tick::ZERO);
        let actual = g.render(&renderer, s.cid, Tick::ZERO).unwrap();
        assert_eq!(source.raw_calls.load(Ordering::Relaxed), 0, "case {case} must retain ordinary source semantics");
        assert_eq!(diff(&cpu, &actual, 1e-3).over, 0, "fallback case {case}");
    }
}

#[test]
fn native_video_supports_differing_composition_and_interpreted_rates() {
    let Some(g) = gpu() else { return };
    for case in 0..2 {
        let mut s = scene(BitDepth::Bpc32);
        if case == 0 {
            s.p.comp_mut(s.cid).unwrap().frame_rate = effectcraft_time::FrameRate::FPS_60;
        } else {
            for item in s.p.items.values_mut() {
                if let ItemKind::Footage(f) = &mut item.kind {
                    f.native_rate = Some(effectcraft_time::FrameRate::FPS_60);
                }
            }
        }
        let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
        let renderer = Renderer::new(&s.p, &source, RenderOpts::default());
        for t in [Tick::ZERO, Tick::from_seconds_f64(0.5), Tick::ZERO] {
            let cpu = renderer.comp_frame_cpu(s.cid, t);
            let before = source.image_calls.load(Ordering::Relaxed);
            let actual = g.render(&renderer, s.cid, t).unwrap();
            assert_eq!(source.image_calls.load(Ordering::Relaxed), before);
            assert_eq!(diff(&cpu, &actual, 2e-6).over, 0);
        }
        assert_eq!(source.raw_calls.load(Ordering::Relaxed), 3);
    }
}

#[test]
fn native_video_effect_preflight_respects_disabled_audio_cpu_and_original_source_steps() {
    let mut s = scene(BitDepth::Bpc32);
    let mut layer = s.p.comp(s.cid).unwrap().layers[0].clone();
    s.effect(&mut layer, effectcraft_effects::bloom::ID, &[]);
    assert!(crate::video::supports_effects(&layer, crate::effects::supports));
    assert!(!crate::video::supports_effects(&layer, |_| false), "an inactive/unsupported accelerator keeps the reference path");
    layer.props.group_mut("effects/#1").unwrap().enabled = false;
    assert!(crate::video::supports_effects(&layer, |_| panic!("disabled steps must not query GPU support")));
    s.effect(&mut layer, "ec.audio.reverb", &[]);
    assert!(crate::video::supports_effects(&layer, |_| panic!("audio does not process source video")));
    s.effect(&mut layer, effectcraft_effects::paint::ID, &[]);
    assert!(!crate::video::supports_effects(&layer, crate::effects::supports));
    layer.switches.effects = false;
    assert!(crate::video::supports_effects(&layer, |_| panic!("the Effects switch disables the entire stack")));
    layer.switches.effects = true;
    layer.props.group_mut("effects/#3").unwrap().enabled = false;
    s.effect(&mut layer, "ec.channel.cccomposite", &[]);
    assert!(!crate::video::supports_effects(&layer, |_| true), "CPU original capture must not cause a native readback");
    layer.props.group_mut("effects/#4").unwrap().kind = effectcraft_project::GroupKind::Effect { effect: "org.test.unknown".into() };
    assert!(!crate::video::supports_effects(&layer, |_| true));
}

#[test]
fn native_video_effect_chains_match_cpu_without_float_source_uploads_or_internal_readbacks() {
    use effectcraft_keyframe::Value;
    use effectcraft_render::Backend;
    let Some(g) = native_gpu() else { return };
    for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
        for (quality, view, operation, mixed) in [(0, 0, 0, false), (0, 1, 0, false), (1, 2, 0, false), (2, 2, 1, false), (2, 2, 0, true)] {
            let mut s = scene(depth);
            s.p.settings.blend_linear = mixed;
            s.p.comp_mut(s.cid).unwrap().frame_rate = effectcraft_time::FrameRate::FPS_120;
            let mut layer = s.p.comp(s.cid).unwrap().layers[0].clone();
            if mixed {
                s.effect(&mut layer, "ec.channel.invert", &[("blend", Value::Scalar(40.0))]);
                s.effect(&mut layer, "ec.stylize.glow", &[("radius", Value::Scalar(4.0)), ("threshold", Value::Scalar(20.0))]);
                crate::tests::set(&mut layer, "transform/position", Value::Vec3([42.0, 28.0, 0.0]));
                crate::tests::set(&mut layer, "transform/rotation", Value::Scalar(7.0));
                crate::tests::set(&mut layer, "transform/opacity", Value::Scalar(63.0));
            }
            s.effect(
                &mut layer,
                effectcraft_effects::bloom::ID,
                &[
                    ("radius", Value::Scalar(8.0)),
                    ("threshold", Value::Scalar(0.2)),
                    ("quality", Value::Enum(quality)),
                    ("view", Value::Enum(view)),
                    ("operation", Value::Enum(operation)),
                ],
            );
            s.p.comp_mut(s.cid).unwrap().layers[0] = layer;
            let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
            for t in [Tick::ZERO, effectcraft_time::FrameRate::FPS_120.frame_duration(), Tick::ZERO] {
                let expected =
                    Renderer::new(&s.p, &source, RenderOpts { backend: Backend::Cpu, ..Default::default() }).comp_frame_cpu_checked(s.cid, t).unwrap();
                let before_images = source.image_calls.load(Ordering::Relaxed);
                let before = g.context().transfer_stats();
                let renderer = Renderer::new(&s.p, &source, RenderOpts { backend: Backend::Gpu, ..Default::default() }).with_accel(Some(g));
                let actual = g.render(&renderer, s.cid, t).unwrap();
                let after = g.context().transfer_stats();
                assert_eq!(source.image_calls.load(Ordering::Relaxed), before_images, "native input must bypass float source conversion");
                assert_eq!(after.uploads - before.uploads, 1, "only the compact YUV source uploads");
                let native = frame(97, 61, 0.2126, 0.0722, t != Tick::ZERO);
                let bytes: usize = native.plane_lengths().iter().map(|n| n.div_ceil(4) * 4).sum();
                assert_eq!(after.upload_bytes - before.upload_bytes, bytes as u64);
                assert_eq!(after.readbacks - before.readbacks, 1, "only the completed frame reads back");
                let difference = diff(&expected, &actual, crate::tests::tolerance(depth));
                assert_eq!(difference.over, 0, "{depth:?} quality={quality} view={view} operation={operation} mixed={mixed}: {difference:?}");
            }
            assert_eq!(source.raw_calls.load(Ordering::Relaxed), 3);
        }
    }
}

#[test]
fn native_video_resident_only_target_declines_cpu_without_callbacks_or_transfers() {
    use effectcraft_render::FxTarget;
    let Some(g) = native_gpu() else { return };
    for checked in [false, true] {
        let mut e = Enc::new(&g.ctx);
        let input = crate::video::upload(&mut e, &frame(9, 5, 0.2126, 0.0722, false), None).unwrap().unwrap();
        let before = g.context().transfer_stats();
        let mut target = crate::effects::GpuFx::new_resident(&mut e, input, [0.0; 2], 1.0, None);
        if checked {
            assert!(!target.cpu_checked(&mut |_| panic!("declined native stage must not call CPU renderer")).unwrap());
        } else {
            target.cpu(&mut |_| panic!("declined native stage must not call CPU renderer"));
        }
        assert!(target.finish().is_none(), "a partial source cannot be a successful resident result");
        assert_eq!(g.context().transfer_stats(), before);
    }
}

#[test]
fn native_video_effect_fallbacks_keep_masks_styles_cpu_controls_and_original_source_semantics() {
    use effectcraft_keyframe::{ShapePath, Value};
    use effectcraft_project::{MaskMode, build};
    use effectcraft_render::Backend;
    let Some(g) = native_gpu() else { return };
    for case in 0..6 {
        let mut s = scene(BitDepth::Bpc32);
        let mut layer = s.p.comp(s.cid).unwrap().layers[0].clone();
        match case {
            0 => s.effect(&mut layer, effectcraft_effects::paint::ID, &[]),
            1 => s.effect(&mut layer, "ec.channel.cccomposite", &[]),
            2 => {
                let mask =
                    build::mask(&mut build::Ids(&mut s.p.next_id), "Original mask", ShapePath::ellipse([48.0, 30.0], 65.0, 43.0), MaskMode::Add, [255; 3]);
                layer.props.sub_mut("masks").unwrap().children.push(mask.into());
            }
            3 => {
                effectcraft_project::styles::add_style(&mut layer, &mut build::Ids(&mut s.p.next_id), "colorOverlay", &s.comp.global_light, true).unwrap();
            }
            4 => s.effect(&mut layer, "ec.generate.cellpattern", &[("cellPattern", Value::Enum(6))]),
            _ => {}
        }
        s.effect(&mut layer, effectcraft_effects::bloom::ID, &[("radius", Value::Scalar(4.0)), ("threshold", Value::Scalar(0.2))]);
        s.p.comp_mut(s.cid).unwrap().layers[0] = layer;
        let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
        let expected =
            Renderer::new(&s.p, &source, RenderOpts { backend: Backend::Cpu, ..Default::default() }).comp_frame_cpu_checked(s.cid, Tick::ZERO).unwrap();
        let before_images = source.image_calls.load(Ordering::Relaxed);
        let before = g.context().transfer_stats();
        let mut renderer = Renderer::new(&s.p, &source, RenderOpts { backend: Backend::Gpu, ..Default::default() });
        if case != 5 {
            renderer.accel = Some(g);
        }
        let actual = g.render(&renderer, s.cid, Tick::ZERO).unwrap();
        assert!(source.image_calls.load(Ordering::Relaxed) > before_images, "case {case} keeps reference source semantics");
        assert_eq!(source.raw_calls.load(Ordering::Relaxed), usize::from(case == 4), "only dynamic control decline speculates a native upload");
        if case == 4 {
            assert_eq!(
                g.context().transfer_stats().readbacks - before.readbacks,
                2,
                "reference Bloom uses its ordinary completed source readback plus final frame"
            );
        }
        assert_eq!(diff(&expected, &actual, 1e-3).over, 0, "fallback case {case}");
    }
}

#[test]
fn native_video_bloom_display_deferred_completion_and_failed_controls_keep_checked_contract() {
    use effectcraft_keyframe::Value;
    use effectcraft_render::{Backend, FrameDiagnostics, LayerCache};
    let Some(g) = native_gpu() else { return };
    let mut s = scene(BitDepth::Bpc32);
    let mut layer = s.p.comp(s.cid).unwrap().layers[0].clone();
    s.effect(&mut layer, effectcraft_effects::bloom::ID, &[("radius", Value::Scalar(8.0)), ("threshold", Value::Scalar(0.2))]);
    let layer_id = layer.id;
    let uid = layer.effects().unwrap().groups().next().unwrap().uid;
    s.p.comp_mut(s.cid).unwrap().layers[0] = layer;
    let source = Source { raw_calls: AtomicUsize::new(0), image_calls: AtomicUsize::new(0), fail: false };
    let cache = LayerCache::default();
    let opts = RenderOpts { backend: Backend::Gpu, ..Default::default() };
    let before = g.context().transfer_stats();
    let renderer = Renderer::new(&s.p, &source, opts).with_accel(Some(g));
    assert!(g.render_display(&renderer, s.cid, Tick::ZERO).is_some());
    assert_eq!(g.context().transfer_stats().readbacks, before.readbacks, "native display effects need no float readback");
    assert_eq!(source.image_calls.load(Ordering::Relaxed), 0);

    let deferred = crate::tests_deferred::deferred_gpu();
    if std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1") {
        assert!(deferred.is_some(), "native checked export passes require a deferred device");
    }
    let Some((deferred, _guard)) = deferred else { return };
    let expected = Renderer::new(&s.p, &source, RenderOpts { backend: Backend::Cpu, ..Default::default() }).comp_frame_cpu_checked(s.cid, Tick::ZERO).unwrap();
    let before_images = source.image_calls.load(Ordering::Relaxed);
    let before = deferred.context().transfer_stats();
    cache.set_gate(deferred.miss_gate());
    let mut renderer = Renderer::new(&s.p, &source, opts).with_accel(Some(deferred));
    renderer.cache = Some(&cache);
    let actual = effectcraft_render::passes::block_on(effectcraft_render::passes::comp_frame_checked(&renderer, s.cid, Tick::ZERO)).unwrap();
    assert_eq!(source.image_calls.load(Ordering::Relaxed), before_images);
    assert_eq!(deferred.context().transfer_stats().readbacks - before.readbacks, 1, "only final frame completion is deferred");
    assert_eq!(diff(&expected, &actual, 1e-3).over, 0);

    s.p.comp_mut(s.cid).unwrap().layer_mut(layer_id).unwrap().props.prop_mut("effects/#1/radius").unwrap().value = Value::Scalar(f64::NAN);
    let before_project = s.p.clone();
    cache.clear();
    let diagnostics = FrameDiagnostics::default();
    let renderer = Renderer::new(&s.p, &source, opts).with_accel(Some(g));
    let mut renderer = renderer.with_diagnostics(&diagnostics);
    renderer.cache = Some(&cache);
    let before_images = source.image_calls.load(Ordering::Relaxed);
    let error = diagnostics.result(g.render(&renderer, s.cid, Tick::ZERO)).unwrap_err();
    assert_eq!((error.comp, error.layer, error.effect_uid, error.effect_id.as_str()), (s.cid, layer_id, uid, effectcraft_effects::bloom::ID));
    assert!(error.message.contains("radius"));
    assert_eq!(source.image_calls.load(Ordering::Relaxed), before_images, "a checked native failure cannot silently substitute CPU pixels");
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(s.p, before_project);
    s.p.comp_mut(s.cid).unwrap().layer_mut(layer_id).unwrap().props.prop_mut("effects/#1/radius").unwrap().value = Value::Scalar(8.0);
    assert!(g.render(&Renderer::new(&s.p, &source, opts).with_accel(Some(g)), s.cid, Tick::ZERO).is_some());
}

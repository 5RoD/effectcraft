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
use effectcraft_render::{FootageSource, RenderOpts, Renderer, video::Yuv420Frame};
use effectcraft_time::Tick;

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

struct Source {
    raw_calls: AtomicUsize,
    image_calls: AtomicUsize,
    fail: bool,
}
impl FootageSource for Source {
    fn frame(&self, _: ItemId, f: &Footage, t: Tick) -> Option<Arc<Image>> {
        self.image_calls.fetch_add(1, Ordering::Relaxed);
        Some(Arc::new(reference(&frame(f.width, f.height, 0.2126, 0.0722, t != Tick::ZERO))))
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
    let mut s = Scene::new(depth);
    let layer = s.footage(97, 61);
    let effectcraft_project::LayerSource::Footage { item } = layer.source else { panic!("footage") };
    let ItemKind::Footage(f) = &mut s.p.item_mut(item).unwrap().kind else { panic!("footage item") };
    f.kind = FootageKind::Video;
    f.duration = Tick::from_seconds_f64(2.0);
    s.push(layer);
    s
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

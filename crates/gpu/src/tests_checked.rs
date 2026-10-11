//! Registered checked effect failures/fallbacks never escape as successful provisional frames.
use crate::tests::{Pattern, Scene};
use effectcraft_keyframe::Value;
use effectcraft_project::BitDepth;
use effectcraft_render::{Accelerator, Backend, LayerCache, RenderOpts, Renderer};
use effectcraft_time::Tick;

#[test]
fn checked_registered_plugins_fail_visibly_in_gpu_and_deferred_stacks_and_recover() {
    use effectcraft_effects::plugin::{EffectPlugin, PluginFrame, PluginManifest, PluginParam, PluginParamKind, PluginParams, register_plugin};
    struct Fault(PluginManifest);
    impl EffectPlugin for Fault {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn render(&self, frame: &mut PluginFrame, params: &PluginParams, _time: f64) -> Result<(), String> {
            match params.f("mode") as u32 {
                1 => {
                    frame.pixels[0] = [0.0; 4];
                    Err("synthetic GPU plug-in error".into())
                }
                2 => {
                    frame.pixels[0] = [0.0; 4];
                    panic!("synthetic GPU plug-in panic")
                }
                _ => Ok(()),
            }
        }
    }
    let spec = register_plugin(std::sync::Arc::new(Fault(PluginManifest {
        api: 1,
        id: "org.test.gpu-checked-plugin".into(),
        name: "Checked GPU Plug-in".into(),
        category: "Test".into(),
        version: String::new(),
        author: String::new(),
        description: String::new(),
        params: vec![PluginParam { id: "mode".into(), name: "Mode".into(), kind: PluginParamKind::Angle { default: 0.0 } }],
    })))
    .unwrap();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    let blocking = crate::tests::gpu();
    let deferred = crate::tests_deferred::deferred_gpu();
    assert!(!required || (blocking.is_some() && deferred.is_some()), "registered plug-in acceptance requires Vulkan and deferred GPU devices");
    let (Some(blocking), Some((deferred, _guard))) = (blocking, deferred) else { return };
    eprintln!("checked registered plug-in adapter: {}", blocking.name());
    for gpu in [blocking, deferred] {
        for adjustment in [false, true] {
            let mut scene = Scene::new(BitDepth::Bpc32);
            let background = scene.solid([0.8, 0.6, 0.2], 97, 61);
            scene.push(background);
            let mut layer = scene.solid([0.2, 0.4, 0.8], 97, 61);
            layer.switches.adjustment = adjustment;
            scene.effect(&mut layer, spec.id, &[]);
            let lid = scene.push(layer);
            let cache = LayerCache::default();
            cache.set_gate(gpu.miss_gate());
            let opts = RenderOpts { backend: Backend::Gpu, ..Default::default() };
            let expected = Renderer::new(&scene.p, &Pattern, opts).comp_frame_cpu_checked(scene.cid, Tick::ZERO).unwrap();
            for (mode, reason) in [(1.0, "synthetic GPU plug-in error"), (2.0, "Rust plug-in panicked")] {
                scene.p.comp_mut(scene.cid).unwrap().layer_mut(lid).unwrap().props.prop_mut("effects/#1/mode").unwrap().value = Value::Scalar(mode);
                let before = scene.p.clone();
                let renderer = checked_renderer(&scene.p, gpu, &cache, opts);
                let error = effectcraft_render::passes::block_on(effectcraft_render::passes::comp_frame_checked(&renderer, scene.cid, Tick::ZERO)).unwrap_err();
                assert_eq!((error.layer, error.effect_id.as_str()), (lid, spec.id));
                assert!(error.message.contains(reason), "{error}");
                assert_eq!(scene.p, before);
            }
            scene.p.comp_mut(scene.cid).unwrap().layer_mut(lid).unwrap().props.prop_mut("effects/#1/mode").unwrap().value = Value::Scalar(0.0);
            let renderer = checked_renderer(&scene.p, gpu, &cache, opts);
            let actual = effectcraft_render::passes::block_on(effectcraft_render::passes::comp_frame_checked(&renderer, scene.cid, Tick::ZERO)).unwrap();
            assert_eq!(crate::tests::diff(&actual, &expected, 1e-3).over, 0, "repair must not reuse failed pixels");
        }
    }
}

#[test]
fn checked_bloom_resident_adjustment_and_deferred_completion_are_explicit() {
    let gpu = crate::tests::gpu();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "Bloom integration requires a working GPU");
    let Some(gpu) = gpu else { return };
    let mut scene = Scene::new(BitDepth::Bpc32);
    let background = scene.footage(97, 61);
    scene.push(background);
    let mut adjustment = scene.solid([1.0; 3], 97, 61);
    adjustment.switches.adjustment = true;
    scene.effect(&mut adjustment, effectcraft_effects::bloom::ID, &[("radius", Value::Scalar(8.0)), ("threshold", Value::Scalar(0.0))]);
    let layer = scene.push(adjustment);
    let reference = Renderer::new(&scene.p, &Pattern, RenderOpts::default()).comp_frame_checked(scene.cid, Tick::ZERO).unwrap();
    let cache = LayerCache::default();
    let mut renderer = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Gpu, ..Default::default() });
    renderer.accel = Some(gpu);
    renderer.cache = Some(&cache);
    let before = gpu.context().transfer_stats();
    let frame = renderer.comp_frame_attempt(scene.cid, Tick::ZERO).unwrap();
    assert!(!frame.cpu_retry, "a valid resident effect must actually run on the GPU");
    let actual = frame.image;
    assert_eq!(gpu.context().transfer_stats().readbacks - before.readbacks, 1, "only the final frame reads back; Bloom remains resident");
    let difference = crate::tests::diff(&reference, &actual, 1e-3);
    assert_eq!(difference.over, 0, "checked resident adjustment vs CPU: {difference:?}");
    // Resident adjustments also run on a browser-style device: only the final frame is
    // deferred. No phantom readback, placeholder cache entry or complete CPU retry.
    let deferred = crate::tests_deferred::deferred_gpu();
    assert!(!required || deferred.is_some(), "Bloom integration requires deferred GPU mode too");
    let Some((deferred, _guard)) = deferred else { return };
    cache.clear();
    cache.set_gate(deferred.miss_gate());
    renderer.accel = Some(deferred);
    let before = deferred.context().transfer_stats();
    let (frame, passes) = checked_passes(deferred, &renderer, scene.cid);
    assert_eq!(passes, 2, "resident adjustment only needs the final frame readback");
    assert_eq!(deferred.context().transfer_stats().readbacks - before.readbacks, 1);
    assert_eq!(crate::tests::diff(&frame, &reference, 1e-3).over, 0);
    let exported = effectcraft_render::passes::block_on(effectcraft_render::passes::comp_frame_checked(&renderer, scene.cid, Tick::ZERO)).unwrap();
    assert_eq!(crate::tests::diff(&exported, &reference, 1e-3).over, 0);
    // A malformed evaluated control fails both attempts and returns contextual failure.
    scene.p.comp_mut(scene.cid).unwrap().layer_mut(layer).unwrap().props.prop_mut("effects/#1/radius").unwrap().value = Value::Scalar(f64::MAX);
    let mut invalid = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Gpu, ..Default::default() });
    invalid.accel = Some(deferred);
    deferred.frame_begin();
    deferred.pass_begin();
    let error = invalid.comp_frame_checked(scene.cid, Tick::ZERO).unwrap_err();
    assert_eq!((error.comp, error.layer), (scene.cid, layer));
    assert_eq!(error.effect_id, effectcraft_effects::bloom::ID);
}

fn checked_passes(gpu: &crate::Gpu, renderer: &Renderer, comp: effectcraft_project::ItemId) -> (effectcraft_raster::Image, usize) {
    gpu.frame_begin();
    for pass in 1..=12 {
        gpu.pass_begin();
        let frame = renderer.comp_frame_attempt(comp, Tick::ZERO).unwrap();
        assert!(!frame.cpu_retry, "valid Bloom must finish through GPU passes");
        if !gpu.pass_missed() {
            return (frame.image, pass);
        }
        effectcraft_render::passes::block_on(gpu.settle().unwrap());
    }
    panic!("checked GPU passes never completed");
}

fn checked_renderer<'a>(project: &'a effectcraft_project::Project, gpu: &'a crate::Gpu, cache: &'a LayerCache, options: RenderOpts) -> Renderer<'a> {
    let mut renderer = Renderer::new(project, &Pattern, options);
    renderer.accel = Some(gpu);
    renderer.cache = Some(cache);
    renderer
}

#[test]
fn checked_bloom_ordinary_mixed_stacks_deferred_cache_and_repair_match_cpu() {
    let gpu = crate::tests_deferred::deferred_gpu();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "resident acceptance requires the deferred Vulkan adapter");
    let Some((gpu, _guard)) = gpu else { return };
    assert!(gpu.supports_effect(effectcraft_effects::bloom::ID));
    for (depth, scale) in [(BitDepth::Bpc32, 1.0), (BitDepth::Bpc32, 0.5), (BitDepth::Bpc16, 0.5), (BitDepth::Bpc8, 1.0)] {
        let mut scene = Scene::new(depth);
        let mut layer = scene.footage(65, 37);
        scene.effect(&mut layer, "ec.blur.gaussian", &[("blurriness", Value::Scalar(2.0))]);
        scene.effect(
            &mut layer,
            effectcraft_effects::bloom::ID,
            &[("radius", Value::Scalar(8.0)), ("threshold", Value::Scalar(0.0)), ("operation", Value::Enum(1))],
        );
        scene.effect(&mut layer, "ec.channel.invert", &[("blend", Value::Scalar(40.0))]);
        crate::tests::set(&mut layer, "transform/rotation", Value::Scalar(8.0));
        let layer = scene.push(layer);
        let options = RenderOpts { backend: Backend::Gpu, scale, roi: Some([0.0, 0.0, 97.0, 61.0]), ..Default::default() };
        let cache = LayerCache::default();
        cache.set_gate(gpu.miss_gate());
        let reference = Renderer::new(&scene.p, &Pattern, options).comp_frame_cpu_checked(scene.cid, Tick::ZERO).unwrap();
        let (actual, first_passes) = checked_passes(gpu, &checked_renderer(&scene.p, gpu, &cache, options), scene.cid);
        assert!(first_passes >= 2, "checked source output must use actual keyed readbacks");
        let tolerance = crate::tests::tolerance(depth) * 2.0;
        assert_eq!(crate::tests::diff(&actual, &reference, tolerance).over, 0, "{depth:?} scale {scale}");
        let (warm, warm_passes) = checked_passes(gpu, &checked_renderer(&scene.p, gpu, &cache, options), scene.cid);
        assert_eq!(warm.data, actual.data);
        assert!(warm_passes <= first_passes);
        scene.p.comp_mut(scene.cid).unwrap().layer_mut(layer).unwrap().props.prop_mut("effects/#2/radius").unwrap().value = Value::Scalar(f64::MAX);
        gpu.frame_begin();
        gpu.pass_begin();
        let error = checked_renderer(&scene.p, gpu, &cache, options).comp_frame_checked(scene.cid, Tick::ZERO).unwrap_err();
        assert_eq!((error.layer, error.effect_id.as_str()), (layer, effectcraft_effects::bloom::ID));
        // Repair uses the same cache: no failed output or deferred placeholder may survive.
        scene.p.comp_mut(scene.cid).unwrap().layer_mut(layer).unwrap().props.prop_mut("effects/#2/radius").unwrap().value = Value::Scalar(4.0);
        let expected = Renderer::new(&scene.p, &Pattern, options).comp_frame_cpu_checked(scene.cid, Tick::ZERO).unwrap();
        let (repaired, _) = checked_passes(gpu, &checked_renderer(&scene.p, gpu, &cache, options), scene.cid);
        assert_eq!(crate::tests::diff(&repaired, &expected, tolerance).over, 0);
    }
}

#[test]
fn checked_raster_gpu_frame_and_display_reject_hostile_geometry_before_transfers() {
    let gpu = crate::tests::gpu();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "raster acceptance requires the Vulkan adapter");
    let Some(gpu) = gpu else { return };
    let scene = Scene::new(BitDepth::Bpc32);
    let mut options: Vec<_> = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0, f64::MAX]
        .into_iter()
        .map(|scale| RenderOpts { backend: Backend::Gpu, scale, ..Default::default() })
        .collect();
    options.extend(
        [
            [0.0, 0.0, f64::from(u32::MAX), f64::from(u32::MAX)],
            [f64::NAN, 0.0, 8.0, 8.0],
            [0.0, f64::INFINITY, 8.0, 8.0],
            [f64::MAX, 0.0, 8.0, 8.0],
            [0.0, 0.0, -1.0, 8.0],
        ]
        .map(|roi| RenderOpts { backend: Backend::Gpu, roi: Some(roi), ..Default::default() }),
    );
    for opts in options {
        assert_raster_rejected(gpu, &scene, opts);
    }
    let mut oversized = Scene::new(BitDepth::Bpc32);
    oversized.p.comp_mut(oversized.cid).unwrap().width = u32::MAX;
    oversized.p.comp_mut(oversized.cid).unwrap().height = u32::MAX;
    assert_raster_rejected(gpu, &oversized, RenderOpts { backend: Backend::Gpu, ..Default::default() });
}

fn assert_raster_rejected(gpu: &crate::Gpu, scene: &Scene, opts: RenderOpts) {
    let cache = LayerCache::default();
    let diagnostics = effectcraft_render::FrameDiagnostics::default();
    let mut renderer = checked_renderer(&scene.p, gpu, &cache, opts);
    renderer.diagnostics = Some(&diagnostics);
    let before = gpu.context().transfer_stats();
    assert!(gpu.render_display(&renderer, scene.cid, Tick::ZERO).is_none());
    assert!(gpu.render(&renderer, scene.cid, Tick::ZERO).is_none());
    let error = diagnostics.result(()).unwrap_err();
    assert_eq!((error.comp, error.layer.0, error.effect_id.as_str()), (scene.cid, 0, "render.raster"));
    assert_eq!(renderer.comp_frame_checked(scene.cid, Tick::ZERO).unwrap_err().effect_id, "render.raster");
    let after = gpu.context().transfer_stats();
    assert_eq!((before.uploads, before.readbacks), (after.uploads, after.readbacks));
    assert_eq!(cache.stats().entries, 0);
}

#[test]
fn checked_raster_gpu_solid_failure_and_valid_scaled_frames_match_cpu() {
    use effectcraft_project::{ItemKind, LayerSource};
    let gpu = crate::tests::gpu();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "raster acceptance requires the Vulkan adapter");
    let Some(gpu) = gpu else { return };
    for adjustment in [false, true] {
        let mut scene = Scene::new(BitDepth::Bpc32);
        let mut layer = scene.solid([0.25, 0.5, 1.0], 97, 61);
        layer.switches.adjustment = adjustment;
        let LayerSource::Solid { item } = layer.source else { panic!("solid fixture") };
        let lid = scene.push(layer);
        let ItemKind::Solid(solid) = &mut scene.p.item_mut(item).unwrap().kind else { panic!("solid fixture") };
        solid.width = u32::MAX;
        solid.height = u32::MAX;
        let cache = LayerCache::default();
        let renderer = checked_renderer(&scene.p, gpu, &cache, RenderOpts { backend: Backend::Gpu, ..Default::default() });
        let error = renderer.comp_frame_checked(scene.cid, Tick::ZERO).unwrap_err();
        assert_eq!((error.comp, error.layer, error.effect_id.as_str()), (scene.cid, lid, "render.raster"));
        assert_eq!(cache.stats().entries, 0);
    }
    let mut scene = Scene::new(BitDepth::Bpc32);
    let layer = scene.solid([0.25, 0.5, 1.0], 97, 61);
    scene.push(layer);
    let cache = LayerCache::default();
    for scale in [0.5, 1.0, 2.0, 4.0] {
        let opts = RenderOpts { backend: Backend::Gpu, scale, ..Default::default() };
        let reference = Renderer::new(&scene.p, &Pattern, opts).comp_frame_cpu_checked(scene.cid, Tick::ZERO).unwrap();
        let frame = checked_renderer(&scene.p, gpu, &cache, opts).comp_frame_attempt(scene.cid, Tick::ZERO).unwrap();
        assert!(!frame.cpu_retry);
        assert_eq!(crate::tests::diff(&frame.image, &reference, 1e-6).over, 0, "scale {scale}");
    }
}

#[test]
fn checked_raster_deferred_ready_frame_cannot_bypass_current_document_bounds() {
    let gpu = crate::tests_deferred::deferred_gpu();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "raster acceptance requires deferred Vulkan mode");
    let Some((gpu, _guard)) = gpu else { return };
    let mut scene = Scene::new(BitDepth::Bpc32);
    let cache = LayerCache::default();
    cache.set_gate(gpu.miss_gate());
    let opts = RenderOpts { backend: Backend::Gpu, ..Default::default() };
    let (_, passes) = checked_passes(gpu, &checked_renderer(&scene.p, gpu, &cache, opts), scene.cid);
    assert_eq!(passes, 2, "the final ready readback is present");
    // Keep the same comp/time/options ready key, without frame_begin clearing it.
    scene.p.comp_mut(scene.cid).unwrap().width = u32::MAX;
    scene.p.comp_mut(scene.cid).unwrap().height = u32::MAX;
    let diagnostics = effectcraft_render::FrameDiagnostics::default();
    let mut renderer = checked_renderer(&scene.p, gpu, &cache, opts);
    renderer.diagnostics = Some(&diagnostics);
    let before = gpu.context().transfer_stats();
    assert!(gpu.render(&renderer, scene.cid, Tick::ZERO).is_none());
    assert_eq!(diagnostics.result(()).unwrap_err().effect_id, "render.raster");
    let after = gpu.context().transfer_stats();
    assert_eq!((before.uploads, before.readbacks), (after.uploads, after.readbacks));
}

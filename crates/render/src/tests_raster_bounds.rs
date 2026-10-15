//! Malformed document/render dimensions must fail before the targeted allocation.
use effectcraft_color::Label;
use effectcraft_project::{Comp, ItemId, ItemKind, LayerId, LayerSource, Project, Solid, build};
use effectcraft_time::{FrameRate, Tick};

use crate::{FrameDiagnostics, Image, LayerCache, NoFootage, RenderOpts, Renderer, raster_bounds};

fn scene() -> (Project, ItemId, LayerId, ItemId) {
    let mut project = Project::default();
    project.settings.bit_depth = effectcraft_project::BitDepth::Bpc32;
    let comp = Comp::new(24, 16, FrameRate::FPS_30, Tick::from_seconds_f64(1.0));
    let cid = project.add_item("Small", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
    let sid = project.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color: [0.25, 0.5, 1.0], width: 24, height: 16, pixel_aspect: 1.0 }));
    let layer = build::layer(&mut project, &comp, "Solid", LayerSource::Solid { item: sid }, (24, 16), None);
    let lid = layer.id;
    project.comp_mut(cid).unwrap().layers.push(layer);
    (project, cid, lid, sid)
}

fn hostile_solid(project: &mut Project, item: ItemId) {
    let ItemKind::Solid(solid) = &mut project.item_mut(item).unwrap().kind else { panic!("solid fixture") };
    solid.width = u32::MAX;
    solid.height = u32::MAX;
}

#[test]
fn checked_raster_capacity_overflow_returns_context_and_preserves_the_document() {
    // On 64-bit, the old constructor's element count fits usize, but its byte capacity
    // overflows Vec's address space. This panics before malloc, not an OOM experiment.
    #[cfg(target_pointer_width = "64")]
    assert!(std::panic::catch_unwind(|| Image::new(u32::MAX, u32::MAX)).is_err());
    let (mut project, cid, _, _) = scene();
    project.comp_mut(cid).unwrap().width = u32::MAX;
    project.comp_mut(cid).unwrap().height = u32::MAX;
    let before = project.clone();
    let cache = LayerCache::default();
    let mut renderer = Renderer::new(&project, &NoFootage, RenderOpts::default());
    renderer.cache = Some(&cache);
    let error = renderer.comp_frame_checked(cid, Tick(37)).unwrap_err();
    assert_eq!((error.comp, error.layer, error.effect_id.as_str(), error.time), (cid, LayerId(0), "render.raster", Tick(37)));
    assert!(error.to_string().starts_with("Render failed in comp "));
    assert!(error.to_string().contains("raster geometry"));
    assert_eq!(project, before);
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(renderer.comp_frame_cpu_checked(cid, Tick::ZERO).unwrap_err().effect_id, "render.raster");
}

#[test]
fn checked_raster_hostile_options_are_terminal_and_do_not_poison_a_warm_cache() {
    let (project, cid, _, _) = scene();
    let cache = LayerCache::default();
    let mut valid = Renderer::new(&project, &NoFootage, RenderOpts::default());
    valid.cache = Some(&cache);
    let expected = valid.comp_frame_checked(cid, Tick::ZERO).unwrap();
    let entries = cache.stats().entries;
    assert!(entries > 0);
    let mut options: Vec<_> =
        [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0, f64::MAX].into_iter().map(|scale| RenderOpts { scale, ..Default::default() }).collect();
    options.extend(
        [
            [0.0, 0.0, f64::MAX, f64::MAX],
            [0.0, 0.0, f64::from(u32::MAX), f64::from(u32::MAX)],
            [f64::NAN, 0.0, 8.0, 8.0],
            [0.0, f64::INFINITY, 8.0, 8.0],
            [f64::MAX, 0.0, 8.0, 8.0],
            [0.0, 0.0, 0.0, 8.0],
            [0.0, 0.0, -1.0, 8.0],
        ]
        .map(|roi| RenderOpts { roi: Some(roi), ..Default::default() }),
    );
    for opts in options {
        let mut invalid = Renderer::new(&project, &NoFootage, opts);
        invalid.cache = Some(&cache);
        assert_eq!(invalid.comp_frame_checked(cid, Tick::ZERO).unwrap_err().effect_id, "render.raster");
        assert_eq!(cache.stats().entries, entries);
    }
    let repaired = valid.comp_frame_checked(cid, Tick::ZERO).unwrap();
    assert_eq!(repaired.data, expected.data);
}

#[test]
fn checked_raster_solid_sources_adjustments_and_cached_inputs_reject_capacity_overflow() {
    for adjustment in [false, true] {
        let (mut project, cid, lid, sid) = scene();
        hostile_solid(&mut project, sid);
        project.comp_mut(cid).unwrap().layer_mut(lid).unwrap().switches.adjustment = adjustment;
        let cache = LayerCache::default();
        let diagnostics = FrameDiagnostics::default();
        let mut renderer = Renderer::new(&project, &NoFootage, RenderOpts::default());
        renderer.diagnostics = Some(&diagnostics);
        renderer.cache = Some(&cache);
        let ctx = renderer.eval_ctx(cid, Tick::ZERO).unwrap();
        let layer = ctx.layer(lid).unwrap();
        assert!(renderer.layer_source(&ctx, layer).is_none());
        assert!(renderer.content_buf(&ctx, layer).is_none());
        assert!(renderer.layer_input(&ctx, layer, 0).is_none());
        assert!(renderer.adjustment_footprint(&ctx, layer).is_none());
        let error = diagnostics.result(()).unwrap_err();
        assert_eq!((error.comp, error.layer, error.effect_id.as_str()), (cid, lid, "render.raster"));
        assert!(error.message.contains("solid source"));
        assert_eq!(cache.stats().entries, 0);
        let error = Renderer::new(&project, &NoFootage, RenderOpts::default()).comp_frame_checked(cid, Tick::ZERO).unwrap_err();
        assert_eq!(error.layer, lid);
    }
}

#[test]
fn checked_raster_nested_and_collapsed_sources_keep_the_failing_comp_context() {
    for collapse in [false, true] {
        for invalid_comp in [false, true] {
            let (mut project, inner, lid, sid) = scene();
            if invalid_comp {
                project.comp_mut(inner).unwrap().width = u32::MAX;
                project.comp_mut(inner).unwrap().height = u32::MAX;
            } else {
                hostile_solid(&mut project, sid);
            }
            let outer = Comp::new(24, 16, FrameRate::FPS_30, Tick::from_seconds_f64(1.0));
            let cid = project.add_item("Outer", Label::Sandstone, None, ItemKind::Comp(outer.clone().into()));
            let mut nested = build::layer(&mut project, &outer, "Nested", LayerSource::Comp { item: inner }, (24, 16), None);
            nested.switches.collapse = collapse;
            project.comp_mut(cid).unwrap().layers.push(nested);
            let error = Renderer::new(&project, &NoFootage, RenderOpts::default()).comp_frame_checked(cid, Tick::ZERO).unwrap_err();
            assert_eq!(error.comp, inner, "collapse {collapse}, invalid comp {invalid_comp}");
            assert_eq!(error.layer, if invalid_comp { LayerId(0) } else { lid });
        }
    }
    // Preserve Resolution validates the actual nested scale, even if the outer scale fits.
    let (mut project, inner, _, _) = scene();
    project.comp_mut(inner).unwrap().width = raster_bounds::MAX_DIMENSION + 1;
    project.comp_mut(inner).unwrap().preserve_resolution = true;
    let outer = Comp::new(24, 16, FrameRate::FPS_30, Tick::from_seconds_f64(1.0));
    let cid = project.add_item("Outer", Label::Sandstone, None, ItemKind::Comp(outer.clone().into()));
    let nested = build::layer(&mut project, &outer, "Nested", LayerSource::Comp { item: inner }, (24, 16), None);
    project.comp_mut(cid).unwrap().layers.push(nested);
    let error = Renderer::new(&project, &NoFootage, RenderOpts { scale: 0.5, ..Default::default() }).comp_frame_checked(cid, Tick::ZERO).unwrap_err();
    assert_eq!(error.comp, inner);
    // An enabled proxy bypasses Preserve Resolution, as the existing source path does.
    // Missing decoded proxy pixels remain transparent; they do not force a full-size render.
    project.item_mut(inner).unwrap().proxy = Some(Box::new(effectcraft_project::Proxy {
        enabled: true,
        footage: effectcraft_project::Footage { width: 8, height: 8, has_video: true, ..Default::default() },
    }));
    assert!(Renderer::new(&project, &NoFootage, RenderOpts { scale: 0.5, ..Default::default() }).comp_frame_checked(cid, Tick::ZERO).is_ok());
}

#[test]
fn checked_raster_valid_frames_keep_exact_pixels_across_output_scales() {
    let (project, cid, _, _) = scene();
    for scale in [0.25, 0.5, 1.0, 2.0, 4.0] {
        let renderer = Renderer::new(&project, &NoFootage, RenderOpts { scale, ..Default::default() });
        let checked = renderer.comp_frame_cpu_checked(cid, Tick::ZERO).unwrap();
        let legacy = renderer.comp_frame_cpu(cid, Tick::ZERO);
        assert_eq!(checked.data, legacy.data);
        assert_eq!([checked.width, checked.height], [(24.0 * scale).round() as u32, (16.0 * scale).round() as u32]);
        assert!(checked.data.iter().all(|pixel| pixel == &[0.25, 0.5, 1.0, 1.0]));
    }
}

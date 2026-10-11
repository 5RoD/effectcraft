//! Checked GPU Native Bloom. Shares every grid/radius/weight/budget with the
//! CPU core; immutable caller input is never used as mutable scratch.

use effectcraft_effects::bloom::{BloomError, BloomPlan, plan, validate_input, validate_offset};
use effectcraft_effects::{Buf, EffectCtx};

use crate::Gpu;
use crate::context::{Enc, GpuImage, Params};
use crate::effects::GBuf;

pub(crate) const KERNELS: &[&str] = &["bloom_extract", "bloom_reduce", "bloom_finish"];

fn parameters(plan: &BloomPlan) -> Params {
    let mut parameters = Params::default();
    parameters.u[0] = [if plan.linear { 0 } else { crate::ops::curve_id(Some(plan.space)) }, plan.view, plan.operation, plan.levels.len() as u32];
    parameters.f[0] = [plan.threshold, plan.knee, plan.gain, plan.pad as f32];
    parameters.f[1] = [plan.luma[0], plan.luma[1], plan.luma[2], 0.0];
    for (index, level) in plan.levels.iter().enumerate() {
        if let Some(row) = parameters.f.get_mut(index + 2) {
            *row = [level.factor as f32, level.pad as f32, level.atlas_y as f32, level.weight];
        }
        if let Some(row) = parameters.f.get_mut(index + 6) {
            *row = [level.size[0] as f32, level.size[1] as f32, 0.0, 0.0];
        }
    }
    parameters
}

/// Reuse two private textures with the existing running-sum box shaders. Every dispatch reads
/// one texture and fully writes the other; successive dispatches preserve queue order, so there
/// is no alias between a kernel's input/output and no stale pooled texel can enter the result.
fn blur(encoder: &mut Enc, mut current: GpuImage, radii: [usize; 3]) -> GpuImage {
    let mut output = encoder.scratch(current.width, current.height);
    for vertical in [false, true] {
        for radius in radii.into_iter().filter(|radius| *radius > 0) {
            let block = if vertical { (4 * radius).clamp(32, 256) } else { 256 } as u32;
            let (lines, count) = if vertical { (current.width, current.height) } else { (current.height, current.width) };
            let mut parameters = Params::default();
            parameters.u[0] = [radius as u32, 0, block, 0];
            encoder.dispatch(if vertical { "box_v" } else { "box_h" }, &parameters, &current, None, &output, None, (lines.div_ceil(64), count.div_ceil(block)));
            std::mem::swap(&mut current, &mut output);
        }
    }
    current
}

/// The exact standalone kernels on an already resident image. Validate before scratch;
/// input/output never alias and no upload/readback occurs in this function.
pub(crate) fn resident(encoder: &mut Enc, context: &EffectCtx, input: GBuf) -> Result<GBuf, BloomError> {
    let plan = plan(context, [input.img.width, input.img.height], input.scale)?;
    validate_offset(input.offset)?;
    resident_plan(encoder, input, &plan)
}

fn resident_plan(encoder: &mut Enc, input: GBuf, plan: &BloomPlan) -> Result<GBuf, BloomError> {
    encoder.g.check_health().map_err(BloomError::Gpu)?;
    if encoder.take_missing() {
        return Err(BloomError::Gpu("a preceding GPU dispatch was unavailable; retry the complete frame on the CPU".into()));
    }
    for entry in ["bloom_extract", "bloom_finish", "bloom_reduce", "box_h", "box_v"] {
        let required = match entry {
            "bloom_reduce" => plan.view != 0 && plan.levels.iter().any(|level| level.factor > 1),
            "box_h" | "box_v" => plan.view != 0 && plan.levels.iter().any(|level| level.radii.iter().any(|radius| *radius > 0)),
            _ => true,
        };
        if required && !encoder.g.has_kernel(entry) {
            return Err(BloomError::Gpu(format!("required kernel {entry} is unavailable; retry the complete frame on the CPU")));
        }
    }
    let fits = |size: [u32; 2]| encoder.g.fits(size[0], size[1]);
    if !fits(plan.source) || !fits(plan.output) || !fits(plan.atlas) || plan.levels.iter().any(|level| !fits(level.size)) {
        return Err(BloomError::Gpu("a planned texture exceeds this device's dimension limit; use the checked CPU core or reduce radius/resolution".into()));
    }
    let source = input.img;
    let parameters = parameters(plan);
    let mut reduced = encoder.scratch(plan.source[0], plan.source[1]);
    // All three bloom kernels store every in-bounds output pixel, including transparent pixels.
    encoder.pixels("bloom_extract", &parameters, &source, None, &reduced, None);
    let output = encoder.scratch(plan.output[0], plan.output[1]);
    if plan.view == 0 {
        encoder.pixels("bloom_finish", &parameters, &source, Some(&reduced), &output, None);
    } else {
        // Only subrectangles are copied, so the atlas/padded images require explicit zeros.
        let atlas = encoder.image(plan.atlas[0], plan.atlas[1]);
        let mut factor = 1;
        for level in &plan.levels {
            while factor < level.factor {
                let next = encoder.scratch(reduced.width.div_ceil(2), reduced.height.div_ceil(2));
                // Transparent-border area averaging preserves energy at odd source dimensions.
                encoder.pixels("bloom_reduce", &Params::default(), &reduced, None, &next, None);
                reduced = next;
                factor = factor.checked_mul(2).ok_or(BloomError::Invalid("pyramid factor overflow"))?;
            }
            let padded = encoder.image(level.size[0], level.size[1]);
            encoder.copy_into(&reduced, &padded, level.pad, level.pad);
            let blurred = blur(encoder, padded, level.radii);
            encoder.copy_into(&blurred, &atlas, 0, level.atlas_y);
        }
        encoder.pixels("bloom_finish", &parameters, &source, Some(&atlas), &output, None);
    }
    if encoder.take_missing() {
        return Err(BloomError::Gpu("a required bloom dispatch was unavailable; no output was published".into()));
    }
    Ok(GBuf { img: output, offset: input.offset.map(|value| value + plan.pad as f64), scale: input.scale })
}

/// Standalone synchronous checked core. Bounds are checked before creating textures;
/// a last-resort driver unwind becomes an explicit GPU error instead of escaping the API.
pub fn render_checked(gpu: &Gpu, context: &EffectCtx, input: &Buf) -> Result<Buf, BloomError> {
    let plan = plan(context, [input.img.width, input.img.height], input.scale)?;
    validate_input(input, &plan)?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        gpu.ctx.check_health().map_err(BloomError::Gpu)?;
        if !gpu.ctx.can_wait() {
            return Err(BloomError::Gpu("standalone render requires synchronous readback; use the checked deferred effect dispatcher".into()));
        }
        let source = gpu.ctx.upload_image(&input.img).ok_or_else(|| BloomError::Gpu("cannot upload the validated input image".into()))?;
        let mut encoder = Enc::new(&gpu.ctx);
        let output = resident_plan(&mut encoder, GBuf { img: source, offset: input.offset, scale: input.scale }, &plan)?;
        let image = encoder.download(&output.img).ok_or_else(|| BloomError::Gpu("readback failed or the device was lost".into()))?;
        Ok(Buf { img: image, offset: output.offset, scale: output.scale })
    }))
    .map_err(|_| BloomError::Gpu("the driver rejected a bounded bloom render".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_color::{ColorSpace, Conversion};
    use effectcraft_effects::{EffectEnv, Params as EffectParams};
    use effectcraft_keyframe::Value;
    use effectcraft_raster::Image;

    fn gpu() -> Option<&'static Gpu> {
        let gpu = crate::tests::gpu();
        let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
        assert!(!required || gpu.is_some(), "Native Bloom acceptance requires a working GPU and successfully compiled kernels");
        gpu
    }

    #[test]
    fn missing_bloom_or_blur_kernels_reject_output_before_publication() {
        if gpu().is_none() {
            return;
        }
        let params = EffectParams { values: [("radius".into(), Value::Scalar(12.0)), ("threshold".into(), Value::Scalar(0.0))].into() };
        let context = EffectCtx { params: &params, time: 0.0, layer_size: [8.0; 2], seed: 0, adjustment: false, env: EffectEnv::default() };
        let input = Buf { img: Image::filled(8, 8, [2.0, 1.0, 0.5, 1.0]), offset: [0.0; 2], scale: 1.0 };
        let before = input.img.clone();
        for entry in ["bloom_extract", "bloom_reduce", "box_h"] {
            let mut device = pollster::block_on(crate::context::GpuContext::request()).unwrap();
            device.drop_kernel(entry);
            let gpu = Gpu::from_context(device);
            let error = render_checked(&gpu, &context, &input).unwrap_err();
            assert!(error.to_string().contains(entry), "{error}");
            assert_eq!(input.img, before);
        }
    }

    fn scene(space: ColorSpace, linear: bool) -> Image {
        let mut image = Image::new(65, 37);
        for y in 0..37 {
            for x in 0..65 {
                let alpha = if (x + y) % 5 == 0 {
                    0.0
                } else if x % 3 == 0 {
                    0.35
                } else {
                    1.0
                };
                let brightness = if (x + y) % 11 == 0 { 16.0 } else { (x as f32 * 0.043 + y as f32 * 0.031).rem_euclid(2.0) };
                let physical = [brightness, brightness * 0.7, brightness * 0.2];
                let authored = Conversion::new(ColorSpace::Srgb, true, space, linear).map(|conversion| conversion.apply(physical)).unwrap_or(physical);
                image.set(x, y, [authored[0] * alpha, authored[1] * alpha, authored[2] * alpha, alpha]);
            }
        }
        // Odd edges, malformed invisible hidden RGB, and nonfinite alpha/channel values must
        // never depend on pooled contents or contribute a hidden emitter.
        image.set(64, 36, [4.0, 1.0, 0.5, 0.5]);
        image.set(0, 0, [16.0, 4.0, 2.0, 0.0]);
        image.set(1, 0, [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0]);
        image.set(2, 0, [4.0, 2.0, 1.0, f32::NAN]);
        image
    }

    fn compare(actual: &Buf, reference: &Buf, description: &str) {
        assert_eq!(
            (actual.img.width, actual.img.height, actual.offset, actual.scale),
            (reference.img.width, reference.img.height, reference.offset, reference.scale)
        );
        for (index, (actual, reference)) in actual.img.data.iter().zip(&reference.img.data).enumerate() {
            for channel in 0..4 {
                let tolerance = if channel == 3 { 1e-4 } else { 1e-5 + 1e-3 * reference[channel].abs() };
                assert!(actual[channel].is_finite() && reference[channel].is_finite());
                assert!(
                    (actual[channel] - reference[channel]).abs() <= tolerance,
                    "{description} pixel {index} channel {channel}: {actual:?} vs {reference:?}, tolerance {tolerance}"
                );
            }
        }
    }

    #[test]
    fn bloom_checked_cpu_gpu_hdr_alpha_color_and_poisoned_scratch_parity() {
        let Some(gpu) = gpu() else { return };
        for (space, linear) in [(ColorSpace::Srgb, true), (ColorSpace::Srgb, false), (ColorSpace::Rec2020, false), (ColorSpace::AcesCg, false)] {
            for quality in 0..=2 {
                let values = effectcraft_effects::bloom::parameters().into_iter().map(|parameter| (parameter.id.to_owned(), parameter.default)).collect();
                let mut parameters = EffectParams { values };
                parameters.values.insert("radius".into(), Value::Scalar(if quality == 2 { 64.0 } else { 16.0 }));
                parameters.values.insert("quality".into(), Value::Enum(quality));
                parameters.values.insert("exposure".into(), Value::Scalar(1.0));
                parameters.values.insert("threshold".into(), Value::Scalar(1.0));
                parameters.values.insert("softKnee".into(), Value::Scalar(0.35));
                parameters.values.insert("spread".into(), Value::Scalar(0.7));
                for (view, operation, scale) in [(0, 0, 1.0), (1, 0, 0.25), (2, 0, 0.5), (2, 1, 1.0)] {
                    parameters.values.insert("view".into(), Value::Enum(view));
                    parameters.values.insert("operation".into(), Value::Enum(operation));
                    let input = Buf { img: scene(space, linear), offset: [3.25, -7.5], scale };
                    let context = EffectCtx {
                        params: &parameters,
                        time: 0.0,
                        layer_size: [65.0, 37.0],
                        seed: 0,
                        adjustment: false,
                        env: EffectEnv { working_space: Some(space), working_linear: linear, ..Default::default() },
                    };
                    let expected = effectcraft_effects::bloom::render_checked(&context, input.clone()).unwrap();
                    let description = format!("{space:?} linear={linear} quality={quality} view={view} operation={operation} scale={scale}");
                    // Enc::scratch is filled with NaNs in every test render. The second render
                    // additionally reuses sizes/content from the first completed frame.
                    for _ in 0..2 {
                        compare(&render_checked(gpu, &context, &input).unwrap(), &expected, &description);
                    }
                    // Reuse the same atlas sizes with a transparent input, proving padded/copy
                    // regions and every full-write output lose all prior bright texels.
                    let dark = Buf { img: Image::new(65, 37), ..input };
                    let output = render_checked(gpu, &context, &dark).unwrap();
                    assert!(output.img.data.iter().all(|pixel| *pixel == [0.0; 4]), "stale halo in {description}");
                }
            }
        }
    }

    #[test]
    fn bloom_checked_gpu_rejects_hostile_request_before_texture_creation() {
        let Some(gpu) = gpu() else { return };
        let parameters = EffectParams { values: [("radius".into(), Value::Scalar(f64::INFINITY))].into() };
        let context = EffectCtx { params: &parameters, time: 0.0, layer_size: [1.0; 2], seed: 0, adjustment: false, env: EffectEnv::default() };
        let input = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: f64::MAX };
        assert!(matches!(render_checked(gpu, &context, &input), Err(BloomError::Invalid(_))));
        let malformed = Buf { img: Image { width: u32::MAX, height: 1, data: vec![] }, ..input };
        assert!(render_checked(gpu, &context, &malformed).is_err());
    }

    #[test]
    fn bloom_checked_gpu_zero_radius_zero_amount_and_adjustment_bounds_match_cpu() {
        let Some(gpu) = gpu() else { return };
        for (radius, amount, adjustment, scale) in [(0.0, 100.0, false, 1.0), (16.0, 0.0, false, 2.0), (4.0, 100.0, true, 0.5)] {
            let parameters = EffectParams {
                values: [("radius".into(), Value::Scalar(radius)), ("amount".into(), Value::Scalar(amount)), ("threshold".into(), Value::Scalar(0.0))].into(),
            };
            let context = EffectCtx {
                params: &parameters,
                time: 0.0,
                layer_size: [1.0; 2],
                seed: 0,
                adjustment,
                env: EffectEnv { working_linear: true, ..Default::default() },
            };
            let input = Buf { img: Image::filled(1, 1, [4.0, 2.0, 0.5, 0.5]), offset: [-0.25, 0.5], scale };
            let expected = effectcraft_effects::bloom::render_checked(&context, input.clone()).unwrap();
            compare(&render_checked(gpu, &context, &input).unwrap(), &expected, "zero/adjustment boundary");
            if adjustment {
                assert_eq!((expected.img.width, expected.img.height), (1, 1));
            }
        }
    }

    #[test]
    fn bloom_checked_resident_views_preserve_input_without_host_transfers() {
        use effectcraft_render::{FxStep, FxTarget};
        let Some(gpu) = gpu() else { return };
        let spec = effectcraft_effects::find(effectcraft_effects::bloom::ID).unwrap();
        assert!(spec.gpu && crate::effects::supports(spec.id));
        for (view, adjustment, scale) in [(0, false, 1.0), (1, false, 0.5), (2, false, 2.0), (2, true, 1.0)] {
            let parameters = EffectParams {
                values: [("radius".into(), Value::Scalar(16.0)), ("view".into(), Value::Enum(view)), ("threshold".into(), Value::Scalar(0.0))].into(),
            };
            let context = || EffectCtx {
                params: &parameters,
                time: 0.0,
                layer_size: [65.0, 37.0],
                seed: 0,
                adjustment,
                env: EffectEnv { working_space: Some(ColorSpace::Rec2020), working_linear: false, ..Default::default() },
            };
            let input = Buf { img: scene(ColorSpace::Rec2020, false), offset: [3.25, -7.5], scale };
            let expected = effectcraft_effects::bloom::render_checked(&context(), input.clone()).unwrap();
            let source = gpu.ctx.upload_image(&input.img).unwrap();
            let mut encoder = Enc::new(&gpu.ctx);
            let before = gpu.ctx.transfer_stats();
            let output = {
                let mut target = crate::effects::GpuFx::new(&mut encoder, source.clone(), input.offset, scale, None);
                let step = FxStep { spec, ctx: context() };
                assert!(!target.gpu(std::slice::from_ref(&step)), "checked kernels cannot enter infallible chains");
                assert!(target.gpu_checked(&step).unwrap());
                target.finish().unwrap()
            };
            assert_eq!(gpu.ctx.transfer_stats(), before, "resident compute must not upload or read back");
            let image = encoder.download(&output.0).unwrap();
            compare(&Buf { img: image, offset: output.1, scale: output.2 }, &expected, "resident view/scale/adjustment");
            let unchanged = encoder.download(&source).unwrap();
            for (actual, expected) in unchanged.data.iter().zip(&input.img.data) {
                for channel in 0..4 {
                    assert!(actual[channel] == expected[channel] || (actual[channel].is_nan() && expected[channel].is_nan()));
                }
            }
        }
    }

    #[test]
    fn bloom_checked_deferred_readback_failure_and_hostile_controls_are_explicit() {
        use effectcraft_render::{Accelerator, FxStep};
        let gpu = crate::tests_deferred::deferred_gpu();
        let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
        assert!(!required || gpu.is_some(), "checked readback acceptance requires deferred GPU mode");
        let Some((gpu, _guard)) = gpu else { return };
        let spec = effectcraft_effects::find(effectcraft_effects::bloom::ID).unwrap();
        let parameters = EffectParams { values: [("radius".into(), Value::Scalar(8.0))].into() };
        fn context(parameters: &EffectParams) -> EffectCtx<'_> {
            EffectCtx { params: parameters, time: 0.0, layer_size: [3.0, 5.0], seed: 0, adjustment: false, env: Default::default() }
        }
        let input = Buf { img: Image::filled(3, 5, [4.0, 2.0, 1.0, 1.0]), offset: [0.0; 2], scale: 1.0 };
        let step = FxStep { spec, ctx: context(&parameters) };
        gpu.frame_begin();
        gpu.pass_begin();
        let key = crate::deferred::chain_key(std::slice::from_ref(&step), &input, None);
        let deferred = gpu.ctx.deferred().unwrap();
        deferred.start(key, 3, 5, input.offset, input.scale)(None);
        let before = gpu.ctx.transfer_stats();
        assert!(gpu.effect_checked(&step, &input, None).unwrap_err().contains("readback failed"));
        assert_eq!(gpu.ctx.transfer_stats(), before);
        let planned = plan(&step.ctx, [input.img.width, input.img.height], input.scale).unwrap();
        let offset = input.offset.map(|value| value + planned.pad as f64);
        for (dimensions, offset, scale, message) in [
            ([u32::MAX, 1], offset, input.scale, "dimensions"),
            (planned.output, [f64::NAN, 0.0], input.scale, "geometry"),
            (planned.output, offset, f64::INFINITY, "geometry"),
            (planned.output, offset, input.scale, "storage"),
        ] {
            deferred.start(key, dimensions[0], dimensions[1], offset, scale)(Some(vec![]));
            assert!(gpu.effect_checked(&step, &input, None).unwrap_err().contains(message));
            assert_eq!(gpu.ctx.transfer_stats(), before, "malformed ready results must fail before allocation or transfer");
        }
        for radius in [f64::NAN, f64::INFINITY, f64::MAX, -1.0] {
            let parameters = EffectParams { values: [("radius".into(), Value::Scalar(radius))].into() };
            let step = FxStep { spec, ctx: context(&parameters) };
            assert!(gpu.effect_checked(&step, &input, None).unwrap_err().contains("radius"));
            assert_eq!(gpu.ctx.transfer_stats(), before, "hostile controls must fail before upload/readback");
        }
        let malformed = Buf { img: Image { width: u32::MAX, height: 1, data: vec![] }, ..input };
        assert!(gpu.effect_checked(&step, &malformed, None).is_err());
        assert_eq!(gpu.ctx.transfer_stats(), before);
    }
}

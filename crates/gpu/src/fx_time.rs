//! GPU effects, time (kernels in `shaders/fx_time.wgsl`, every entry point prefixed `ftm_`):
//! Time Difference, Time Displacement, CC Force Motion Blur, CC Wide Time, Pixel Motion Blur and
//! Timewarp. Like Echo (`fx_noise`), the other frames come from the effect host on the CPU
//! (`effectcraft_effects::time_frames`: fetched at the CPU effect's layer times and placed on
//! one grid) and are uploaded; the kernels combine them with the CPU effect's arithmetic in its
//! order (weighted sums one frame per pass, so the float rounding matches).
//!
//! Time Displacement's per-pixel time (the map's luminance, quantised to the time resolution)
//! is computed on the CPU, since it decides which frames to fetch; the GPU gathers each pixel
//! from its frame. Timewarp's motion vectors (block matching and smoothing,
//! `effectcraft_raster::flow`) are estimated on the CPU (`effectcraft_effects::timewarp_plan`);
//! the frame building (Whole Frames, Frame Mix, Pixel Motion's warp along the vectors with its
//! error fallback, the shutter samples' average) runs on the GPU. With a Matte Layer the plan
//! also carries the matte-masked frames (and the foreground's and background's own vectors);
//! the GPU builds both layers and shows them as Show asks (`ftm_layer`).

use effectcraft_effects::util::{fit_layer, unpremul};
use effectcraft_effects::{Buf, EffectCtx, TwStep};
use effectcraft_keyframe::Value;

use crate::context::{Enc, GpuImage, Params};
use crate::effects::GBuf;

/// Compute entry points in `fx_time.wgsl`.
pub(crate) const KERNELS: &[&str] = &["ftm_acc", "ftm_mix", "ftm_layer", "ftm_difference", "ftm_displace", "ftm_motion"];

/// Effect ids implemented here.
pub(crate) const IDS: &[&str] =
    &["ec.time.timedifference", "ec.time.timedisplacement", "ec.time.ccforcemotionblur", "ec.time.ccwidetime", "ec.time.pixelmotionblur", "ec.time.timewarp"];

/// Run effect `id` (one of [`IDS`]); `None` = this parameter combination runs on the CPU.
pub(crate) fn apply(e: &mut Enc, id: &str, ctx: &EffectCtx, b: GBuf) -> Option<GBuf> {
    match id {
        "ec.time.timedifference" => time_difference(e, ctx, b),
        "ec.time.timedisplacement" => time_displacement(e, ctx, b),
        "ec.time.ccforcemotionblur" => {
            let n = ctx.params.f("motionBlurLevels").round().clamp(2.0, 64.0) as usize;
            let angle = if ctx.params.b("overrideShutterAngle") { ctx.params.f("shutterAngle") } else { 180.0 };
            let span = angle.clamp(0.0, 3600.0) / 360.0 / ctx.fps();
            mean_of(e, ctx, b, &(0..n).map(|i| ctx.time + span * i as f64 / n as f64).collect::<Vec<_>>())
        }
        "ec.time.ccwidetime" => {
            let fwd = ctx.params.f("forwardSteps").round().clamp(0.0, 45.0) as i64;
            let back = ctx.params.f("backwardSteps").round().clamp(0.0, 45.0) as i64;
            let fd = 1.0 / ctx.fps();
            mean_of(e, ctx, b, &(-back..=fwd).map(|k| ctx.time + k as f64 * fd).collect::<Vec<_>>())
        }

        _ => None,
    }
}

fn kernel(e: &mut Enc, entry: &str, p: &Params, src: &GpuImage, aux: Option<&GpuImage>, data: Option<&wgpu::Buffer>) -> GpuImage {
    let out = e.scratch(src.width, src.height);
    e.pixels(entry, p, src, aux, &out, data);
    out
}

/// time_fx::average: Σ frame × weight, one frame per pass in the CPU's order.
fn average(e: &mut Enc, frames: &[GpuImage], weights: &[f32]) -> Option<GpuImage> {
    let first = frames.first()?;
    let mut acc = e.zeros(first.width, first.height);
    for (f, &k) in frames.iter().zip(weights) {
        let mut p = Params::default();
        p.f[0][0] = k;
        acc = kernel(e, "ftm_acc", &p, &acc, Some(f), None);
    }
    Some(acc)
}

fn on_grid(img: GpuImage, grid: &Buf) -> GBuf {
    GBuf { img, offset: grid.offset, scale: grid.scale }
}

/// time_fx::mean_of: the plain average of the layer at `times`.
fn mean_of(e: &mut Enc, ctx: &EffectCtx, b: GBuf, times: &[f64]) -> Option<GBuf> {
    if times.is_empty() {
        return Some(b);
    }
    let Some((grid, imgs)) = effectcraft_effects::time_frames(ctx, times) else { return Some(b) };
    let frames = imgs.iter().map(|i| e.g.upload_image(i)).collect::<Option<Vec<_>>>()?;
    let k = 1.0 / times.len() as f32;
    let img = average(e, &frames, &vec![k; times.len()])?;
    Some(on_grid(img, &grid))
}

// ---------------------------------------------------------------- Time Difference

fn time_difference(e: &mut Enc, ctx: &EffectCtx, b: GBuf) -> Option<GBuf> {
    let Some(host) = ctx.env.host else { return Some(b) };
    let pr = ctx.params;
    let off = pr.f("timeOffset");
    let target_id = pr.get("targetLayer").and_then(Value::as_layer);
    let (grid, cur, target) = match target_id.and_then(|id| host.layer_at(id, ctx.env.comp_time + off, false)) {
        Some(other) => {
            let Some((grid, mut imgs)) = effectcraft_effects::time_frames(ctx, &[ctx.time]) else { return Some(b) };
            let cur = imgs.pop().unwrap_or_default();
            let tgt = fit_layer(ctx, &Buf { img: cur.clone(), ..grid.clone() }, &other, true);
            (grid, cur, tgt)
        }
        None => {
            let Some((grid, mut imgs)) = effectcraft_effects::time_frames(ctx, &[ctx.time, ctx.time + off]) else { return Some(b) };
            let tgt = imgs.pop().unwrap_or_default();
            let cur = imgs.pop().unwrap_or_default();
            (grid, cur, tgt)
        }
    };
    let (cur, target) = (e.g.upload_image(&cur)?, e.g.upload_image(&target)?);
    let mut p = Params::default();
    p.u[0] = [pr.b("absoluteDifference") as u32, pr.e("alphaChannel"), 0, 0];
    p.f[0][0] = 1.0 + pr.f("contrast") as f32 / 25.0;
    let img = kernel(e, "ftm_difference", &p, &cur, Some(&target), None);
    Some(on_grid(img, &grid))
}

// ---------------------------------------------------------------- Time Displacement

fn time_displacement(e: &mut Enc, ctx: &EffectCtx, b: GBuf) -> Option<GBuf> {
    if ctx.env.host.is_none() {
        return Some(b);
    }
    let pr = ctx.params;
    let max = pr.f("maxDisplacementTime");
    let res = pr.f("timeResolution").clamp(1.0, 999.0);
    let Some((grid, mut imgs)) = effectcraft_effects::time_frames(ctx, &[ctx.time]) else { return Some(b) };
    let grid = Buf { img: imgs.pop().unwrap_or_default(), ..grid };
    let map = match ctx.layer_param("displacementMapLayer", true) {
        Some(o) => fit_layer(ctx, &grid, &o, pr.b("stretchMap")),
        None => grid.img.clone(),
    };
    // The CPU effect's per-pixel time step (luminance → −max..+max, quantised).
    let span = 2.0 * max.abs() * res;
    let step = if span > 63.0 { 2.0 * max.abs() / 63.0 } else { 1.0 / res };
    let idx: Vec<i32> = map
        .data
        .iter()
        .map(|p| {
            let (c, a) = unpremul(*p);
            let l = if a > 0.0 { effectcraft_color::luminance(c[0], c[1], c[2]) } else { 0.5 };
            (((l as f64 - 0.5) * 2.0 * max) / step).round() as i32
        })
        .collect();
    let mut keys = idx.clone();
    keys.sort_unstable();
    keys.dedup();
    if keys == [0] {
        return Some(on_grid(e.g.upload_image(&grid.img)?, &grid));
    }
    let times: Vec<f64> = keys.iter().map(|k| ctx.time + *k as f64 * step).collect();
    let Some((fgrid, frames)) = effectcraft_effects::time_frames(ctx, &times) else { return Some(on_grid(e.g.upload_image(&grid.img)?, &grid)) };
    let dx = (fgrid.offset[0] - grid.offset[0]).round() as i32;
    let dy = (fgrid.offset[1] - grid.offset[1]).round() as i32;
    let which: Vec<u8> = idx.iter().flat_map(|k| (keys.binary_search(k).unwrap_or(0) as u32).to_le_bytes()).collect();
    let data = e.bytes(which);
    let (w, h) = (grid.img.width, grid.img.height);
    let mut acc = e.zeros(w, h);
    for (fi, f) in frames.iter().enumerate() {
        let frame = e.g.upload_image(f)?;
        let mut p = Params::default();
        p.u[0] = [fi as u32, w, dx as u32, dy as u32];
        let out = e.scratch(w, h);
        e.pixels("ftm_displace", &p, &acc, Some(&frame), &out, Some(&data));
        acc = out;
    }
    Some(on_grid(acc, &grid))
}

// ---------------------------------------------------------------- Timewarp

struct WarpGpu {
    raw: Vec<Option<GpuImage>>,
    cropped: Vec<Option<GpuImage>>,
    flows: Vec<Option<wgpu::Buffer>>,
    samples: [Option<GpuImage>; 3],
    accumulators: [Option<GpuImage>; 2],
}

impl WarpGpu {
    fn target(e: &mut Enc, plan: &effectcraft_effects::TimewarpPlan, slot: &mut Option<GpuImage>) -> GpuImage {
        slot.get_or_insert_with(|| e.scratch(plan.grid.width, plan.grid.height)).clone()
    }

    fn frame(&mut self, e: &mut Enc, plan: &effectcraft_effects::TimewarpPlan, index: usize, crop: bool) -> Result<GpuImage, String> {
        let crop = crop && plan.pm.crops != [0; 4];
        let cache = if crop { &mut self.cropped } else { &mut self.raw };
        let slot = cache.get_mut(index).ok_or("Timewarp GPU: missing prepared frame index")?;
        if slot.is_none() {
            let source = plan.frames.get(index).ok_or("Timewarp GPU: missing prepared source")?;
            let image = if crop { e.g.upload_image(&effectcraft_effects::timewarp_crop(source, &plan.pm)) } else { e.g.upload_image(source) };
            *slot = Some(image.ok_or("Timewarp GPU: cannot upload a validated source frame")?);
        }
        slot.clone().ok_or_else(|| "Timewarp GPU: uploaded frame is missing".into())
    }

    fn motion(&mut self, e: &mut Enc, plan: &effectcraft_effects::TimewarpPlan, indices: [usize; 3], fraction: f32, slot: usize) -> Result<GpuImage, String> {
        let [a, b, flow] = indices;
        let first = self.frame(e, plan, a, true)?;
        let second = self.frame(e, plan, b, true)?;
        let field = plan.flows.get(flow).ok_or("Timewarp GPU: missing prepared motion field")?;
        let buffer = self.flows.get_mut(flow).ok_or("Timewarp GPU: missing motion buffer slot")?;
        if buffer.is_none() {
            let bytes: &[u8] = bytemuck::cast_slice(&field.v);
            if bytes.len().max(16) as u64 > e.g.device.limits().max_storage_buffer_binding_size {
                return Err("Timewarp GPU: motion field exceeds the device storage-buffer limit".into());
            }
            let mut data = Vec::new();
            data.try_reserve_exact(bytes.len().max(16)).map_err(|error| format!("Timewarp GPU: cannot stage motion field: {error}"))?;
            data.extend_from_slice(bytes);
            *buffer = Some(e.bytes(data));
        }
        let output = Self::target(e, plan, self.samples.get_mut(slot).ok_or("Timewarp GPU: missing motion output slot")?);
        let mut parameters = Params::default();
        parameters.u[0] = [if field.v.is_empty() { 0 } else { field.cols as u32 }, field.rows as u32, plan.pm.extreme as u32, plan.pm.build_from_one as u32];
        parameters.f[0] = [field.block as f32, fraction, plan.pm.error_threshold, 0.0];
        e.pixels("ftm_motion", &parameters, &first, Some(&second), &output, buffer.as_ref());
        Ok(output)
    }
}

/// Run a checked Timewarp over resident pixels. An unavailable host retains the input;
/// missing retimed samples with available current geometry are transparent.
/// rejected controls/plans/kernels remain errors rather than legacy Option pass-through.
pub(crate) fn checked(e: &mut Enc, id: &str, ctx: &EffectCtx, input: GBuf) -> Result<GBuf, String> {
    if !input.scale.is_finite() || input.scale <= 0.0 || input.offset.iter().any(|value| !value.is_finite() || value.abs() > 1e12) {
        return Err("Timewarp GPU: resident input geometry is invalid".into());
    }
    let size = [input.img.width, input.img.height];
    let plan = if id == effectcraft_effects::timewarp::PMB_ID {
        effectcraft_effects::timewarp::pmb_plan_for_input(ctx, size)?
    } else {
        effectcraft_effects::timewarp::plan_for_input(ctx, size)?
    };
    prepared(e, input, plan.as_ref())
}

pub(crate) fn prepared(e: &mut Enc, input: GBuf, plan: Option<&effectcraft_effects::TimewarpPlan>) -> Result<GBuf, String> {
    e.g.check_health()?;
    let Some(plan) = plan else { return Ok(input) };
    plan.validate()?;
    if !e.g.fits(plan.grid.width, plan.grid.height) {
        return Err("Timewarp GPU: output grid exceeds the device texture limit".into());
    }
    if plan.steps.len() > 1 && !e.g.has_kernel("ftm_acc") {
        return Err("Timewarp GPU: unavailable accumulation kernel ftm_acc; use Software Only".into());
    }
    for (step, _) in &plan.steps {
        let entries: &[&str] = match step {
            TwStep::Whole(_) => &[],
            TwStep::Mix(..) => &["ftm_mix"],
            TwStep::Motion { .. } => &["ftm_motion"],
            TwStep::Layered { .. } => &["ftm_motion", "ftm_layer"],
        };
        for entry in entries {
            if !e.g.has_kernel(entry) {
                return Err(format!("Timewarp GPU: unavailable kernel {entry}; use Software Only"));
            }
        }
    }
    let mut cache = WarpGpu {
        raw: vec![None; plan.frames.len()],
        cropped: vec![None; plan.frames.len()],
        flows: vec![None; plan.flows.len()],
        samples: [None, None, None],
        accumulators: [None, None],
    };
    let mut accumulator = e.zeros(plan.grid.width, plan.grid.height);
    for (sample, (step, weight)) in plan.steps.iter().enumerate() {
        let image = match *step {
            TwStep::Whole(index) => cache.frame(e, plan, index, false)?,
            TwStep::Mix(a, b, fraction) => {
                let first = cache.frame(e, plan, a, false)?;
                let second = cache.frame(e, plan, b, false)?;
                let output = WarpGpu::target(e, plan, &mut cache.samples[0]);
                let mut parameters = Params::default();
                parameters.f[0][0] = fraction;
                e.pixels("ftm_mix", &parameters, &first, Some(&second), &output, None);
                output
            }
            TwStep::Motion { a, b, w, flow } => cache.motion(e, plan, [a, b, flow], w, 0)?,
            TwStep::Layered { fg, bg, w, show } => {
                let foreground = cache.motion(e, plan, fg, w, 1)?;
                let background = cache.motion(e, plan, bg, w, 2)?;
                let output = WarpGpu::target(e, plan, &mut cache.samples[0]);
                let mut parameters = Params::default();
                parameters.u[0][0] = show;
                e.pixels("ftm_layer", &parameters, &foreground, Some(&background), &output, None);
                output
            }
        };
        if plan.steps.len() == 1 {
            return Ok(GBuf { img: image, offset: plan.grid.offset, scale: plan.grid.scale });
        }
        let output = WarpGpu::target(e, plan, &mut cache.accumulators[sample % 2]);
        let mut parameters = Params::default();
        parameters.f[0][0] = *weight;
        e.pixels("ftm_acc", &parameters, &accumulator, Some(&image), &output, None);
        accumulator = output;
    }
    Ok(GBuf { img: accumulator, offset: plan.grid.offset, scale: plan.grid.scale })
}

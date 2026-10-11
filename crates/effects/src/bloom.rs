//! Original Native Bloom with checked CPU and GPU registry runners.
//! A normalized four-scale Gaussian mixture scatters linear premultiplied emission. The CPU
//! and GPU share this bounded plan, actual box radii, sample centers, atlas layout and weights.

use std::fmt;

use effectcraft_color::{ColorSpace, Conversion};
use effectcraft_keyframe::Value;
use effectcraft_raster::{Image, Px};
use rayon::prelude::*;

use crate::{Buf, EffectCtx, ParamSpec, num, p, popup, slider};

pub const MAX_DIMENSION: u32 = 16384;
pub const MAX_IMAGE_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 1536 * 1024 * 1024;
pub const MAX_WORK: u64 = 2_000_000_000;
pub const MAX_EFFECTIVE_RADIUS: f64 = 4096.0;
const INPUT_HEADROOM: f32 = 1e12;
pub const ID: &str = "ec.stylize.bloom";

pub fn spec() -> crate::EffectSpec {
    crate::EffectSpec {
        id: ID,
        name: "Native Bloom",
        category: "Stylize",
        params: parameters(),
        render: crate::EffectRender::Checked(|context, input| render_checked(context, input).map_err(|error| error.to_string())),
        gpu: true,
        float: true,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BloomError {
    Invalid(&'static str),
    Unsupported(String),
    Allocation(String),
    Gpu(String),
}

impl fmt::Display for BloomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "Native Bloom: {message}"),
            Self::Unsupported(message) => write!(f, "Native Bloom request exceeds its supported budget: {message}; reduce Radius or resolution"),
            Self::Allocation(message) => write!(f, "Native Bloom cannot allocate scratch: {message}"),
            Self::Gpu(message) => write!(f, "Native Bloom GPU: {message}"),
        }
    }
}
impl std::error::Error for BloomError {}

/// Animatable controls shared by the registered checked runner and standalone core tests.
pub fn parameters() -> Vec<ParamSpec> {
    vec![
        p("radius", "Radius (outer sigma)", num(32.0), slider(0.0, 1024.0, 0.0, 256.0, 2)),
        p("exposure", "Exposure (stops)", num(0.0), slider(-16.0, 16.0, -4.0, 4.0, 2)),
        p("amount", "Amount (%)", num(100.0), slider(0.0, 1000.0, 0.0, 100.0, 2)),
        p("threshold", "HDR Threshold", num(1.0), slider(0.0, 1_000_000.0, 0.0, 16.0, 3)),
        p("softKnee", "Soft Knee", num(0.5), slider(0.0, 1.0, 0.0, 1.0, 3)),
        p("spread", "Halo Spread", num(0.5), slider(0.0, 1.0, 0.0, 1.0, 3)),
        p("quality", "Quality", Value::Enum(1), popup(&["Draft", "Balanced", "Final"])),
        p("view", "View", Value::Enum(2), popup(&["Source Emission", "Glow Only", "Final"])),
        p("operation", "Composite", Value::Enum(0), popup(&["Add", "Screen"])),
    ]
}

#[derive(Clone, Debug)]
pub struct BloomLevel {
    pub factor: u32,
    pub source: [u32; 2],
    pub size: [u32; 2],
    pub pad: u32,
    pub atlas_y: u32,
    pub radii: [usize; 3],
    pub weight: f32,
}

#[derive(Clone, Debug)]
pub struct BloomPlan {
    pub source: [u32; 2],
    pub output: [u32; 2],
    pub atlas: [u32; 2],
    pub pad: u32,
    pub levels: Vec<BloomLevel>,
    pub threshold: f32,
    pub knee: f32,
    pub gain: f32,
    pub view: u32,
    pub operation: u32,
    pub space: ColorSpace,
    pub linear: bool,
    pub luma: [f32; 3],
    pub total_bytes: u64,
    pub work: u64,
}

fn number(ctx: &EffectCtx, id: &'static str, default: f64, min: f64, max: f64) -> Result<f64, BloomError> {
    let value = match ctx.params.get(id) {
        None => default,
        Some(Value::Scalar(value)) => *value,
        _ => return Err(BloomError::Invalid(id)),
    };
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(BloomError::Invalid(id));
    }
    Ok(value)
}

fn choice(ctx: &EffectCtx, id: &'static str, default: u32, maximum: u32) -> Result<u32, BloomError> {
    let value = match ctx.params.get(id) {
        None => default as f64,
        Some(Value::Enum(value)) => *value as f64,
        Some(Value::Scalar(value)) => *value,
        _ => return Err(BloomError::Invalid(id)),
    };
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=maximum as f64).contains(&value) {
        return Err(BloomError::Invalid(id));
    }
    Ok(value as u32)
}

fn area(size: [u32; 2]) -> Result<u64, BloomError> {
    let pixels = u64::from(size[0]).checked_mul(u64::from(size[1])).ok_or(BloomError::Invalid("image dimensions overflow"))?;
    if size.contains(&0) || size.iter().any(|&dimension| dimension > MAX_DIMENSION) || pixels > MAX_IMAGE_PIXELS {
        return Err(BloomError::Unsupported(format!("image {} × {}", size[0], size[1])));
    }
    Ok(pixels)
}

/// The existing three-box Gaussian approximation, computed once for both backends. Sigma is
/// already bounded to at most 16 pixels before this arithmetic or any integer conversion.
fn radii(sigma: f64) -> [usize; 3] {
    if sigma <= 0.05 {
        return [0; 3];
    }
    let mut low = (4.0 * sigma * sigma + 1.0).sqrt().floor() as i64;
    if low % 2 == 0 {
        low -= 1;
    }
    let high = low + 2;
    let count = ((12.0 * sigma * sigma - (3 * low * low) as f64 - 12.0 * low as f64 - 9.0) / (-4.0 * low as f64 - 4.0)).round() as i64;
    std::array::from_fn(|index| (((if (index as i64) < count { low } else { high }) - 1) / 2).max(0) as usize)
}

/// Validate controls, dimensions, all planned allocations and work before either backend runs.
/// The accounting deliberately includes caller/input upload, output/readback/staging copies,
/// the full downsample chain, atlas, and four blur-sized buffers per component. It is a
/// conservative per-effect limit; it does not budget other layers or the renderer's caches.
pub fn plan(ctx: &EffectCtx, source: [u32; 2], scale: f64) -> Result<BloomPlan, BloomError> {
    let source_area = area(source)?;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(BloomError::Invalid("render scale must be finite and positive"));
    }
    let radius = number(ctx, "radius", 32.0, 0.0, 1024.0)? * scale;
    if !radius.is_finite() || radius > MAX_EFFECTIVE_RADIUS {
        return Err(BloomError::Invalid("effective radius exceeds 4096 rendered pixels"));
    }
    let threshold = number(ctx, "threshold", 1.0, 0.0, 1_000_000.0)? as f32;
    let knee = threshold * number(ctx, "softKnee", 0.5, 0.0, 1.0)? as f32;
    let gain = (2.0_f64.powf(number(ctx, "exposure", 0.0, -16.0, 16.0)?) * number(ctx, "amount", 100.0, 0.0, 1000.0)? / 100.0) as f32;
    let spread = number(ctx, "spread", 0.5, 0.0, 1.0)? as f32;
    let quality = choice(ctx, "quality", 1, 2)?;
    let view = choice(ctx, "view", 2, 2)?;
    let operation = choice(ctx, "operation", 0, 1)?;
    let space = ctx.env.working_space.unwrap_or(ColorSpace::Srgb);
    let linear = ctx.env.working_linear || space.is_linear();
    let luma = space.to_xyz()[1].map(|value| value as f32);
    let mut levels = Vec::with_capacity(4);
    let mut atlas = [1, 1];
    let mut pad = 0;
    let mut pixels = source_area.checked_mul(6).ok_or(BloomError::Invalid("memory accounting overflow"))?;
    let mut work = source_area.checked_mul(32).ok_or(BloomError::Invalid("work accounting overflow"))?;
    if view != 0 {
        let limit = [4.0, 8.0, 16.0].get(quality as usize).copied().ok_or(BloomError::Invalid("quality"))?;
        let mut factor = [4_u32, 2, 1].get(quality as usize).copied().ok_or(BloomError::Invalid("quality"))?;
        while radius / (8.0 * factor as f64) > limit {
            factor = factor.checked_mul(2).ok_or(BloomError::Invalid("pyramid factor overflow"))?;
        }
        if radius == 0.0 {
            factor = 1;
        }
        let box_radii = radii(radius / (8.0 * factor as f64));
        let local_pad = if radius > 0.0 { box_radii.iter().sum::<usize>() as u32 + 2 } else { 0 };
        let inner = [0.55_f32, 0.30, 0.12, 0.03];
        let outer = [0.05_f32, 0.15, 0.30, 0.50];
        let mut remaining = 1.0_f32;
        let mut atlas_height = 0_u32;
        let count = if radius == 0.0 { 1 } else { 4 };
        for index in 0..count {
            let weight = if index == count - 1 {
                remaining
            } else {
                let inner = inner.get(index).copied().ok_or(BloomError::Invalid("component weight index"))?;
                let outer = outer.get(index).copied().ok_or(BloomError::Invalid("component weight index"))?;
                inner + (outer - inner) * spread
            };
            remaining -= weight;
            let level_source = [source[0].div_ceil(factor), source[1].div_ceil(factor)];
            let size = level_source.map(|dimension| dimension.checked_add(local_pad * 2));
            let size = [size[0].ok_or(BloomError::Invalid("pyramid width overflow"))?, size[1].ok_or(BloomError::Invalid("pyramid height overflow"))?];
            let level_area = area(size)?;
            let support = if radius == 0.0 { 0 } else { factor.checked_mul(local_pad + 1).ok_or(BloomError::Invalid("halo support overflow"))? };
            pad = pad.max(support);
            atlas[0] = atlas[0].max(size[0]);
            levels.push(BloomLevel { factor, source: level_source, size, pad: local_pad, atlas_y: atlas_height, radii: box_radii, weight });
            atlas_height = atlas_height.checked_add(size[1]).ok_or(BloomError::Invalid("atlas height overflow"))?;
            pixels = pixels
                .checked_add(level_area.checked_mul(4).ok_or(BloomError::Invalid("blur memory overflow"))?)
                .ok_or(BloomError::Invalid("scratch memory overflow"))?;
            work = work
                .checked_add(level_area.checked_mul(48).ok_or(BloomError::Invalid("blur work overflow"))?)
                .ok_or(BloomError::Invalid("work accounting overflow"))?;
            if index + 1 < count {
                factor = factor.checked_mul(2).ok_or(BloomError::Invalid("pyramid factor overflow"))?;
            }
        }
        atlas[1] = atlas_height;
        pixels = pixels.checked_add(area(atlas)?).ok_or(BloomError::Invalid("atlas memory overflow"))?;
        let mut reduced = source;
        let mut down_factor = 1;
        while down_factor < factor {
            reduced = reduced.map(|dimension| dimension.div_ceil(2));
            let reduced_area = area(reduced)?;
            pixels = pixels.checked_add(reduced_area).ok_or(BloomError::Invalid("pyramid memory overflow"))?;
            work = work.checked_add(reduced_area * 4).ok_or(BloomError::Invalid("reduction work overflow"))?;
            down_factor = down_factor.checked_mul(2).ok_or(BloomError::Invalid("pyramid factor overflow"))?;
        }
    }
    if ctx.adjustment || view == 0 {
        pad = 0;
    }
    let border = pad.checked_mul(2).ok_or(BloomError::Invalid("output border overflow"))?;
    let output = [
        source[0].checked_add(border).ok_or(BloomError::Invalid("output width overflow"))?,
        source[1].checked_add(border).ok_or(BloomError::Invalid("output height overflow"))?,
    ];
    let output_area = area(output)?;
    pixels = pixels
        .checked_add(output_area.checked_mul(3).ok_or(BloomError::Invalid("output memory overflow"))?)
        .ok_or(BloomError::Invalid("total memory overflow"))?;
    work = work
        .checked_add(output_area.checked_mul(64).ok_or(BloomError::Invalid("composite work overflow"))?)
        .ok_or(BloomError::Invalid("total work overflow"))?;
    let total_bytes = pixels.checked_mul(16).ok_or(BloomError::Invalid("byte count overflow"))?;
    if total_bytes > MAX_TOTAL_BYTES || work > MAX_WORK {
        return Err(BloomError::Unsupported(format!("{total_bytes} bytes / {work} work units; limits {MAX_TOTAL_BYTES} / {MAX_WORK}")));
    }
    Ok(BloomPlan { source, output, atlas, pad, levels, threshold, knee, gain, view, operation, space, linear, luma, total_bytes, work })
}

pub fn validate_input(input: &Buf, plan: &BloomPlan) -> Result<(), BloomError> {
    if [input.img.width, input.img.height] != plan.source || u64::try_from(input.img.data.len()).ok() != Some(area(plan.source)?) {
        return Err(BloomError::Invalid("malformed image storage"));
    }
    validate_offset(input.offset)
}

/// Geometry shared by host-backed and resident texture inputs.
pub fn validate_offset(offset: [f64; 2]) -> Result<(), BloomError> {
    if offset.iter().any(|value| !value.is_finite() || value.abs() > 1e12) {
        return Err(BloomError::Invalid("buffer offset must be finite and bounded"));
    }
    Ok(())
}

fn allocate(size: [u32; 2]) -> Result<Image, BloomError> {
    let len = usize::try_from(area(size)?).map_err(|_| BloomError::Invalid("image storage exceeds address space"))?;
    let mut data = vec![];
    data.try_reserve_exact(len).map_err(|error| BloomError::Allocation(error.to_string()))?;
    data.resize(len, [0.0; 4]);
    Ok(Image { width: size[0], height: size[1], data })
}

fn sane(value: f32, headroom: f32) -> f32 {
    if value.is_finite() { value.clamp(-headroom, headroom) } else { 0.0 }
}

/// Linear premultiplied source. Invisible hidden RGB never emits; invalid channels become zero.
pub fn linear_pixel(pixel: Px, plan: &BloomPlan) -> Px {
    let alpha = sane(pixel[3], 1.0).max(0.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let straight = [0, 1, 2].map(|channel| sane(sane(pixel[channel], INPUT_HEADROOM) / alpha, INPUT_HEADROOM));
    let straight = if plan.linear { straight } else { Conversion::linearize(plan.space).apply(straight) };
    let straight = straight.map(|value| sane(value, INPUT_HEADROOM));
    [straight[0] * alpha, straight[1] * alpha, straight[2] * alpha, alpha]
}

/// Continuous monotone quadratic knee around the threshold, followed by linear excess. The
/// factor is applied to premultiplied RGB once; alpha is not multiplied into RGB a second time.
pub fn emission(pixel: Px, plan: &BloomPlan) -> Px {
    let source = linear_pixel(pixel, plan);
    if source[3] <= 0.0 {
        return [0.0; 4];
    }
    let light = ((source[0] * plan.luma[0] + source[1] * plan.luma[1] + source[2] * plan.luma[2]) / source[3]).max(0.0);
    let mut excess = (light - plan.threshold).max(0.0);
    if plan.knee > 0.0 {
        let x = (light - plan.threshold + plan.knee).clamp(0.0, 2.0 * plan.knee);
        excess = excess.max(x * x / (4.0 * plan.knee));
    }
    let fraction = if light > 0.0 { (excess / light).clamp(0.0, 1.0) } else { 0.0 };
    source.map(|value| value * fraction)
}

fn encode(pixel: Px, plan: &BloomPlan) -> Px {
    let alpha = sane(pixel[3], 1.0).max(0.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let straight = [0, 1, 2].map(|channel| sane(pixel[channel] / alpha, 1e20));
    let straight = if plan.linear { straight } else { Conversion::delinearize(plan.space).apply(straight) };
    let rgb = straight.map(|value| sane(value, 1e20) * alpha);
    [rgb[0], rgb[1], rgb[2], alpha]
}

/// Linear Add or Screen with reference-white-bounded interaction, retaining HDR excess. The
/// union alpha gives transparent halos usable by ordinary source-over compositing/export.
pub fn composite(source: Px, glow: Px, plan: &BloomPlan) -> Px {
    let mut glow = glow.map(|value| value * plan.gain);
    glow[3] = glow[3].clamp(0.0, 1.0);
    if plan.view == 1 {
        return encode(glow, plan);
    }
    let source = linear_pixel(source, plan);
    let rgb = [0, 1, 2].map(|channel| {
        let mut value = source[channel] + glow[channel];
        if plan.operation == 1 {
            value -= source[channel].clamp(0.0, source[3]) * glow[channel].clamp(0.0, glow[3]);
        }
        value
    });
    encode([rgb[0], rgb[1], rgb[2], source[3] + glow[3] * (1.0 - source[3])], plan)
}

fn half(source: &Image) -> Result<Image, BloomError> {
    let mut output = allocate([source.width.div_ceil(2), source.height.div_ceil(2)])?;
    output.rows_mut().for_each(|(y, row)| {
        for (x, pixel) in row.iter_mut().enumerate() {
            let (x, y) = (x as i64 * 2, y as i64 * 2);
            let a = source.get(x, y);
            let b = source.get(x + 1, y);
            let c = source.get(x, y + 1);
            let d = source.get(x + 1, y + 1);
            *pixel = std::array::from_fn(|channel| (a[channel] + b[channel] + c[channel] + d[channel]) * 0.25);
        }
    });
    Ok(output)
}

fn copy(source: &Image, output: &mut Image, origin: [u32; 2]) -> Result<(), BloomError> {
    let right = origin[0].checked_add(source.width).ok_or(BloomError::Invalid("copy width overflow"))?;
    let bottom = origin[1].checked_add(source.height).ok_or(BloomError::Invalid("copy height overflow"))?;
    if right > output.width || bottom > output.height {
        return Err(BloomError::Invalid("copy exceeds planned image rectangle"));
    }
    for (y, row) in source.data.chunks(source.width as usize).enumerate() {
        let target_y = y.checked_add(origin[1] as usize).ok_or(BloomError::Invalid("copy row overflow"))?;
        let start = target_y
            .checked_mul(output.width as usize)
            .and_then(|start| start.checked_add(origin[0] as usize))
            .ok_or(BloomError::Invalid("copy offset overflow"))?;
        let end = start.checked_add(row.len()).ok_or(BloomError::Invalid("copy length overflow"))?;
        output.data.get_mut(start..end).ok_or(BloomError::Invalid("copy exceeds planned image bounds"))?.copy_from_slice(row);
    }
    Ok(())
}

fn sample(atlas: &Image, level: &BloomLevel, x: f32, y: f32) -> Px {
    let x = x / level.factor as f32 + level.pad as f32 - 0.5;
    let y = y / level.factor as f32 + level.pad as f32 - 0.5;
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let get = |x: i64, y: i64| {
        if x < 0 || y < 0 || x >= level.size[0] as i64 || y >= level.size[1] as i64 { [0.0; 4] } else { atlas.get(x, y + level.atlas_y as i64) }
    };
    let (a, b, c, d) = (get(x0, y0), get(x0 + 1, y0), get(x0, y0 + 1), get(x0 + 1, y0 + 1));
    std::array::from_fn(|channel| {
        let top = a[channel] + (b[channel] - a[channel]) * tx;
        let bottom = c[channel] + (d[channel] - c[channel]) * tx;
        top + (bottom - top) * ty
    })
}

/// Checked CPU runner. Unsupported requests and allocation failures remain visible to callers.
pub fn render_checked(ctx: &EffectCtx, input: Buf) -> Result<Buf, BloomError> {
    let plan = plan(ctx, [input.img.width, input.img.height], input.scale)?;
    validate_input(&input, &plan)?;
    let mut reduced = allocate(plan.source)?;
    reduced.data.par_iter_mut().zip(&input.img.data).for_each(|(output, &pixel)| *output = emission(pixel, &plan));
    if plan.view == 0 {
        reduced.data.par_iter_mut().for_each(|pixel| *pixel = encode(*pixel, &plan));
        return Ok(Buf { img: reduced, ..input });
    }
    let mut atlas = allocate(plan.atlas)?;
    let mut factor = 1;
    for level in &plan.levels {
        while factor < level.factor {
            reduced = half(&reduced)?;
            factor = factor.checked_mul(2).ok_or(BloomError::Invalid("pyramid factor overflow"))?;
        }
        let mut padded = allocate(level.size)?;
        copy(&reduced, &mut padded, [level.pad; 2])?;
        let blurred = effectcraft_raster::blur::box_passes_checked(&padded, level.radii, level.radii).map_err(BloomError::Allocation)?;
        copy(&blurred, &mut atlas, [0, level.atlas_y])?;
    }
    let mut output = allocate(plan.output)?;
    output.rows_mut().for_each(|(y, row)| {
        for (x, pixel) in row.iter_mut().enumerate() {
            let (sx, sy) = (x as f32 + 0.5 - plan.pad as f32, y as f32 + 0.5 - plan.pad as f32);
            let mut halo = [0.0_f32; 4];
            for level in &plan.levels {
                let sampled = sample(&atlas, level, sx, sy);
                for (target, sample) in halo.iter_mut().zip(sampled) {
                    *target += sample * level.weight;
                }
            }
            *pixel = composite(input.img.get(x as i64 - plan.pad as i64, y as i64 - plan.pad as i64), halo, &plan);
        }
    });
    Ok(Buf { img: output, offset: input.offset.map(|value| value + plan.pad as f64), scale: input.scale })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectEnv, Params};

    fn params(values: &[(&str, Value)]) -> Params {
        let mut params = Params { values: parameters().into_iter().map(|parameter| (parameter.id.to_owned(), parameter.default)).collect() };
        for (key, value) in values {
            params.values.insert((*key).to_owned(), value.clone());
        }
        params
    }

    fn context(params: &Params) -> EffectCtx<'_> {
        EffectCtx { params, time: 0.0, layer_size: [65.0; 2], seed: 0, adjustment: false, env: EffectEnv { working_linear: true, ..Default::default() } }
    }

    fn buffer(image: Image, scale: f64) -> Buf {
        Buf { img: image, offset: [3.25, -7.5], scale }
    }

    #[test]
    fn bloom_plan_rejects_hostile_controls_and_dimensions_before_allocation() {
        for (name, value) in [
            ("radius", f64::NAN),
            ("radius", f64::INFINITY),
            ("radius", -1.0),
            ("radius", 1025.0),
            ("exposure", 17.0),
            ("amount", 1001.0),
            ("threshold", -1.0),
            ("softKnee", 1.01),
            ("spread", f64::NEG_INFINITY),
            ("quality", 0.5),
            ("view", 3.0),
            ("operation", 2.0),
        ] {
            let parameters = params(&[(name, num(value))]);
            assert!(plan(&context(&parameters), [65; 2], 1.0).is_err(), "{name}={value}");
        }
        for value in [Value::Str("32".into()), Value::Bool(true), Value::Vec2([32.0; 2])] {
            let parameters = params(&[("radius", value)]);
            assert!(plan(&context(&parameters), [65; 2], 1.0).is_err());
        }
        let parameters = params(&[]);
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert!(plan(&context(&parameters), [65; 2], scale).is_err());
        }
        for size in [[0, 65], [u32::MAX, 1], [16384, 16384], [8000, 2000]] {
            assert!(plan(&context(&parameters), size, 1.0).is_err(), "{size:?}");
        }
        let parameters = params(&[("radius", num(0.0)), ("view", Value::Enum(0))]);
        let small = plan(&context(&parameters), [1; 2], 4.0).unwrap();
        assert_eq!(small.pad, 0);
        assert!(small.levels.is_empty());
        assert!(small.total_bytes <= MAX_TOTAL_BYTES && small.work <= MAX_WORK);
    }

    #[test]
    fn bloom_plan_is_deterministic_normalized_and_resolution_scaled() {
        for quality in 0..=2 {
            for scale in [0.25, 0.5, 1.0, 2.0, 4.0] {
                let parameters = params(&[("radius", num(64.0)), ("quality", Value::Enum(quality))]);
                let first = plan(&context(&parameters), [65, 37], scale).unwrap();
                let again = plan(&context(&parameters), [65, 37], scale).unwrap();
                assert_eq!(first.output, again.output);
                assert_eq!(first.total_bytes, again.total_bytes);
                assert_eq!(first.work, again.work);
                assert!((first.levels.iter().map(|level| level.weight).sum::<f32>() - 1.0).abs() < 1e-7);
                for level in &first.levels {
                    assert_eq!(level.source, first.source.map(|dimension| dimension.div_ceil(level.factor)));
                    assert!(level.radii.iter().all(|radius| *radius <= 16));
                    assert!(level.size[1] + level.atlas_y <= first.atlas[1]);
                    assert!(level.size[0] <= first.atlas[0]);
                    assert!(level.weight >= 0.0);
                }
                assert_eq!(first.output, first.source.map(|dimension| dimension + first.pad * 2));
            }
        }
        let parameters = params(&[("radius", num(1024.0))]);
        let error = plan(&context(&parameters), [65; 2], 64.0).unwrap_err();
        assert!(error.to_string().contains("4096"));
    }

    #[test]
    fn bloom_rejects_malformed_pixels_offsets_and_storage() {
        let parameters = params(&[]);
        let ctx = context(&parameters);
        let malformed = Image { width: 65, height: 37, data: vec![[1.0; 4]; 3] };
        assert!(render_checked(&ctx, buffer(malformed, 1.0)).is_err());
        for offset in [[f64::NAN, 0.0], [0.0, f64::INFINITY], [1e13, 0.0]] {
            let mut input = buffer(Image::new(3, 3), 1.0);
            input.offset = offset;
            assert!(render_checked(&ctx, input).is_err());
        }
        let p = plan(&ctx, [1; 2], 1.0).unwrap();
        for pixel in [[f32::NAN; 4], [f32::MAX, f32::INFINITY, f32::NEG_INFINITY, 0.0], [1.0, 2.0, 3.0, -1.0]] {
            assert_eq!(emission(pixel, &p), [0.0; 4]);
        }
        let emitted = emission([f32::MAX, f32::INFINITY, f32::NEG_INFINITY, 0.5], &p);
        assert!(emitted.iter().all(|channel| channel.is_finite()));
    }

    #[test]
    fn bloom_threshold_knee_is_continuous_monotone_and_alpha_is_applied_once() {
        let parameters = params(&[("softKnee", num(0.25))]);
        let p = plan(&context(&parameters), [1; 2], 1.0).unwrap();
        let mut previous = 0.0;
        for step in 0..=2000 {
            let brightness = step as f32 / 1000.0;
            let next = emission([brightness, brightness, brightness, 1.0], &p)[0];
            assert!(next + 1e-7 >= previous);
            assert!((next - previous).abs() <= 0.0011, "jump at {brightness}: {next} vs {previous}");
            previous = next;
        }
        assert_eq!(emission([0.0, 0.0, 0.0, 1.0], &p), [0.0; 4]);
        assert_eq!(emission([16.0, 4.0, 2.0, 0.0], &p), [0.0; 4]);
        let opaque = emission([4.0, 4.0, 4.0, 1.0], &p);
        let half_alpha = emission([2.0, 2.0, 2.0, 0.5], &p);
        for channel in 0..4 {
            assert!((half_alpha[channel] - opaque[channel] * 0.5).abs() < 1e-6);
        }
        assert!(opaque[0] > 1.0, "HDR emission must survive thresholding");
    }

    /// Independently specified normalized full-resolution Gaussian mixture, used for energy
    /// and radial second moment. It does not reuse the core's box filter or pyramid grid.
    fn reference_moments(radius: f64, spread: f64) -> (f64, f64) {
        let inner = [0.55, 0.30, 0.12, 0.03];
        let outer = [0.05, 0.15, 0.30, 0.50];
        let mut mass = 0.0;
        let mut radial_moment = 0.0;
        for index in 0..4 {
            let sigma = radius / 8.0 * 2.0_f64.powi(index as i32);
            let weight = inner[index] + (outer[index] - inner[index]) * spread;
            let support = (sigma * 6.0).ceil() as i64;
            let samples: Vec<_> = (-support..=support).map(|x| (x, (-0.5 * (x as f64 / sigma).powi(2)).exp())).collect();
            let normalization: f64 = samples.iter().map(|(_, value)| value).sum();
            mass += weight * samples.iter().map(|(_, value)| value / normalization).sum::<f64>().powi(2);
            radial_moment += weight * 2.0 * samples.iter().map(|(x, value)| (*x as f64).powi(2) * value / normalization).sum::<f64>();
        }
        (mass, radial_moment)
    }

    #[test]
    fn bloom_hdr_impulse_energy_and_profile_match_an_independent_reference() {
        for radius in [16.0, 64.0, 256.0] {
            let parameters = params(&[("radius", num(radius)), ("threshold", num(0.0)), ("quality", Value::Enum(2)), ("view", Value::Enum(1))]);
            let mut image = Image::new(65, 65);
            image.set(32, 32, [16.0, 4.0, 1.0, 1.0]);
            let output = render_checked(&context(&parameters), buffer(image, 1.0)).unwrap();
            let p = plan(&context(&parameters), [65; 2], 1.0).unwrap();
            let center = 32.5 + p.pad as f64;
            let mut mass = [0.0_f64; 4];
            let mut moment = 0.0;
            for (index, pixel) in output.img.data.iter().enumerate() {
                let x = (index % output.img.width as usize) as f64 + 0.5 - center;
                let y = (index / output.img.width as usize) as f64 + 0.5 - center;
                for channel in 0..4 {
                    mass[channel] += pixel[channel] as f64;
                }
                moment += (x * x + y * y) * pixel[3] as f64;
                assert!(pixel.iter().all(|value| value.is_finite()));
            }
            let (reference_mass, reference_moment) = reference_moments(radius, 0.5);
            for (actual, expected) in mass.into_iter().zip([16.0, 4.0, 1.0, 1.0]) {
                assert!((actual / expected - reference_mass).abs() <= 0.01, "radius {radius}, energy {actual}/{expected}");
            }
            assert!((moment / reference_moment - 1.0).abs() <= 0.25, "radius {radius}, radial moment {moment} vs {reference_moment}");
        }
    }

    #[test]
    fn bloom_odd_edges_and_preview_resolutions_preserve_energy_and_offset() {
        for quality in 0..=2 {
            for scale in [0.25, 0.5, 1.0, 2.0] {
                let parameters = params(&[("radius", num(16.0)), ("threshold", num(0.0)), ("quality", Value::Enum(quality)), ("view", Value::Enum(1))]);
                for position in [[0, 0], [36, 18], [64, 36]] {
                    let mut image = Image::new(65, 37);
                    image.set(position[0], position[1], [4.0, 2.0, 1.0, 0.5]);
                    let output = render_checked(&context(&parameters), buffer(image, scale)).unwrap();
                    let p = plan(&context(&parameters), [65, 37], scale).unwrap();
                    assert_eq!(output.offset, [3.25 + p.pad as f64, -7.5 + p.pad as f64]);
                    assert_eq!(output.scale, scale);
                    let mass: f64 = output.img.data.iter().map(|pixel| pixel[0] as f64).sum();
                    assert!((mass / 4.0 - 1.0).abs() < 0.01, "quality {quality} scale {scale} edge {position:?}: {mass}");
                }
            }
        }
    }

    #[test]
    fn bloom_color_spaces_select_the_same_physical_radiance_without_double_decode() {
        let parameters = params(&[("radius", num(0.0)), ("threshold", num(0.5)), ("softKnee", num(0.0)), ("view", Value::Enum(0))]);
        let physical = [4.0, 1.5, 0.75];
        let mut baseline: Option<[f32; 3]> = None;
        for (space, linear) in [(ColorSpace::Srgb, true), (ColorSpace::Srgb, false), (ColorSpace::Rec2020, false), (ColorSpace::AcesCg, false)] {
            let mut ctx = context(&parameters);
            ctx.env.working_space = Some(space);
            ctx.env.working_linear = linear;
            let authored = Conversion::new(ColorSpace::Srgb, true, space, linear).map(|conversion| conversion.apply(physical)).unwrap_or(physical);
            let pixel = [authored[0] * 0.5, authored[1] * 0.5, authored[2] * 0.5, 0.5];
            let p = plan(&ctx, [1; 2], 1.0).unwrap();
            let emitted = emission(pixel, &p);
            let common = Conversion::new(space, true, ColorSpace::Srgb, true)
                .map(|conversion| conversion.apply([emitted[0], emitted[1], emitted[2]]))
                .unwrap_or([emitted[0], emitted[1], emitted[2]]);
            if let Some(reference) = baseline {
                for (actual, expected) in common.into_iter().zip(reference) {
                    assert!((actual - expected).abs() < 2e-5, "{space:?} linear={linear}: {actual} vs {expected}");
                }
            } else {
                baseline = Some(common);
            }
            let output = render_checked(&ctx, buffer(Image::filled(1, 1, pixel), 1.0)).unwrap();
            let encoded = output.img.get(0, 0);
            let decoded = linear_pixel(encoded, &p);
            for (actual, expected) in decoded.into_iter().zip(emitted) {
                assert!((actual - expected).abs() < 2e-5);
            }
        }
    }

    #[test]
    fn bloom_views_compositing_zero_amount_and_adjustment_bounds_are_explicit() {
        let parameters = params(&[("radius", num(0.0)), ("threshold", num(0.0)), ("amount", num(0.0))]);
        let ctx = context(&parameters);
        let pixel = [2.0, 1.0, 0.25, 0.5];
        let output = render_checked(&ctx, buffer(Image::filled(3, 3, pixel), 1.0)).unwrap();
        assert!(output.img.data.iter().all(|actual| *actual == pixel));
        let parameters = params(&[("radius", num(0.0)), ("threshold", num(0.0))]);
        let mut p = plan(&context(&parameters), [1; 2], 1.0).unwrap();
        let glow = [2.0, 0.25, 0.1, 0.5];
        let added = composite(pixel, glow, &p);
        for (actual, expected) in added.into_iter().zip([4.0, 1.25, 0.35, 0.75]) {
            assert!((actual - expected).abs() < 1e-6);
        }
        p.operation = 1;
        let screened = composite(pixel, glow, &p);
        for (actual, expected) in screened.into_iter().zip([3.75, 1.125, 0.325, 0.75]) {
            assert!((actual - expected).abs() < 1e-6);
        }
        p.view = 1;
        assert_eq!(composite(pixel, glow, &p), glow);
        let parameters = params(&[]);
        let mut ctx = context(&parameters);
        ctx.adjustment = true;
        let p = plan(&ctx, [65, 37], 1.0).unwrap();
        assert_eq!(p.pad, 0);
        assert_eq!(p.output, p.source);
    }
}

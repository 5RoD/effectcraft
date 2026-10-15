//! Checked Timewarp preparation. Source times share unique frames; shutter outputs stream
//! through one accumulator in their original order, with checked geometry, storage and pixels.

use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use effectcraft_raster::{Image, flow::Flow};
use effectcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use rayon::prelude::*;

use crate::time_fx::{PixelMotion, layered, matte_image, matte_of, times_mask, timewarp_flow, timewarp_flow_search, timewarp_interpolate};
use crate::{Buf, EffectCtx, LayerPixels};

pub const ID: &str = "ec.time.timewarp";
pub const PMB_ID: &str = "ec.time.pixelmotionblur";
pub const MAX_DIMENSION: u32 = 16_384;
pub const MAX_IMAGE_PIXELS: u64 = 32 * 1024 * 1024;
/// Source/prepared RGBA magnitude bound; matching and brightness correction use f32.
pub const MAX_CHANNEL_MAGNITUDE: f32 = 1e12;
pub const MAX_TOTAL_BYTES: u64 = 1536 * 1024 * 1024;
/// Conservative SAD comparisons, smoothing neighbours and interpolation taps per invocation.
pub const MAX_WORK: u64 = 32_000_000_000;
pub const MAX_SAMPLES: usize = 64;
const MAX_PREPARED_FRAMES: usize = MAX_SAMPLES * 2 * 4;
const MAX_PREPARED_FLOWS: usize = MAX_SAMPLES * 2;
const MAX_OFFSET: f64 = 1e12;

fn invalid(message: impl std::fmt::Display) -> String {
    format!("Timewarp: {message}")
}

fn add(a: u64, b: u64) -> Result<u64, String> {
    a.checked_add(b).ok_or_else(|| invalid("budget arithmetic overflow"))
}

fn mul(a: u64, b: u64) -> Result<u64, String> {
    a.checked_mul(b).ok_or_else(|| invalid("budget arithmetic overflow"))
}

fn sampling_domain(coordinates: &[f64]) -> Result<(), String> {
    if coordinates.iter().any(|value| !value.is_finite() || value.abs() > MAX_OFFSET) {
        return Err(invalid("resampling coordinates exceed the finite +/-1e12-pixel supported range"));
    }
    Ok(())
}

fn area(width: u32, height: u32) -> Result<u64, String> {
    let pixels = mul(u64::from(width), u64::from(height))?;
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION || pixels > MAX_IMAGE_PIXELS {
        return Err(invalid(format!("unsupported frame {width} × {height}; maximum dimension {MAX_DIMENSION}, maximum pixels {MAX_IMAGE_PIXELS}")));
    }
    Ok(pixels)
}

fn finite_pixels(image: &Image) -> Result<(), String> {
    if image.data.par_iter().any(|pixel| pixel.iter().any(|channel| !channel.is_finite() || channel.abs() > MAX_CHANNEL_MAGNITUDE)) {
        return Err(invalid("frame contains non-finite RGBA channels or exceeds supported +/-1e12 magnitude"));
    }
    Ok(())
}

fn validate_input_geometry(input: &Buf) -> Result<(), String> {
    let pixels = area(input.img.width, input.img.height)?;
    if usize::try_from(pixels).ok() != Some(input.img.data.len()) {
        return Err(invalid("frame storage does not match its dimensions"));
    }
    if !input.scale.is_finite() || input.scale <= 0.0 || input.offset.iter().any(|v| !v.is_finite() || v.abs() > MAX_OFFSET) {
        return Err(invalid("frame scale must be positive and finite, and offsets must be finite and within +/-1e12 pixels"));
    }
    Ok(())
}

/// Validate host geometry, storage and RGBA before resampling or GPU upload.
/// This performs one source-channel scan and does not allocate.
pub fn validate_input(input: &Buf) -> Result<(), String> {
    validate_input_geometry(input)?;
    finite_pixels(&input.img)
}

/// Controls are checked even when the host has no frames. A malformed expression must not
/// become a saturating integer cast or a successful pass-through.
pub fn validate_controls(ctx: &EffectCtx) -> Result<(), String> {
    validate_controls_for(ctx, ID)
}

fn validate_controls_for(ctx: &EffectCtx, id: &str) -> Result<(), String> {
    checked_time(ctx.time)?;
    checked_time(ctx.env.comp_time)?;
    if !ctx.env.frame_rate.is_finite() || ctx.env.frame_rate < 0.0 || !ctx.fps().is_finite() || ctx.fps() > 1000.0 {
        return Err(invalid("frame rate must be finite and within (0, 1000] (zero means unknown)"));
    }
    if ctx.layer_size.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err(invalid("layer dimensions must be finite and nonnegative"));
    }
    let spec = crate::find(id).ok_or_else(|| invalid("effect registry is unavailable"))?;
    for parameter in &spec.params {
        let Some(value) = ctx.params.get(parameter.id) else { continue };
        let valid = match (&parameter.ui, value) {
            (ParamUi::Slider { min, max, .. }, Value::Scalar(value)) => value.is_finite() && (*min..=*max).contains(value),
            (ParamUi::Popup { options }, Value::Enum(value)) => (*value as usize) < options.len(),
            (ParamUi::Popup { options }, Value::Scalar(value)) => value.is_finite() && value.fract() == 0.0 && *value >= 0.0 && *value < options.len() as f64,
            (ParamUi::Checkbox, Value::Bool(_)) | (ParamUi::Layer, Value::Layer(_)) => true,
            _ => false,
        };
        if !valid {
            return Err(invalid(format!("{} must match its documented type and range", parameter.name)));
        }
    }
    Ok(())
}

fn checked_time(seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() || seconds < Tick::MIN.seconds() || seconds > Tick::MAX.seconds() {
        return Err(invalid("source/comp time exceeds the supported finite Tick range"));
    }
    Ok(())
}

/// Broadcast boundaries are exact Tick keys. Do not snap custom frame rates to a nearby
/// broadcast rate: their first representable boundary is the ceiling of frame / fps.
fn frame_key(frame: f64, fps: f64) -> Result<Tick, String> {
    let seconds = frame / fps;
    checked_time(seconds)?;
    let key = if let Some(rate) = broadcast_rate(fps) { rate.tick_of(frame as i64) } else { Tick((seconds * TICKS_PER_SECOND as f64).ceil() as i64) };
    if !(Tick::MIN..=Tick::MAX).contains(&key) {
        return Err(invalid("source frame boundary exceeds the supported Tick range"));
    }
    Ok(key)
}

fn broadcast_rate(fps: f64) -> Option<FrameRate> {
    FrameRate::COMMON.into_iter().find(|rate| rate.as_f64() == fps)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimewarpGrid {
    pub width: u32,
    pub height: u32,
    pub offset: [f64; 2],
    pub scale: f64,
}

impl TimewarpGrid {
    fn wrap(self, img: Image) -> Buf {
        Buf { img, offset: self.offset, scale: self.scale }
    }

    fn image(self) -> Result<Image, String> {
        let pixels = usize::try_from(area(self.width, self.height)?).map_err(|_| invalid("frame exceeds address space"))?;
        let mut data = Vec::new();
        data.try_reserve_exact(pixels).map_err(|error| invalid(format!("cannot allocate a planned frame: {error}")))?;
        data.resize(pixels, [0.0; 4]);
        Ok(Image { width: self.width, height: self.height, data })
    }

    /// Consume already aligned pixels, avoiding the former full-frame clone.
    fn place(self, source: Option<Buf>) -> Result<Image, String> {
        let Some(source) = source else { return self.image() };
        let [x0, y0] = self.offset.map(|v| -v);
        if (source.scale - self.scale).abs() < 1e-9
            && source.img.width == self.width
            && source.img.height == self.height
            && (source.offset[0] + x0).abs() < 1e-6
            && (source.offset[1] + y0).abs() < 1e-6
        {
            return Ok(source.img);
        }
        let k = source.scale / self.scale;
        let same_scale = (k - 1.0).abs() < 1e-9;
        let dx = source.offset[0] + x0;
        let dy = source.offset[1] + y0;
        sampling_domain(&[dx, dy, (self.width as f64 + 0.5) * k + dx, (self.height as f64 + 0.5) * k + dy])?;
        let mut output = self.image()?;
        output.rows_mut().for_each(|(y, row)| {
            for (x, pixel) in row.iter_mut().enumerate() {
                *pixel = if same_scale && dx.fract() == 0.0 && dy.fract() == 0.0 {
                    source.img.get(x as i64 + dx as i64, y as i64 + dy as i64)
                } else {
                    let sx = (x as f64 + 0.5) * k + dx - 0.5;
                    let sy = (y as f64 + 0.5) * k + dy - 0.5;
                    source.img.sample_bilinear(sx + 0.5, sy + 0.5)
                };
            }
        });
        Ok(output)
    }

    /// fit_layer(..., stretch=false), using geometry without a blank full-resolution Buf.
    fn fit(self, ctx: &EffectCtx, source: LayerPixels) -> Result<Image, String> {
        // fetch_layer checked these owned source channels before budgeting/fitting.
        validate_input_geometry(&source.buf)?;
        if source.size.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err(invalid("matte/warp layer dimensions are invalid"));
        }
        let (dx, dy) = ((source.size[0] - ctx.layer_size[0]) * 0.5, (source.size[1] - ctx.layer_size[1]) * 0.5);
        let inv = 1.0 / self.scale.max(1e-9);
        let coordinates = |x: f64, y: f64| {
            [
                ((x + 0.5 - self.offset[0]) * inv + dx) * source.buf.scale + source.buf.offset[0],
                ((y + 0.5 - self.offset[1]) * inv + dy) * source.buf.scale + source.buf.offset[1],
            ]
        };
        sampling_domain(&coordinates(0.0, 0.0))?;
        sampling_domain(&coordinates((self.width - 1) as f64, (self.height - 1) as f64))?;
        let mut output = self.image()?;
        output.rows_mut().for_each(|(y, row)| {
            for (x, pixel) in row.iter_mut().enumerate() {
                let px = (x as f64 + 0.5 - self.offset[0]) * inv;
                let py = (y as f64 + 0.5 - self.offset[1]) * inv;
                *pixel =
                    source.buf.img.sample_bilinear((px + dx) * source.buf.scale + source.buf.offset[0], (py + dy) * source.buf.scale + source.buf.offset[1]);
            }
        });
        Ok(output)
    }
}

fn union(bufs: &[Option<Buf>]) -> Result<Option<TimewarpGrid>, String> {
    let Some(first) = bufs.iter().flatten().next() else { return Ok(None) };
    let scale = first.scale;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for source in bufs.iter().flatten() {
        // plan_for_input checked every owned host frame before this geometry walk.
        validate_input_geometry(source)?;
        let k = scale / source.scale;
        let edges =
            [-source.offset[0] * k, -source.offset[1] * k, (source.img.width as f64 - source.offset[0]) * k, (source.img.height as f64 - source.offset[1]) * k];
        if edges.iter().any(|value| !value.is_finite() || value.abs() > MAX_OFFSET) {
            return Err(invalid("source extent or relative render scale is unsupported"));
        }
        x0 = x0.min(edges[0]);
        y0 = y0.min(edges[1]);
        x1 = x1.max(edges[2]);
        y1 = y1.max(edges[3]);
    }
    let (x0, y0) = (x0.floor(), y0.floor());
    let dimensions = [x1.ceil() - x0, y1.ceil() - y0];
    if dimensions.iter().any(|dimension| !dimension.is_finite() || *dimension > MAX_DIMENSION as f64 || *dimension <= 0.0) {
        return Err(invalid("union of source frames exceeds the supported dimensions"));
    }
    let (width, height) = (dimensions[0] as u32, dimensions[1] as u32);
    area(width, height)?;
    Ok(Some(TimewarpGrid { width, height, offset: [-x0, -y0], scale }))
}

#[derive(Clone, Copy, Debug)]
pub enum TwStep {
    Whole(usize),
    Mix(usize, usize, f32),
    Motion { a: usize, b: usize, w: f32, flow: usize },
    Layered { fg: [usize; 3], bg: [usize; 3], w: f32, show: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct TimewarpBudget {
    pub total_bytes: u64,
    pub work: u64,
    pub frame_limit: usize,
    pub flow_limit: usize,
    preparation_bytes: u64,
}

#[derive(Clone, Copy)]
struct BudgetRequest {
    input_pixels: u64,
    unique: usize,
    pairs: usize,
    steps: usize,
    method: u32,
    matte: bool,
    warp: bool,
    show: u32,
    radius: u32,
}

/// A conservative peak across preparation and execution, rather than a sum of scratch whose
/// lifetimes do not overlap. Includes source placement, masked frames, matte/warp preparation,
/// matching pyramids/vectors, uploaded unique frames, five private GPU step/accumulator images
/// and output/readback staging. Renderer/decoder caches and other effects are separate.
fn budget(grid: TimewarpGrid, raw_bytes: u64, pm: &PixelMotion, request: BudgetRequest) -> Result<TimewarpBudget, String> {
    let BudgetRequest { input_pixels, unique, pairs, steps, method, matte, warp, show, radius } = request;
    let pixels = area(grid.width, grid.height)?;
    if unique == 0 || unique > MAX_SAMPLES * 2 || steps == 0 || steps > MAX_SAMPLES || pairs > steps {
        return Err(invalid("unsupported source frame or shutter sample count"));
    }
    if pm.tile < 2 || pm.window < 4 || pm.tile > MAX_DIMENSION || pm.window > MAX_DIMENSION || pm.iterations > 200 || radius > 64 {
        return Err(invalid("scaled motion spacing/window or smoothing iterations exceed the supported range"));
    }
    let derived = if matte {
        if method == 2 {
            unique.checked_mul(if show == 3 { 3 } else { 2 })
        } else if show > 0 {
            Some(unique)
        } else {
            Some(0)
        }
    } else {
        Some(0)
    }
    .ok_or_else(|| invalid("derived frame count overflow"))?;
    let frame_limit = unique.checked_add(derived).ok_or_else(|| invalid("frame count overflow"))?;
    let flow_limit = if method == 2 { pairs.checked_mul(if matte && !warp { 2 } else { 1 }) } else { Some(0) }.ok_or_else(|| invalid("flow count overflow"))?;
    let vectors = mul(u64::from(grid.width.div_ceil(pm.tile)), u64::from(grid.height.div_ceil(pm.tile)))?;
    let retained_vectors = mul(mul(vectors, 8)?, flow_limit as u64)?;
    let image_bytes = mul(pixels, 16)?;
    let input_bytes = mul(if input_pixels == 0 { pixels } else { input_pixels }, 16)?;
    let placement = add(add(raw_bytes, mul(image_bytes, unique as u64)?)?, input_bytes)?;
    let prep_images = add(frame_limit as u64, if warp { unique as u64 } else { 0 })?;
    let prep = add(
        input_bytes,
        add(
            add(mul(image_bytes, add(prep_images, 8)?)?, if matte { mul(mul(pixels, 4)?, unique as u64)? } else { 0 })?,
            add(retained_vectors, mul(vectors, 64)?)?,
        )?,
    )?;
    let copies = if pm.crops == [0; 4] { 2 } else { 3 };
    let execution = add(mul(input_bytes, 2)?, add(mul(image_bytes, add(mul(frame_limit as u64, copies)?, 8)?)?, mul(retained_vectors, 2)?)?)?;
    let total_bytes = placement.max(prep).max(execution);
    let mut matching = 0;
    if grid.width >= pm.tile && grid.height >= pm.tile && grid.width >= pm.window && grid.height >= pm.window {
        let mut levels = 1;
        let (mut w, mut h) = (grid.width, grid.height);
        while levels < 4 && w / 2 >= pm.tile.max(pm.window) * 2 && h / 2 >= pm.tile.max(pm.window) * 2 {
            w /= 2;
            h /= 2;
            levels += 1;
        }
        for level in 0..levels {
            let window = u64::from((pm.window >> level).max(4));
            let candidates = if level == levels - 1 { mul(add(mul(u64::from(radius), 2)?, 1)?, add(mul(u64::from(radius), 2)?, 1)?)? } else { 54 };
            matching = add(matching, mul(mul(window, window)?, candidates)?)?;
        }
        matching = add(matching, mul(mul(u64::from(pm.window), u64::from(pm.window))?, 5)?)?;
    }
    let smoothing = add(if pm.local_smoothness > 0.0 { mul(u64::from(pm.iterations), 9)? } else { 0 }, if pm.global_smoothness > 0.0 { 49 } else { 0 })?;
    let flow_work = mul(mul(vectors, add(matching, add(smoothing, 81)?)?)?, flow_limit as u64)?;
    let taps = if method == 2 { if pm.extreme { 32 } else { 8 } } else { 2 };
    let step_work = mul(mul(pixels, steps as u64)?, add(mul(taps, if matte && method == 2 { 2 } else { 1 })?, 8)?)?;
    let work = add(flow_work, add(step_work, mul(pixels, add(unique as u64, derived as u64)?)?)?)?;
    if total_bytes > MAX_TOTAL_BYTES || work > MAX_WORK {
        return Err(invalid(format!(
            "request needs up to {total_bytes} bytes and {work} work units; limits are {MAX_TOTAL_BYTES} bytes and {MAX_WORK} units. Reduce shutter span, resolution, Vector Detail, Block Size or smoothing; quality is never silently reduced"
        )));
    }
    Ok(TimewarpBudget { total_bytes, work, frame_limit, flow_limit, preparation_bytes: prep })
}

pub struct TimewarpPlan {
    pub grid: TimewarpGrid,
    /// One owned pixel buffer per unique source/derived frame; steps share indices.
    pub frames: Vec<Image>,
    pub steps: Vec<(TwStep, f32)>,
    pub flows: Vec<Flow>,
    pub pm: PixelMotion,
    pub budget: TimewarpBudget,
}

type KeyedFlow = (([usize; 2], bool), Flow);

impl TimewarpPlan {
    fn frame(&self, index: usize) -> Result<&Image, String> {
        self.frames.get(index).ok_or_else(|| invalid("step references a missing frame"))
    }
    fn flow(&self, index: usize) -> Result<&Flow, String> {
        self.flows.get(index).ok_or_else(|| invalid("step references a missing motion field"))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.steps.is_empty()
            || self.steps.len() > MAX_SAMPLES
            || self.frames.is_empty()
            || self.frames.len() > MAX_PREPARED_FRAMES
            || self.flows.len() > MAX_PREPARED_FLOWS
            || self.budget.frame_limit > MAX_PREPARED_FRAMES
            || self.budget.flow_limit > MAX_PREPARED_FLOWS
            || self.frames.len() > self.budget.frame_limit
            || self.flows.len() > self.budget.flow_limit
        {
            return Err(invalid("malformed prepared frame/sample count"));
        }
        if self.budget.total_bytes > MAX_TOTAL_BYTES || self.budget.work > MAX_WORK {
            return Err(invalid("prepared plan exceeds its byte/work budget"));
        }
        let pixels = usize::try_from(area(self.grid.width, self.grid.height)?).map_err(|_| invalid("frame exceeds address space"))?;
        // Public plans can be modified after preparation. A forged declaration cannot
        // bypass absolute limits or understate even the retained/execution storage.
        let image_bytes = mul(pixels as u64, 16)?;
        let vector_bytes = self.flows.iter().try_fold(0, |bytes, flow| add(bytes, mul(flow.v.len() as u64, 8)?))?;
        let retained = add(mul(image_bytes, self.frames.len() as u64)?, vector_bytes)?;
        let copies = if self.pm.crops == [0; 4] { 2 } else { 3 };
        let execution = add(mul(image_bytes, add(mul(self.frames.len() as u64, copies)?, 8)?)?, mul(vector_bytes, 2)?)?;
        if retained > self.budget.total_bytes || execution > self.budget.total_bytes || execution > MAX_TOTAL_BYTES {
            return Err(invalid("prepared storage exceeds the declared/absolute byte budget"));
        }
        let work = self.steps.iter().try_fold(0, |work, (step, _)| {
            let motion_taps = if self.pm.extreme { 32 } else { 8 };
            let taps = match step {
                TwStep::Whole(_) => 1,
                TwStep::Mix(..) => 2,
                TwStep::Motion { .. } => motion_taps,
                TwStep::Layered { .. } => motion_taps * 2,
            };
            add(work, mul(pixels as u64, taps + 8)?)
        })?;
        if work > self.budget.work || work > MAX_WORK {
            return Err(invalid("prepared interpolation exceeds the declared/absolute work budget"));
        }
        if !self.grid.scale.is_finite()
            || self.grid.scale <= 0.0
            || self.grid.offset.iter().any(|value| !value.is_finite() || value.abs() > MAX_OFFSET)
            || self.pm.tile < 2
            || self.pm.tile > MAX_DIMENSION
            || self.pm.window < 4
            || self.pm.window > MAX_DIMENSION
            || self.pm.iterations > 200
        {
            return Err(invalid("prepared geometry or motion settings are invalid"));
        }
        if [self.pm.global_smoothness, self.pm.local_smoothness, self.pm.error_threshold]
            .iter()
            .chain(self.pm.weights.iter())
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(invalid("prepared motion weights are invalid"));
        }
        for frame in &self.frames {
            if frame.width != self.grid.width || frame.height != self.grid.height || frame.data.len() != pixels {
                return Err(invalid("prepared frame storage differs from the output grid"));
            }
            finite_pixels(frame)?;
        }
        for flow in &self.flows {
            if flow.block < 2
                || flow.block > MAX_DIMENSION
                || flow.width != self.grid.width
                || flow.height != self.grid.height
                || flow.cols != self.grid.width.div_ceil(flow.block) as usize
                || flow.rows != self.grid.height.div_ceil(flow.block) as usize
                || flow.cols.checked_mul(flow.rows) != Some(flow.v.len())
                || flow.v.iter().flatten().any(|value| !value.is_finite() || value.abs() > (MAX_DIMENSION * 4) as f32)
            {
                return Err(invalid("prepared motion field storage or vectors are invalid"));
            }
        }
        for (step, weight) in &self.steps {
            if !weight.is_finite() || !(0.0..=1.0).contains(weight) {
                return Err(invalid("shutter weight is invalid"));
            }
            let fraction = match *step {
                TwStep::Whole(index) => {
                    self.frame(index)?;
                    None
                }
                TwStep::Mix(a, b, fraction) => {
                    self.frame(a)?;
                    self.frame(b)?;
                    Some(fraction)
                }
                TwStep::Motion { a, b, w, flow } => {
                    self.frame(a)?;
                    self.frame(b)?;
                    self.flow(flow)?;
                    Some(w)
                }
                TwStep::Layered { fg, bg, w, show } => {
                    if show > 3 {
                        return Err(invalid("prepared Show choice is invalid"));
                    }
                    for indices in [fg, bg] {
                        self.frame(indices[0])?;
                        self.frame(indices[1])?;
                        self.flow(indices[2])?;
                    }
                    Some(w)
                }
            };
            if fraction.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
                return Err(invalid("prepared interpolation fraction is invalid"));
            }
        }
        Ok(())
    }

    pub fn render_cpu(&self) -> Result<Buf, String> {
        self.validate()?;
        let mut output = if self.steps.len() > 1 { Some(self.grid.image()?) } else { None };
        for (step, weight) in &self.steps {
            // Whole samples borrow the source; interpolated samples die after this one pass.
            let sample = match *step {
                TwStep::Whole(index) => std::borrow::Cow::Borrowed(self.frame(index)?),
                TwStep::Mix(a, b, fraction) => std::borrow::Cow::Owned(effectcraft_raster::flow::mix(self.frame(a)?, self.frame(b)?, fraction)),
                TwStep::Motion { a, b, w, flow } => {
                    std::borrow::Cow::Owned(timewarp_interpolate(self.frame(a)?, self.frame(b)?, w, self.flow(flow)?, &self.pm))
                }
                TwStep::Layered { fg, bg, w, show } => {
                    let foreground = timewarp_interpolate(self.frame(fg[0])?, self.frame(fg[1])?, w, self.flow(fg[2])?, &self.pm);
                    let background = timewarp_interpolate(self.frame(bg[0])?, self.frame(bg[1])?, w, self.flow(bg[2])?, &self.pm);
                    std::borrow::Cow::Owned(layered(foreground, background, show))
                }
            };
            if self.steps.len() == 1 {
                return Ok(self.grid.wrap(sample.into_owned()));
            }
            let output = output.as_mut().ok_or_else(|| invalid("shutter accumulator is missing"))?;
            output.data.par_iter_mut().zip(sample.data.par_iter()).for_each(|(accumulator, pixel)| {
                for channel in 0..4 {
                    accumulator[channel] += pixel[channel] * weight;
                }
            });
        }
        Ok(self.grid.wrap(output.ok_or_else(|| invalid("shutter accumulator is missing"))?))
    }
}

fn split_tick(source: Tick, fps: f64) -> Result<(f64, f32), String> {
    if let Some(rate) = broadcast_rate(fps) {
        let before = rate.frame_at(source);
        let elapsed = source.0.checked_sub(rate.tick_of(before).0).ok_or_else(|| invalid("source frame fraction overflows"))?;
        return Ok((before as f64, (elapsed as f64 / rate.frame_duration().0 as f64) as f32));
    }
    // Correct the floating estimate with Tick boundaries, without an epsilon that could
    // advance a legitimate instant just before a frame boundary.
    let mut before = (source.seconds() * fps).floor();
    for _ in 0..4 {
        if frame_key(before, fps)? > source {
            before -= 1.0;
        } else if frame_key(before + 1.0, fps)? <= source {
            before += 1.0;
        } else {
            let start = frame_key(before, fps)?;
            let end = frame_key(before + 1.0, fps)?;
            let duration = end.0.checked_sub(start.0).filter(|duration| *duration > 0).ok_or_else(|| invalid("source frame duration is unsupported"))?;
            let elapsed = source.0.checked_sub(start.0).ok_or_else(|| invalid("source frame fraction overflows"))?;
            return Ok((before, (elapsed as f64 / duration as f64) as f32));
        }
    }
    Err(invalid("source frame position cannot be represented"))
}

fn checked_tick(ticks: f64) -> Result<Tick, String> {
    if !ticks.is_finite() || ticks < Tick::MIN.0 as f64 || ticks > Tick::MAX.0 as f64 {
        return Err(invalid("retimed source exceeds the supported finite Tick range"));
    }
    let tick = Tick(ticks.round() as i64);
    if !(Tick::MIN..=Tick::MAX).contains(&tick) {
        return Err(invalid("retimed source exceeds the supported Tick range"));
    }
    Ok(tick)
}

struct Instants {
    /// (Interpolation fraction, shutter weight); integer keys are kept separately.
    values: Vec<(f32, f32)>,
    keys: Vec<Tick>,
    indices: Vec<[usize; 2]>,
}

fn instants(ctx: &EffectCtx) -> Result<Instants, String> {
    let fps = ctx.fps();
    let by_frame = ctx.params.e("adjustTimeBy") == 1;
    let speed = ctx.params.f("speed");
    let current = ctx.env.host.and_then(|host| host.time_ticks()).map(|(layer, _)| layer).unwrap_or_else(|| Tick::from_seconds_f64(ctx.time));
    if !(Tick::MIN..=Tick::MAX).contains(&current) {
        return Err(invalid("host layer time exceeds the supported Tick range"));
    }
    let integrated = if by_frame { None } else { ctx.env.host.and_then(|host| host.integrated_time("speed", 100.0, current)) };
    let integrated = integrated.transpose().map_err(invalid)?;
    let source = if let Some(source) = integrated {
        source
    } else if by_frame || speed == 100.0 {
        current
    } else if speed == -100.0 {
        Tick(current.0.checked_neg().ok_or_else(|| invalid("reverse source time overflows"))?)
    } else {
        checked_tick(current.0 as f64 * speed / 100.0)?
    };
    let enabled = ctx.params.b("motionBlur/enableMotionBlur");
    let samples = if enabled { ctx.params.f("motionBlur/shutterSamples").round().clamp(1.0, MAX_SAMPLES as f64) as usize } else { 1 };
    let angle = if ctx.params.e("motionBlur/shutterControl") == 1 { ctx.params.f("motionBlur/shutterAngle") } else { 180.0 };
    let span = if enabled { angle / 360.0 * if by_frame { 1.0 } else { speed / 100.0 } } else { 0.0 };
    if !span.is_finite() {
        return Err(invalid("shutter span is non-finite"));
    }
    let mut instants = Vec::with_capacity(samples);
    let mut keys = Vec::with_capacity(samples * 2);
    let mut sample_keys = Vec::with_capacity(samples);
    for sample in 0..samples {
        let offset = if samples > 1 { span * sample as f64 / (samples - 1) as f64 } else { 0.0 };
        let (frame, fraction) = if by_frame {
            let position = ctx.params.f("sourceFrame") + offset;
            let before = position.floor();
            (before, (position - before) as f32)
        } else {
            let instant = if integrated.is_some() && sample > 0 {
                // Shutter offsets are output-time offsets. Integrate through rate changes
                // inside the exposure instead of extrapolating the speed at its start.
                let output_offset = angle / 360.0 * sample as f64 / (samples - 1) as f64;
                let delta = checked_tick(output_offset / fps * TICKS_PER_SECOND as f64)?;
                let output_time = Tick(current.0.checked_add(delta.0).ok_or_else(|| invalid("shutter output time overflows"))?);
                ctx.env
                    .host
                    .and_then(|host| host.integrated_time("speed", 100.0, output_time))
                    .ok_or_else(|| invalid("the host no longer supplies the animated rate curve"))?
                    .map_err(invalid)?
            } else {
                let delta = checked_tick(offset / fps * TICKS_PER_SECOND as f64)?;
                Tick(source.0.checked_add(delta.0).ok_or_else(|| invalid("shutter source time overflows"))?)
            };
            if !(Tick::MIN..=Tick::MAX).contains(&instant) {
                return Err(invalid("shutter source time exceeds the supported Tick range"));
            }
            split_tick(instant, fps)?
        };
        if !frame.is_finite() || frame.abs() > (1_u64 << 52) as f64 {
            return Err(invalid("source frame arithmetic exceeds exact integer precision"));
        }
        let pair = [frame_key(frame, fps)?, frame_key(frame + 1.0, fps)?];
        keys.extend(pair);
        sample_keys.push(pair);
        instants.push((fraction, 1.0 / samples as f32));
    }
    indexed_instants(instants, keys, sample_keys)
}

fn indexed_instants(values: Vec<(f32, f32)>, mut keys: Vec<Tick>, sample_keys: Vec<[Tick; 2]>) -> Result<Instants, String> {
    keys.sort_unstable();
    keys.dedup();
    let indices = sample_keys
        .into_iter()
        .map(|pair| {
            Ok([
                keys.binary_search(&pair[0]).map_err(|_| invalid("source key is missing"))?,
                keys.binary_search(&pair[1]).map_err(|_| invalid("source key is missing"))?,
            ])
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Instants { values, keys, indices })
}

/// Center the exposure on the exact host Tick. Fetch only integer source boundaries;
/// fractional samples synthesize along a shared measured field rather than decoding
/// the same source frame once for every shutter instant.
fn pmb_instants(ctx: &EffectCtx) -> Result<Instants, String> {
    let fps = ctx.fps();
    let current = ctx.env.host.and_then(|host| host.time_ticks()).map(|(layer, _)| layer).unwrap_or_else(|| Tick::from_seconds_f64(ctx.time));
    if !(Tick::MIN..=Tick::MAX).contains(&current) {
        return Err(invalid("host layer time exceeds the supported Tick range"));
    }
    let samples = ctx.params.f("shutterSamples").round().clamp(1.0, MAX_SAMPLES as f64) as usize;
    let angle = if ctx.params.e("shutterControl") == 1 { ctx.params.f("shutterAngle") } else { 180.0 };
    let span = angle / 360.0 / fps;
    let mut values = Vec::with_capacity(samples);
    let mut keys = Vec::with_capacity(samples * 2);
    let mut sample_keys = Vec::with_capacity(samples);
    for sample in 0..samples {
        let offset = if samples > 1 { span * (sample as f64 / (samples - 1) as f64 - 0.5) } else { 0.0 };
        let delta = checked_tick(offset * TICKS_PER_SECOND as f64)?;
        let instant = Tick(current.0.checked_add(delta.0).ok_or_else(|| invalid("shutter source time overflows"))?);
        if !(Tick::MIN..=Tick::MAX).contains(&instant) {
            return Err(invalid("shutter source time exceeds the supported Tick range"));
        }
        let (frame, fraction) = split_tick(instant, fps)?;
        let pair = [frame_key(frame, fps)?, frame_key(frame + 1.0, fps)?];
        keys.extend(pair);
        sample_keys.push(pair);
        values.push((fraction, 1.0 / samples as f32));
    }
    indexed_instants(values, keys, sample_keys)
}

fn fetch_layer(ctx: &EffectCtx, id: &str, keys: &[Tick], grid: TimewarpGrid, budget: &mut TimewarpBudget) -> Result<Option<Vec<Image>>, String> {
    let Some(layer) = ctx.params.get(id).and_then(Value::as_layer) else { return Ok(None) };
    let Some(host) = ctx.env.host else { return Ok(None) };
    let masks = ctx.params.get(&crate::layer_source_id(id)).is_none_or(|value| value.as_enum() != 0);
    let (local, comp) = host.time_ticks().unwrap_or_else(|| (Tick::from_seconds_f64(ctx.time), Tick::from_seconds_f64(ctx.env.comp_time)));
    if !(Tick::MIN..=Tick::MAX).contains(&local) || !(Tick::MIN..=Tick::MAX).contains(&comp) {
        return Err(invalid("host current times exceed the supported Tick range"));
    }
    let shift = comp.0.checked_sub(local.0).ok_or_else(|| invalid("layer time shift overflows"))?;
    keys.iter()
        .map(|key| {
            let time = Tick(key.0.checked_add(shift).ok_or_else(|| invalid("shifted source time overflows"))?);
            if !(Tick::MIN..=Tick::MAX).contains(&time) {
                return Err(invalid("shifted source time exceeds the supported Tick range"));
            }
            match host.layer_at_tick(layer, time, masks) {
                Some(source) => {
                    validate_input(&source.buf)?;
                    let peak = add(budget.preparation_bytes, mul(area(source.buf.img.width, source.buf.img.height)?, 16)?)?;
                    if peak > MAX_TOTAL_BYTES {
                        return Err(invalid("matte/warp source and planned scratch exceed the per-effect byte budget"));
                    }
                    budget.total_bytes = budget.total_bytes.max(peak);
                    grid.fit(ctx, source)
                }
                None => grid.image(),
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// Returns no plan only when there is no host/source. Invalid controls, extents and budgets
/// are checked errors, propagated to preview/export by the registered checked effect.
pub fn timewarp_plan(ctx: &EffectCtx) -> Result<Option<TimewarpPlan>, String> {
    prepare(ctx, 0, false)
}

pub fn plan_for_input(ctx: &EffectCtx, size: [u32; 2]) -> Result<Option<TimewarpPlan>, String> {
    prepare(ctx, area(size[0], size[1])?, false)
}

/// Pixel Motion Blur uses the same checked, bounded, source-sharing plan and CPU/GPU
/// synthesis as Timewarp. Its Vector Detail controls spacing and its wider coarse
/// search follows fast displacements; both are included in the work budget.
pub fn pmb_plan(ctx: &EffectCtx) -> Result<Option<TimewarpPlan>, String> {
    prepare(ctx, 0, true)
}

pub fn pmb_plan_for_input(ctx: &EffectCtx, size: [u32; 2]) -> Result<Option<TimewarpPlan>, String> {
    prepare(ctx, area(size[0], size[1])?, true)
}

fn prepare(ctx: &EffectCtx, input_pixels: u64, pmb: bool) -> Result<Option<TimewarpPlan>, String> {
    validate_controls_for(ctx, if pmb { PMB_ID } else { ID })?;
    let Instants { values: instants, keys, indices } = if pmb { pmb_instants(ctx)? } else { instants(ctx)? };
    let Some(host) = ctx.env.host else { return Ok(None) };
    // Keep Timewarp's established source sampling for existing projects. Opting in
    // samples the processed prefix at each source time, as PMB already does. The
    // renderer host clamps MAX to this effect's index, excluding it and later effects.
    let source_effects = if pmb || ctx.params.b("processPreviousEffects") { usize::MAX } else { 0 };
    let mut raw_bytes = 0;
    let mut sources = Vec::with_capacity(keys.len());
    for key in &keys {
        let source = host.self_at_tick(*key, source_effects);
        if let Some(source) = &source {
            validate_input(source)?;
            raw_bytes = add(raw_bytes, mul(area(source.img.width, source.img.height)?, 16)?)?;
            if add(raw_bytes, mul(input_pixels, 16)?)? > MAX_TOTAL_BYTES {
                return Err(invalid("unique decoded source frames and input exceed the per-effect byte budget"));
            }
        }
        sources.push(source);
    }
    let grid = match union(&sources)? {
        Some(grid) => grid,
        None => {
            // Missing samples are transparent, including when *all* requested times lie
            // outside a precomp. Borrow only the current source's geometry; its pixels
            // must never become replacement samples. Unsupported hosts still pass through.
            let current = host.time_ticks().map(|(layer, _)| layer).unwrap_or_else(|| Tick::from_seconds_f64(ctx.time));
            if !(Tick::MIN..=Tick::MAX).contains(&current) {
                return Err(invalid("host layer time exceeds the supported Tick range"));
            }
            let Some(reference) = host.self_at_tick(current, source_effects) else { return Ok(None) };
            validate_input(&reference)?;
            raw_bytes = mul(area(reference.img.width, reference.img.height)?, 16)?;
            union(&[Some(reference)])?.ok_or_else(|| invalid("current source geometry is unavailable"))?
        }
    };
    // Validate floating products before PixelMotion's integer conversions.
    let window = if pmb { 8.0 } else { ctx.params.get("tuning/blockSize").map(Value::as_f64).unwrap_or(8.0).max(4.0) };
    for value in [32.0 * grid.scale, window * grid.scale] {
        if !value.is_finite() || value > MAX_DIMENSION as f64 {
            return Err(invalid("scaled motion spacing/window exceeds 16384 pixels"));
        }
    }
    if !pmb {
        for crop in ["sourceCrops/leftCrop", "sourceCrops/rightCrop", "sourceCrops/topCrop", "sourceCrops/bottomCrop"] {
            let value = ctx.params.f(crop) * grid.scale;
            if !value.is_finite() || value > u32::MAX as f64 {
                return Err(invalid("scaled source crop exceeds its integer range"));
            }
        }
    }
    let pm = if pmb {
        let detail = ctx.params.f("vectorDetail") / 100.0;
        PixelMotion {
            tile: (32.0 * (1.0f64 / 16.0).powf(detail) * grid.scale).round().max(2.0) as u32,
            window: (8.0 * grid.scale).round().max(4.0) as u32,
            global_smoothness: 0.5,
            local_smoothness: 0.5,
            iterations: 20,
            build_from_one: false,
            correct_luminance: false,
            extreme: false,
            error_threshold: 0.1,
            weights: [0.3, 0.59, 0.11],
            crops: [0; 4],
        }
    } else {
        PixelMotion::from(ctx, grid.scale)
    };
    let method = if pmb { 2 } else { ctx.params.e("method") };
    let matte = !pmb && ctx.params.get("matteLayer").and_then(Value::as_layer).is_some();
    let warp = !pmb && ctx.params.get("warpLayer").and_then(Value::as_layer).is_some();
    let show = if pmb { 0 } else { ctx.params.e("show") };
    let radius = if pmb { (grid.width.max(grid.height) / 4).clamp(8, 64) } else { 4 };
    // Integer-aligned/Whole samples do not estimate motion, even with expensive tuning.
    let mut pairs: Vec<_> = instants.iter().zip(&indices).filter_map(|((fraction, _), pair)| (method == 2 && *fraction >= 1e-4).then_some(*pair)).collect();
    pairs.sort_unstable();
    pairs.dedup();
    let mut budget = budget(
        grid,
        raw_bytes,
        &pm,
        BudgetRequest { input_pixels, unique: keys.len(), pairs: pairs.len(), steps: instants.len(), method, matte, warp, show, radius },
    )?;
    let mut frames = sources.into_iter().map(|source| grid.place(source)).collect::<Result<Vec<_>, _>>()?;
    let mattes = if matte { fetch_layer(ctx, "matteLayer", &keys, grid, &mut budget)? } else { None }
        .map(|images| images.iter().map(|image| matte_of(image, ctx.params.e("matteChannel"))).collect::<Vec<_>>());
    let warps = if warp { fetch_layer(ctx, "warpLayer", &keys, grid, &mut budget)? } else { None };
    let mut derived: Vec<((usize, u32), usize)> = Vec::new();
    let mut flows: Vec<KeyedFlow> = Vec::new();
    let mut steps = Vec::with_capacity(instants.len());
    let one = !pmb && method > 0 && ctx.params.b("tuning/buildFromOneImage");
    let derive = |frames: &mut Vec<Image>, cache: &mut Vec<((usize, u32), usize)>, index: usize, mode: u32| -> Result<usize, String> {
        let Some(mattes) = &mattes else { return Ok(index) };
        if let Some((_, derived)) = cache.iter().find(|(key, _)| *key == (index, mode)) {
            return Ok(*derived);
        }
        let frame = frames.get(index).ok_or_else(|| invalid("derived source frame is missing"))?;
        let matte = mattes.get(index).ok_or_else(|| invalid("derived matte frame is missing"))?;
        let image = if mode == 3 { matte_image(matte, frame) } else { times_mask(frame, matte, mode == 2) };
        let derived = frames.len();
        frames.push(image);
        cache.push(((index, mode), derived));
        Ok(derived)
    };
    for ((fraction, weight), [a, b]) in instants.into_iter().zip(indices) {
        let step = if method == 0 || fraction < 1e-4 || (one && method == 1) {
            let pick = if method == 0 || fraction < 1e-4 || fraction < 0.5 { a } else { b };
            TwStep::Whole(if show > 0 { derive(&mut frames, &mut derived, pick, show)? } else { pick })
        } else if method == 1 {
            let (a, b) = if show > 0 { (derive(&mut frames, &mut derived, a, show)?, derive(&mut frames, &mut derived, b, show)?) } else { (a, b) };
            TwStep::Mix(a, b, fraction)
        } else if matte {
            // Derive each foreground/background source once, regardless of sample count.
            let fa = derive(&mut frames, &mut derived, a, 1)?;
            let fb = derive(&mut frames, &mut derived, b, 1)?;
            let ga = derive(&mut frames, &mut derived, a, 2)?;
            let gb = derive(&mut frames, &mut derived, b, 2)?;
            // The warp layer supplies one field shared by both masks.
            let (fg_flow, bg_flow) = if warp {
                let index = prepare_flow(&frames, warps.as_deref(), &mut flows, [a, b], &pm, radius)?;
                (index, index)
            } else {
                (prepare_flow(&frames, None, &mut flows, [fa, fb], &pm, radius)?, prepare_flow(&frames, None, &mut flows, [ga, gb], &pm, radius)?)
            };
            TwStep::Layered { fg: [fa, fb, fg_flow], bg: [ga, gb, bg_flow], w: fraction, show }
        } else {
            TwStep::Motion { a, b, w: fraction, flow: prepare_flow(&frames, warps.as_deref(), &mut flows, [a, b], &pm, radius)? }
        };
        steps.push((step, weight));
    }
    Ok(Some(TimewarpPlan { grid, frames, steps, flows: flows.into_iter().map(|(_, flow)| flow).collect(), pm, budget }))
}

fn prepare_flow(
    frames: &[Image],
    warps: Option<&[Image]>,
    flows: &mut Vec<KeyedFlow>,
    pair: [usize; 2],
    pm: &PixelMotion,
    radius: u32,
) -> Result<usize, String> {
    let key = (pair, warps.is_some());
    if let Some(index) = flows.iter().position(|(cached, _)| *cached == key) {
        return Ok(index);
    }
    let sources = warps.unwrap_or(frames);
    let first = sources.get(pair[0]).ok_or_else(|| invalid("motion/warp source is missing"))?;
    let second = sources.get(pair[1]).ok_or_else(|| invalid("motion/warp source is missing"))?;
    let index = flows.len();
    let flow = if radius == 4 { timewarp_flow(first, second, pm)? } else { timewarp_flow_search(first, second, pm, radius as i32)? };
    flows.push((key, flow));
    Ok(index)
}

pub fn render_checked(ctx: &EffectCtx, input: Buf) -> Result<Buf, String> {
    validate_input(&input)?;
    match plan_for_input(ctx, [input.img.width, input.img.height])? {
        Some(plan) => plan.render_cpu(),
        None => Ok(input),
    }
}

pub fn pmb_render_checked(ctx: &EffectCtx, input: Buf) -> Result<Buf, String> {
    validate_input(&input)?;
    match pmb_plan_for_input(ctx, [input.img.width, input.img.height])? {
        Some(plan) => plan.render_cpu(),
        None => Ok(input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectEnv, EffectHost, Params};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SIZE: [f64; 2] = [32.0, 20.0];

    #[test]
    fn exact_tick_split_never_advances_instants_before_a_boundary() {
        for rate in FrameRate::COMMON {
            for frame in [-31_000_003, -37, -1, 0, 1, 37, 31_000_003] {
                let boundary = rate.tick_of(frame);
                for delta in [-7, -1, 0, 1, 7] {
                    let time = Tick(boundary.0 + delta);
                    let (before, fraction) = split_tick(time, rate.as_f64()).unwrap();
                    assert_eq!(before as i64, frame - i64::from(delta < 0), "rate={rate:?}, frame={frame}, delta={delta}");
                    assert_eq!(fraction == 0.0, delta == 0);
                }
                let mut seconds = boundary.seconds();
                for _ in 0..4 {
                    seconds = seconds.next_down();
                }
                for _ in 0..9 {
                    let time = Tick::from_seconds_f64(seconds);
                    let (before, _) = split_tick(time, rate.as_f64()).unwrap();
                    assert_eq!(before as i64, rate.frame_at(time), "float ULP neighbour at {rate:?}, {frame}");
                    seconds = seconds.next_up();
                }
                assert_eq!(frame_key(frame as f64, rate.as_f64()).unwrap(), boundary);
            }
        }
    }

    #[test]
    fn custom_rate_boundaries_do_not_snap_to_broadcast_rates() {
        for fps in [59.999, 60.001, 119.999, 120.001, 10.0] {
            for frame in [-10001, -5, -1, 0, 1, 5, 10001] {
                let boundary = frame_key(frame as f64, fps).unwrap();
                assert_eq!(boundary.0, (frame as f64 / fps * TICKS_PER_SECOND as f64).ceil() as i64);
                for delta in [-1, 0, 1] {
                    assert_eq!(split_tick(Tick(boundary.0 + delta), fps).unwrap().0 as i64, frame - i64::from(delta < 0));
                }
            }
        }
    }

    #[derive(Default)]
    struct Host {
        source_reads: AtomicUsize,
        layer_reads: AtomicUsize,
        expected_effects: usize,
    }
    impl EffectHost for Host {
        fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
            None
        }
        fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
            None
        }
        fn self_at(&self, time: f64, effects: usize) -> Option<Buf> {
            assert_eq!(effects, self.expected_effects);
            self.source_reads.fetch_add(1, Ordering::Relaxed);
            let mut image = Image::new(32, 20);
            for (index, pixel) in image.data.iter_mut().enumerate() {
                let x = index % 32;
                let y = index / 32;
                let value = (0.5 + 0.3 * (x as f64 * 0.43 - time * 4.0).sin() + 0.1 * (y as f64 * 0.19).cos()) as f32;
                *pixel = [value * 0.7, value * 0.3, value * 0.5, 0.7];
            }
            Some(Buf { img: image, offset: [0.0; 2], scale: 1.0 })
        }
        fn layer_at(&self, _: u64, time: f64, _: bool) -> Option<LayerPixels> {
            self.layer_reads.fetch_add(1, Ordering::Relaxed);
            let mut image = Image::new(32, 20);
            for (index, pixel) in image.data.iter_mut().enumerate() {
                let alpha = ((index % 32) as f32 / 32.0 + time as f32 * 0.2).clamp(0.0, 1.0);
                *pixel = [alpha, alpha, alpha, alpha];
            }
            Some(LayerPixels { buf: Buf { img: image, offset: [0.0; 2], scale: 1.0 }, size: SIZE })
        }
    }

    fn parameters(samples: usize, method: u32) -> Params {
        let spec = crate::find(ID).unwrap();
        let mut parameters = Params { values: spec.params.iter().map(|parameter| (parameter.id.into(), crate::default_value(parameter, SIZE))).collect() };
        for (key, value) in [
            ("method", Value::Enum(method)),
            ("adjustTimeBy", Value::Enum(1)),
            ("sourceFrame", Value::Scalar(4.4)),
            ("motionBlur/enableMotionBlur", Value::Bool(true)),
            ("motionBlur/shutterControl", Value::Enum(1)),
            ("motionBlur/shutterAngle", Value::Scalar(0.0)),
            ("motionBlur/shutterSamples", Value::Scalar(samples as f64)),
        ] {
            parameters.values.insert(key.into(), value);
        }
        parameters
    }

    fn context<'a>(parameters: &'a Params, host: &'a dyn EffectHost) -> EffectCtx<'a> {
        EffectCtx {
            params: parameters,
            time: 0.7,
            layer_size: SIZE,
            seed: 1,
            adjustment: false,
            env: EffectEnv { host: Some(host), frame_rate: 10.0, comp_time: 0.7, ..Default::default() },
        }
    }

    fn pmb_parameters(samples: usize, angle: f64, detail: f64) -> Params {
        let spec = crate::find(PMB_ID).unwrap();
        let mut parameters = Params { values: spec.params.iter().map(|parameter| (parameter.id.into(), crate::default_value(parameter, SIZE))).collect() };
        for (key, value) in [
            ("shutterControl", Value::Enum(1)),
            ("shutterSamples", Value::Scalar(samples as f64)),
            ("shutterAngle", Value::Scalar(angle)),
            ("vectorDetail", Value::Scalar(detail)),
        ] {
            parameters.values.insert(key.into(), value);
        }
        parameters
    }

    #[test]
    fn pmb_repeated_shutter_samples_share_sources_and_motion_fields() {
        let host = Host { expected_effects: usize::MAX, ..Default::default() };
        let parameters = pmb_parameters(64, 0.0, 60.0);
        let mut ctx = context(&parameters, &host);
        ctx.time = 0.74;
        let plan = pmb_plan(&ctx).unwrap().unwrap();
        assert_eq!(host.source_reads.load(Ordering::Relaxed), 2);
        assert_eq!(plan.steps.len(), 64);
        assert_eq!(plan.frames.len(), 2);
        assert_eq!(plan.flows.len(), 1);
        assert_eq!(plan.render_cpu().unwrap().img.data, collected_reference(&plan).data);
    }

    #[test]
    fn timewarp_processed_input_is_explicit_and_rejects_wrong_types_before_source_reads() {
        for method in 0..=2 {
            for processed in [None, Some(false), Some(true)] {
                let host = Host { expected_effects: if processed == Some(true) { usize::MAX } else { 0 }, ..Default::default() };
                let mut parameters = parameters(1, method);
                parameters.values.remove("processPreviousEffects");
                if let Some(value) = processed {
                    parameters.values.insert("processPreviousEffects".into(), Value::Bool(value));
                }
                let plan = timewarp_plan(&context(&parameters, &host)).unwrap().unwrap();
                assert!(host.source_reads.load(Ordering::Relaxed) > 0);
                assert!(plan.render_cpu().is_ok());
            }
        }
        let host = Host::default();
        let mut parameters = parameters(1, 0);
        parameters.values.insert("processPreviousEffects".into(), Value::Scalar(1.0));
        let error = timewarp_plan(&context(&parameters, &host)).err().unwrap();
        assert!(error.contains("Process Previous Effects"), "{error}");
        assert_eq!(host.source_reads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn pmb_missing_sources_use_preceding_effects_for_reference_geometry_only() {
        struct CurrentOnly;
        impl EffectHost for CurrentOnly {
            fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
                None
            }
            fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
                None
            }
            fn self_at(&self, time: f64, effects: usize) -> Option<Buf> {
                assert_eq!(effects, usize::MAX);
                (time == 0.74).then(|| Buf { img: Image::filled(4, 3, [0.5; 4]), offset: [2.0, -1.0], scale: 0.5 })
            }
        }
        let parameters = pmb_parameters(16, 0.0, 20.0);
        let mut ctx = context(&parameters, &CurrentOnly);
        ctx.time = 0.74;
        let plan = pmb_plan(&ctx).unwrap().unwrap();
        assert_eq!((plan.grid.width, plan.grid.height, plan.grid.offset, plan.grid.scale), (4, 3, [2.0, -1.0], 0.5));
        assert!(plan.render_cpu().unwrap().img.data.iter().all(|pixel| *pixel == [0.0; 4]));
    }

    #[test]
    fn pmb_exposure_uses_exact_host_ticks_and_centers_on_the_current_frame() {
        struct Exact(Tick);
        impl EffectHost for Exact {
            fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
                None
            }
            fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
                None
            }
            fn self_at(&self, _: f64, _: usize) -> Option<Buf> {
                panic!("PMB must transport exact Tick boundaries")
            }
            fn self_at_tick(&self, time: Tick, effects: usize) -> Option<Buf> {
                assert_eq!(effects, usize::MAX);
                let value = (time.0 - self.0.0) as f64 / TICKS_PER_SECOND as f64;
                Some(Buf { img: Image::filled(1, 1, [value as f32; 4]), offset: [0.0; 2], scale: 1.0 })
            }
            fn time_ticks(&self) -> Option<(Tick, Tick)> {
                Some((self.0, self.0))
            }
        }
        for rate in [FrameRate::FPS_60, FrameRate::FPS_120, FrameRate::FPS_59_94, FrameRate::FPS_119_88] {
            let host = Exact(rate.tick_of(31_000_003));
            let parameters = pmb_parameters(5, 180.0, 20.0);
            let mut ctx = context(&parameters, &host);
            ctx.time = host.0.seconds();
            ctx.env.comp_time = ctx.time;
            ctx.env.frame_rate = rate.as_f64();
            let plan = pmb_plan(&ctx).unwrap().unwrap();
            assert_eq!(plan.frames.len(), 3);
            let output = plan.render_cpu().unwrap();
            assert!(output.img.data[0][0].abs() < 1e-8, "rate={rate:?}: {:?}", output.img.data[0]);
        }
    }

    #[test]
    fn pmb_rejects_invalid_controls_before_host_reads_and_preserves_checked_errors() {
        struct Damaged;
        impl EffectHost for Damaged {
            fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
                None
            }
            fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
                None
            }
            fn self_at(&self, _: f64, effects: usize) -> Option<Buf> {
                assert_eq!(effects, usize::MAX);
                Some(Buf { img: Image::filled(1, 1, [f32::NAN; 4]), offset: [0.0; 2], scale: 1.0 })
            }
        }
        let host = Host::default();
        for (id, value) in [
            ("vectorDetail", Value::Scalar(f64::NAN)),
            ("vectorDetail", Value::Scalar(101.0)),
            ("shutterSamples", Value::Scalar(f64::INFINITY)),
            ("shutterSamples", Value::Scalar(65.0)),
            ("shutterAngle", Value::Scalar(-1.0)),
            ("shutterControl", Value::Enum(2)),
        ] {
            let mut parameters = pmb_parameters(16, 180.0, 20.0);
            parameters.values.insert(id.into(), value);
            assert!(pmb_plan(&context(&parameters, &host)).is_err(), "{id}");
        }
        assert_eq!(host.source_reads.load(Ordering::Relaxed), 0);
        let parameters = pmb_parameters(16, 180.0, 20.0);
        assert!(pmb_plan(&context(&parameters, &Damaged)).err().unwrap().contains("non-finite RGBA"));
        let bad_input = Buf { img: Image { width: 32, height: 20, data: vec![] }, offset: [0.0; 2], scale: 1.0 };
        assert!(pmb_render_checked(&context(&parameters, &host), bad_input).unwrap_err().contains("storage"));
        let mut ctx = context(&parameters, &host);
        ctx.env.host = None;
        let input = Buf { img: Image::filled(32, 20, [0.1, 0.2, 0.3, 0.5]), offset: [0.0; 2], scale: 1.0 };
        assert_eq!(pmb_render_checked(&ctx, input.clone()).unwrap().img.data, input.img.data);
    }

    #[test]
    fn pmb_vector_detail_changes_spacing_and_measured_motion_builds_subframes() {
        struct Moving;
        impl EffectHost for Moving {
            fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
                None
            }
            fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
                None
            }
            fn self_at(&self, time: f64, effects: usize) -> Option<Buf> {
                assert_eq!(effects, usize::MAX);
                let frame = (time * 10.0).round() as i64;
                let mut image = Image::new(64, 32);
                for (index, pixel) in image.data.iter_mut().enumerate() {
                    let x = ((index % 64) as i64 - frame * 6).rem_euclid(64);
                    let y = (index / 64) as i64;
                    let value = ((x * 17 + y * 23 + x * y * 3).rem_euclid(101) as f32) / 100.0;
                    *pixel = [value, value * 0.5, value * 0.25, 1.0];
                }
                Some(Buf { img: image, offset: [0.0; 2], scale: 1.0 })
            }
        }
        let coarse = pmb_parameters(5, 180.0, 0.0);
        let fine = pmb_parameters(5, 180.0, 100.0);
        let coarse = pmb_plan(&context(&coarse, &Moving)).unwrap().unwrap();
        let mut fine = pmb_plan(&context(&fine, &Moving)).unwrap().unwrap();
        assert_eq!((coarse.pm.tile, fine.pm.tile), (32, 2));
        assert!(fine.flows.iter().flat_map(|flow| &flow.v).flatten().any(|value| value.abs() > 0.5));
        let motion = fine.render_cpu().unwrap();
        for field in &mut fine.flows {
            field.v.fill([0.0; 2]);
        }
        assert_ne!(motion.img.data, fine.render_cpu().unwrap().img.data, "PMB must synthesize shutter frames along the measured motion");
    }

    #[test]
    fn missing_retimed_samples_use_current_geometry_without_current_pixels() {
        struct CurrentOnly(Buf);
        impl EffectHost for CurrentOnly {
            fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
                None
            }
            fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
                None
            }
            fn self_at(&self, time: f64, _: usize) -> Option<Buf> {
                (time == 0.7).then(|| self.0.clone())
            }
        }
        let input = Buf { img: Image::filled(32, 20, [0.1, 0.2, 0.3, 1.0]), offset: [2.0, -3.0], scale: 0.5 };
        let host = CurrentOnly(input.clone());
        for method in 0..=2 {
            for samples in [1, 8] {
                let mut params = parameters(samples, method);
                params.values.insert("sourceFrame".into(), Value::Scalar(-3.5));
                params.values.insert("motionBlur/shutterAngle".into(), Value::Scalar(180.0));
                let ctx = context(&params, &host);
                let output = render_checked(&ctx, input.clone()).unwrap();
                assert_eq!((output.img.width, output.img.height, output.offset, output.scale), (32, 20, input.offset, input.scale));
                assert!(output.img.data.iter().all(|pixel| *pixel == [0.0; 4]), "method={method}, samples={samples}");
                let malformed = CurrentOnly(Buf { img: Image::filled(32, 20, [f32::NAN; 4]), ..input.clone() });
                assert!(render_checked(&context(&params, &malformed), input.clone()).unwrap_err().contains("non-finite RGBA"));
            }
        }
        let params = parameters(1, 0);
        let mut ctx = context(&params, &host);
        ctx.env.host = None;
        assert_eq!(render_checked(&ctx, input.clone()).unwrap().img.data, input.img.data);
    }

    /// The previous materialized-output path: retain every output, then sum per pixel in
    /// shutter order. Tiny synthetic frames test arithmetic without large allocations.
    fn collected_reference(plan: &TimewarpPlan) -> Image {
        let frames = &plan.frames;
        let images: Vec<Image> = plan
            .steps
            .iter()
            .map(|(step, _)| match *step {
                TwStep::Whole(index) => frames[index].clone(),
                TwStep::Mix(a, b, fraction) => effectcraft_raster::flow::mix(&frames[a], &frames[b], fraction),
                TwStep::Motion { a, b, w, flow } => timewarp_interpolate(&frames[a], &frames[b], w, &plan.flows[flow], &plan.pm),
                TwStep::Layered { fg, bg, w, show } => layered(
                    timewarp_interpolate(&frames[fg[0]], &frames[fg[1]], w, &plan.flows[fg[2]], &plan.pm),
                    timewarp_interpolate(&frames[bg[0]], &frames[bg[1]], w, &plan.flows[bg[2]], &plan.pm),
                    show,
                ),
            })
            .collect();
        if images.len() == 1 {
            return images[0].clone();
        }
        let mut output = Image::new(plan.grid.width, plan.grid.height);
        for (index, pixel) in output.data.iter_mut().enumerate() {
            for (image, (_, weight)) in images.iter().zip(&plan.steps) {
                for channel in 0..4 {
                    pixel[channel] += image.data[index][channel] * weight;
                }
            }
        }
        output
    }

    #[test]
    fn repeated_sixty_four_samples_share_unique_sources_mattes_and_motion_fields() {
        let host = Host::default();
        let mut parameters = parameters(64, 2);
        parameters.values.insert("matteLayer".into(), Value::Layer(Some(7)));
        let plan = timewarp_plan(&context(&parameters, &host)).unwrap().unwrap();
        assert_eq!(host.source_reads.load(Ordering::Relaxed), 2);
        assert_eq!(host.layer_reads.load(Ordering::Relaxed), 2);
        assert_eq!(plan.steps.len(), 64);
        assert_eq!(plan.frames.len(), 6, "two source and four masked frames, rather than one set per sample");
        assert_eq!(plan.flows.len(), 2, "foreground and background fields are shared by all fractions");
        assert_eq!(plan.frames.iter().map(|image| image.data.len()).sum::<usize>(), 6 * 32 * 20);
        assert_eq!(plan.render_cpu().unwrap().img.data, collected_reference(&plan).data);
    }

    #[test]
    fn streaming_matches_collected_outputs_at_all_supported_sample_counts() {
        for samples in [1, 2, 8, 16, 32, 64] {
            for method in 0..=2 {
                for (matte, show) in [(false, 0), (true, 0), (true, 1), (true, 2), (true, 3)] {
                    let mut parameters = parameters(samples, method);
                    parameters.values.insert("motionBlur/shutterAngle".into(), Value::Scalar(180.0));
                    parameters.values.insert("show".into(), Value::Enum(show));
                    if matte {
                        parameters.values.insert("matteLayer".into(), Value::Layer(Some(7)));
                    }
                    let host = Host::default();
                    let plan = timewarp_plan(&context(&parameters, &host)).unwrap().unwrap();
                    assert_eq!(
                        plan.render_cpu().unwrap().img.data,
                        collected_reference(&plan).data,
                        "samples {samples}, method {method}, matte {matte}, show {show}"
                    );
                    assert!(plan.frames.len() <= plan.budget.frame_limit);
                    assert!(plan.flows.len() <= plan.budget.flow_limit);
                }
            }
        }
    }

    #[test]
    fn hostile_controls_fail_before_host_reads_and_integer_casts() {
        let host = Host::default();
        for (key, value) in [
            ("speed", f64::NAN),
            ("speed", f64::INFINITY),
            ("sourceFrame", f64::MAX),
            ("motionBlur/shutterSamples", 1e30),
            ("motionBlur/shutterAngle", -1.0),
            ("tuning/blockSize", f64::MAX),
            ("tuning/vectorDetail", 101.0),
            ("tuning/weighting/redWeighting", f64::NEG_INFINITY),
            ("method", 1.5),
        ] {
            let mut parameters = parameters(1, 2);
            parameters.values.insert(key.into(), Value::Scalar(value));
            assert!(timewarp_plan(&context(&parameters, &host)).is_err(), "{key}: {value}");
        }
        let parameters = parameters(1, 2);
        for time in [f64::NAN, f64::INFINITY, f64::MAX] {
            let mut context = context(&parameters, &host);
            context.time = time;
            assert!(timewarp_plan(&context).is_err());
        }
        for fps in [f64::NAN, f64::INFINITY, -1.0, 1001.0, 1e-300] {
            let mut context = context(&parameters, &host);
            context.env.frame_rate = fps;
            assert!(timewarp_plan(&context).is_err());
        }
        assert_eq!(host.source_reads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn capacity_overflow_and_union_extents_are_rejected_without_allocating_pixels() {
        let malformed = Buf { img: Image { width: u32::MAX, height: u32::MAX, data: Vec::new() }, offset: [0.0; 2], scale: 1.0 };
        assert!(validate_input(&malformed).is_err());
        let parameters = parameters(1, 2);
        let host = Host::default();
        let context = context(&parameters, &host);
        for scale in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            let input = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale };
            assert!(render_checked(&context, input).is_err());
        }
        let first = Buf { img: Image::new(1, 1), offset: [-1e9, 0.0], scale: 1.0 };
        let second = Buf { img: Image::new(1, 1), offset: [1e9, 0.0], scale: 1.0 };
        assert!(union(&[Some(first), Some(second)]).is_err());
        for scale in [f64::NAN, f64::INFINITY, 0.0, -1.0, 1e-300] {
            let first = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1.0 };
            let second = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale };
            assert!(union(&[Some(first), Some(second)]).is_err());
        }
        // The reverse scale ratio can keep the union small while making sample coordinates
        // enormous. Previously this escaped into i64::MAX + 1 in bilinear sampling.
        let tiny = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1e-300 };
        let ordinary = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1.0 };
        let grid = union(&[Some(tiny), Some(ordinary)]).unwrap().unwrap();
        assert!(grid.place(Some(Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1.0 })).is_err());
        let grid = TimewarpGrid { width: 1, height: 1, offset: [0.0; 2], scale: 1.0 };
        let source = LayerPixels { buf: Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1e300 }, size: [1.0; 2] };
        assert!(grid.fit(&context, source).is_err());
    }

    #[test]
    fn aggregate_budget_bounds_quality_without_allocating_1440p_frames() {
        let parameters = parameters(64, 2);
        let host = Host::default();
        let context = context(&parameters, &host);
        let motion = PixelMotion::from(&context, 1.0);
        let grid = TimewarpGrid { width: 2560, height: 1440, offset: [0.0; 2], scale: 1.0 };
        let request = BudgetRequest { input_pixels: 2560 * 1440, unique: 2, pairs: 1, steps: 64, method: 2, matte: false, warp: false, show: 0, radius: 4 };
        let plan = budget(grid, 2 * 2560 * 1440 * 16, &motion, request).unwrap();
        assert!(plan.total_bytes < MAX_TOTAL_BYTES);
        assert_eq!(plan.frame_limit, 2);
        assert_eq!(plan.flow_limit, 1);
        let wide_search = budget(grid, 0, &motion, BudgetRequest { radius: 64, ..request }).unwrap();
        assert!(wide_search.work > plan.work, "PMB's wider matching radius must be included in the work budget");
        assert!(budget(grid, 0, &motion, BudgetRequest { radius: 65, ..request }).is_err());
        assert!(budget(grid, 0, &motion, BudgetRequest { unique: 128, pairs: 64, ..request }).is_err());
        let maximum_matching = PixelMotion { tile: 2, window: 64, ..motion };
        assert!(budget(grid, 0, &maximum_matching, request).is_err(), "SAD work must be bounded independently of memory");
        assert!(budget(grid, 0, &maximum_matching, BudgetRequest { pairs: 0, ..request }).is_ok(), "integer-aligned samples do not do SAD work");
        assert!(budget(grid, u64::MAX, &motion, request).is_err());
        assert!(budget(grid, 0, &motion, BudgetRequest { unique: usize::MAX, ..request }).is_err());
        assert!(budget(TimewarpGrid { width: u32::MAX, height: u32::MAX, ..grid }, 0, &motion, request).is_err());
    }

    #[test]
    fn prepared_bad_indices_fractions_and_fields_return_errors() {
        let parameters = parameters(2, 2);
        let host = Host::default();
        let mut plan = timewarp_plan(&context(&parameters, &host)).unwrap().unwrap();
        let saved = plan.steps.clone();
        for step in [TwStep::Whole(usize::MAX), TwStep::Mix(0, 1, f32::NAN), TwStep::Motion { a: 0, b: 1, w: 0.5, flow: usize::MAX }] {
            plan.steps[0].0 = step;
            assert!(plan.render_cpu().is_err());
        }
        plan.steps = saved;
        plan.flows[0].v.clear();
        assert!(plan.render_cpu().is_err());
    }

    #[test]
    fn nonfinite_source_channels_and_mutated_motion_controls_are_checked() {
        struct InvalidSource(f32);
        impl EffectHost for InvalidSource {
            fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
                None
            }
            fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
                None
            }
            fn self_at(&self, _: f64, _: usize) -> Option<Buf> {
                Some(Buf { img: Image::filled(1, 1, [self.0, 0.0, 0.0, 1.0]), offset: [0.0; 2], scale: 1.0 })
            }
        }
        let parameters = parameters(1, 2);
        let host = Host::default();
        let ctx = context(&parameters, &host);
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, -f32::MAX, MAX_CHANNEL_MAGNITUDE.next_up(), -MAX_CHANNEL_MAGNITUDE.next_up()] {
            assert!(timewarp_plan(&context(&parameters, &InvalidSource(value))).err().unwrap().contains("non-finite RGBA"));
            for channel in 0..4 {
                let mut input = Buf { img: Image::filled(1, 1, [0.25; 4]), offset: [0.0; 2], scale: 1.0 };
                input.img.data[0][channel] = value;
                assert!(validate_input(&input).unwrap_err().contains("non-finite RGBA"));
                let mut plan = timewarp_plan(&ctx).unwrap().unwrap();
                plan.frames[0].data[0][channel] = value;
                assert!(plan.render_cpu().unwrap_err().contains("non-finite RGBA"));
            }
        }
        for threshold in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let mut plan = timewarp_plan(&ctx).unwrap().unwrap();
            plan.pm.error_threshold = threshold;
            assert!(plan.render_cpu().unwrap_err().contains("motion weights"));
        }
        let mut malformed = timewarp_plan(&ctx).unwrap().unwrap();
        malformed.frames[0].data.pop();
        assert!(malformed.render_cpu().unwrap_err().contains("frame storage"));
        assert!(timewarp_plan(&ctx).unwrap().unwrap().render_cpu().is_ok(), "a later valid plan still works");
    }

    #[test]
    fn auxiliary_channels_are_checked_before_fitting_can_hide_them() {
        struct InvalidLayer(f32);
        impl EffectHost for InvalidLayer {
            fn layer(&self, _: u64, _: bool) -> Option<LayerPixels> {
                None
            }
            fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
                None
            }
            fn self_at(&self, time: f64, effects: usize) -> Option<Buf> {
                Host::default().self_at(time, effects)
            }
            fn layer_at(&self, _: u64, _: f64, _: bool) -> Option<LayerPixels> {
                Some(LayerPixels { buf: Buf { img: Image::filled(32, 20, [self.0; 4]), offset: [10_000.0, 0.0], scale: 1.0 }, size: SIZE })
            }
        }
        for control in ["matteLayer", "warpLayer"] {
            let mut params = parameters(1, 2);
            params.values.insert(control.into(), Value::Layer(Some(7)));
            for value in [f32::NAN, f32::INFINITY, MAX_CHANNEL_MAGNITUDE.next_up()] {
                let error = timewarp_plan(&context(&params, &InvalidLayer(value))).err().unwrap();
                assert!(error.contains("non-finite RGBA"), "{control}: {error}");
            }
            let host = Host::default();
            assert!(timewarp_plan(&context(&params, &host)).unwrap().unwrap().render_cpu().is_ok());
        }
    }

    #[test]
    fn channel_limit_keeps_matching_brightness_and_filter_intermediates_finite() {
        // Conservative arithmetic bounds, not a large image/expensive matching test.
        let magnitude = MAX_CHANNEL_MAGNITUDE as f64 * 1.01;
        let mean_sum = magnitude * MAX_IMAGE_PIXELS as f64;
        let brightness_gain = 2.0 * magnitude / 1e-4;
        let corrected = magnitude * brightness_gain;
        let luma = corrected * 1.26;
        // A nonnegative f32 accumulation can round an individual increment up;
        // reserve a further factor two instead of assuming a 1% long-sum error.
        let sad = 4.0 * luma * (MAX_DIMENSION as f64).powi(2);
        let parabola_sum = 4.0 * sad;
        let bicubic_sum = magnitude * 16.0 * 1.5 * 1.5;
        let matte_composite = magnitude + magnitude * (1.0 + magnitude);
        let shutter_sum = matte_composite * MAX_SAMPLES as f64;
        for bound in [mean_sum, brightness_gain, corrected, luma, sad, parabola_sum, bicubic_sum, matte_composite, shutter_sum] {
            assert!(bound.is_finite() && bound < f32::MAX as f64 * 0.5, "insufficient f32 headroom: {bound}");
        }
        for magnitude in [-MAX_CHANNEL_MAGNITUDE, MAX_CHANNEL_MAGNITUDE] {
            assert!(validate_input(&Buf { img: Image::filled(1, 1, [magnitude, 0.0, 0.0, 1.0]), offset: [0.0; 2], scale: 1.0 }).is_ok());
        }
        // Near the largest allowed luminance ratio, normal and Extreme filters keep
        // finite pixels. Flat windows also exercise the zero-curvature denominator.
        let a = Image::filled(8, 8, [MAX_CHANNEL_MAGNITUDE; 4]);
        let b = Image::filled(8, 8, [1.0001e-4; 4]);
        let parameters = parameters(1, 2);
        let host = Host::default();
        let mut motion = PixelMotion::from(&context(&parameters, &host), 1.0);
        motion.tile = 2;
        motion.window = 4;
        motion.correct_luminance = true;
        let flow = timewarp_flow(&a, &b, &motion).unwrap();
        assert!(flow.v.iter().flatten().all(|value| value.is_finite()));
        for extreme in [false, true] {
            motion.extreme = extreme;
            let output = timewarp_interpolate(&a, &b, 0.9, &flow, &motion);
            assert!(output.data.iter().flatten().all(|value| value.is_finite()));
        }
        for value in [f32::NAN, f32::INFINITY, f32::MAX, 3.0 * MAX_CHANNEL_MAGNITUDE] {
            let invalid = Image::filled(8, 8, [value; 4]);
            assert!(timewarp_flow(&invalid, &b, &motion).unwrap_err().contains("luminance mean"));
        }
        motion.correct_luminance = false;
        assert!(timewarp_flow(&Image::filled(8, 8, [f32::NAN; 4]), &b, &motion).unwrap_err().contains("non-finite SAD"));
        assert!(timewarp_flow(&a, &b, &motion).is_ok(), "matching remains usable after a checked error");
    }

    #[test]
    fn public_plan_cannot_forge_limits_storage_work_or_vector_spacing() {
        let parameters = parameters(2, 2);
        let host = Host::default();
        let context = context(&parameters, &host);
        for change in 0..7 {
            let mut plan = timewarp_plan(&context).unwrap().unwrap();
            match change {
                0 => plan.budget.frame_limit = usize::MAX,
                1 => plan.budget.flow_limit = usize::MAX,
                2 => plan.budget.total_bytes = 0,
                3 => plan.budget.work = 0,
                4 => {
                    plan.frames = vec![Image::new(1, 1); MAX_PREPARED_FRAMES + 1];
                    plan.budget.frame_limit = MAX_PREPARED_FRAMES + 1;
                }
                5 => {
                    plan.flows = vec![Flow::default(); MAX_PREPARED_FLOWS + 1];
                    plan.budget.flow_limit = MAX_PREPARED_FLOWS + 1;
                }
                _ => {
                    let flow = &mut plan.flows[0];
                    flow.block = MAX_DIMENSION + 1;
                    flow.cols = 1;
                    flow.rows = 1;
                    flow.v = vec![[0.0; 2]];
                }
            }
            assert!(plan.render_cpu().is_err(), "mutation {change} must fail before output allocation");
        }
    }

    #[test]
    fn tiny_former_placement_coordinate_overflow_is_checked_before_sampling() {
        let grid = TimewarpGrid { width: 1, height: 1, offset: [0.0; 2], scale: 1e-300 };
        let source = Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1.0 };
        let formerly_unchecked_x = 0.5 * source.scale / grid.scale;
        let cast = (formerly_unchecked_x - 0.5).floor() as i64;
        assert_eq!(cast, i64::MAX);
        assert!(cast.checked_add(1).is_none(), "the old bilinear neighbour arithmetic overflowed");
        // One pixel and no large allocation. Debug overflow checks expose the former panic;
        // release builds use the checked arithmetic witness rather than relying on a panic.
        if cfg!(debug_assertions) {
            assert!(std::panic::catch_unwind(|| source.img.sample_bilinear(formerly_unchecked_x, 0.5)).is_err());
        }
        assert!(grid.place(Some(source)).unwrap_err().contains("resampling coordinates"));
    }
}

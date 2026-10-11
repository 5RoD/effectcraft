//! The effect plug-in API, version [`PLUGIN_API_VERSION`].
//!
//! A plug-in is anything implementing [`EffectPlugin`]: a [`PluginManifest`] (stable id,
//! display name, Effects & Presets category, typed parameters) and a `render` that processes a
//! [`PluginFrame`] in place. [`register_plugin`] turns it into an ordinary [`EffectSpec`]:
//! instances are property groups like every built-in effect, so plug-in parameters animate,
//! take expressions, show in Effect Controls and are addressable by agents, and the effect is
//! applied by id (`effect.apply {"effect": "org.example.posterize"}`) like a built-in.
//!
//! Plug-ins come from Rust code (implement the trait and register it at start-up) or from
//! sandboxed WebAssembly modules (`effectcraft-plugin`, see `docs/plugins.md`), which implement
//! the same contract through a small C-like ABI.
//!
//! The contract (stable within an API version):
//! * pixels are premultiplied RGBA `f32` (0–1, may exceed 1 in 32 bpc projects), row-major,
//!   in the layer's buffer, which may be scaled (preview resolution) and padded;
//! * parameters are flattened to `f64`s in declaration order: slider / angle / checkbox (0 or 1)
//!   / popup (0-based index) take one value, point two (**frame pixels**, already mapped to the
//!   buffer), colour four (RGBA 0–1);
//! * `time` is layer time in seconds; `scale` is the buffer's pixels per layer pixel (scale
//!   pixel distances like blur radii by it);
//! * rendering must be deterministic (same inputs, same output) and may run on several threads
//!   at once.

use std::sync::{Arc, OnceLock, RwLock};

use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use serde::{Deserialize, Serialize};

use crate::{Buf, CheckedRenderFn, EffectCtx, EffectSpec, ParamSpec};

/// The plug-in API version this build implements.
pub const PLUGIN_API_VERSION: u32 = 1;

/// The most plug-in effects one process can register.
pub const MAX_PLUGINS: usize = 64;

/// Per-frame limits at the plug-in boundary (including padded effect buffers).
pub const MAX_PLUGIN_DIMENSION: u32 = 16_384;
pub const MAX_PLUGIN_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_PLUGIN_PARAMS: usize = 256;

/// A plug-in parameter's kind, default and range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PluginParamKind {
    Slider {
        default: f64,
        #[serde(default)]
        min: f64,
        #[serde(default = "hundred")]
        max: f64,
        /// Slider range (defaults to min..max).
        #[serde(default, rename = "sliderMin")]
        slider_min: Option<f64>,
        #[serde(default, rename = "sliderMax")]
        slider_max: Option<f64>,
        #[serde(default = "two")]
        decimals: u8,
    },
    Angle {
        #[serde(default)]
        default: f64,
    },
    Checkbox {
        #[serde(default)]
        default: bool,
    },
    Popup {
        options: Vec<String>,
        /// 0-based index.
        #[serde(default)]
        default: u32,
    },
    /// Default as fractions of the layer size (`[0.5, 0.5]` = centre).
    Point {
        #[serde(default = "centre")]
        default: [f64; 2],
    },
    /// RGBA, 0–1.
    Color {
        #[serde(default = "white")]
        default: [f64; 4],
    },
}

fn hundred() -> f64 {
    100.0
}
fn two() -> u8 {
    2
}
fn centre() -> [f64; 2] {
    [0.5, 0.5]
}
fn white() -> [f64; 4] {
    [1.0; 4]
}

impl PluginParamKind {
    /// How many `f64`s the parameter flattens to.
    pub fn width(&self) -> usize {
        match self {
            PluginParamKind::Point { .. } => 2,
            PluginParamKind::Color { .. } => 4,
            _ => 1,
        }
    }
}

/// One parameter of a plug-in effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginParam {
    /// Stable id (the property's match name; letters, digits, `_`).
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: PluginParamKind,
}

/// What a plug-in declares about itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// The plug-in API version it was written for (must be [`PLUGIN_API_VERSION`]).
    pub api: u32,
    /// Stable, reverse-domain effect id (`org.example.posterize`); `ec.` is reserved.
    pub id: String,
    pub name: String,
    /// Effects & Presets category: one of [`crate::CATEGORIES`] or a new one.
    pub category: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: Vec<PluginParam>,
}

impl PluginManifest {
    /// Check the manifest against this API version.
    pub fn validate(&self) -> Result<(), String> {
        if self.api != PLUGIN_API_VERSION {
            return Err(format!("plug-in `{}` targets plug-in API {} but this build implements API {PLUGIN_API_VERSION}", self.id, self.api));
        }
        let ok_id = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok_id(&self.id) || self.id.starts_with("ec.") {
            return Err(format!("bad plug-in id `{}` (reverse-domain, letters, digits, `.`, `_`, `-`; `ec.` is reserved)", self.id));
        }
        if self.name.trim().is_empty() || self.category.trim().is_empty() {
            return Err(format!("plug-in `{}` needs a name and a category", self.id));
        }
        if self.params.len() > MAX_PLUGIN_PARAMS {
            return Err(format!("plug-in `{}` has {} parameters (at most {MAX_PLUGIN_PARAMS})", self.id, self.params.len()));
        }
        let mut seen = std::collections::HashSet::new();
        for p in &self.params {
            if p.id.is_empty() || !p.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || !seen.insert(p.id.as_str()) {
                return Err(format!("plug-in `{}`: bad or duplicate parameter id `{}`", self.id, p.id));
            }
            if let PluginParamKind::Popup { options, default } = &p.kind
                && (options.is_empty() || *default as usize >= options.len())
            {
                return Err(format!("plug-in `{}`: popup `{}` needs options and a default index in range", self.id, p.id));
            }
            if let PluginParamKind::Slider { default, min, max, slider_min, slider_max, .. } = &p.kind {
                let (lo, hi) = (slider_min.unwrap_or(*min), slider_max.unwrap_or(*max));
                if [*default, *min, *max, lo, hi].iter().any(|v| !v.is_finite()) || min > max || lo > hi || default < min || default > max {
                    return Err(format!("plug-in `{}`: slider `{}` needs min ≤ default ≤ max (and sliderMin ≤ sliderMax)", self.id, p.id));
                }
            }
            let finite = match &p.kind {
                PluginParamKind::Angle { default } => default.is_finite(),
                PluginParamKind::Point { default } => default.iter().all(|v| v.is_finite()),
                PluginParamKind::Color { default } => default.iter().all(|v| v.is_finite()),
                _ => true,
            };
            if !finite {
                return Err(format!("plug-in `{}`: parameter `{}` needs finite defaults", self.id, p.id));
            }
        }
        Ok(())
    }

    /// Total flattened parameter count.
    pub fn flat_len(&self) -> usize {
        self.params.iter().fold(0usize, |n, p| n.saturating_add(p.kind.width()))
    }
}

/// The pixels a plug-in renders, in place.
pub struct PluginFrame<'a> {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA, row-major, `width * height` pixels.
    pub pixels: &'a mut [[f32; 4]],
    /// Buffer pixels per layer pixel (preview resolution).
    pub scale: f64,
}

impl PluginFrame<'_> {
    /// Validate host inputs before a native call or WebAssembly transfer allocation.
    pub fn validate(&self, params: &PluginParams, time: f64) -> Result<(), String> {
        self.validate_geometry(time)?;
        if params.manifest.params.len() > MAX_PLUGIN_PARAMS || params.flat.len() != params.manifest.flat_len() {
            return Err("plug-in flattened parameter count does not match its bounded manifest".into());
        }
        if let Some(index) = params.flat.iter().position(|value| !value.is_finite()) {
            return Err(format!("plug-in flattened parameter {index} must be finite"));
        }
        Ok(())
    }

    fn validate_geometry(&self, time: f64) -> Result<(), String> {
        if self.width == 0 || self.height == 0 || self.width > MAX_PLUGIN_DIMENSION || self.height > MAX_PLUGIN_DIMENSION {
            return Err(format!("plug-in frame dimensions must be 1..={MAX_PLUGIN_DIMENSION} pixels"));
        }
        let pixels = u64::from(self.width).checked_mul(u64::from(self.height)).ok_or("plug-in frame pixel count overflow")?;
        if pixels > MAX_PLUGIN_PIXELS {
            return Err(format!("plug-in frame exceeds the {MAX_PLUGIN_PIXELS}-pixel limit"));
        }
        let count = usize::try_from(pixels).map_err(|_| "plug-in frame exceeds the address space")?;
        let bytes = count.checked_mul(std::mem::size_of::<[f32; 4]>()).ok_or("plug-in frame byte capacity overflow")?;
        if bytes > isize::MAX as usize || self.pixels.len() != count {
            return Err("plug-in frame pixel storage does not match its dimensions".into());
        }
        if !self.scale.is_finite() || self.scale <= 0.0 || !time.is_finite() {
            return Err("plug-in scale must be finite and positive; time must be finite".into());
        }
        Ok(())
    }
}

/// Parameter values for one render, flattened (see the module docs) with typed accessors.
pub struct PluginParams<'a> {
    pub manifest: &'a PluginManifest,
    pub flat: &'a [f64],
}

impl PluginParams<'_> {
    fn slot(&self, id: &str) -> Option<&[f64]> {
        let mut at = 0usize;
        for p in &self.manifest.params {
            let w = p.kind.width();
            let end = at.checked_add(w)?;
            if p.id == id {
                return self.flat.get(at..end);
            }
            at = end;
        }
        None
    }
    /// Slider / angle / popup index / checkbox (0 or 1).
    pub fn f(&self, id: &str) -> f64 {
        self.slot(id).and_then(|s| s.first().copied()).unwrap_or(0.0)
    }
    pub fn b(&self, id: &str) -> bool {
        self.f(id) != 0.0
    }
    /// Point in frame pixels.
    pub fn point(&self, id: &str) -> [f64; 2] {
        self.slot(id).and_then(|s| Some([*s.first()?, *s.get(1)?])).unwrap_or([0.0; 2])
    }
    pub fn color(&self, id: &str) -> [f64; 4] {
        self.slot(id).and_then(|s| Some([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?])).unwrap_or([1.0; 4])
    }
}

/// An effect plug-in.
pub trait EffectPlugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    /// Process `frame` in place. `time` is layer time in seconds. Return an actionable error
    /// on failure; the host discards this provisional frame instead of publishing/caching it.
    /// Native code may already have modified the provisional pixels when it fails.
    fn render(&self, frame: &mut PluginFrame, params: &PluginParams, time: f64) -> Result<(), String>;
    /// Where the plug-in came from (a file path, `built-in`…), for listings.
    fn source(&self) -> String {
        "built-in".into()
    }
}

static SLOTS: [OnceLock<Arc<dyn EffectPlugin>>; MAX_PLUGINS] = [const { OnceLock::new() }; MAX_PLUGINS];

/// Registered plug-in effects, in registration order.
fn plugin_specs() -> &'static RwLock<Vec<&'static EffectSpec>> {
    static P: OnceLock<RwLock<Vec<&'static EffectSpec>>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(vec![]))
}

/// The registered plug-in effects.
pub fn plugins() -> Vec<&'static EffectSpec> {
    plugin_specs().read().map(|v| v.clone()).unwrap_or_default()
}

/// The plug-in behind a registered effect id.
pub fn plugin(id: &str) -> Option<Arc<dyn EffectPlugin>> {
    SLOTS.iter().filter_map(|s| s.get()).find(|p| p.manifest().id == id).cloned()
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn param_spec(p: &PluginParam) -> ParamSpec {
    let (default, ui) = match &p.kind {
        PluginParamKind::Slider { default, min, max, slider_min, slider_max, decimals } => (
            Value::Scalar(*default),
            ParamUi::Slider { min: *min, max: *max, slider_min: slider_min.unwrap_or(*min), slider_max: slider_max.unwrap_or(*max), decimals: *decimals },
        ),
        PluginParamKind::Angle { default } => (Value::Scalar(*default), ParamUi::Angle),
        PluginParamKind::Checkbox { default } => (Value::Bool(*default), ParamUi::Checkbox),
        PluginParamKind::Popup { options, default } => (Value::Enum(*default), ParamUi::Popup { options: options.clone() }),
        PluginParamKind::Point { default } => (Value::Vec2(*default), ParamUi::Point),
        PluginParamKind::Color { default } => (Value::Color(*default), ParamUi::Color),
    };
    ParamSpec { id: leak(&p.id), name: leak(&p.name), default, ui }
}

/// Register a plug-in effect: it appears in Effects & Presets under its category and is
/// applied by id like a built-in. Fails for a bad manifest, a taken id or when
/// [`MAX_PLUGINS`] are registered.
pub fn register_plugin(plugin: Arc<dyn EffectPlugin>) -> Result<&'static EffectSpec, String> {
    let m = plugin.manifest().clone();
    m.validate()?;
    let mut specs = plugin_specs().write().map_err(|_| "plug-in registry poisoned".to_string())?;
    if crate::registry().iter().any(|s| s.id == m.id) || specs.iter().any(|s| s.id == m.id) {
        return Err(format!("an effect with id `{}` is already registered", m.id));
    }
    let slot = SLOTS.iter().position(|s| s.get().is_none()).ok_or_else(|| format!("too many plug-ins (at most {MAX_PLUGINS})"))?;
    let trampoline = *TRAMPOLINES.get(slot).ok_or("plug-in trampoline unavailable")?;
    SLOTS.get(slot).ok_or("plug-in slot unavailable")?.set(plugin).map_err(|_| "plug-in slot taken".to_string())?;
    let spec: &'static EffectSpec = Box::leak(Box::new(EffectSpec {
        id: leak(&m.id),
        name: leak(&m.name),
        category: leak(&m.category),
        params: m.params.iter().map(param_spec).collect(),
        render: crate::EffectRender::Checked(trampoline),
        gpu: false,
        float: true,
    }));
    specs.push(spec);
    Ok(spec)
}

/// Each slot needs its own `fn` (effect render functions are plain function pointers).
fn trampoline<const I: usize>(ctx: &EffectCtx, buf: Buf) -> Result<Buf, String> {
    run_slot(I, ctx, buf)
}

macro_rules! trampolines {
    ($($i:literal)*) => { [$(trampoline::<$i> as CheckedRenderFn),*] };
}

const TRAMPOLINES: [CheckedRenderFn; MAX_PLUGINS] = trampolines!(
    0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
    32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63
);

/// Flatten an instance's evaluated parameters for the plug-in (points mapped to frame pixels).
pub fn flatten(m: &PluginManifest, ctx: &EffectCtx, buf: &Buf) -> Vec<f64> {
    let mut out = Vec::with_capacity(m.flat_len());
    for p in &m.params {
        let v = ctx.params.get(&p.id);
        match &p.kind {
            PluginParamKind::Point { .. } => {
                let q = v.map(Value::as_vec2).unwrap_or([0.0; 2]);
                let (x, y) = buf.to_px(q);
                out.extend([x, y]);
            }
            PluginParamKind::Color { .. } => {
                let c = v.map(Value::as_color).unwrap_or([1.0; 4]);
                out.extend(c.iter().map(|x| *x as f64));
            }
            PluginParamKind::Checkbox { .. } => out.push(if v.map(Value::as_bool).unwrap_or(false) { 1.0 } else { 0.0 }),
            PluginParamKind::Popup { .. } => out.push(v.map(Value::as_enum).unwrap_or(0) as f64),
            _ => out.push(v.map(Value::as_f64).unwrap_or(0.0)),
        }
    }
    out
}

fn run_slot(slot: usize, ctx: &EffectCtx, mut buf: Buf) -> Result<Buf, String> {
    let plugin = SLOTS.get(slot).and_then(OnceLock::get).ok_or("plug-in slot unavailable")?;
    // A native plug-in has no sandbox. Keep escaped Rust panics inside the checked boundary;
    // provisional pixels are discarded by the caller, with no full-frame backup copy.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PluginFrame { width: buf.img.width, height: buf.img.height, pixels: &mut buf.img.data, scale: buf.scale }.validate_geometry(ctx.time)?;
        let m = plugin.manifest();
        m.validate()?;
        for parameter in &m.params {
            if ctx.params.get(&parameter.id).is_some_and(|value| value.components().iter().any(|v| !v.is_finite())) {
                return Err(format!("plug-in parameter `{}` must be finite", parameter.id));
            }
        }
        let flat = flatten(m, ctx, &buf);
        let params = PluginParams { manifest: m, flat: &flat };
        let mut frame = PluginFrame { width: buf.img.width, height: buf.img.height, pixels: &mut buf.img.data, scale: buf.scale };
        frame.validate(&params, ctx.time)?;
        plugin.render(&mut frame, &params, ctx.time)?;
        Ok(buf)
    }))
    .map_err(|payload| {
        let message =
            payload.downcast_ref::<String>().map(String::as_str).or_else(|| payload.downcast_ref::<&str>().copied()).unwrap_or("non-string panic payload");
        format!("Rust plug-in panicked: {}", message.chars().take(1024).collect::<String>())
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectEnv, EffectRender, Params};
    use effectcraft_raster::Image;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Faulting {
        manifest: PluginManifest,
        calls: Arc<AtomicUsize>,
    }

    impl EffectPlugin for Faulting {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }

        fn render(&self, frame: &mut PluginFrame, params: &PluginParams, _time: f64) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            frame.pixels[0] = [0.1, 0.2, 0.3, 1.0];
            match params.f("mode") as u32 {
                1 => Err("synthetic native error after modifying pixels".into()),
                2 => panic!("synthetic native panic after modifying pixels"),
                _ => Ok(()),
            }
        }
    }

    fn faulting() -> (&'static EffectSpec, Arc<AtomicUsize>) {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "api": 1, "id": "org.test.checked-native", "name": "Checked Native", "category": "Test",
            "params": [
                {"id": "mode", "name": "Mode", "type": "slider", "default": 0, "min": 0, "max": 2},
                {"id": "point", "name": "Point", "type": "point"},
                {"id": "on", "name": "On", "type": "checkbox"},
                {"id": "choice", "name": "Choice", "type": "popup", "options": ["A"]},
                {"id": "tint", "name": "Tint", "type": "color"}
            ]
        }))
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let spec = register_plugin(Arc::new(Faulting { manifest, calls: calls.clone() })).unwrap();
        (spec, calls)
    }

    #[test]
    fn checked_native_plugin_errors_panics_and_hostile_inputs_do_not_escape_or_invoke_invalid_frames() {
        let (spec, calls) = faulting();
        assert!(matches!(spec.render, EffectRender::Checked(_)));
        let render = |params: &Params, image: Image, time: f64, scale: f64, offset: [f64; 2]| {
            let context = EffectCtx { params, time, layer_size: [2.0; 2], seed: 0, adjustment: false, env: EffectEnv::default() };
            crate::apply(spec, &context, Buf { img: image, offset, scale })
        };
        let mut params = Params::default();
        params.values.insert("mode".into(), Value::Scalar(1.0));
        assert!(render(&params, Image::new(2, 2), 0.0, 1.0, [0.0; 2]).unwrap_err().contains("native error"));
        params.values.insert("mode".into(), Value::Scalar(2.0));
        let error = render(&params, Image::new(2, 2), 0.0, 1.0, [0.0; 2]).unwrap_err();
        assert!(error.contains("Rust plug-in panicked: synthetic native panic"), "{error}");
        params.values.insert("mode".into(), Value::Scalar(0.0));
        assert_eq!(render(&params, Image::new(2, 2), 0.0, 1.0, [0.0; 2]).unwrap().img.get(0, 0), [0.1, 0.2, 0.3, 1.0]);
        let valid_calls = calls.load(Ordering::SeqCst);
        for image in [
            Image { width: u32::MAX, height: u32::MAX, data: Vec::new() },
            Image { width: 8192, height: 8192, data: Vec::new() },
            Image { width: 0, height: 2, data: Vec::new() },
            Image { width: 2, height: 2, data: vec![[0.0; 4]; 3] },
            Image { width: 2, height: 2, data: vec![[0.0; 4]; 5] },
        ] {
            assert!(render(&params, image, 0.0, 1.0, [0.0; 2]).is_err());
        }
        for scale in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
            assert!(render(&params, Image::new(2, 2), 0.0, scale, [0.0; 2]).is_err());
        }
        for time in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(render(&params, Image::new(2, 2), time, 1.0, [0.0; 2]).is_err());
        }
        for id in ["mode", "point", "on", "choice", "tint"] {
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut bad = params.clone();
                bad.values.insert(id.into(), Value::Scalar(value));
                assert!(render(&bad, Image::new(2, 2), 0.0, 1.0, [0.0; 2]).unwrap_err().contains(id));
            }
        }
        assert!(render(&params, Image::new(2, 2), 0.0, 1.0, [f64::INFINITY, 0.0]).unwrap_err().contains("flattened parameter"));
        let mut overflow = params.clone();
        overflow.values.insert("point".into(), Value::Vec2([f64::MAX; 2]));
        assert!(render(&overflow, Image::new(2, 2), 0.0, 4.0, [0.0; 2]).unwrap_err().contains("flattened parameter"));
        assert_eq!(calls.load(Ordering::SeqCst), valid_calls, "bad host data never reaches native code");
        assert!(render(&params, Image::new(2, 2), -7.0, 4.0, [0.0; 2]).is_ok());
    }

    #[test]
    fn plugin_typed_accessors_do_not_index_an_incorrect_parameter_kind() {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "api": 1, "id": "org.test.accessors", "name": "Accessors", "category": "Test",
            "params": [{"id": "one", "name": "One", "type": "angle"}]
        }))
        .unwrap();
        let params = PluginParams { manifest: &manifest, flat: &[7.0] };
        assert_eq!(params.point("one"), [0.0; 2]);
        assert_eq!(params.color("one"), [1.0; 4]);
        for kind in [
            PluginParamKind::Angle { default: f64::NAN },
            PluginParamKind::Point { default: [f64::INFINITY; 2] },
            PluginParamKind::Color { default: [f64::NAN; 4] },
        ] {
            let mut bad = manifest.clone();
            bad.params[0].kind = kind;
            assert!(bad.validate().unwrap_err().contains("finite defaults"));
        }
    }
}

//! Shared source for baseline/candidate measurements; no external footage or codecs.
//! Run only after correctness tests. Timings include host generation and GPU completion,
//! but exclude capture/checksum work and device initialization. See the companion runner.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use effectcraft_effects::{Buf, EffectCtx, EffectEnv, EffectHost, EffectRender, LayerPixels, Params};
use effectcraft_gpu::Gpu;
use effectcraft_keyframe::Value;
use effectcraft_raster::Image;
use effectcraft_render::{Accelerator, FxStep};
use effectcraft_time::{FrameRate, Tick};
use rayon::prelude::*;
use serde_json::{Value as Json, json};

const ID: &str = "ec.time.timewarp";
const MAX_CAPTURE: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Case {
    method: u32,
    samples: u32,
    matte: bool,
    extreme: bool,
}

struct Host {
    width: u32,
    height: u32,
    fps: u32,
    reads: AtomicUsize,
    layers: AtomicUsize,
    nanos: AtomicU64,
}

impl Host {
    fn image(&self, time: f64, matte: bool) -> Image {
        let start = Instant::now();
        // Match the native host's floor-of-exact-Tick selection. This deliberately retains
        // the old Timewarp fetch quantization; source-boundary fidelity is a separate test.
        let frame = FrameRate { num: self.fps as i64, den: 1 }.frame_at(Tick::from_seconds_f64(time));
        let mut image = Image::new(self.width, self.height);
        image.rows_mut().for_each(|(y, row)| {
            for (x, pixel) in row.iter_mut().enumerate() {
                let shifted = x as i64 - frame * 3;
                let checker = ((shifted.div_euclid(7) + (y as i64).div_euclid(5)).rem_euclid(2)) as f32;
                let dx = shifted as f64 - self.width as f64 * 0.35;
                let dy = y as f64 - self.height as f64 * 0.55;
                let radius = (self.height as f64 * 0.16).max(2.0);
                let bright = (dx * dx + dy * dy < radius * radius) as u8 as f32;
                let occluder = x > self.width as usize / 2 && x < self.width as usize * 3 / 5;
                let alpha = if occluder { 0.85 } else { 0.6 };
                *pixel = if matte {
                    let a = ((shifted as f32 / self.width as f32) * 0.8 + 0.2).clamp(0.0, 1.0);
                    [a; 4]
                } else {
                    [alpha * (0.1 + checker * 0.35 + bright * 1.8), alpha * (0.2 + checker * 0.15), alpha * (0.15 + bright * 0.9), alpha]
                };
            }
        });
        self.nanos.fetch_add(start.elapsed().as_nanos().min(u64::MAX as u128) as u64, Relaxed);
        image
    }
}

impl EffectHost for Host {
    fn layer(&self, id: u64, effects: bool) -> Option<LayerPixels> {
        self.layer_at(id, 0.0, effects)
    }
    fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
        None
    }
    fn self_at(&self, time: f64, _: usize) -> Option<Buf> {
        self.reads.fetch_add(1, Relaxed);
        Some(Buf { img: self.image(time, false), offset: [0.0; 2], scale: 1.0 })
    }
    fn layer_at(&self, id: u64, time: f64, _: bool) -> Option<LayerPixels> {
        if id != 7 {
            return None;
        }
        self.layers.fetch_add(1, Relaxed);
        Some(LayerPixels { buf: Buf { img: self.image(time, true), offset: [0.0; 2], scale: 1.0 }, size: [self.width as f64, self.height as f64] })
    }
}

fn cases(suite: &str) -> Result<(u32, u32, Vec<Case>), String> {
    let base = |samples: &[u32]| -> Vec<Case> {
        (0..=2).flat_map(|method| samples.iter().map(move |&samples| Case { method, samples, matte: false, extreme: false })).collect()
    };
    match suite {
        "tiny" => {
            let mut cases = base(&[1, 8, 64]);
            cases.extend([
                Case { method: 2, samples: 8, matte: true, extreme: false },
                Case { method: 2, samples: 64, matte: true, extreme: false },
                Case { method: 2, samples: 8, matte: false, extreme: true },
                Case { method: 2, samples: 64, matte: false, extreme: true },
            ]);
            Ok((31, 19, cases))
        }
        // Four samples keep the historical implementation away from the pathological
        // full-resolution 64-output allocation. Same controls are used by both variants.
        "playback" => Ok((2560, 1440, base(&[1, 4]))),
        "high-samples" => {
            let mut cases = base(&[64]);
            cases.push(Case { method: 2, samples: 64, matte: false, extreme: true });
            Ok((2560, 1440, cases))
        }
        _ => Err("suite must be tiny, playback, or high-samples".into()),
    }
}

fn number(value: Option<String>, label: &str, min: u32, max: u32) -> Result<u32, String> {
    let n = value.ok_or_else(|| format!("missing {label}"))?.parse::<u32>().map_err(|_| format!("invalid {label}"))?;
    if !(min..=max).contains(&n) { Err(format!("{label} must be {min}..={max}")) } else { Ok(n) }
}

fn capture(writer: &mut BufWriter<File>, case: usize, frame: u32, output: &Buf) -> Result<(), String> {
    for n in [case as u32, frame, output.img.width, output.img.height] {
        writer.write_all(&n.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    for n in [output.offset[0], output.offset[1], output.scale] {
        writer.write_all(&n.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    // The benchmark targets Windows x64. Fail rather than write a mislabeled capture.
    if !cfg!(target_endian = "little") {
        return Err("capture requires a little-endian target".into());
    }
    writer.write_all(bytemuck::cast_slice(&output.img.data)).map_err(|e| e.to_string())
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    values.get(((values.len().saturating_sub(1)) as f64 * fraction).ceil() as usize).copied().unwrap_or(0.0)
}

fn benchmark(mut args: impl Iterator<Item = String>) -> Result<Json, String> {
    let (mut suite, mut backend) = ("tiny".to_owned(), "cpu".to_owned());
    let (mut fps, mut frames, mut warmup) = (120, 3, 0);
    let mut path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--suite" => suite = args.next().ok_or("missing suite")?,
            "--backend" => backend = args.next().ok_or("missing backend")?,
            "--fps" => fps = number(args.next(), "fps", 60, 144)?,
            "--frames" => frames = number(args.next(), "frames", 1, 1200)?,
            "--warmup" => warmup = number(args.next(), "warmup", 0, 5)?,
            "--capture" => path = Some(PathBuf::from(args.next().ok_or("missing capture path")?)),
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    if ![60, 120, 144].contains(&fps) || !["cpu", "gpu"].contains(&backend.as_str()) {
        return Err("fps must be 60/120/144 and backend must be cpu/gpu".into());
    }
    let (width, height, cases) = cases(&suite)?;
    let bytes = (u64::from(width) * u64::from(height) * 16 + 40)
        .checked_mul(cases.len() as u64)
        .and_then(|n| n.checked_mul(frames as u64))
        .ok_or("capture size overflow")?;
    if path.is_some() && bytes > MAX_CAPTURE {
        return Err(format!("capture would exceed {MAX_CAPTURE} bytes; use a smaller correctness run, then omit --capture for long throughput runs"));
    }
    let spec = effectcraft_effects::find(ID).ok_or("Timewarp is not registered")?;
    if suite == "high-samples" && !matches!(spec.render, EffectRender::Checked(_)) {
        return Err("full-resolution 64-sample historical plans are intentionally prohibited".into());
    }
    let gpu = if backend == "gpu" { Some(Gpu::try_headless()?) } else { None };
    let adapter = gpu.as_ref().map(Accelerator::name);
    if let Some(name) = &adapter
        && (!name.contains("NVIDIA") || !name.contains("Vulkan"))
    {
        return Err(format!("acceptance requires the actual NVIDIA Vulkan adapter, got {name}"));
    }
    let mut writer = path.map(File::create).transpose().map_err(|e| e.to_string())?.map(BufWriter::new);
    let mut results = Vec::new();
    for (case_index, case) in cases.into_iter().enumerate() {
        let size = [width as f64, height as f64];
        let mut parameters = Params { values: spec.params.iter().map(|p| (p.id.into(), effectcraft_effects::default_value(p, size))).collect() };
        for (key, value) in [
            ("method", Value::Enum(case.method)),
            ("speed", Value::Scalar(50.0)),
            ("motionBlur/enableMotionBlur", Value::Bool(true)),
            ("motionBlur/shutterControl", Value::Enum(1)),
            ("motionBlur/shutterAngle", Value::Scalar(180.0)),
            ("motionBlur/shutterSamples", Value::Scalar(case.samples as f64)),
            ("matteLayer", Value::Layer(case.matte.then_some(7))),
            ("tuning/filtering", Value::Enum(case.extreme as u32)),
        ] {
            parameters.values.insert(key.into(), value);
        }
        let host = Host { width, height, fps, reads: AtomicUsize::new(0), layers: AtomicUsize::new(0), nanos: AtomicU64::new(0) };
        let transfers = gpu.as_ref().map(|g| g.context().transfer_stats());
        let pool = gpu.as_ref().map(|g| g.context().pool_stats());
        let mut millis = Vec::new();
        let mut generation = Vec::new();
        let mut checksums = Vec::new();
        for index in 0..frames + warmup {
            // A distinct output instant each time; no output cache. Initialization of the
            // incoming layer buffer is outside the measured effect invocation.
            let time = (8.8 + index as f64) / fps as f64;
            let input = Buf { img: Image::new(width, height), offset: [0.0; 2], scale: 1.0 };
            let ctx = EffectCtx {
                params: &parameters,
                time,
                layer_size: size,
                seed: 1,
                adjustment: false,
                env: EffectEnv { host: Some(&host), comp_time: time, frame_rate: fps as f64, ..Default::default() },
            };
            let before = host.nanos.load(Relaxed);
            let start = Instant::now();
            let output = match &gpu {
                Some(gpu) => {
                    let step = FxStep { spec, ctx };
                    match spec.render {
                        EffectRender::Checked(_) => gpu.effect_checked(&step, &input, None)?,
                        EffectRender::Legacy(_) => gpu.effects(&[step], &input, None),
                    }
                    .ok_or("GPU did not execute Timewarp; CPU fallback is prohibited in this harness")?
                }
                None => effectcraft_effects::apply(spec, &ctx, input)?,
            };
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if index >= warmup {
                millis.push(elapsed);
                generation.push((host.nanos.load(Relaxed) - before) as f64 / 1e6);
                let mut hash = 0xcbf29ce484222325u64;
                for value in output.img.data.iter().flatten() {
                    if !value.is_finite() {
                        return Err("output contains nonfinite pixels".into());
                    }
                    hash = (hash ^ value.to_bits() as u64).wrapping_mul(0x100000001b3);
                }
                checksums.push(format!("{hash:016x}"));
                if let Some(writer) = &mut writer {
                    capture(writer, case_index, index - warmup, &output)?;
                }
            }
        }
        let mut ordered = millis.clone();
        ordered.sort_by(f64::total_cmp);
        let after = gpu.as_ref().map(|g| g.context().transfer_stats());
        let after_pool = gpu.as_ref().map(|g| g.context().pool_stats());
        let delta = transfers.zip(after).map(|(a, b)| {
            json!({"uploads":b.uploads-a.uploads,"upload_bytes":b.upload_bytes-a.upload_bytes,
            "readbacks":b.readbacks-a.readbacks,"readback_bytes":b.readback_bytes-a.readback_bytes})
        });
        let pool_delta = pool.zip(after_pool).map(|(a, b)| json!({"hits":b.0-a.0,"misses":b.1-a.1}));
        results.push(json!({"method":case.method,"samples":case.samples,"matte":case.matte,"extreme":case.extreme,
            "first_ms":millis.first(),"frame_ms":millis,"p50_ms":percentile(&ordered,0.5),"p95_ms":percentile(&ordered,0.95),
            "p99_ms":percentile(&ordered,0.99),"effect_fps":frames as f64*1000.0/ordered.iter().sum::<f64>(),
            "host_generation_ms":generation,"source_reads":host.reads.load(Relaxed),"layer_reads":host.layers.load(Relaxed),
            "checksums":checksums,"transfers":delta,"pool":pool_delta}));
    }
    if let Some(writer) = writer.as_mut() {
        writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(json!({"suite":suite,"backend":backend,"adapter":adapter,"width":width,"height":height,"fps":fps,
        "frames":frames,"warmup":warmup,"frame_budget_ms":1000.0/fps as f64,"capture_bytes":writer.is_some().then_some(bytes),
        "results":results,"scope":"direct effects; source generation included; device init, input init, checksum/capture excluded; no decode, UI, encoding, disk-cache or export-concurrency measurement"}))
}

fn compare(mut args: impl Iterator<Item = String>) -> Result<Json, String> {
    let left = File::open(args.next().ok_or("missing left capture")?).map_err(|e| e.to_string())?;
    let right = File::open(args.next().ok_or("missing right capture")?).map_err(|e| e.to_string())?;
    if left.metadata().map_err(|e| e.to_string())?.len() > MAX_CAPTURE || right.metadata().map_err(|e| e.to_string())?.len() > MAX_CAPTURE {
        return Err("capture exceeds the supported file-size bound".into());
    }
    let mut left = BufReader::new(left);
    let mut right = BufReader::new(right);
    let tolerance = args.next().ok_or("missing tolerance")?.parse::<f32>().map_err(|_| "invalid tolerance")?;
    if !tolerance.is_finite() || !(0.0..=0.01).contains(&tolerance) || args.next().is_some() {
        return Err("tolerance must be finite and 0..=0.01".into());
    }
    let (mut records, mut channels, mut different, mut maximum) = (0u64, 0u64, 0u64, 0f32);
    loop {
        let mut a = [0u8; 40];
        let mut b = [0u8; 40];
        let n = left.read(&mut a[..1]).map_err(|e| e.to_string())?;
        let m = right.read(&mut b[..1]).map_err(|e| e.to_string())?;
        if n == 0 && m == 0 {
            break;
        }
        if n != 1 || m != 1 {
            return Err("capture record counts differ".into());
        }
        left.read_exact(&mut a[1..]).map_err(|e| e.to_string())?;
        right.read_exact(&mut b[1..]).map_err(|e| e.to_string())?;
        if a != b {
            return Err("capture frame order/geometry differs".into());
        }
        let width = u32::from_le_bytes(a.get(8..12).ok_or("bad header")?.try_into().map_err(|_| "bad header")?);
        let height = u32::from_le_bytes(a.get(12..16).ok_or("bad header")?.try_into().map_err(|_| "bad header")?);
        if width == 0 || height == 0 || width > 2560 || height > 1440 {
            return Err("unsupported capture dimensions".into());
        }
        let mut remaining = u64::from(width) * u64::from(height) * 16;
        let (mut a, mut b) = ([0u8; 65536], [0u8; 65536]);
        while remaining > 0 {
            let len = remaining.min(a.len() as u64) as usize;
            let a = a.get_mut(..len).ok_or("bad capture chunk")?;
            let b = b.get_mut(..len).ok_or("bad capture chunk")?;
            left.read_exact(a).map_err(|e| e.to_string())?;
            right.read_exact(b).map_err(|e| e.to_string())?;
            for (a, b) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
                let a = f32::from_le_bytes(*a);
                let b = f32::from_le_bytes(*b);
                if !a.is_finite() || !b.is_finite() {
                    return Err("nonfinite capture pixels".into());
                }
                let delta = (a - b).abs();
                maximum = maximum.max(delta);
                different += (delta > tolerance) as u64;
                channels += 1;
            }
            remaining -= len as u64;
        }
        records += 1;
        if records > 16 * 1200 {
            return Err("capture exceeds supported record count".into());
        }
    }
    if records == 0 || different != 0 {
        return Err(format!("capture mismatch: {records} records, {different}/{channels} channels over {tolerance}, max {maximum}"));
    }
    Ok(json!({"records":records,"channels":channels,"tolerance":tolerance,"max_abs":maximum,"over":different}))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("run") => benchmark(args),
        Some("compare") => compare(args),
        _ => Err("usage: timewarp_bench run --suite tiny|playback|high-samples --backend cpu|gpu [--fps 120 --frames 3 --warmup 0 --capture PATH]; compare LEFT RIGHT TOLERANCE".into()),
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("Timewarp benchmark: {error}");
            std::process::exit(1);
        }
    }
}

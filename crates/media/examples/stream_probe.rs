//! Standalone metadata/read correctness and process-resource probe, without viewer/GPU work.
//! Example timings are diagnostic; production timeline acceptance uses the real desktop viewer.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo, clippy::unimplemented, clippy::unreachable)]

use std::io::Write;
use std::time::Instant;

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("usage: stream_probe <media path> [frames: 0..16]")?;
    let count = match args.next() {
        Some(value) => value.to_str().ok_or("frame count must be valid UTF-8")?.parse::<u32>().map_err(|e| e.to_string())?,
        None => 0,
    };
    if count > 16 || args.next().is_some() {
        return Err("usage: stream_probe <media path> [frames: 0..16]".into());
    }
    let started = Instant::now();
    let footage = effectcraft_media::probe(&path).map_err(|e| e.to_string())?;
    let probe_ms = started.elapsed().as_secs_f64() * 1000.0;
    let pool = effectcraft_media::MediaPool::new();
    pool.set_budget(128 << 20);
    let mut frames = Vec::new();
    for index in 0..count {
        let started = Instant::now();
        let image = pool.frame_at(&footage, footage.frame_rate.tick_of(i64::from(index))).map_err(|e| e.to_string())?;
        let fingerprint = image
            .data
            .iter()
            .flatten()
            .fold(0xcbf2_9ce4_8422_2325u64, |value, channel| (value ^ u64::from(channel.to_bits())).wrapping_mul(0x0000_0100_0000_01b3));
        frames.push(serde_json::json!({"index": index, "width": image.width, "height": image.height,
            "diagnosticElapsedMs": started.elapsed().as_secs_f64() * 1000.0,
            "pixelFingerprint64": format!("{fingerprint:016x}")}));
    }
    let stats = pool.stats();
    let output = serde_json::json!({"scope": "standalone metadata/frame correctness; not timeline or production-speed acceptance",
        "footage": footage, "diagnosticProbeElapsedMs": probe_ms, "frames": frames,
        "cache": {"bytes": stats.bytes, "frames": stats.frames, "opened": stats.opened,
                  "hits": stats.hits, "misses": stats.misses, "prefetched": stats.prefetched}});
    writeln!(std::io::stdout().lock(), "{output}").map_err(|e| e.to_string())
}

fn main() -> Result<(), String> {
    run()
}

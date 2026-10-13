//! Run with the normal release profile. Measures preparation separately from steady lookups;
//! this microbenchmark does not measure media decoding, optical flow or overall playback.
use effectcraft_keyframe::{Keyframe, Value, integral::ScalarIntegral};
use effectcraft_time::{FrameRate, Tick};
use std::{hint::black_box, time::Instant};

fn main() -> Result<(), String> {
    let mut rows = Vec::new();
    for count in [2usize, 256, 16_000, 100_000] {
        let keys: Vec<_> = (0..count)
            .map(|i| {
                let value = 50.0 + 25.0 * (i as f64 * 0.37).sin();
                Keyframe::new(FrameRate::FPS_120.tick_of(i as i64), Value::Scalar(value))
            })
            .collect();
        let started = Instant::now();
        let curve = ScalarIntegral::new(&keys)?;
        let preparation_ms = started.elapsed().as_secs_f64() * 1000.0;
        let end = keys.last().ok_or("benchmark keys are empty")?.time.0;
        let queries = 50_000usize;
        let mut runs = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            for i in 0..queries {
                let ticks = (end as i128 * ((i * 7919) % queries) as i128 / queries as i128) as i64;
                black_box(curve.between(Tick::ZERO, black_box(Tick(ticks)))?);
            }
            runs.push(started.elapsed().as_secs_f64() * 1e9 / queries as f64);
        }
        runs.sort_by(f64::total_cmp);
        let median = *runs.get(3).ok_or("benchmark median is missing")?;
        rows.push(serde_json::json!({"keys":count,"preparationMs":preparation_ms,"curveBytes":curve.memory_bytes(),"queriesPerRun":queries,"runs":7,"medianNanosecondsPerQuery":median,"nanosecondsPerQuery":runs}));
    }
    println!("{}", serde_json::json!({"scope":"scalar integral lookup microbenchmark; no overall playback or export speed claim","rows":rows}));
    Ok(())
}

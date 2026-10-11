//! Bounded, opt-in observation at the active viewer's paint site. No pixels or cache access.

use crate::frames::FrameKey;
use effectcraft_engine::preview::PreviewPlan;
use serde_json::{Value, json};

pub const DEFAULT_SAMPLES: usize = 8192;
pub const MAX_SAMPLES: usize = 120_000;
pub const DEFAULT_SECONDS: f64 = 30.0;

#[derive(Clone, Copy)]
pub enum Paint {
    Frame { key: FrameKey, native: bool },
    Missing,
    Snapshot,
    Wireframe,
}

#[derive(Clone, Copy)]
pub struct Sample {
    pub update: u64,
    pub now: f64,
    pub requested: FrameKey,
    pub paint: Paint,
    pub playing: bool,
    pub caching: bool,
    pub waiting: bool,
    pub plan: Option<PreviewPlan>,
}

#[derive(Default)]
pub struct Observer {
    run: Option<Run>,
    pending: Option<Sample>,
}

struct Run {
    started: f64,
    seconds: f64,
    limit: usize,
    samples: Vec<Sample>,
    stopped: Option<&'static str>,
    error: Option<String>,
}

impl Observer {
    pub fn start(&mut self, now: f64, limit: usize, seconds: f64) -> Result<(), String> {
        if !now.is_finite() || now < 0.0 || !(1..=MAX_SAMPLES).contains(&limit) || !seconds.is_finite() || !(0.001..=300.0).contains(&seconds) {
            return Err("viewer observation requires a finite clock, 1..=120000 samples and 0.001..=300 seconds".into());
        }
        let mut samples = Vec::new();
        samples.try_reserve_exact(limit).map_err(|e| format!("cannot reserve viewer observations: {e}"))?;
        self.run = Some(Run { started: now, seconds, limit, samples, stopped: None, error: None });
        self.pending = None;
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.stopped.is_none())
    }

    pub fn stop(&mut self) {
        if let Some(run) = &mut self.run {
            run.stopped.get_or_insert("explicit");
        }
    }

    pub fn control(&mut self, params: &Value, now: f64) -> Result<Value, String> {
        if !params.is_object() {
            return Err("viewer observation parameters must be an object".into());
        }
        let action = match params.get("action") {
            None => "status",
            Some(value) => value.as_str().ok_or("observation action must be a string")?,
        };
        let integer = |name: &str, default: usize| -> Result<usize, String> {
            match params.get(name) {
                None => Ok(default),
                Some(value) => value.as_u64().and_then(|v| usize::try_from(v).ok()).ok_or_else(|| format!("observation {name} must be a nonnegative integer")),
            }
        };
        let offset = integer("offset", 0)?;
        let count = integer("count", 0)?;
        if count > 2048 {
            return Err("read at most 2048 viewer samples per page".into());
        }
        match action {
            "start" => {
                let seconds = match params.get("seconds") {
                    None => DEFAULT_SECONDS,
                    Some(value) => value.as_f64().ok_or("observation seconds must be numeric")?,
                };
                self.start(now, integer("maxSamples", DEFAULT_SAMPLES)?, seconds)?;
            }
            "stop" => self.stop(),
            "reset" => {
                self.run = None;
                self.pending = None;
            }
            "status" | "read" => {}
            _ => return Err("observation action must be start, stop, reset, status or read".into()),
        }
        if let Some(run) = &mut self.run
            && run.stopped.is_none()
            && now.is_finite()
            && now - run.started > run.seconds
        {
            run.stopped = Some("durationLimit");
        }
        self.read(offset, count)
    }

    /// Discard earlier egui passes; a viewer hidden in the final pass produces no sample.
    pub fn begin_pass(&mut self) {
        self.pending = None;
    }

    pub fn stage(&mut self, sample: Sample) {
        if self.active() {
            self.pending = Some(sample);
        }
    }

    pub fn finish_pass(&mut self, discarded: bool) {
        if let Some(sample) = self.pending.take()
            && !discarded
        {
            self.record(sample);
        }
    }

    /// Called once for a visible active viewer paint. Does not request a repaint or allocate.
    pub fn record(&mut self, sample: Sample) {
        let Some(run) = &mut self.run else { return };
        if run.stopped.is_some() {
            return;
        }
        let previous = run.samples.last();
        if previous.is_some_and(|s| s.update == sample.update) {
            return;
        }
        if !sample.now.is_finite() || sample.now < run.started || previous.is_some_and(|s| sample.now < s.now || sample.update < s.update) {
            run.stopped = Some("invalidClock");
            run.error = Some("viewer clock/update moved backward or became nonfinite".into());
            return;
        }
        if sample.plan.is_some_and(|p| p.step < 1 || p.start > p.end || !p.fps.is_finite() || p.fps <= 0.0) {
            run.stopped = Some("invalidPlan");
            run.error = Some("viewer preview plan has invalid range, step or frame rate".into());
            return;
        }
        if sample.now - run.started > run.seconds {
            run.stopped = Some("durationLimit");
            return;
        }
        if run.samples.len() >= run.limit {
            run.stopped = Some("sampleLimit");
            return;
        }
        run.samples.push(sample);
        if run.samples.len() == run.limit {
            run.stopped = Some("sampleLimit");
        }
    }

    /// Reading is explicit and bounded. Quantile scratch allocation is fallible.
    pub fn read(&self, offset: usize, count: usize) -> Result<Value, String> {
        if count > 2048 {
            return Err("read at most 2048 viewer samples per page".into());
        }
        let Some(run) = &self.run else { return Ok(json!({"recording": false, "samples": 0})) };
        let mut holds = Vec::new();
        holds.try_reserve_exact(run.samples.len()).map_err(|e| format!("cannot summarize viewer cadence: {e}"))?;
        let mut distinct = 0_u64;
        let mut repeated = 0_u64;
        let mut stale = 0_u64;
        let mut missing = 0_u64;
        let mut snapshots = 0_u64;
        let mut wireframes = 0_u64;
        let mut native = 0_u64;
        let mut skipped = 0_u64;
        let mut loops = 0_u64;
        let mut reversed = 0_u64;
        let mut transitions = 0_u64;
        let mut unclassified = 0_u64;
        let mut previous: Option<&Sample> = None;
        let mut held: Option<(FrameKey, f64)> = None;
        let mut first_match = None;
        for sample in &run.samples {
            match sample.paint {
                Paint::Frame { key, native: is_native } => {
                    native = native.saturating_add(u64::from(is_native));
                    if key != sample.requested {
                        stale = stale.saturating_add(1);
                    } else if first_match.is_none() {
                        first_match = Some((sample.now - run.started) * 1000.0);
                    }
                    if let Some((last, since)) = held {
                        if last == key {
                            repeated = repeated.saturating_add(1);
                        } else {
                            distinct = distinct.saturating_add(1);
                            holds.push((sample.now - since) * 1000.0);
                            held = Some((key, sample.now));
                        }
                    } else {
                        distinct = distinct.saturating_add(1);
                        held = Some((key, sample.now));
                    }
                    if let Some(before) = previous {
                        classify(before, sample, &mut skipped, &mut loops, &mut reversed, &mut transitions, &mut unclassified);
                    }
                    previous = Some(sample);
                }
                Paint::Missing => {
                    missing = missing.saturating_add(1);
                    previous = None;
                    held = None;
                }
                Paint::Snapshot => {
                    snapshots = snapshots.saturating_add(1);
                    previous = None;
                    held = None;
                }
                Paint::Wireframe => {
                    wireframes = wireframes.saturating_add(1);
                    previous = None;
                    held = None;
                }
            }
        }
        holds.sort_unstable_by(f64::total_cmp);
        let percentile = |percent: usize| {
            if holds.is_empty() {
                None
            } else {
                let index = holds.len().saturating_mul(percent).div_ceil(100).saturating_sub(1);
                holds.get(index).copied()
            }
        };
        let mut page = Vec::new();
        page.try_reserve_exact(count.min(run.samples.len().saturating_sub(offset))).map_err(|e| format!("cannot read viewer samples: {e}"))?;
        page.extend(run.samples.iter().skip(offset).take(count).map(sample_json));
        let last = run.samples.last().map(|s| s.now);
        Ok(json!({
            "metric": "active composition viewer draw cadence; not OS scanout",
            "recording": run.stopped.is_none(), "stopReason": run.stopped, "error": run.error,
            "started": run.started, "observedSeconds": last.map(|t| t - run.started),
            "sampleLimit": run.limit, "durationLimitSeconds": run.seconds, "samples": run.samples.len(),
            "firstRequestedDrawMs": first_match,
            "distinctFrames": distinct, "repeatedDraws": repeated, "staleDraws": stale,
            "missingTextureDraws": missing, "snapshotDraws": snapshots, "wireframeDraws": wireframes,
            "nativeTextureDraws": native, "minimumSkippedSelectedFrames": skipped,
            "observedLoops": loops, "reverseTransitions": reversed, "seriesTransitions": transitions,
            "unclassifiedTransitions": unclassified,
            "completedHoldIntervals": holds.len(), "holdMs": {"p50": percentile(50), "p95": percentile(95), "p99": percentile(99), "max": holds.last()},
            "unfinishedHoldMs": held.zip(last).map(|((_, since), last)| (last - since) * 1000.0),
            "pageOffset": offset, "page": page,
            "limits": "hidden viewers create no samples; multiple loops between draws and interrupted runs cannot be reconstructed"
        }))
    }
}

fn classify(before: &Sample, after: &Sample, skipped: &mut u64, loops: &mut u64, reversed: &mut u64, transitions: &mut u64, unclassified: &mut u64) {
    let (Paint::Frame { key: a, .. }, Paint::Frame { key: b, .. }) = (before.paint, after.paint) else { return };
    if a == b {
        return;
    }
    if !a.same_series(&b) {
        *transitions = transitions.saturating_add(1);
        return;
    }
    if !before.playing || !after.playing || before.caching || after.caching {
        if b.frame < a.frame {
            *reversed = reversed.saturating_add(1);
        }
        return;
    }
    let (Some(p), Some(q)) = (before.plan, after.plan) else { return };
    if p != q || a != before.requested || b != after.requested {
        *unclassified = unclassified.saturating_add(1);
        return;
    }
    let step = i128::from(p.step);
    let start = i128::from(p.start);
    let end = i128::from(p.end);
    let (from, to) = (i128::from(a.frame), i128::from(b.frame));
    if from < start || from > end || to < start || to > end || (from - start) % step != 0 || (to - start) % step != 0 {
        *unclassified = unclassified.saturating_add(1);
        return;
    }
    let slots = (end - start) / step + 1;
    let elapsed_steps = (after.now - before.now) * p.fps / p.step as f64;
    if !elapsed_steps.is_finite() || elapsed_steps >= slots as f64 {
        *unclassified = unclassified.saturating_add(1);
        return;
    }
    let distance = if to < from {
        if !p.looping {
            *reversed = reversed.saturating_add(1);
            return;
        }
        slots - (from - start) / step + (to - start) / step
    } else {
        (to - from) / step
    };
    if distance as f64 > elapsed_steps.ceil() + 1.0 {
        // A CTI jump while playing is not evidence that the scheduled preview dropped frames.
        *unclassified = unclassified.saturating_add(1);
        return;
    }
    if to < from {
        *loops = loops.saturating_add(1);
    }
    if let Ok(gap) = u64::try_from(distance.saturating_sub(1)) {
        *skipped = skipped.saturating_add(gap);
    }
}

fn key_json(key: FrameKey) -> Value {
    json!({"revision": key.revision, "content": key.content, "comp": key.comp,
        "frame": key.frame, "scale": key.scale, "view": key.view, "options": key.opts})
}

fn sample_json(sample: &Sample) -> Value {
    let (shown, native, paint) = match sample.paint {
        Paint::Frame { key, native } => (Some(key_json(key)), Some(native), "frame"),
        Paint::Missing => (None, None, "missing"),
        Paint::Snapshot => (None, None, "snapshot"),
        Paint::Wireframe => (None, None, "wireframe"),
    };
    json!({"update": sample.update, "time": sample.now, "requested": key_json(sample.requested),
        "shown": shown, "native": native, "paint": paint, "playing": sample.playing,
        "caching": sample.caching, "waiting": sample.waiting,
        "plan": sample.plan.map(|p| json!({"start": p.start, "end": p.end, "step": p.step, "fps": p.fps, "looping": p.looping}))})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(frame: i64) -> FrameKey {
        FrameKey { revision: 1, content: 1, comp: 1, frame, scale: 1000, view: 0, opts: 0 }
    }
    fn sample(update: u64, requested: i64, shown: i64) -> Sample {
        Sample {
            update,
            now: update as f64 / 120.0,
            requested: key(requested),
            paint: Paint::Frame { key: key(shown), native: true },
            playing: true,
            caching: false,
            waiting: false,
            plan: Some(PreviewPlan {
                start: 0,
                end: 119,
                first: 0,
                step: 1,
                fps: 120.0,
                looping: true,
                cache_first: false,
                video: true,
                audio: false,
                origin: Default::default(),
            }),
        }
    }

    #[test]
    fn advancing_clock_with_stale_texture_does_not_claim_new_frames() {
        let mut o = Observer::default();
        o.start(0.0, 10, 1.0).unwrap();
        for i in 0..5 {
            o.record(sample(i, i as i64, 0));
        }
        let s = o.read(0, 10).unwrap();
        assert_eq!(s["distinctFrames"], 1);
        assert_eq!(s["repeatedDraws"], 4);
        assert_eq!(s["staleDraws"], 4);
        assert_eq!(s["minimumSkippedSelectedFrames"], 0);
        assert_eq!(s["completedHoldIntervals"], 0);
    }

    #[test]
    fn planned_skip_loop_and_reverse_are_separate_from_missing_selected_frames() {
        let mut o = Observer::default();
        o.start(0.0, 20, 5.0).unwrap();
        for (i, (frame, now)) in [(0, 0.0), (2, 2.0 / 120.0), (6, 6.0 / 120.0), (118, 118.0 / 120.0), (0, 1.0)].into_iter().enumerate() {
            let mut s = sample(i as u64, frame, frame);
            s.now = now;
            if let Some(p) = &mut s.plan {
                p.step = 2;
            }
            o.record(s);
        }
        let mut reverse = sample(5, 0, 0);
        reverse.now = 1.1;
        reverse.playing = false;
        o.record(reverse);
        let mut reverse = sample(6, -1, -1);
        reverse.now = 1.2;
        reverse.playing = false;
        o.record(reverse);
        let s = o.read(0, 0).unwrap();
        assert_eq!(s["minimumSkippedSelectedFrames"], 56);
        assert_eq!(s["observedLoops"], 1);
        assert_eq!(s["reverseTransitions"], 1);
        assert_eq!(s["distinctFrames"], 6);
    }

    #[test]
    fn observations_are_bounded_and_failed_start_preserves_old_samples() {
        let mut o = Observer::default();
        o.start(0.0, 2, 1.0).unwrap();
        o.record(sample(0, 0, 0));
        o.record(sample(0, 1, 1));
        o.record(sample(1, 1, 1));
        o.record(sample(2, 2, 2));
        assert!(!o.active());
        assert!(o.start(f64::NAN, 10, 1.0).is_err());
        assert!(o.start(0.0, MAX_SAMPLES + 1, 1.0).is_err());
        assert!(o.read(0, 2049).is_err());
        let s = o.read(usize::MAX, 2).unwrap();
        assert_eq!(s["samples"], 2);
        assert_eq!(s["stopReason"], "sampleLimit");
        assert_eq!(s["page"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn invalid_clock_and_plan_preserve_valid_observations() {
        for invalid in [f64::NAN, f64::INFINITY, -1.0] {
            let mut o = Observer::default();
            o.start(0.0, 10, 1.0).unwrap();
            o.record(sample(1, 1, 1));
            let mut s = sample(2, 2, 2);
            s.now = invalid;
            o.record(s);
            assert_eq!(o.read(0, 0).unwrap()["samples"], 1);
            assert_eq!(o.read(0, 0).unwrap()["stopReason"], "invalidClock");
        }
        let mut o = Observer::default();
        o.start(0.0, 10, 1.0).unwrap();
        let mut s = sample(0, 0, 0);
        if let Some(p) = &mut s.plan {
            p.step = 0;
        }
        o.record(s);
        assert_eq!(o.read(0, 0).unwrap()["stopReason"], "invalidPlan");
    }

    #[test]
    fn control_rejects_wrong_types_and_expiry_without_paints_adds_no_frames() {
        let mut o = Observer::default();
        for invalid in [json!(null), json!([]), json!({"action":"start", "seconds":"10"}), json!({"action":"start", "maxSamples":1.5}), json!({"action":true})]
        {
            assert!(o.control(&invalid, 0.0).is_err());
        }
        o.control(&json!({"action":"start", "seconds":1}), 0.0).unwrap();
        assert!(o.active());
        let result = o.control(&json!({"action":"status"}), 2.0).unwrap();
        assert_eq!(result["samples"], 0);
        assert_eq!(result["stopReason"], "durationLimit");
        assert!(result["firstRequestedDrawMs"].is_null());
    }

    #[test]
    fn discarded_and_hidden_final_passes_do_not_manufacture_presentation() {
        let mut o = Observer::default();
        o.start(0.0, 10, 1.0).unwrap();
        o.begin_pass();
        o.stage(sample(0, 0, 0));
        o.finish_pass(true);
        o.begin_pass();
        o.finish_pass(false);
        assert_eq!(o.read(0, 0).unwrap()["samples"], 0);
        o.begin_pass();
        o.stage(sample(1, 1, 1));
        o.finish_pass(false);
        assert_eq!(o.read(0, 0).unwrap()["samples"], 1);
    }

    #[test]
    fn snapshots_missing_frames_and_cursor_jumps_do_not_count_as_dropped_playback() {
        let mut o = Observer::default();
        o.start(0.0, 10, 1.0).unwrap();
        o.record(sample(0, 0, 0));
        o.record(sample(1, 90, 90));
        for (i, paint) in [Paint::Missing, Paint::Snapshot, Paint::Wireframe].into_iter().enumerate() {
            let mut s = sample(i as u64 + 2, 90, 90);
            s.paint = paint;
            o.record(s);
        }
        let result = o.read(0, 0).unwrap();
        assert_eq!(result["minimumSkippedSelectedFrames"], 0);
        assert_eq!(result["unclassifiedTransitions"], 1);
        assert_eq!(result["missingTextureDraws"], 1);
        assert_eq!(result["snapshotDraws"], 1);
        assert_eq!(result["wireframeDraws"], 1);
    }
}

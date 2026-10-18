//! Interpolate only audio actually submitted for a predicted device playback time.
//! Fixed metadata storage, with no allocation or waiting in the device callback.
use std::sync::{
    Mutex, TryLockError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const SEGMENTS: usize = 64;

#[derive(Clone, Copy)]
struct Segment {
    at: Instant,
    first: u64,
    real: u64,
}

struct History {
    slots: [Option<Segment>; SEGMENTS],
    head: usize,
    len: usize,
    rate: u32,
}

impl History {
    fn at(&self, offset: usize) -> Option<Segment> {
        (offset < self.len).then(|| self.slots.get((self.head + offset) % SEGMENTS).copied().flatten()).flatten()
    }

    fn prune(&mut self, now: Instant) {
        while self.at(1).is_some_and(|next| next.at <= now) {
            if let Some(slot) = self.slots.get_mut(self.head) {
                *slot = None;
            }
            self.head = (self.head + 1) % SEGMENTS;
            self.len = self.len.saturating_sub(1);
        }
    }

    fn record(&mut self, now: Instant, segment: Segment) -> bool {
        if self.rate == 0
            || segment.real > u64::from(self.rate) * 2
            || segment.first.checked_add(segment.real).is_none()
            || segment.at.checked_duration_since(now).is_none_or(|delay| delay > Duration::from_secs(2))
            || self.at(self.len.saturating_sub(1)).is_some_and(|last| segment.at < last.at || segment.first < last.first.saturating_add(last.real))
        {
            return false;
        }
        self.prune(now);
        if self.len == SEGMENTS {
            // Keep the currently audible and future buffers. Retry next callback when room
            // opens; a missing timestamp holds the clock, never invents real samples.
            return false;
        }
        let Some(slot) = self.slots.get_mut((self.head + self.len) % SEGMENTS) else { return false };
        *slot = Some(segment);
        self.len += 1;
        true
    }

    fn position(&mut self, now: Instant) -> Option<u64> {
        self.prune(now);
        let current = self.at(0)?;
        let elapsed = now.checked_duration_since(current.at)?;
        let frames = (elapsed.as_nanos().saturating_mul(u128::from(self.rate)) / 1_000_000_000).min(u128::from(current.real)) as u64;
        Some(current.first.saturating_add(frames))
    }
}

pub(super) struct PlaybackClock {
    history: Mutex<History>,
    audible: AtomicU64,
    valid: AtomicBool,
}

impl PlaybackClock {
    pub(super) fn new(rate: u32) -> Self {
        Self { history: Mutex::new(History { slots: [None; SEGMENTS], head: 0, len: 0, rate }), audible: AtomicU64::new(0), valid: AtomicBool::new(false) }
    }

    pub(super) fn record(&self, now: Instant, at: Instant, first: u64, real: u64) {
        let mut history = match self.history.try_lock() {
            Ok(history) => history,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        if history.record(now, Segment { at, first, real }) {
            self.valid.store(true, Ordering::Release);
        }
    }

    pub(super) fn position(&self, now: Instant) -> Option<u64> {
        if !self.valid.load(Ordering::Acquire) {
            return None;
        }
        let mut history = match self.history.try_lock() {
            Ok(history) => history,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Some(self.audible.load(Ordering::Relaxed)),
        };
        let value = history.position(now).unwrap_or_else(|| self.audible.load(Ordering::Relaxed));
        Some(self.audible.fetch_max(value, Ordering::Relaxed).max(value))
    }

    pub(super) fn clear(&self) {
        let mut history = self.history.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        history.slots.fill(None);
        history.head = 0;
        history.len = 0;
        self.audible.store(0, Ordering::Relaxed);
        self.valid.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(now: Instant, millis: u64) -> Instant {
        now + Duration::from_millis(millis)
    }

    #[test]
    fn device_clock_interpolates_between_callbacks_and_waits_for_output_latency() {
        let now = Instant::now();
        let clock = PlaybackClock::new(48_000);
        assert_eq!(clock.position(now), None);
        clock.record(now, ms(now, 20), 0, 480);
        clock.record(ms(now, 10), ms(now, 30), 480, 480);
        assert_eq!(clock.position(ms(now, 19)), Some(0));
        assert_eq!(clock.position(ms(now, 20)), Some(0));
        assert_eq!(clock.position(ms(now, 25)), Some(240));
        assert_eq!(clock.position(ms(now, 28)), Some(384));
        assert_eq!(clock.position(ms(now, 35)), Some(720));
        assert_eq!(clock.position(ms(now, 90)), Some(960), "never extrapolate beyond submitted real frames");
    }

    #[test]
    fn device_clock_holds_through_partial_and_whole_underruns_then_resumes() {
        let now = Instant::now();
        let clock = PlaybackClock::new(48_000);
        clock.record(now, ms(now, 20), 0, 240);
        clock.record(ms(now, 10), ms(now, 30), 240, 0);
        clock.record(ms(now, 20), ms(now, 40), 240, 480);
        assert_eq!(clock.position(ms(now, 23)), Some(144));
        assert_eq!(clock.position(ms(now, 27)), Some(240));
        assert_eq!(clock.position(ms(now, 35)), Some(240));
        assert_eq!(clock.position(ms(now, 45)), Some(480));
        assert_eq!(clock.position(ms(now, 60)), Some(720));
    }

    #[test]
    fn device_clock_storage_is_bounded_without_discarding_future_audio() {
        let now = Instant::now();
        let mut history = History { slots: [None; SEGMENTS], head: 0, len: 0, rate: 48_000 };
        for i in 0..SEGMENTS {
            assert!(history.record(now, Segment { at: ms(now, 10 + i as u64), first: i as u64 * 48, real: 48 }));
        }
        assert!(!history.record(now, Segment { at: ms(now, 74), first: 3072, real: 48 }));
        assert_eq!(history.len, SEGMENTS);
        assert_eq!(history.position(ms(now, 15)), Some(240));
        assert!(history.record(ms(now, 15), Segment { at: ms(now, 74), first: 3072, real: 48 }));
        assert_eq!(history.position(ms(now, 74)), Some(3072));
    }

    #[test]
    fn device_clock_rejects_invalid_timestamps_and_counts_and_recovers_after_restart() {
        let now = Instant::now();
        let clock = PlaybackClock::new(48_000);
        clock.record(now, ms(now, 3000), 0, 480);
        clock.record(now, now, u64::MAX, 480);
        clock.record(now, now, 0, u64::MAX);
        assert_eq!(clock.position(now), None);
        clock.record(now, ms(now, 20), 0, 480);
        clock.record(now, ms(now, 19), 480, 480);
        clock.record(now, ms(now, 30), 100, 480);
        assert_eq!(clock.position(ms(now, 50)), Some(480));
        assert_eq!(clock.position(ms(now, 25)), Some(480), "timestamp jitter cannot reverse the published clock");
        clock.clear();
        assert_eq!(clock.position(ms(now, 50)), None);
        clock.record(ms(now, 50), ms(now, 60), 0, 480);
        assert_eq!(clock.position(ms(now, 65)), Some(240));
        assert_eq!(PlaybackClock::new(0).position(now), None);
    }

    #[test]
    fn device_clock_contention_never_blocks_callback_or_reader_and_poison_recovers() {
        let now = Instant::now();
        let clock = PlaybackClock::new(48_000);
        clock.record(now, ms(now, 20), 0, 480);
        assert_eq!(clock.position(ms(now, 25)), Some(240));
        let guard = clock.history.lock().unwrap();
        clock.record(ms(now, 10), ms(now, 30), 480, 480);
        assert_eq!(clock.position(ms(now, 26)), Some(240));
        drop(guard);
        assert_eq!(clock.position(ms(now, 26)), Some(288));
        let _ = std::panic::catch_unwind(|| {
            let _guard = clock.history.lock().unwrap();
            panic!("synthetic clock poison");
        });
        clock.record(ms(now, 10), ms(now, 30), 480, 480);
        assert_eq!(clock.position(ms(now, 35)), Some(720));
        clock.clear();
        assert_eq!(clock.position(now), None);
    }
}

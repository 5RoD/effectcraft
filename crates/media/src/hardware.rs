//! Safe host controls and process-wide hardware decoder counters.
//!
//! OS adapter registration belongs to the desktop executable. These wrappers keep FilmCraft
//! types inside the media boundary and do not require a native platform on the web.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct Work {
    calls: AtomicU64,
    nanoseconds: AtomicU64,
}

static DECODE: Work = Work { calls: AtomicU64::new(0), nanoseconds: AtomicU64::new(0) };
static CONVERT: Work = Work { calls: AtomicU64::new(0), nanoseconds: AtomicU64::new(0) };

/// Process-wide source-frame and colour-conversion wall time, including read-ahead.
/// Concurrent operations overlap, so accumulated time is not total playback latency.
#[derive(Clone, Copy, Debug)]
pub struct MediaWorkStats {
    pub decode_calls: u64,
    pub decode_nanoseconds: u64,
    pub conversion_calls: u64,
    pub conversion_nanoseconds: u64,
}

pub fn media_work_stats() -> MediaWorkStats {
    MediaWorkStats {
        decode_calls: DECODE.calls.load(Ordering::Relaxed),
        decode_nanoseconds: DECODE.nanoseconds.load(Ordering::Relaxed),
        conversion_calls: CONVERT.calls.load(Ordering::Relaxed),
        conversion_nanoseconds: CONVERT.nanoseconds.load(Ordering::Relaxed),
    }
}

pub(crate) struct WorkTimer {
    work: &'static Work,
    started: web_time::Instant,
}

impl Drop for WorkTimer {
    fn drop(&mut self) {
        let elapsed = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        // Keep the atomic update API available on the workspace's Rust 1.95 minimum.
        #[allow(deprecated)]
        let _ = self.work.nanoseconds.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_add(elapsed)));
        #[allow(deprecated)]
        let _ = self.work.calls.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_add(1)));
    }
}

pub(crate) fn decode_timer() -> WorkTimer {
    WorkTimer { work: &DECODE, started: web_time::Instant::now() }
}

pub(crate) fn conversion_timer() -> WorkTimer {
    WorkTimer { work: &CONVERT, started: web_time::Instant::now() }
}

/// Snapshot of native decoder activity. A registered backend alone does not prove that a stream
/// uses it: compare `frames` before and after decoding. Counters are process-wide and monotonic.
#[derive(Clone, Copy, Debug)]
pub struct HardwareDecoderStats {
    pub enabled: bool,
    pub backend: Option<&'static str>,
    pub frames: u64,
    pub sessions: u64,
    pub declined: u64,
    pub fallbacks: u64,
}

/// Select hardware decoding for newly opened sources. Existing decoders retain their mode.
pub fn set_hardware_decoding(enabled: bool) {
    filmcraft_codecs::hw::set_hardware_decoding(enabled);
}

/// Read counters without opening a source, clearing a cache or changing playback.
pub fn hardware_decoder_stats() -> HardwareDecoderStats {
    let stats = filmcraft_codecs::hw::hw_stats();
    HardwareDecoderStats {
        enabled: filmcraft_codecs::hw::hardware_decoding(),
        backend: filmcraft_codecs::hw::hw_backend(),
        frames: stats.frames,
        sessions: stats.sessions,
        declined: stats.declined,
        fallbacks: stats.fallbacks,
    }
}

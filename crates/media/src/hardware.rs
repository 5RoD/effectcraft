//! Safe host controls and process-wide hardware decoder counters.
//!
//! OS adapter registration belongs to the desktop executable. These wrappers keep FilmCraft
//! types inside the media boundary and do not require a native platform on the web.

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

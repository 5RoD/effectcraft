//! Audio preview output through cpal (the device and channel mapping chosen in Settings ▸ Audio;
//! the default output device when none is set or it is gone).
//!
//! cpal streams are not `Send` on every platform, so each preview runs its stream on a small
//! dedicated thread that owns it until stopped. The callback pulls interleaved stereo from the
//! frontend's [`AudioFeed`] and maps it onto the device's channels (mono: L+R average; extra
//! channels: silence), converting to the device's sample format.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use effectcraft_ui_egui::audio::workers::AudioWorkers;
use effectcraft_ui_egui::audio::{AudioDevice, AudioFeed, AudioOutput};

mod clock;
mod device_list;
use clock::PlaybackClock;
pub use device_list::service as devices;

static OUTPUTS: AudioWorkers = AudioWorkers::new();

/// Device callback period: about 10 ms, clamped so Pulse/PipeWire/ALSA does not open a
/// multi-hundred-millisecond default buffer that plays as silence at the start of preview (#554).
fn preferred_buffer_frames(rate: u32) -> u32 {
    (rate / 100).clamp(256, 2048)
}

/// Wait until the feeder has mixed a block, or until preview is stopped. Starting the stream
/// before that fills the device with silence, so the first real samples are heard late (#554).
fn wait_to_play(feed: &AudioFeed, stop: &mpsc::Receiver<()>) -> bool {
    loop {
        if feed.queued_frames() > 0 {
            return true;
        }
        match stop.recv_timeout(Duration::from_millis(2)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

pub struct CpalOut {
    out: AudioOutput,
    rate: u32,
    latency: Arc<AtomicU64>,
    clock: Arc<PlaybackClock>,
    stop: Option<mpsc::Sender<()>>,
    feed: Option<Arc<AudioFeed>>,
}

/// The named output device, or the default one.
fn device(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if !name.is_empty() {
        #[allow(deprecated)]
        let found = host.output_devices().ok().and_then(|mut d| d.find(|d| d.name().ok().as_deref() == Some(name)));
        if found.is_some() {
            return found;
        }
    }
    host.default_output_device()
}

/// Open the output device (`None` without one).
pub fn open(out: &AudioOutput) -> Option<Box<dyn AudioDevice>> {
    let dev = device(&out.device)?;
    let cfg = dev.default_output_config().ok()?;
    // Settings ▸ Audio ▸ Preview Sample Rate: open the device at that rate when it supports it
    // (otherwise the preview is mixed at that rate and resampled to the device's).
    let supports = |r: u32| {
        dev.supported_output_configs()
            .map(|mut c| c.any(|c| c.channels() == cfg.channels() && c.min_sample_rate().0 <= r && r <= c.max_sample_rate().0))
            .unwrap_or(false)
    };
    let rate = if out.rate > 0 && supports(out.rate) { out.rate } else { cfg.sample_rate().0 };
    Some(Box::new(CpalOut { out: out.clone(), rate, latency: Arc::new(AtomicU64::new(0)), clock: Arc::new(PlaybackClock::new(rate)), stop: None, feed: None }))
}

fn run<T: SizedSample + FromSample<f32>>(
    dev: &cpal::Device,
    cfg: &cpal::StreamConfig,
    feed: Arc<AudioFeed>,
    latency: Arc<AtomicU64>,
    clock: Arc<PlaybackClock>,
    map: (usize, usize),
) -> Result<cpal::Stream, String> {
    let ch = cfg.channels as usize;
    // Output mapping: device channels (0-based) for left and right; out of range → 1 and 2.
    let (left, right) = if map.0 < ch && map.1 < ch { map } else { (0, 1) };
    let rate = cfg.sample_rate.0 as f64;
    let mut stereo: Vec<f32> = Vec::new();
    dev.build_output_stream(
        cfg,
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            let callback = Instant::now();
            let frames = out.len() / ch.max(1);
            stereo.resize(frames * 2, 0.0);
            let first = feed.consumed();
            feed.pull(&mut stereo);
            let real = feed.consumed().saturating_sub(first);
            for (f, s) in out.chunks_mut(ch.max(1)).zip(stereo.as_chunks::<2>().0.iter()) {
                for (c, o) in f.iter_mut().enumerate() {
                    // Mono devices, or left and right mapped to the same channel, get the mix.
                    let v = if ch == 1 || (c == left && c == right) {
                        (s[0] + s[1]) * 0.5
                    } else if c == left {
                        s[0]
                    } else if c == right {
                        s[1]
                    } else {
                        0.0
                    };
                    *o = T::from_sample(v.clamp(-1.0, 1.0));
                }
            }
            // Frames between "handed over" and "audible": device latency plus this buffer.
            let ts = info.timestamp();
            let delay = ts.playback.duration_since(&ts.callback);
            let dev_lat = delay.map(|d| d.as_secs_f64()).unwrap_or(0.0);
            latency.store(((dev_lat * rate) as u64).saturating_add(frames as u64), Ordering::Relaxed);
            if let Some(at) = delay.and_then(|delay| callback.checked_add(delay)) {
                clock.record(callback, at, first, real);
            }
        },
        |e| log::warn!("audio output: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

impl AudioDevice for CpalOut {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn start(&mut self, feed: Arc<AudioFeed>) -> Result<(), String> {
        self.stop();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let latency = self.latency.clone();
        let clock = self.clock.clone();
        let rate = self.rate;
        let name = self.out.device.clone();
        let map = ((self.out.left.max(1) - 1) as usize, (self.out.right.max(1) - 1) as usize);
        let active_feed = feed.clone();
        OUTPUTS.spawn("ec-audio-out", move || {
            let Some(dev) = device(&name) else {
                let _ = ready_tx.send(Err("no audio output device".into()));
                return;
            };
            let sup = match dev.default_output_config() {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            let mut cfg: cpal::StreamConfig = sup.config();
            cfg.sample_rate = cpal::SampleRate(rate);
            // A short period only where the default one is long (ALSA/PulseAudio on Linux, #554),
            // and only within what the device reports it takes.
            if cfg!(target_os = "linux") {
                let want = preferred_buffer_frames(rate);
                cfg.buffer_size = match sup.buffer_size() {
                    cpal::SupportedBufferSize::Range { min, max } if *min <= *max => cpal::BufferSize::Fixed(want.clamp(*min, *max)),
                    _ => cpal::BufferSize::Default,
                };
            }
            let build = |cfg: &cpal::StreamConfig| match sup.sample_format() {
                cpal::SampleFormat::F32 => run::<f32>(&dev, cfg, feed.clone(), latency.clone(), clock.clone(), map),
                cpal::SampleFormat::I16 => run::<i16>(&dev, cfg, feed.clone(), latency.clone(), clock.clone(), map),
                cpal::SampleFormat::U16 => run::<u16>(&dev, cfg, feed.clone(), latency.clone(), clock.clone(), map),
                cpal::SampleFormat::I32 => run::<i32>(&dev, cfg, feed.clone(), latency.clone(), clock.clone(), map),
                f => Err(format!("unsupported sample format {f:?}")),
            };
            let stream = match build(&cfg) {
                Ok(s) => s,
                Err(_) => {
                    // Some hosts reject a fixed period; a large default still plays, just later.
                    cfg.buffer_size = cpal::BufferSize::Default;
                    match build(&cfg) {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    }
                }
            };
            let _ = ready_tx.send(Ok(()));
            if !wait_to_play(&feed, &stop_rx) {
                return;
            }
            if let Err(e) = stream.play() {
                log::warn!("audio output: {e}");
                return;
            }
            let _ = stop_rx.recv();
            drop(stream);
        })?;
        self.stop = Some(stop_tx);
        self.feed = Some(active_feed);
        ready_rx.recv().map_err(|e| e.to_string())?
    }

    fn stop(&mut self) {
        if let Some(feed) = self.feed.take() {
            feed.close();
        }
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
            // A late callback belongs to the old stream, including after this device restarts.
            self.clock = Arc::new(PlaybackClock::new(self.rate));
            self.latency = Arc::new(AtomicU64::new(0));
        } else {
            self.clock.clear();
            self.latency.store(0, Ordering::Relaxed);
        }
    }

    fn latency_frames(&self) -> u64 {
        self.latency.load(Ordering::Relaxed)
    }

    fn played_frames(&self) -> Option<u64> {
        self.clock.position(Instant::now())
    }
}

impl Drop for CpalOut {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_output_silences_the_feed_and_isolates_late_callbacks() {
        let feed = Arc::new(AudioFeed::default());
        feed.push(&[0.5; 8]);
        let (tx, rx) = mpsc::channel();
        let mut output = CpalOut {
            out: AudioOutput { device: String::new(), left: 1, right: 2, rate: 48_000 },
            rate: 48_000,
            latency: Arc::new(AtomicU64::new(480)),
            clock: Arc::new(PlaybackClock::new(48_000)),
            stop: Some(tx),
            feed: Some(feed.clone()),
        };
        let previous_clock = output.clock.clone();
        let previous_latency = output.latency.clone();
        output.stop();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let now = Instant::now();
        previous_clock.record(now, now, 0, 480);
        previous_latency.store(999, Ordering::Relaxed);
        assert_eq!(output.played_frames(), None, "old callbacks cannot advance a stopped or restarted device");
        assert_eq!(output.latency_frames(), 0);
        feed.push(&[0.9; 8]);
        let mut samples = [1.0; 8];
        feed.pull(&mut samples);
        assert_eq!(samples, [0.0; 8]);
        assert_eq!(feed.queued_frames(), 0);
    }

    #[test]
    fn preferred_buffer_is_about_ten_milliseconds() {
        assert_eq!(preferred_buffer_frames(48_000), 480);
        assert_eq!(preferred_buffer_frames(8_000), 256);
        assert_eq!(preferred_buffer_frames(192_000), 1920);
    }

    #[test]
    fn wait_to_play_returns_once_the_feeder_has_samples() {
        let feed = Arc::new(AudioFeed::default());
        let (_tx, rx) = mpsc::channel();
        let f = feed.clone();
        let h = std::thread::spawn(move || wait_to_play(&f, &rx));
        assert_eq!(feed.queued_frames(), 0);
        std::thread::sleep(Duration::from_millis(20));
        feed.push(&[0.1, 0.2]);
        assert!(h.join().unwrap());
        assert_eq!(feed.queued_frames(), 1);
    }

    #[test]
    fn wait_to_play_stops_without_samples() {
        let feed = AudioFeed::default();
        let (tx, rx) = mpsc::channel();
        let h = std::thread::spawn(move || wait_to_play(&feed, &rx));
        tx.send(()).unwrap();
        assert!(!h.join().unwrap());
    }
}

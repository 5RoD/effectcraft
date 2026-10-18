//! Latest-request native audio scrubbing; no decoding or mixing on the UI thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, TryLockError, Weak};

use effectcraft_engine::project::{ItemId, Project};
use effectcraft_engine::time::Tick;

use super::{AudioDevice, AudioFeed, FEEDERS, device_rate, mix_block};
use crate::frames::RenderSource;

struct Request {
    source: RenderSource,
    comp: ItemId,
    time: Tick,
    frames: usize,
    mix: u32,
    cancel: Arc<AtomicBool>,
}

struct Worker {
    latest: Arc<Mutex<Option<Request>>>,
    wake: SyncSender<()>,
    alive: Arc<AtomicBool>,
}

impl Worker {
    fn new(feed: Arc<AudioFeed>, rate: u32) -> Result<Self, String> {
        let latest: Arc<Mutex<Option<Request>>> = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (wake, rx) = mpsc::sync_channel(1);
        let (work, running) = (latest.clone(), alive.clone());
        FEEDERS.spawn("ec-audio-scrub", move || {
            struct Done(Arc<AtomicBool>);
            impl Drop for Done {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _done = Done(running);
            while rx.recv().is_ok() {
                let request = work.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                let Some(request) = request else { continue };
                if request.cancel.load(Ordering::Acquire) || feed.closed.load(Ordering::Acquire) {
                    continue;
                }
                let samples = snippet(&request.source, request.comp, request.time, request.frames, rate, request.mix);
                // Check cancellation while holding the same queue lock used by UI clear:
                // an obsolete result cannot be published after the next gesture clears sound.
                feed.replace_current(&samples, &request.cancel);
            }
        })?;
        Ok(Self { latest, wake, alive })
    }

    fn submit(&self, request: Request) -> Result<Option<Request>, String> {
        if !self.alive.load(Ordering::Acquire) {
            return Err("audio scrub worker stopped; retry the gesture".into());
        }
        let mut latest = match self.latest.try_lock() {
            Ok(latest) => latest,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Ok(Some(request)),
        };
        let obsolete = latest.replace(request);
        drop(latest);
        drop(obsolete);
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(None),
            Err(TrySendError::Disconnected(())) => Err("audio scrub worker stopped; retry the gesture".into()),
        }
    }
}

/// One open scrub output. Native work has one in-flight read, a replaceable latest request
/// and, only during mailbox contention, one UI retry slot. Superseded requests are cancelled.
pub struct AudioScrub {
    pub feed: Arc<AudioFeed>,
    device: Box<dyn AudioDevice>,
    pub rate: u32,
    last: Option<(ItemId, i64)>,
    project: Option<Weak<Project>>,
    pub last_used: f64,
    worker: Option<Worker>,
    pending: Option<Request>,
    cancel: Option<Arc<AtomicBool>>,
    error: Option<String>,
}

impl AudioScrub {
    pub fn open(mut device: Box<dyn AudioDevice>, now: f64) -> Result<Self, String> {
        let rate = device_rate(device.sample_rate())?;
        let feed = Arc::new(AudioFeed::default());
        if let Err(error) = device.start(feed.clone()) {
            feed.close();
            device.stop();
            return Err(error);
        }
        let worker = if cfg!(target_arch = "wasm32") {
            None
        } else {
            match Worker::new(feed.clone(), rate) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    feed.close();
                    device.stop();
                    return Err(error);
                }
            }
        };
        Ok(Self { feed, device, rate, last: None, project: None, last_used: now, worker, pending: None, cancel: None, error: None })
    }

    /// A changed project snapshot or active comp must retire pending and queued sound.
    pub(crate) fn context_current(&self, project: &Arc<Project>, comp: Option<ItemId>) -> bool {
        self.project.as_ref().is_none_or(|previous| previous.ptr_eq(&Arc::downgrade(project)) && self.last.map(|(id, _)| id) == comp)
    }

    /// Request one faded 30–100 ms snippet unless the project/comp/frame is unchanged. Native
    /// completion is asynchronous; the existing browser path still mixes inline.
    pub fn play(&mut self, source: &RenderSource, comp: ItemId, time: Tick, mix_rate: u32, now: f64) -> bool {
        self.last_used = now;
        if self.feed.closed.load(Ordering::Acquire) {
            return false;
        }
        let Some(composition) = source.project.comp(comp) else { return false };
        let frame = composition.frame_rate.frame_at(time);
        if self.context_current(&source.project, Some(comp)) && self.last == Some((comp, frame)) {
            return false;
        }
        self.last = Some((comp, frame));
        self.project = Some(Arc::downgrade(&source.project));
        let frames = (composition.frame_duration().seconds().clamp(0.03, 0.1) * f64::from(self.rate)) as usize;
        let mix = if mix_rate == 0 { self.rate } else { mix_rate.clamp(8000, 192_000) };
        let time = composition.frame_rate.tick_of(frame);
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.feed.clear();
        if cfg!(target_arch = "wasm32") {
            self.feed.push(&snippet(source, comp, time, frames, self.rate, mix));
            return true;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.pending = Some(Request { source: source.clone(), comp, time, frames, mix, cancel });
        if let Err(error) = self.flush() {
            self.fail(error);
            return false;
        }
        true
    }

    fn flush(&mut self) -> Result<(), String> {
        let Some(worker) = &self.worker else { return Ok(()) };
        if !worker.alive.load(Ordering::Acquire) {
            return Err("audio scrub worker stopped; retry the gesture".into());
        }
        if let Some(request) = self.pending.take() {
            self.pending = worker.submit(request)?;
        }
        Ok(())
    }

    fn fail(&mut self, error: String) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        self.error = Some(error);
        self.feed.close();
        self.device.stop();
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    /// Retry a contended mailbox; the device pump also moves browser samples to Web Audio.
    pub fn pump(&mut self) {
        if self.error.is_none()
            && let Err(error) = self.flush()
        {
            self.fail(error);
        }
        self.device.pump();
    }
}

impl Drop for AudioScrub {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        self.feed.close();
        // Disconnect wakes an idle worker. A busy read retires its snapshots on that worker.
        self.worker = None;
        self.device.stop();
    }
}

fn snippet(source: &RenderSource, comp: ItemId, time: Tick, frames: usize, rate: u32, mix: u32) -> Vec<f32> {
    let mut samples = mix_block(source, comp, time, frames, rate, mix);
    let fade = (rate as usize).saturating_mul(3).saturating_div(1000).clamp(1, frames / 2 + 1);
    let count = samples.len() / 2;
    for (index, channels) in samples.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let gain = (index.min(count.saturating_sub(1).saturating_sub(index)) as f32 / fade as f32).min(1.0);
        for sample in channels {
            *sample *= gain;
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::super::tests::audio_fixture;
    use super::*;
    use effectcraft_engine::project::Footage;
    use effectcraft_engine::render::FootageSource;
    use std::time::{Duration, Instant};

    struct Probe {
        calls: Arc<Mutex<Vec<Tick>>>,
        entered: mpsc::Sender<()>,
        release: Option<Mutex<mpsc::Receiver<()>>>,
        panics: bool,
    }
    impl FootageSource for Probe {
        fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<effectcraft_engine::raster::Image>> {
            None
        }
        fn audio(&self, _: ItemId, _: &Footage, time: Tick, frames: usize, _: u32) -> Option<Vec<f32>> {
            let first = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(time);
                calls.len() == 1
            };
            if first {
                self.entered.send(()).unwrap();
                if let Some(release) = &self.release {
                    release.lock().unwrap().recv_timeout(Duration::from_secs(30)).unwrap();
                }
            }
            assert!(!self.panics, "synthetic scrub decoder panic");
            Some(vec![if time < Tick::from_seconds_f64(0.5) { 0.25 } else { 0.75 }; frames * 2])
        }
    }

    struct Device {
        stopped: Arc<AtomicBool>,
        reject: bool,
    }
    impl AudioDevice for Device {
        fn sample_rate(&self) -> u32 {
            48_000
        }
        fn start(&mut self, _: Arc<AudioFeed>) -> Result<(), String> {
            if self.reject { Err("synthetic output unavailable".into()) } else { Ok(()) }
        }
        fn stop(&mut self) {
            self.stopped.store(true, Ordering::Relaxed);
        }
    }

    fn output() -> (AudioScrub, Arc<AtomicBool>) {
        let stopped = Arc::new(AtomicBool::new(false));
        (AudioScrub::open(Box::new(Device { stopped: stopped.clone(), reject: false }), 0.0).unwrap(), stopped)
    }

    fn wait_samples(scrub: &mut AudioScrub) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while scrub.feed.queued_frames() == 0 {
            scrub.pump();
            assert!(scrub.take_error().is_none());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn scrubbing_never_waits_for_decode_and_only_mixes_the_latest_request() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (source, comp) = audio_fixture(Arc::new(Probe { calls: calls.clone(), entered: entered_tx, release: Some(Mutex::new(release_rx)), panics: false }));
        let (mut scrub, _) = output();
        assert!(scrub.play(&source, comp, Tick::ZERO, 0, 0.0));
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        for frame in 1..=24 {
            let time = source.project.comp(comp).unwrap().frame_rate.tick_of(frame);
            assert!(scrub.play(&source, comp, time, 0, frame as f64 / 30.0));
        }
        assert_eq!(calls.lock().unwrap().len(), 1, "UI requests return while the first read is gated");
        assert_eq!(scrub.feed.queued_frames(), 0);
        release_tx.send(()).unwrap();
        wait_samples(&mut scrub);
        assert_eq!(calls.lock().unwrap().len(), 2, "superseded requests are coalesced before decoding");
        assert_eq!(scrub.feed.queued_frames(), 1600);
        let mut samples = vec![0.0; 3200];
        scrub.feed.pull(&mut samples);
        assert_eq!(samples[0], 0.0);
        assert!((samples[1000] - 0.75).abs() < 1e-6, "only the final request was published");
        assert!(!scrub.play(&source, comp, Tick::from_seconds_f64(0.8), 0, 1.0), "holding the same frame does not replay it");
    }

    #[test]
    fn dropping_a_scrub_does_not_join_a_blocked_read_or_publish_its_result() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let probe = Arc::new(Probe { calls: calls.clone(), entered: entered_tx, release: Some(Mutex::new(release_rx)), panics: false });
        let retained = Arc::downgrade(&probe);
        let (source, comp) = audio_fixture(probe);
        let (mut scrub, stopped) = output();
        let feed = scrub.feed.clone();
        scrub.play(&source, comp, Tick::ZERO, 0, 0.0);
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        scrub.play(&source, comp, Tick::from_seconds_f64(0.8), 0, 0.8);
        drop(source);
        drop(scrub);
        assert!(stopped.load(Ordering::Relaxed));
        assert!(retained.upgrade().is_some(), "the read is still gated after UI shutdown returns");
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while retained.upgrade().is_some() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(feed.queued_frames(), 0);
    }

    #[test]
    fn scrub_identity_includes_project_snapshot_even_when_comp_and_frame_match() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::channel();
        let (source, comp) = audio_fixture(Arc::new(Probe { calls: calls.clone(), entered: tx, release: None, panics: false }));
        let (mut scrub, _) = output();
        assert!(scrub.play(&source, comp, Tick::ZERO, 0, 0.0));
        wait_samples(&mut scrub);
        assert!(scrub.context_current(&source.project, Some(comp)));
        assert!(!scrub.context_current(&source.project, None));
        assert!(!scrub.context_current(&source.project, Some(ItemId(comp.0 + 1))));
        assert!(!scrub.play(&source, comp, Tick::ZERO, 0, 0.1));
        let mut next = source.clone();
        next.project = Arc::new((*source.project).clone());
        assert!(!scrub.context_current(&next.project, Some(comp)));
        assert!(scrub.play(&next, comp, Tick::ZERO, 0, 0.2));
        wait_samples(&mut scrub);
        assert!(scrub.context_current(&next.project, Some(comp)));
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn new_project_cancels_gated_scrub_before_it_can_publish_without_an_active_comp() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (source, comp) = audio_fixture(Arc::new(Probe { calls: calls.clone(), entered: entered_tx, release: Some(Mutex::new(release_rx)), panics: false }));
        let mut session = effectcraft_engine::Session::default();
        session.replace_project((*source.project).clone(), None);
        session.footage = source.footage.clone();
        let mut app = crate::EffectcraftApp::new(session);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        app.hooks.audio_device = Some(Box::new(move |_| Some(Box::new(Device { stopped: stop.clone(), reject: false }))));
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_| app);
        harness.run_steps(2);
        let now = harness.ctx.input(|input| input.time);
        harness.state_mut().scrub_audio(comp, Tick::ZERO, now);
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let scrub = harness.state().scrub.as_ref().unwrap();
        let feed = scrub.feed.clone();
        let alive = scrub.worker.as_ref().unwrap().alive.clone();
        harness.state_mut().session.execute("file.newProject", serde_json::json!({})).unwrap();
        harness.step();
        assert!(harness.state().session.active_comp_id().is_none());
        assert!(harness.state().scrub.is_none(), "even an empty project must retire the old scrub");
        assert!(stopped.load(Ordering::Relaxed), "switching does not wait for the gated decoder");
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(feed.queued_frames(), 0, "a late old-project result must remain silent");
    }

    #[test]
    fn scrub_mailbox_contention_retries_latest_and_poison_recovers() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::channel();
        let (source, comp) = audio_fixture(Arc::new(Probe { calls, entered: tx, release: None, panics: false }));
        let (mut scrub, _) = output();
        let mailbox = scrub.worker.as_ref().unwrap().latest.clone();
        let guard = mailbox.lock().unwrap();
        assert!(scrub.play(&source, comp, Tick::ZERO, 0, 0.0));
        assert!(scrub.pending.is_some(), "contention never blocks the UI");
        drop(guard);
        let poisoned = mailbox.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned.lock().unwrap();
                panic!("synthetic mailbox poison");
            })
            .join()
            .is_err()
        );
        wait_samples(&mut scrub);
        assert!(scrub.pending.is_none());
        assert_eq!(scrub.feed.queued_frames(), 1600);
    }

    #[test]
    fn scrub_decoder_panics_stop_output_and_report_a_retryable_error() {
        let (tx, _rx) = mpsc::channel();
        let (source, comp) = audio_fixture(Arc::new(Probe { calls: Arc::default(), entered: tx, release: None, panics: true }));
        let (mut scrub, stopped) = output();
        scrub.play(&source, comp, Tick::ZERO, 0, 0.0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while scrub.error.is_none() {
            scrub.pump();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(scrub.take_error().unwrap().contains("retry"));
        assert!(stopped.load(Ordering::Relaxed));
        assert_eq!(scrub.feed.queued_frames(), 0);
    }

    #[test]
    fn failed_scrub_output_start_stops_the_partial_device() {
        let stopped = Arc::new(AtomicBool::new(false));
        assert!(AudioScrub::open(Box::new(Device { stopped: stopped.clone(), reject: true }), 0.0).is_err());
        assert!(stopped.load(Ordering::Relaxed));
    }
}

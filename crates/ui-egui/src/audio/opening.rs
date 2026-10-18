//! Native device discovery and startup never wait on the UI thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Weak};

use effectcraft_engine::project::{ItemId, Project};
use effectcraft_engine::time::Tick;

use super::{AudioDevice, AudioDeviceFactory, AudioFeed, AudioOutput, AudioPlayback, AudioScrub, AudioWorkers};

// Includes abandoned openings still inside a slow driver. Repeated gestures cannot spawn
// unlimited workers; audio mixing and device cleanup retain their separate bounded registries.
static OPENERS: AudioWorkers = AudioWorkers::new();

pub(crate) enum Opened {
    Playback(AudioPlayback),
    Scrub(AudioScrub),
}

pub(crate) struct Opening {
    project: Weak<Project>,
    comp: ItemId,
    output: AudioOutput,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<Option<Opened>, String>>,
    /// Only the latest scrub gesture is mixed once its device is ready.
    pub scrub: Option<(Tick, u32, f64)>,
}

impl Opening {
    pub fn new(
        factory: AudioDeviceFactory,
        output: AudioOutput,
        project: &Arc<Project>,
        comp: ItemId,
        scrub: Option<(Tick, u32, f64)>,
        open: impl FnOnce(Box<dyn AudioDevice>) -> Result<Opened, String> + Send + 'static,
    ) -> Result<Self, String> {
        let (tx, result) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (stopped, out) = (cancel.clone(), output.clone());
        OPENERS.spawn("ec-audio-open", move || {
            if stopped.load(Ordering::Acquire) {
                return;
            }
            let value = match factory(&out) {
                Some(mut device) => {
                    if stopped.load(Ordering::Acquire) {
                        device.stop();
                        return;
                    }
                    open(device).map(Some)
                }
                None => Ok(None),
            };
            // Dropping a late result stops its output on this worker. Playback feeds stay
            // suspended until accepted; scrub outputs have no samples before the UI submits.
            if !stopped.load(Ordering::Acquire) {
                let _ = tx.send(value);
            }
        })?;
        Ok(Self { project: Arc::downgrade(project), comp, output, cancel, result, scrub })
    }

    pub fn context_current(&self, project: &Arc<Project>, comp: Option<ItemId>, output: &AudioOutput) -> bool {
        self.project.ptr_eq(&Arc::downgrade(project)) && Some(self.comp) == comp && &self.output == output
    }

    pub fn poll(&self) -> Option<Result<Option<Opened>, String>> {
        match self.result.try_recv() {
            Ok(value) => Some(value),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("audio device opening stopped; retry preview".into())),
        }
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl AudioFeed {
    pub(crate) fn suspended() -> Self {
        Self { suspended: AtomicBool::new(true), ..Self::default() }
    }

    pub(crate) fn resume(&self) {
        self.suspended.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn output() -> AudioOutput {
        AudioOutput { device: String::new(), left: 1, right: 2, rate: 48_000 }
    }

    fn wait(opening: &Opening) -> Result<Option<Opened>, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = opening.poll() {
                return result;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    struct Gated {
        entered: mpsc::Sender<Arc<AudioFeed>>,
        release: Receiver<()>,
        stopped: Arc<AtomicBool>,
        feed: Option<Arc<AudioFeed>>,
    }

    impl AudioDevice for Gated {
        fn sample_rate(&self) -> u32 {
            48_000
        }
        fn start(&mut self, feed: Arc<AudioFeed>) -> Result<(), String> {
            feed.push(&[0.25; 8]);
            self.feed = Some(feed.clone());
            self.entered.send(feed).unwrap();
            self.release.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok(())
        }
        fn stop(&mut self) {
            if let Some(feed) = &self.feed {
                feed.close();
            }
            self.stopped.store(true, Ordering::Release);
        }
    }

    #[test]
    fn slow_device_start_is_silent_until_accepted_and_late_cancelled_outputs_stop() {
        for cancel in [false, true] {
            let mut session = effectcraft_engine::Session::default();
            session.execute("comp.new", serde_json::json!({"width":64,"height":36,"duration":1})).unwrap();
            let comp = session.active_comp_id().unwrap();
            let app = crate::EffectcraftApp::new(session);
            let source = app.render_source();
            let (entered_tx, entered) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let stopped = Arc::new(AtomicBool::new(false));
            let device = Mutex::new(Some(Gated { entered: entered_tx, release: release_rx, stopped: stopped.clone(), feed: None }));
            let factory: AudioDeviceFactory = Arc::new(move |_| device.lock().unwrap().take().map(|d| Box::new(d) as Box<dyn AudioDevice>));
            let opening = Opening::new(factory, output(), &app.session.project, comp, None, move |device| {
                AudioPlayback::start_with_feed(
                    device,
                    source,
                    comp,
                    Tick::ZERO,
                    Tick::ZERO,
                    Tick::from_seconds_f64(1.0),
                    false,
                    48_000,
                    Arc::new(AudioFeed::suspended()),
                )
                .map(Opened::Playback)
            })
            .unwrap();
            let feed = entered.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(opening.poll().is_none(), "driver is still gated");
            let mut samples = [1.0; 8];
            feed.pull(&mut samples);
            assert_eq!(samples, [0.0; 8]);
            assert_eq!((feed.consumed(), feed.queued_frames()), (0, 4));
            if cancel {
                drop(opening);
                release.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while !stopped.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            } else {
                release.send(()).unwrap();
                let Some(Opened::Playback(playback)) = wait(&opening).unwrap() else { panic!("playback result") };
                feed.pull(&mut samples);
                assert_eq!(samples, [0.0; 8], "publication alone cannot play sound");
                playback.feed.resume();
                feed.pull(&mut samples);
                assert_eq!(samples, [0.25; 8]);
                drop(playback);
            }
            feed.pull(&mut samples);
            assert_eq!(samples, [0.0; 8], "stopped output stays silent");
        }
    }

    #[test]
    fn failed_or_panicking_device_discovery_returns_a_retryable_result() {
        let project = Arc::new(Project::default());
        for panic in [false, true] {
            let factory: AudioDeviceFactory = Arc::new(move |_| {
                if panic {
                    panic!("synthetic driver discovery panic");
                }
                None
            });
            let opening = Opening::new(factory, output(), &project, ItemId(1), None, |_| panic!("no device was returned")).unwrap();
            if panic {
                assert!(wait(&opening).err().unwrap().contains("retry preview"));
            } else {
                assert!(matches!(wait(&opening), Ok(None)));
            }
        }
    }
}

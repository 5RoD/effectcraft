//! Cached audio output names; no operating-system discovery during a settings frame.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use effectcraft_ui_egui::audio::workers::AudioWorkers;

static QUERIES: AudioWorkers = AudioWorkers::new();
const REFRESH: Duration = Duration::from_secs(1);
type Query = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;

#[derive(Default)]
struct List {
    names: Vec<String>,
    pending: Option<Receiver<Result<Vec<String>, String>>>,
    last: Option<Instant>,
    error: Option<String>,
}

impl List {
    fn poll(&mut self, now: Instant, query: &Query, ctx: &egui::Context) -> Vec<String> {
        let result = self.pending.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("audio device discovery stopped; retrying".into())),
        });
        if let Some(result) = result {
            self.pending = None;
            self.last = Some(now);
            match result {
                Ok(names) => {
                    self.names = names;
                    self.error = None;
                }
                Err(error) => self.failed(error),
            }
        }
        if self.pending.is_none() && self.last.is_none_or(|last| now.saturating_duration_since(last) >= REFRESH) {
            self.last = Some(now);
            let (tx, rx) = mpsc::sync_channel(1);
            let (query, ctx) = (query.clone(), ctx.clone());
            match QUERIES.spawn("ec-audio-devices", move || {
                let _ = tx.send(query());
                ctx.request_repaint();
            }) {
                Ok(()) => self.pending = Some(rx),
                Err(error) => self.failed(error),
            }
        }
        // Called only while Audio Settings is visible. Hotplug refreshes do not keep a closed
        // settings page active; a stalled driver retains just one bounded in-flight query.
        ctx.request_repaint_after(REFRESH);
        self.names.clone()
    }

    fn failed(&mut self, error: String) {
        if self.error.as_ref() != Some(&error) {
            log::warn!("audio output list: {error}");
        }
        self.error = Some(error);
    }
}

#[allow(deprecated)]
fn query() -> Result<Vec<String>, String> {
    let devices = cpal::default_host().output_devices().map_err(|error| format!("cannot list audio outputs: {error}"))?;
    Ok(devices
        .filter_map(|device| match device.name() {
            Ok(name) => Some(name),
            Err(error) => {
                log::warn!("audio output name unavailable: {error}");
                None
            }
        })
        .collect())
}

/// Read cached names and request background refreshes while the page is visible.
pub fn service(ctx: egui::Context) -> Box<dyn Fn() -> Vec<String>> {
    let state = RefCell::new(List::default());
    let query: Query = Arc::new(query);
    Box::new(move || match state.try_borrow_mut() {
        Ok(mut state) => state.poll(Instant::now(), &query, &ctx),
        Err(_) => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn finish(state: &mut List, now: Instant, query: &Query, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.pending.is_some() {
            state.poll(now, query, ctx);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn slow_audio_device_lists_never_block_settings_or_duplicate_requests() {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let query: Query = Arc::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
            entered_tx.send(std::thread::current().id()).unwrap();
            release_rx.lock().unwrap().recv_timeout(Duration::from_secs(30)).unwrap();
            Ok(vec!["Original output".into()])
        });
        let (mut state, ctx, now) = (List::default(), egui::Context::default(), Instant::now());
        let started = Instant::now();
        assert!(state.poll(now, &query, &ctx).is_empty());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_ne!(entered.recv_timeout(Duration::from_secs(10)).unwrap(), std::thread::current().id());
        for _ in 0..100 {
            assert!(state.poll(now, &query, &ctx).is_empty());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        release.send(()).unwrap();
        finish(&mut state, now, &query, &ctx);
        for _ in 0..100 {
            assert_eq!(state.poll(now, &query, &ctx), ["Original output"]);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1, "cached frames never query the OS");
    }

    #[test]
    fn audio_device_refresh_preserves_cached_names_on_failure_and_recovers_hotplug() {
        let sequence = Arc::new(AtomicUsize::new(0));
        let count = sequence.clone();
        let query: Query = Arc::new(move || match count.fetch_add(1, Ordering::Relaxed) {
            0 => Ok(vec!["Old device".into()]),
            1 => Err("synthetic enumeration failure".into()),
            2 => panic!("synthetic driver enumeration panic"),
            _ => Ok(vec!["New device".into()]),
        });
        let (mut state, ctx, mut now) = (List::default(), egui::Context::default(), Instant::now());
        for round in 0..4 {
            state.poll(now, &query, &ctx);
            finish(&mut state, now, &query, &ctx);
            assert_eq!(state.names, [if round < 3 { "Old device" } else { "New device" }]);
            assert_eq!(state.error.is_some(), round == 1 || round == 2);
            assert_eq!(sequence.load(Ordering::Relaxed), round + 1);
            now += REFRESH;
        }
    }
}

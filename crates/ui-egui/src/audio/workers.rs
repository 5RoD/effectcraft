//! Bounded native audio workers. Stopping signals their work; only completed threads are joined.

use std::sync::{Mutex, TryLockError};
use std::thread::JoinHandle;

/// Includes active and still-closing workers, so a stalled decoder or driver cannot cause
/// unlimited thread creation when playback is toggled repeatedly.
const LIMIT: usize = 8;

pub struct AudioWorkers {
    threads: Mutex<[Option<JoinHandle<()>>; LIMIT]>,
}

impl Default for AudioWorkers {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioWorkers {
    pub const fn new() -> Self {
        Self { threads: Mutex::new([const { None }; LIMIT]) }
    }

    /// Start work without waiting for another worker to stop. Completed workers are reaped
    /// before admission; a full or contended registry returns a recoverable busy error.
    pub fn spawn(&self, name: &'static str, work: impl FnOnce() + Send + 'static) -> Result<(), String> {
        if name.contains('\0') {
            return Err("audio worker name contains a NUL character".into());
        }
        if cfg!(target_arch = "wasm32") {
            return Err("native audio workers are unavailable in this browser".into());
        }
        let mut threads = match self.threads.try_lock() {
            Ok(threads) => threads,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Err("audio workers are busy; retry playback".into()),
        };
        for slot in threads.iter_mut() {
            if slot.as_ref().is_some_and(JoinHandle::is_finished)
                && let Some(thread) = slot.take()
                && thread.join().is_err()
            {
                log::error!("an audio worker stopped unexpectedly");
            }
        }
        let slot = threads.iter_mut().find(|slot| slot.is_none()).ok_or("audio workers are still stopping; retry playback")?;
        *slot = Some(
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
                        log::error!("{name}: audio processing failed; restart preview to retry");
                    }
                })
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn closing_audio_workers_stay_bounded_and_finished_slots_are_reused() {
        let workers = AudioWorkers::new();
        let mut release = Vec::new();
        for _ in 0..LIMIT {
            let (tx, rx) = mpsc::channel::<()>();
            workers
                .spawn("ec-audio-test", move || {
                    let _ = rx.recv_timeout(Duration::from_secs(30));
                })
                .unwrap();
            release.push(tx);
        }
        assert!(workers.spawn("ec-audio-rejected", || {}).is_err());
        drop(release);
        let deadline = Instant::now() + Duration::from_secs(10);
        while workers.threads.lock().unwrap().iter().flatten().any(|thread| !thread.is_finished()) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let (tx, rx) = mpsc::channel();
        workers
            .spawn("ec-audio-reused", move || {
                tx.send(()).unwrap();
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn audio_worker_admission_never_waits_for_registry_contention_and_poison_recovers() {
        let workers = Arc::new(AudioWorkers::new());
        let guard = workers.threads.lock().unwrap();
        assert!(workers.spawn("ec-audio-contended", || {}).is_err());
        drop(guard);
        let poisoned = workers.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned.threads.lock().unwrap();
                panic!("synthetic registry poison");
            })
            .join()
            .is_err()
        );
        let (tx, rx) = mpsc::channel();
        workers
            .spawn("ec-audio-recovered", move || {
                tx.send(()).unwrap();
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn audio_worker_panics_are_contained_and_completed_slots_are_reaped() {
        let workers = AudioWorkers::new();
        workers.spawn("ec-audio-panic", || panic!("synthetic worker panic")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while workers.threads.lock().unwrap().iter().flatten().any(|thread| !thread.is_finished()) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let (tx, rx) = mpsc::channel();
        workers
            .spawn("ec-audio-after-panic", move || {
                tx.send(()).unwrap();
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(workers.threads.lock().unwrap().iter().flatten().count(), 1);
    }

    #[test]
    fn invalid_audio_worker_names_return_an_error_without_spawning() {
        let workers = AudioWorkers::new();
        assert!(workers.spawn("invalid\0audio", || {}).is_err());
        assert!(workers.threads.lock().unwrap().iter().all(Option::is_none));
    }
}

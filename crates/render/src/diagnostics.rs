//! Frame-local terminal errors. Nested renderers share the sink; failed intermediate pixels
//! cannot be published or cached. A first-error slot bounds repeated motion-blur failures.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use effectcraft_project::{ItemId, LayerId};
use effectcraft_time::Tick;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrameFailure {
    pub comp: ItemId,
    pub layer: LayerId,
    pub effect_uid: u64,
    pub effect_id: String,
    pub time: Tick,
    pub message: String,
}

impl fmt::Display for FrameFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.effect_id == "render.raster" {
            write!(f, "Render failed in comp {}", self.comp.0)?;
            if self.layer.0 != 0 {
                write!(f, " / layer {}", self.layer.0)?;
            }
            return write!(f, " / raster geometry at {:.6}s: {}", self.time.seconds(), self.message);
        }
        write!(
            f,
            "Render failed in comp {} / layer {} / effect {} (@{}) at {:.6}s: {}",
            self.comp.0,
            self.layer.0,
            self.effect_id,
            self.effect_uid,
            self.time.seconds(),
            self.message
        )
    }
}
impl std::error::Error for FrameFailure {}

#[derive(Default)]
pub struct FrameDiagnostics {
    first: Mutex<Option<FrameFailure>>,
    failed: AtomicBool,
}

impl FrameDiagnostics {
    pub fn record(&self, mut error: FrameFailure) {
        self.failed.store(true, Ordering::Release);
        error.message = error.message.chars().take(2048).collect();
        error.effect_id = error.effect_id.chars().take(256).collect();
        let mut first = self.first.lock().unwrap_or_else(PoisonError::into_inner);
        if first.is_none() {
            *first = Some(error);
        }
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub fn result<T>(&self, value: T) -> Result<T, FrameFailure> {
        match self.first.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            Some(error) => Err(error),
            None => Ok(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_frame_diagnostics_keep_first_context_and_bound_unicode_messages() {
        let diagnostics = FrameDiagnostics::default();
        assert_eq!(diagnostics.result(12).unwrap(), 12);
        let failure =
            FrameFailure { comp: ItemId(5), layer: LayerId(7), effect_uid: 11, effect_id: "界".repeat(1000), time: Tick(37), message: "界".repeat(5000) };
        diagnostics.record(failure);
        diagnostics.record(FrameFailure {
            comp: ItemId(99),
            layer: LayerId(99),
            effect_uid: 99,
            effect_id: "second".into(),
            time: Tick(99),
            message: "second".into(),
        });
        assert!(diagnostics.failed());
        let first = diagnostics.result(0).unwrap_err();
        assert_eq!((first.comp, first.layer, first.effect_uid, first.time), (ItemId(5), LayerId(7), 11, Tick(37)));
        assert_eq!(first.message.chars().count(), 2048);
        assert_eq!(first.effect_id.chars().count(), 256);
    }
}

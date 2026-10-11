//! Per-run crash recovery ownership. An OS lease skips live sessions without guessing whether
//! a PID was reused. Each native run publishes only its own marker and uses its own autosave
//! slots. Unresolved dead sessions are retained until explicitly recovered or dismissed.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::autosave::{Recovery, Sentinel};
use crate::config::ConfigStore;
use crate::prefs::Prefs;

const DIRECTORY: &str = "RecoverySessions";
const MAX_MARKERS: usize = 1024;
const MAX_SENTINEL_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct Claim {
    name: String,
    _lease: Arc<std::fs::File>,
}

#[derive(Clone, Debug, Default)]
pub struct Run {
    pub name: Option<String>,
    lease: Option<Arc<std::fs::File>>,
    claims: Vec<Claim>,
    pub offered: Option<Recovery>,
    pub pending: VecDeque<Recovery>,
}

fn valid_marker(name: &str) -> bool {
    name.strip_prefix("Session-").and_then(|s| s.strip_suffix(".json")).is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
}

fn lease_name(marker: &str) -> String {
    format!("{marker}.lease")
}

impl Run {
    pub fn begin(store: &dyn ConfigStore, prefs: &Prefs) -> std::io::Result<Self> {
        if store.native_recovery_dir().is_none() || cfg!(target_arch = "wasm32") {
            return Ok(Self { offered: crate::autosave::begin(store, prefs), ..Default::default() });
        }
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other)?.as_nanos();
        let name = format!("{DIRECTORY}/Session-{}-{time:x}-{:x}.json", crate::autosave::pid(), NEXT.fetch_add(1, Ordering::Relaxed));
        let lease = store.try_lease(&lease_name(&name))?.ok_or_else(|| std::io::Error::other("recovery session name is already leased"))?;
        let mut run = Self { name: Some(name.clone()), lease: Some(Arc::new(lease)), ..Default::default() };
        run.update(store, &Sentinel { pid: crate::autosave::pid(), ..Default::default() })?;
        let mut markers = store.list_bounded(DIRECTORY, MAX_MARKERS)?;
        markers.retain(|m| valid_marker(m));
        markers.sort();
        // The old single marker is read once for migration. New native runs never write it.
        let legacy = store.read_required_bounded(crate::autosave::SENTINEL, MAX_SENTINEL_BYTES);
        let mut names: Vec<_> = markers.into_iter().map(|m| format!("{DIRECTORY}/{m}")).collect();
        if legacy.is_ok() {
            names.insert(0, crate::autosave::SENTINEL.into());
        } else if let Err(e) = legacy
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e);
        }
        for marker in names {
            if marker == name {
                continue;
            }
            let Some(lease) = store.try_lease(&lease_name(&marker))? else { continue };
            // Keep the lease while the dialog is pending, so another new instance cannot
            // offer or remove the same dead session. A crash releases it automatically.
            let recovery = match store.read_required_bounded(&marker, MAX_SENTINEL_BYTES) {
                Ok(text) => match serde_json::from_str::<Sentinel>(&text) {
                    Ok(sentinel) => crate::autosave::recover_sentinel(store, prefs, sentinel),
                    Err(e) => Recovery {
                        project: None,
                        autosave: None,
                        dirty: true,
                        completed_edits: false,
                        warning: Some(format!("Cannot read recovery marker {marker}: {e}; the marker was preserved.")),
                    },
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if !recovery.dirty && recovery.project.is_none() && recovery.autosave.is_none() && recovery.warning.is_none() {
                // A run which failed before opening/creating any document has no work to
                // recover. Clearing its empty marker cannot remove document data.
                store.remove(&marker)?;
                drop(lease);
                let _ = store.remove(&lease_name(&marker));
                continue;
            }
            run.claims.push(Claim { name: marker, _lease: Arc::new(lease) });
            run.pending.push_back(recovery);
        }
        run.offered = run.pending.pop_front();
        Ok(run)
    }

    pub fn update(&self, store: &dyn ConfigStore, sentinel: &Sentinel) -> std::io::Result<()> {
        let text = serde_json::to_string(sentinel).map_err(std::io::Error::other)?;
        store.write(self.name.as_deref().unwrap_or(crate::autosave::SENTINEL), &text)
    }

    /// The caller has successfully opened the offered file, or explicitly dismissed it.
    /// Remove only its marker; the recovered file and original journal remain available.
    pub fn resolve(&mut self, store: &dyn ConfigStore) -> std::io::Result<()> {
        if let Some(claim) = self.claims.first() {
            store.remove(&claim.name)?;
            let claim = self.claims.remove(0);
            let lease = lease_name(&claim.name);
            drop(claim);
            let _ = store.remove(&lease);
        }
        self.offered = self.pending.pop_front();
        Ok(())
    }

    pub fn end(&mut self, store: &dyn ConfigStore) -> std::io::Result<()> {
        let marker = self.name.as_deref().unwrap_or(crate::autosave::SENTINEL);
        store.remove(marker)?;
        if let Some(name) = self.name.take() {
            self.lease = None;
            let _ = store.remove(&lease_name(&name));
        }
        Ok(())
    }

    /// A separate rotating checkpoint folder for each run, even for two instances editing
    /// the same project. Its name is internal; the project stem remains human-readable.
    pub fn autosave_folder(&self, base: PathBuf) -> PathBuf {
        match self.name.as_deref().and_then(|name| Path::new(name).file_stem()) {
            Some(stem) => base.join("Sessions").join(stem),
            None => base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DirConfig;

    #[test]
    fn marker_validation_rejects_paths_and_unicode_without_slicing_them() {
        for name in ["../Session-1.json", "Session-../1.json", "Session-💥.json", "Session-.json", "Session-1.json.tmp"] {
            assert!(!valid_marker(name));
        }
        assert!(valid_marker("Session-123-abc-1.json"));
    }

    #[test]
    fn live_runs_have_independent_markers_and_a_released_run_is_claimed_once() {
        let dir = std::env::temp_dir().join(format!("ec-recovery-leases-{}", crate::autosave::pid()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = DirConfig::new(&dir);
        let prefs = Prefs::default();
        let mut first = Run::begin(&store, &prefs).unwrap();
        let name = first.name.clone().unwrap();
        let second = Run::begin(&store, &prefs).unwrap();
        assert!(second.offered.is_none(), "live sessions must not be offered as crashes");
        assert_ne!(first.name, second.name);
        let second_name = second.name.clone().unwrap();
        second.update(&store, &Sentinel { project: Some("Saved.ecproj".into()), dirty: true, ..Default::default() }).unwrap();
        first.end(&store).unwrap();
        assert!(store.read(&second_name).is_some(), "one clean exit must not erase another marker");
        drop(second);
        let mut third = Run::begin(&store, &prefs).unwrap();
        assert!(third.offered.is_some());
        assert_eq!(third.claims[0].name, second_name);
        let mut fourth = Run::begin(&store, &prefs).unwrap();
        assert!(fourth.offered.is_none(), "a pending recovery must not be claimed twice");
        third.resolve(&store).unwrap();
        assert!(store.read(&second_name).is_none());
        assert!(store.read(&name).is_none());
        third.end(&store).unwrap();
        fourth.end(&store).unwrap();
        drop((first, third, fourth));
        let _ = std::fs::remove_dir_all(dir);
    }
}

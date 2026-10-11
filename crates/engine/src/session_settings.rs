//! Session side of Settings, keyboard shortcut presets, auto-save and crash recovery.

use std::path::Path;

use crate::autosave::{self, Sentinel};
use crate::prefs::{PREFS_FILE, Prefs};
use crate::shortcuts::{Keymaps, SHORTCUTS_FILE, ShortcutTable, UiCommand};
use crate::{EngineError, Result, Session};

impl Session {
    /// Load settings, shortcut presets and scripts' settings from the config store and apply
    /// them.
    pub fn load_settings(&mut self) {
        if let Some(c) = &self.config {
            if let Some(t) = c.read(PREFS_FILE) {
                self.prefs = Prefs::from_json(&t);
            }
            if let Some(t) = c.read(SHORTCUTS_FILE) {
                self.keymaps = Keymaps::from_json(&t);
            }
        }
        crate::commands::scripts::load_script_settings(self);
        if let Some(config) = &self.config {
            match config.read_bounded(crate::ease_presets::EASE_PRESETS_FILE, 1 << 20) {
                Ok(Some(text)) => self.ease_presets = crate::ease_presets::parse(&text),
                Ok(None) => {}
                Err(error) => log::warn!("cannot load the native easing library: {error}"),
            }
        }
        self.prefs_changed();
        self.shortcuts_changed();
    }

    /// Settings changed: apply what the engine owns (cache budgets) and tell frontends.
    pub fn prefs_changed(&mut self) {
        self.prefs.normalize();
        self.apply_cache_budgets();
        self.configure_disk_cache();
        self.footage.set_conform_folder(self.conformed_audio_folder());
        // Switches Affect Nested Comps changes what precomp layers render: drop cached pixels.
        let nested = self.prefs.general.switches_affect_nested_comps;
        if self.applied_nested_switches.replace(nested).is_some_and(|was| was != nested) {
            self.layer_cache.clear();
            self.events.push(crate::Event::PurgeCaches);
        }
        // Fewer undo levels apply right away.
        let levels = self.history_limit();
        let current = self.project.clone();
        let retained = (self.history.undo.len(), self.history.redo.len(), self.history.branch_states());
        self.history.trim(levels, &current);
        if retained != (self.history.undo.len(), self.history.redo.len(), self.history.branch_states()) {
            self.bump();
        }
        // Settings ▸ Roto Brush / Face Tracking: load (or drop) the chosen trained models.
        self.apply_models();
        self.prefs_revision += 1;
    }

    /// A Disk settings folder: the setting, or (for frontends with a settings store) `name` next
    /// to the platform disk cache folder. Headless sessions without the setting stay off disk.
    fn cache_folder(&self, setting: &str, name: &str) -> Option<std::path::PathBuf> {
        let s = setting.trim();
        if !s.is_empty() {
            return Some(std::path::PathBuf::from(s));
        }
        // (the browser has no file system: no default folder there)
        if self.config.is_none() || cfg!(target_arch = "wasm32") {
            return None;
        }
        let base = effectcraft_render::disk_cache::default_folder();
        Some(base.parent().map(|p| p.join(name)).unwrap_or_else(|| base.join(name)))
    }

    /// Settings ▸ Disk ▸ Database and Cache Folder (media cache: audio waveform summaries).
    pub fn media_cache_folder(&self) -> Option<std::path::PathBuf> {
        self.cache_folder(&self.prefs.disk.media_cache_folder, "Media Cache")
    }

    /// Settings ▸ Disk ▸ Conformed Audio Folder (decoded footage audio, see
    /// `MediaPool::set_conform_folder`).
    pub fn conformed_audio_folder(&self) -> Option<std::path::PathBuf> {
        self.cache_folder(&self.prefs.disk.conformed_media_folder, "Conformed Audio")
    }

    /// Apply the Memory & CPU cache budgets, limited by RAM Reserved for Other Applications and
    /// reduced while the system is low on memory (see [`crate::prefs::Prefs::cache_budgets`]).
    pub fn apply_cache_budgets(&mut self) {
        let b = self.prefs.cache_budgets(self.sys_memory);
        self.layer_cache.set_budget(b.layer);
        self.footage.set_cache_budget(b.media);
    }

    /// Take the latest reading of the system's memory and start the next one in the background
    /// (desktop frontends call this every few seconds; it never waits for the system), then
    /// re-apply the cache budgets when the reading changes them. Returns the effective budgets.
    pub fn memory_tick(&mut self) -> crate::prefs::CacheBudgets {
        let before = self.prefs.cache_budgets(self.sys_memory);
        self.sys_memory = self.memory_watch.latest().or(self.sys_memory);
        self.memory_watch.poll();
        let after = self.prefs.cache_budgets(self.sys_memory);
        if after != before {
            self.apply_cache_budgets();
            // Frontends re-apply their preview cache budget.
            self.prefs_revision += 1;
            if after.reduced && !before.reduced {
                self.toast("The system is low on memory: cache sizes were reduced");
            }
        }
        after
    }

    /// Apply Settings ▸ Media & Disk Cache ▸ Disk Cache: open (or close) the persistent cache
    /// and attach it to the layer cache. Without a folder setting the platform cache folder is
    /// used, but only by frontends with a settings store (headless sessions stay off disk).
    pub fn configure_disk_cache(&mut self) {
        use effectcraft_render::disk_cache::{DiskCache, default_folder, footage_salt};
        let d = &self.prefs.disk;
        let folder = if d.disk_cache_folder.trim().is_empty() {
            (self.config.is_some() && !cfg!(target_arch = "wasm32")).then(default_folder)
        } else {
            Some(std::path::PathBuf::from(d.disk_cache_folder.trim()))
        };
        let max = d.disk_cache_max_gb.max(1) as u64 * (1 << 30);
        // (no file system in the browser: the disk cache stays off there)
        let Some(folder) = folder.filter(|_| d.disk_cache_enabled && !cfg!(target_arch = "wasm32")) else {
            self.disk_cache = None;
            self.layer_cache.set_disk(None);
            return;
        };
        let dc = match &self.disk_cache {
            Some(c) if c.folder() == folder.as_path() => {
                c.set_max_bytes(max);
                c.clone()
            }
            _ => match DiskCache::open(&folder, max) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("disk cache at {}: {e}", folder.display());
                    self.disk_cache = None;
                    self.layer_cache.set_disk(None);
                    return;
                }
            },
        };
        self.layer_cache.set_disk(Some((dc.clone(), footage_salt(&self.project))));
        self.disk_cache = Some(dc);
    }

    /// Write settings to the config store.
    pub fn save_prefs(&self) {
        if let Some(c) = &self.config
            && let Err(e) = c.write(PREFS_FILE, &self.prefs.to_json())
        {
            log::warn!("saving settings: {e}");
        }
    }

    /// Shortcut presets changed: rebuild the table and save them.
    pub fn shortcuts_changed(&mut self) {
        self.shortcut_table = std::sync::OnceLock::new();
        if let Some(c) = &self.config
            && let Err(e) = c.write(SHORTCUTS_FILE, &self.keymaps.to_json())
        {
            log::warn!("saving keyboard shortcuts: {e}");
        }
    }

    /// The frontend's own bindable commands (tools, timeline navigation…).
    pub fn set_ui_commands(&mut self, cmds: Vec<UiCommand>) {
        self.ui_commands = cmds;
        self.shortcut_table = std::sync::OnceLock::new();
    }

    /// The active shortcut preset resolved against every bindable command.
    pub fn shortcuts(&self) -> &ShortcutTable {
        self.shortcut_table.get_or_init(|| ShortcutTable::build(&self.keymaps, &self.ui_commands))
    }

    /// Remember a project in File ▸ Open Recent and in the crash-recovery sentinel.
    pub(crate) fn note_project_path(&mut self, path: &str) {
        self.prefs.push_recent(path);
        self.save_prefs();
        let _ = self.autosave_wait();
        self.autosave = autosave::AutoSaveState {
            last: self.autosave.last,
            sentinel: self.autosave.sentinel,
            background: self.autosave.background,
            run: self.autosave.run.clone(),
            ..Default::default()
        };
        self.update_sentinel();
    }

    /// Refresh the crash-recovery sentinel (when one is active).
    pub fn update_sentinel(&self) {
        if let Some(c) = &self.config
            && self.autosave.sentinel
        {
            let s = Sentinel {
                pid: autosave::pid(),
                project: self.path.clone(),
                autosave: self.autosave.last_path.clone(),
                autosave_folder: self.autosave_folder().map(|p| p.to_string_lossy().into_owned()),
                dirty: self.needs_save(),
                edit_journal: self.recovery_journal.name.clone(),
                edit_journal_revision: self.recovery_journal.saved_revision,
            };
            if let Err(e) = self.autosave.run.update(c.as_ref(), &s) {
                log::warn!("cannot update the recovery marker: {e}");
            }
        }
    }

    /// Fallback folder for auto-saves of untitled projects (next to the settings).
    pub fn default_autosave_root(&self) -> Option<std::path::PathBuf> {
        self.config.as_ref().and_then(|c| c.dir())
    }

    pub fn autosave_folder(&self) -> Option<std::path::PathBuf> {
        let root = self.default_autosave_root();
        self.autosave_folder_override
            .clone()
            .or_else(|| autosave::folder(&self.prefs, self.path.as_deref(), root.as_deref()))
            .map(|base| self.autosave.run.autosave_folder(base))
    }

    /// Apply host isolation and the native run's separate autosave slot without persisting
    /// either folder in the user's settings.
    pub fn autosave_prefs(&self) -> crate::prefs::Prefs {
        let mut prefs = self.prefs.clone();
        if let Some(folder) = self.autosave_folder() {
            prefs.auto_save.location = "custom".into();
            prefs.auto_save.folder = folder.to_string_lossy().into_owned();
        }
        prefs
    }

    /// Where auto-saves live: the config store's [`crate::config::FileOps`], else the file system.
    pub fn file_ops(&self) -> &dyn crate::config::FileOps {
        match &self.config {
            Some(c) => c.files(),
            None => &crate::config::StdFiles,
        }
    }

    /// Write an auto-save now (whether or not the project is dirty). Returns its path.
    ///
    /// With [`autosave::AutoSaveState::background`] the project is serialised and written on a
    /// background thread (the path is returned at once; [`Session::autosave_wait`] waits).
    /// Auto-saves are compact JSON (opened like any project); File ▸ Save writes pretty JSON.
    pub fn autosave_now(&mut self) -> Result<String> {
        // One write at a time: the previous one must land before its slot rotates.
        self.autosave_wait()?;
        let root = self.default_autosave_root();
        let fail = |e: std::io::Error| EngineError::Other(format!("auto-save failed: {e}"));
        let prefs = self.autosave_prefs();
        let plan = autosave::plan_in(self.file_ops(), &prefs, self.path.as_deref(), root.as_deref(), self.autosave.last_slot).map_err(fail)?;
        if self.autosave.background && !cfg!(target_arch = "wasm32") {
            let project = self.project.clone();
            let history = self.history.clone();
            let config = self.config.clone();
            let job = plan.clone();
            let revision = self.revision;
            let pending = autosave::PendingWrite::spawn(plan.path.clone(), self.revision, plan.slot, move || {
                let json = recovery_text(&project, &history, revision).map_err(std::io::Error::other)?;
                match &config {
                    Some(c) => job.write(c.files(), json.as_bytes()),
                    None => job.write(&crate::config::StdFiles, json.as_bytes()),
                }
            })
            .map_err(fail)?;
            self.autosave.pending = Some(pending);
        } else {
            let json = recovery_text(&self.project, &self.history, self.revision)?;
            plan.write(self.file_ops(), json.as_bytes()).map_err(fail)?;
            self.autosave_completed(&plan.path, plan.slot, self.revision);
        }
        Ok(plan.path.to_string_lossy().to_string())
    }

    fn autosave_completed(&mut self, path: &Path, slot: u32, revision: u64) {
        self.autosave.last_slot = Some(slot);
        self.autosave.saved_revision = Some(revision);
        self.autosave.last_path = Some(path.to_string_lossy().to_string());
        self.update_sentinel();
        self.toast(format!("Auto-saved {}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
    }

    /// Wait for a background auto-save ([`autosave::AutoSaveState::background`]) to land.
    pub fn autosave_wait(&mut self) -> Result<()> {
        match self.autosave.pending.take() {
            Some(w) => {
                w.join().map_err(|e| EngineError::Other(format!("auto-save failed: {e}")))?;
                self.autosave_completed(&w.path, w.slot, w.revision);
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Call periodically with wall-clock seconds: auto-saves a dirty project once the interval
    /// has passed since the last auto-save (or since the first call). Returns the written path.
    pub fn autosave_tick(&mut self, now: f64) -> Option<Result<String>> {
        if self.autosave.pending.as_ref().is_some_and(autosave::PendingWrite::is_finished)
            && let Err(e) = self.autosave_wait()
        {
            return Some(Err(e));
        }
        // A timer tick must never join an unfinished writer on the UI thread.
        if self.autosave.pending.is_some() {
            return None;
        }
        let a = &self.prefs.auto_save;
        if !a.enabled {
            self.autosave.last = Some(now);
            return None;
        }
        let interval = if a.interval_seconds > 0 { a.interval_seconds as f64 } else { a.interval_minutes.max(1) as f64 * 60.0 };
        let last = *self.autosave.last.get_or_insert(now);
        if now - last < interval {
            return None;
        }
        self.autosave.last = Some(now);
        if !self.needs_save() || self.autosave.saved_revision == Some(self.revision) {
            return None;
        }
        if self.path.is_none() && self.prefs.auto_save.location != "custom" && self.default_autosave_root().is_none() && self.autosave_folder_override.is_none()
        {
            return None;
        }
        Some(self.autosave_now())
    }

    /// At launch: start crash recovery. Returns the previous run's state when it didn't exit
    /// cleanly (and the setting asks for it).
    pub fn begin_recovery(&mut self) -> Option<autosave::Recovery> {
        let c = self.config.clone()?;
        if self.autosave.sentinel {
            return self.autosave.run.offered.clone().filter(|_| self.prefs.startup.offer_crash_recovery);
        }
        self.autosave.run = match crate::recovery_sessions::Run::begin(c.as_ref(), &self.prefs) {
            Ok(run) => run,
            Err(e) => {
                self.recovery_journal.error = Some(format!("Edit recovery: cannot start recovery protection: {e}"));
                return None;
            }
        };
        let r = self.autosave.run.offered.clone();
        self.autosave.sentinel = true;
        self.recovery_journal.active = c.native_recovery_dir().is_some() && !cfg!(target_arch = "wasm32");
        self.commit_edit_recovery();
        self.update_sentinel();
        r.filter(|_| self.prefs.startup.offer_crash_recovery)
    }

    /// Clean exit: remove the crash-recovery sentinel.
    pub fn end_recovery(&mut self) {
        self.commit_edit_recovery();
        if self.recovery_journal.error.is_some() {
            return;
        }
        if let Err(e) = self.autosave_wait() {
            self.toast(e.to_string());
            return;
        }
        if let Some(c) = &self.config
            && self.autosave.sentinel
        {
            // A headless/control-channel open also resolves the chosen recovery once its
            // protected replacement has landed. Other pending projects remain recoverable.
            let opened = self.autosave.run.offered.as_ref().is_some_and(|r| self.path.is_some() && (self.path == r.autosave || self.path == r.project));
            if opened && let Err(e) = self.autosave.run.resolve(c.as_ref()) {
                self.toast(format!("Cannot resolve the previous recovery marker: {e}"));
                return;
            }
            if let Err(e) = self.autosave.run.end(c.as_ref()) {
                self.toast(format!("Cannot close the recovery marker: {e}"));
                return;
            }
            if let Some(name) = &self.recovery_journal.name {
                crate::recovery_journal::remove(c.as_ref(), name);
            }
            self.autosave.sentinel = false;
            self.recovery_journal.active = false;
        }
    }

    /// Complete edits are acknowledged only after the journal's atomic write returns. Errors
    /// remain visible independently of transient status/toast messages. Retrying creates a
    /// checked checkpoint containing the whole currently retained branching history.
    pub fn commit_edit_recovery(&mut self) {
        if self.recovery_journal.depth > 0 {
            return;
        }
        self.write_edit_recovery();
    }

    fn write_edit_recovery(&mut self) {
        if !self.recovery_journal.active || !self.prefs.auto_save.protect_every_edit || self.recovery_journal.error.is_some() {
            return;
        }
        if self.recovery_journal.saved_revision == Some(self.revision) && self.recovery_journal.error.is_none() {
            return;
        }
        let Some(config) = self.config.clone() else { return };
        if self.recovery_journal.commit(config.as_ref(), &self.project, &self.history, self.path.clone(), self.revision).is_ok() {
            let sentinel = Sentinel {
                pid: autosave::pid(),
                project: self.path.clone(),
                autosave: self.autosave.last_path.clone(),
                autosave_folder: self.autosave_folder().map(|p| p.to_string_lossy().into_owned()),
                dirty: self.needs_save(),
                edit_journal: self.recovery_journal.name.clone(),
                edit_journal_revision: self.recovery_journal.saved_revision,
            };
            let result = self.autosave.run.update(config.as_ref(), &sentinel);
            if let Err(e) = result {
                self.recovery_journal.error = Some(format!("Edit recovery: cannot publish the recovery journal: {e}"));
                self.recovery_journal.saved_revision = None;
            } else {
                self.recovery_journal.cleanup_obsolete(config.as_ref());
            }
        }
    }

    pub fn retry_edit_recovery(&mut self) -> Result<()> {
        // This read-only command must report its completed write, while a nested script or
        // batch must never publish an intermediate project before its transaction settles.
        if self.recovery_journal.depth > 1 {
            return Err(EngineError::Other("Retry edit recovery after the current script or batch finishes.".into()));
        }
        let previous_revision = self.recovery_journal.saved_revision;
        self.recovery_journal.reset();
        if !self.autosave.sentinel {
            let _ = self.begin_recovery();
        }
        self.write_edit_recovery();
        if let Some(error) = &self.recovery_journal.error {
            self.recovery_journal.saved_revision = previous_revision;
            return Err(EngineError::Other(error.clone()));
        }
        Ok(())
    }

    /// Resolve one offered crashed session after its file has opened successfully, or after
    /// the user explicitly dismisses it. Failed opens/writes leave the old marker untouched.
    pub fn resolve_recovery(&mut self, dismiss: bool) -> Result<Option<autosave::Recovery>> {
        if !dismiss {
            if self.recovery_journal.error.is_some()
                || (self.recovery_journal.active && self.prefs.auto_save.protect_every_edit && self.recovery_journal.saved_revision != Some(self.revision))
            {
                return Err(EngineError::Other(
                    "The recovered project is open, but its new recovery checkpoint could not be stored; the original recovery was preserved.".into(),
                ));
            }
            let opened = self.autosave.run.offered.as_ref().is_some_and(|r| self.path.is_some() && (self.path == r.autosave || self.path == r.project));
            if !opened {
                return Err(EngineError::Other("Open the offered recovery successfully before resolving its marker.".into()));
            }
        }
        let c = self.config.clone().ok_or_else(|| EngineError::Other("Recovery storage is unavailable.".into()))?;
        self.autosave.run.resolve(c.as_ref()).map_err(|e| EngineError::Other(format!("Cannot resolve the recovery marker: {e}")))?;
        Ok(self.autosave.run.offered.clone())
    }
}

fn recovery_text(project: &std::sync::Arc<effectcraft_project::Project>, history: &crate::history::History, revision: u64) -> Result<String> {
    let mut json = crate::history_file::encode(project, history, true)?;
    if json.pop() != Some('}') {
        return Err(EngineError::Other("invalid recovery checkpoint object".into()));
    }
    json.push_str(&format!(",\"recoveryRevision\":{revision}}}"));
    Ok(json)
}

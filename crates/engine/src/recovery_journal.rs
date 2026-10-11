//! Durable edit recovery. A checked project/history checkpoint is followed by small atomic
//! records. Records name snapshots independently of their in-memory addresses. Replay never
//! executes commands, expressions, importers or plugins. A damaged tail stops at the last
//! complete record; the original journal remains available.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};

use effectcraft_project::{Item, ItemId, ItemKind, Layer, LayerId, Project};
use serde::{Deserialize, Serialize};

use crate::config::ConfigStore;
use crate::history::{Branch, History};
use crate::{EngineError, Result, history_file, remote};

const VERSION: u32 = 1;
const PREFIX: &str = "EditRecovery-";
pub(crate) const MAX_CHECKPOINT_BYTES: usize = 512 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_JOURNAL_BYTES: usize = 1024 * 1024 * 1024;
const MAX_RECORDS: u64 = 16_384;

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("Edit recovery: {e}"))
}

fn pointer(p: &Arc<Project>) -> usize {
    Arc::as_ptr(p) as usize
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Topology {
    current: u64,
    undo: Vec<(String, u64)>,
    redo: Vec<(String, u64)>,
    branches: Vec<StoredBranch>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct StoredBranch {
    parent: u64,
    steps: Vec<(String, u64)>,
}

/// The unchanged prefix and replacement suffix, rather than the whole growing undo stack.
#[derive(Serialize, Deserialize)]
struct Suffix<T> {
    keep: usize,
    tail: Vec<T>,
}

impl<T: Clone + PartialEq> Suffix<T> {
    fn between(old: &[T], new: &[T]) -> Self {
        let keep = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        Self { keep, tail: new.iter().skip(keep).cloned().collect() }
    }

    fn apply(self, old: &mut Vec<T>) -> Result<()> {
        if self.keep > old.len() {
            return Err(error("invalid history prefix"));
        }
        old.truncate(self.keep);
        old.extend(self.tail);
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct TopologyChange {
    current: u64,
    undo: Suffix<(String, u64)>,
    redo: Suffix<(String, u64)>,
    branches: Suffix<StoredBranch>,
}

impl TopologyChange {
    fn between(old: &Topology, new: &Topology) -> Self {
        Self {
            current: new.current,
            undo: Suffix::between(&old.undo, &new.undo),
            redo: Suffix::between(&old.redo, &new.redo),
            branches: Suffix::between(&old.branches, &new.branches),
        }
    }

    fn apply(self, old: &mut Topology) -> Result<()> {
        self.undo.apply(&mut old.undo)?;
        self.redo.apply(&mut old.redo)?;
        self.branches.apply(&mut old.branches)?;
        old.current = self.current;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
enum LayerChange {
    Full(Box<Layer>),
    Patch { id: LayerId, ops: Vec<remote::Op> },
}

#[derive(Serialize, Deserialize)]
struct LayersChange {
    order: Vec<LayerId>,
    changed: Vec<LayerChange>,
}

#[derive(Serialize, Deserialize)]
struct ItemChange {
    metadata: Item,
    layers: Option<LayersChange>,
}

#[derive(Serialize, Deserialize)]
struct ProjectChange {
    /// Only the project settings, ID allocator, render queue and templates; never inline items.
    metadata: Project,
    removed: Vec<ItemId>,
    changed: Vec<ItemChange>,
}

impl ProjectChange {
    fn between(old: &Project, new: &Project) -> Result<Self> {
        let mut changed = vec![];
        for (id, item) in &new.items {
            if old.items.get(id).is_some_and(|before| same_item(before, item)) {
                continue;
            }
            let mut metadata = item.clone();
            let layers = if let ItemKind::Comp(comp) = &mut metadata.kind {
                let original = item.as_comp().ok_or_else(|| error("invalid composition"))?;
                let previous: BTreeMap<_, _> = old.comp(*id).into_iter().flat_map(|c| &c.layers).map(|l| (l.id, l)).collect();
                let mut changed = vec![];
                for layer in &original.layers {
                    match previous.get(&layer.id) {
                        Some(before) if **before == *layer => {}
                        Some(before) => {
                            let value = serde_json::to_value(layer).map_err(error)?;
                            // Patch payloads are generic JSON. Validate the changed layer's
                            // typed representation too, before acknowledging the record.
                            let _: Layer = serde_json::from_value(value.clone()).map_err(error)?;
                            changed.push(LayerChange::Patch { id: layer.id, ops: remote::diff(&serde_json::to_value(before).map_err(error)?, &value) });
                        }
                        None => changed.push(LayerChange::Full(Box::new(layer.clone()))),
                    }
                }
                Arc::make_mut(comp).layers.clear();
                Some(LayersChange { order: original.layers.iter().map(|l| l.id).collect(), changed })
            } else {
                None
            };
            changed.push(ItemChange { metadata, layers });
        }
        let mut metadata = new.clone();
        metadata.items.clear();
        Ok(Self { metadata, removed: old.items.keys().filter(|id| !new.items.contains_key(id)).copied().collect(), changed })
    }

    fn apply(self, base: &Project) -> Result<Project> {
        if !self.metadata.items.is_empty() || self.metadata.schema > effectcraft_project::SCHEMA_VERSION {
            return Err(error("invalid project metadata"));
        }
        let mut project = self.metadata;
        project.items = base.items.clone();
        for id in self.removed {
            if project.items.remove(&id).is_none() {
                return Err(error("missing removed item"));
            }
        }
        for mut change in self.changed {
            match (&mut change.metadata.kind, change.layers) {
                (ItemKind::Comp(comp), Some(layers)) if comp.layers.is_empty() => {
                    let mut available: BTreeMap<_, _> = base.comp(change.metadata.id).into_iter().flat_map(|c| &c.layers).map(|l| (l.id, l.clone())).collect();
                    for layer in layers.changed {
                        match layer {
                            LayerChange::Full(layer) => {
                                if available.insert(layer.id, *layer).is_some() {
                                    return Err(error("duplicate new layer"));
                                }
                            }
                            LayerChange::Patch { id, ops } => {
                                let layer = available.get_mut(&id).ok_or_else(|| error("missing changed layer"))?;
                                let mut value = serde_json::to_value(&*layer).map_err(error)?;
                                remote::patch(&mut value, &ops).map_err(error)?;
                                *layer = serde_json::from_value(value).map_err(error)?;
                                if layer.id != id {
                                    return Err(error("changed layer ID"));
                                }
                            }
                        }
                    }
                    let mut ordered = Vec::with_capacity(layers.order.len());
                    for id in layers.order {
                        ordered.push(available.remove(&id).ok_or_else(|| error("missing or repeated layer"))?);
                    }
                    Arc::make_mut(comp).layers = ordered;
                }
                (ItemKind::Comp(_), _) | (_, Some(_)) => return Err(error("invalid composition metadata")),
                _ => {}
            }
            project.items.insert(change.metadata.id, change.metadata);
        }
        project.fix_next_id();
        project.validate_id_space().map_err(error)?;
        Ok(project)
    }
}

fn same_item(a: &Item, b: &Item) -> bool {
    let kind = match (&a.kind, &b.kind) {
        (ItemKind::Comp(x), ItemKind::Comp(y)) => Arc::ptr_eq(x, y) || x == y,
        (x, y) => x == y,
    };
    kind && a.id == b.id && a.name == b.name && a.label == b.label && a.comment == b.comment && a.parent == b.parent && a.proxy == b.proxy
}

#[derive(Serialize, Deserialize)]
struct NewState {
    id: u64,
    base: u64,
    change: ProjectChange,
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    revision: u64,
    path: Option<String>,
    file: String,
    topology: Topology,
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    revision: u64,
    sequence: u64,
    path: Option<String>,
    states: Vec<NewState>,
    history: TopologyChange,
}

/// Writer state. Snapshot addresses are only lookup keys; records use monotonic numeric IDs.
struct Encoder {
    known: HashMap<usize, (Weak<Project>, u64)>,
    next_id: u64,
    last: (u64, Arc<Project>),
    topology: Topology,
}

impl Encoder {
    fn initial(project: &Arc<Project>, history: &History) -> Result<Self> {
        let mut encoder = Self { known: HashMap::new(), next_id: 1, last: (0, project.clone()), topology: Topology::default() };
        let mut exhausted = false;
        let mut add = |p: &Arc<Project>| {
            if let Some((_, id)) = encoder.known.get(&pointer(p)) {
                return *id;
            }
            let id = encoder.next_id;
            let Some(next_id) = id.checked_add(1) else {
                exhausted = true;
                return 0;
            };
            encoder.next_id = next_id;
            encoder.known.insert(pointer(p), (Arc::downgrade(p), id));
            id
        };
        encoder.topology = topology(project, history, &mut add);
        if exhausted {
            return Err(error("snapshot IDs exhausted"));
        }
        encoder.last.0 = encoder.topology.current;
        Ok(encoder)
    }

    fn record(&mut self, project: &Arc<Project>, history: &History, path: Option<String>, sequence: u64, revision: u64) -> Result<Record> {
        let mut states = vec![];
        let mut failure = None;
        let mut add = |p: &Arc<Project>| {
            if let Some((weak, id)) = self.known.get(&pointer(p))
                && weak.ptr_eq(&Arc::downgrade(p))
                && weak.strong_count() > 0
            {
                return *id;
            }
            let id = self.next_id;
            let Some(next_id) = id.checked_add(1) else {
                failure = Some(error("snapshot IDs exhausted"));
                return 0;
            };
            match ProjectChange::between(&self.last.1, p) {
                Ok(change) => states.push(NewState { id, base: self.last.0, change }),
                Err(e) => failure = Some(e),
            }
            self.next_id = next_id;
            self.known.insert(pointer(p), (Arc::downgrade(p), id));
            self.last = (id, p.clone());
            id
        };
        let next = topology(project, history, &mut add);
        if let Some(e) = failure {
            return Err(e);
        }
        let history = TopologyChange::between(&self.topology, &next);
        self.topology = next;
        Ok(Record { version: VERSION, revision, sequence, path, states, history })
    }
}

fn topology(project: &Arc<Project>, history: &History, add: &mut impl FnMut(&Arc<Project>) -> u64) -> Topology {
    let current = add(project);
    let undo = history.undo.iter().map(|(label, p)| (label.clone(), add(p))).collect();
    let redo = history.redo.iter().map(|(label, p)| (label.clone(), add(p))).collect();
    let branches = history
        .branches
        .iter()
        .map(|b| StoredBranch { parent: add(&b.parent), steps: b.steps.iter().map(|(label, p)| (label.clone(), add(p))).collect() })
        .collect();
    Topology { current, undo, redo, branches }
}

fn restore(topology: &Topology, states: &BTreeMap<u64, Arc<Project>>) -> Result<(Arc<Project>, History)> {
    let get = |id: u64| states.get(&id).cloned().ok_or_else(|| error("missing snapshot"));
    let steps = |v: &[(String, u64)]| v.iter().map(|(label, id)| Ok((label.clone(), get(*id)?))).collect::<Result<Vec<_>>>();
    let current = get(topology.current)?;
    let mut history = History { undo: steps(&topology.undo)?, redo: steps(&topology.redo)?, ..Default::default() };
    for b in &topology.branches {
        history.branches.push(Branch { parent: get(b.parent)?, steps: steps(&b.steps)? });
    }
    Ok((current, history))
}

/// One active run. Failed writers are discarded so retry starts with a fresh checked checkpoint
/// containing all currently retained edits, including edits made while storage was unavailable.
#[derive(Default)]
pub struct RecoveryJournal {
    pub active: bool,
    /// Completed outer commands checkpoint once, after nested commands/batches settle.
    pub(crate) depth: usize,
    pub name: Option<String>,
    pub saved_revision: Option<u64>,
    pub error: Option<String>,
    encoder: Option<Encoder>,
    sequence: u64,
    bytes: usize,
    obsolete: Vec<String>,
}

impl RecoveryJournal {
    pub fn reset(&mut self) {
        self.encoder = None;
        self.saved_revision = None;
        self.error = None;
    }

    /// Only after the new journal's sentinel reference is durably published may the previous
    /// epoch be removed. A publication failure must leave both recovery choices intact.
    pub fn cleanup_obsolete(&mut self, store: &dyn ConfigStore) {
        for name in self.obsolete.drain(..) {
            remove(store, &name);
        }
    }

    pub fn commit(&mut self, store: &dyn ConfigStore, project: &Arc<Project>, history: &History, path: Option<String>, revision: u64) -> Result<()> {
        let result = self.write(store, project, history, path, revision);
        if let Err(e) = &result {
            self.encoder = None;
            self.error = Some(e.to_string());
        } else {
            self.error = None;
        }
        result
    }

    fn start_epoch(&mut self, store: &dyn ConfigStore, project: &Arc<Project>, history: &History, path: Option<String>, revision: u64) -> Result<()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(error)?.as_nanos();
        let name = format!("{PREFIX}{}-{time:x}-{:x}", crate::autosave::pid(), NEXT.fetch_add(1, Ordering::Relaxed));
        let encoder = Encoder::initial(project, history)?;
        let checkpoint =
            Checkpoint { version: VERSION, revision, path, file: history_file::encode(project, history, true)?, topology: encoder.topology.clone() };
        // A valid current project alone does not prove that redo/branch snapshots can
        // be restored. Check the whole archive once at the epoch boundary.
        history_file::decode(&checkpoint.file, project)?;
        let text = serde_json::to_string(&checkpoint).map_err(error)?;
        if text.len() > MAX_CHECKPOINT_BYTES {
            return Err(error("checkpoint exceeds the 512 MiB recovery limit; save the project manually"));
        }
        store.write(&format!("{name}/checkpoint.json"), &text).map_err(error)?;
        if let Some(previous) = self.name.replace(name) {
            self.obsolete.push(previous);
        }
        self.encoder = Some(encoder);
        self.sequence = 0;
        self.bytes = text.len();
        Ok(())
    }

    fn write(&mut self, store: &dyn ConfigStore, project: &Arc<Project>, history: &History, path: Option<String>, revision: u64) -> Result<()> {
        if self.encoder.is_none() || self.sequence >= MAX_RECORDS {
            self.start_epoch(store, project, history, path, revision)?;
        } else {
            let next = self.sequence.checked_add(1).ok_or_else(|| error("record sequence exhausted"))?;
            let encoder = self.encoder.as_mut().ok_or_else(|| error("missing writer"))?;
            {
                let record = encoder.record(project, history, path.clone(), next, revision)?;
                let text = serde_json::to_string(&record).map_err(error)?;
                if text.len() > MAX_RECORD_BYTES {
                    return Err(error("edit record exceeds the 64 MiB recovery limit; save the project manually"));
                }
                // Round-trip catches NaN/Infinity and malformed changed values before a record
                // can be acknowledged as stored. The original retained checkpoint survives.
                let _: Record = serde_json::from_str(&text).map_err(error)?;
                let bytes = self.bytes.checked_add(text.len()).ok_or_else(|| error("journal byte count exhausted"))?;
                if bytes > MAX_JOURNAL_BYTES {
                    // The replacement includes the complete retained history. Publication
                    // still precedes old-epoch cleanup, including when this write fails.
                    self.start_epoch(store, project, history, path, revision)?;
                } else {
                    let name = self.name.as_ref().ok_or_else(|| error("missing journal name"))?;
                    store.write(&format!("{name}/{next:020}.json"), &text).map_err(error)?;
                    self.sequence = next;
                    self.bytes = bytes;
                }
            }
        }
        self.saved_revision = Some(revision);
        Ok(())
    }
}

/// A completed recovery project; journal files remain untouched for inspection or later retry.
pub struct Recovered {
    pub path: String,
    pub revision: u64,
    pub warning: Option<String>,
}

fn valid_name(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
}

fn record_name(name: &str) -> bool {
    name.len() == 25 && name.ends_with(".json") && name.as_bytes().get(..20).is_some_and(|s| s.iter().all(u8::is_ascii_digit))
}

pub fn recover(store: &dyn ConfigStore, name: &str) -> Result<Recovered> {
    if !valid_name(name) {
        return Err(error("invalid journal name"));
    }
    let checkpoint_text = store.read_required_bounded(&format!("{name}/checkpoint.json"), MAX_CHECKPOINT_BYTES).map_err(error)?;
    let mut total_bytes = checkpoint_text.len();
    let checkpoint: Checkpoint = serde_json::from_str(&checkpoint_text).map_err(error)?;
    drop(checkpoint_text);
    if checkpoint.version != VERSION {
        return Err(error("unsupported checkpoint version"));
    }
    let project = Arc::new(Project::from_json(&checkpoint.file)?);
    let history = history_file::decode(&checkpoint.file, &project)?.unwrap_or_default();
    let initial = Encoder::initial(&project, &history)?;
    if initial.topology != checkpoint.topology {
        return Err(error("checkpoint history references do not match"));
    }
    let mut states = BTreeMap::new();
    topology(&project, &history, &mut |p| {
        let id = initial.known.get(&pointer(p)).map(|(_, id)| *id).unwrap_or(0);
        states.insert(id, p.clone());
        id
    });
    let mut current = checkpoint.topology;
    let mut revision = checkpoint.revision;
    let mut warning = None;
    let mut sequence = 1_u64;
    let mut names = match store.list_bounded(name, usize::try_from(MAX_RECORDS).map_err(error)?.saturating_add(3)) {
        Ok(names) => names,
        Err(e) => {
            warning = Some(format!("Restored the checked checkpoint only. {e}; the remaining journal was preserved."));
            vec![]
        }
    };
    names.sort();
    for entry in names.into_iter().filter(|n| record_name(n)) {
        let mut apply = || -> Result<(Topology, Vec<(u64, Arc<Project>)>, u64)> {
            let text = store.read_required_bounded(&format!("{name}/{entry}"), MAX_RECORD_BYTES).map_err(error)?;
            total_bytes = total_bytes.checked_add(text.len()).ok_or_else(|| error("replay byte count exhausted"))?;
            if total_bytes > MAX_JOURNAL_BYTES || sequence > MAX_RECORDS {
                return Err(error("journal exceeds the supported replay limit"));
            }
            let record: Record = serde_json::from_str(&text).map_err(error)?;
            if record.version != VERSION || record.sequence != sequence || entry != format!("{sequence:020}.json") || record.revision < revision {
                return Err(error("unsupported, missing or reordered record"));
            }
            let mut added = BTreeMap::new();
            for state in record.states {
                let base = added.get(&state.base).or_else(|| states.get(&state.base)).ok_or_else(|| error("missing base snapshot"))?;
                if states.contains_key(&state.id) || added.contains_key(&state.id) || state.id == 0 {
                    return Err(error("repeated snapshot ID"));
                }
                let project = Arc::new(state.change.apply(base)?);
                added.insert(state.id, project);
            }
            let mut next = current.clone();
            record.history.apply(&mut next)?;
            // Validate references without mutating the last good cursor or shared states.
            let valid = |id| states.contains_key(&id) || added.contains_key(&id);
            if !valid(next.current)
                || next.undo.iter().chain(&next.redo).any(|(_, id)| !valid(*id))
                || next.branches.iter().any(|b| !valid(b.parent) || b.steps.iter().any(|(_, id)| !valid(*id)))
            {
                return Err(error("missing history snapshot"));
            }
            Ok((next, added.into_iter().collect(), record.revision))
        };
        match apply() {
            Ok((next, added, record_revision)) => {
                states.extend(added);
                current = next;
                revision = record_revision;
                sequence = sequence.checked_add(1).ok_or_else(|| error("record sequence exhausted"))?;
            }
            Err(e) => {
                warning = Some(format!("Restored through record {}. {e}; the remaining journal was preserved.", sequence - 1));
                break;
            }
        }
    }
    let (project, history) = restore(&current, &states)?;
    let file = history_file::encode(&project, &history, true)?;
    let path = store.native_recovery_dir().ok_or_else(|| error("no native recovery file directory"))?.join(name).join("Recovered.ecproj");
    store.files().write(&path, file.as_bytes()).map_err(error)?;
    Ok(Recovered { path: path.to_string_lossy().to_string(), revision, warning })
}

/// Remove only files belonging to this validated journal. No recursive filesystem operation.
pub fn remove(store: &dyn ConfigStore, name: &str) {
    if valid_name(name) {
        for entry in store.list(name) {
            if entry == "checkpoint.json" || record_name(&entry) {
                let _ = store.remove(&format!("{name}/{entry}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use serde_json::json;

    use super::*;
    use crate::Session;
    use crate::config::{DirConfig, FileOps};

    struct Store {
        dir: DirConfig,
        fail: AtomicBool,
        writes: AtomicU64,
    }

    impl Store {
        fn new() -> Arc<Self> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!("ec-edit-recovery-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Arc::new(Self { dir: DirConfig::new(dir), fail: AtomicBool::new(false), writes: AtomicU64::new(0) })
        }
    }

    impl ConfigStore for Store {
        fn read(&self, name: &str) -> Option<String> {
            self.dir.read(name)
        }
        fn read_required_bounded(&self, name: &str, limit: usize) -> std::io::Result<String> {
            self.dir.read_required_bounded(name, limit)
        }
        fn write(&self, name: &str, data: &str) -> std::io::Result<()> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            if self.fail.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("disk full (test)"));
            }
            self.dir.write(name, data)
        }
        fn remove(&self, name: &str) -> std::io::Result<()> {
            self.dir.remove(name)
        }
        fn list(&self, dir: &str) -> Vec<String> {
            self.dir.list(dir)
        }
        fn dir(&self) -> Option<PathBuf> {
            self.dir.dir()
        }
        fn native_recovery_dir(&self) -> Option<PathBuf> {
            self.dir.native_recovery_dir()
        }
        fn files(&self) -> &dyn FileOps {
            self.dir.files()
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir.dir);
        }
    }

    fn session(store: Arc<Store>) -> Session {
        let mut s = Session::new();
        s.config = Some(store);
        assert!(s.begin_recovery().is_none());
        s.execute("comp.new", json!({"name":"Main", "width":320, "height":180})).unwrap();
        s
    }

    fn restored(store: &Store, name: &str) -> Session {
        let recovered = recover(store, name).unwrap();
        assert!(recovered.warning.is_none());
        let mut s = Session::new();
        s.execute("file.open", json!({"path": recovered.path})).unwrap();
        s
    }

    fn graph(s: &Session) -> Vec<(String, Option<usize>, bool, bool)> {
        s.history_tree().into_iter().map(|n| (n.label, n.parent, n.current, n.future)).collect()
    }

    #[test]
    fn completed_edits_branches_cursor_and_unmodified_comps_survive_without_a_timer_save() {
        let store = Store::new();
        let mut s = session(store.clone());
        for n in 0..105 {
            s.execute("layer.newNull", json!({"name":format!("Layer {n}")})).unwrap();
        }
        for _ in 0..3 {
            assert!(s.undo());
        }
        s.execute("layer.newNull", json!({"name":"Alternative"})).unwrap();
        let branch = s.history_tree().into_iter().find(|n| n.label == "New Null Object" && n.depth > 0).unwrap();
        assert!(s.goto_history(&branch.id));
        let recovered = restored(&store, s.recovery_journal.name.as_deref().unwrap());
        assert_eq!(*s.project, *recovered.project);
        assert_eq!(graph(&s), graph(&recovered));
        assert!(s.autosave.last_path.is_none(), "no rotating autosave was needed");
        assert_eq!(s.recovery_journal.saved_revision, Some(s.revision));
        assert!(s.recovery_journal.error.is_none());
        drop(s); // Release the crashed run's lease, without a clean exit.
        let mut reopened = Session::new();
        reopened.config = Some(store.clone());
        let offered = reopened.begin_recovery().unwrap();
        assert!(offered.completed_edits && offered.warning.is_none());
        assert!(Path::new(&offered.autosave.unwrap()).is_file());
    }

    #[test]
    fn retry_command_reports_the_completed_write_and_never_acknowledges_a_failed_retry() {
        let store = Store::new();
        let mut s = session(store.clone());
        let good = s.project.clone();
        let name = s.recovery_journal.name.clone().unwrap();
        let revision = s.recovery_journal.saved_revision;
        store.fail.store(true, Ordering::Relaxed);
        s.execute("layer.newNull", json!({"name":"Pending during storage failure"})).unwrap();
        let error = s.execute("file.retryEditRecovery", json!({})).unwrap_err();
        assert!(error.to_string().contains("disk full"));
        assert_eq!(s.recovery_journal.saved_revision, revision);
        assert_eq!(*restored(&store, &name).project, *good);
        store.fail.store(false, Ordering::Relaxed);
        let reply = s.execute("file.retryEditRecovery", json!({})).unwrap();
        let stored = &reply["completedEdits"];
        assert_eq!(stored["savedRevision"], stored["currentRevision"]);
        assert_eq!(stored["savedRevision"], s.revision);
        assert!(stored["error"].is_null());
        let new_name = s.recovery_journal.name.as_deref().unwrap();
        assert_eq!(*restored(&store, new_name).project, *s.project);
        assert_eq!(graph(&restored(&store, new_name)), graph(&s));
    }

    #[test]
    fn retry_inside_a_batch_cannot_publish_an_intermediate_transaction() {
        let store = Store::new();
        let mut s = session(store.clone());
        let good = s.project.clone();
        let good_graph = graph(&s);
        let error = s
            .execute(
                "engine.batch",
                json!({"steps":[
                    {"command":"layer.newNull", "params":{"name":"Must roll back"}},
                    {"command":"file.retryEditRecovery"}
                ]}),
            )
            .unwrap_err();
        assert!(error.to_string().contains("after the current script or batch"));
        assert_eq!(*s.project, *good);
        let recovered = restored(&store, s.recovery_journal.name.as_deref().unwrap());
        assert_eq!(*recovered.project, *good);
        assert_eq!(graph(&recovered), good_graph);
    }

    #[test]
    fn a_failed_write_preserves_old_recovery_and_retry_captures_edits_made_during_failure() {
        let store = Store::new();
        let mut s = session(store.clone());
        s.execute("layer.newNull", json!({"name":"Stored"})).unwrap();
        let good = s.project.clone();
        let name = s.recovery_journal.name.clone().unwrap();
        let revision = s.recovery_journal.saved_revision;
        store.fail.store(true, Ordering::Relaxed);
        s.execute("layer.newNull", json!({"name":"Not yet stored"})).unwrap();
        assert!(s.recovery_journal.error.as_deref().unwrap().contains("disk full"));
        assert_eq!(s.recovery_journal.saved_revision, revision);
        assert_eq!(*restored(&store, &name).project, *good);
        s.execute("layer.newNull", json!({"name":"Also pending"})).unwrap();
        store.fail.store(false, Ordering::Relaxed);
        s.retry_edit_recovery().unwrap();
        let new_name = s.recovery_journal.name.as_deref().unwrap();
        assert_ne!(name, new_name);
        assert_eq!(*restored(&store, new_name).project, *s.project);
        assert_eq!(graph(&restored(&store, new_name)), graph(&s));
        assert!(s.recovery_journal.error.is_none());
        assert!(store.read(&format!("{name}/checkpoint.json")).is_none());
    }

    #[test]
    fn a_damaged_or_missing_tail_restores_the_last_good_edit_without_replacing_journal_files() {
        let store = Store::new();
        let mut s = session(store.clone());
        s.execute("layer.newNull", json!({"name":"Good"})).unwrap();
        let good = s.project.clone();
        let name = s.recovery_journal.name.clone().unwrap();
        let bad_entry = format!("{name}/{:020}.json", s.recovery_journal.sequence + 1);
        store.write(&bad_entry, "{torn").unwrap();
        // A hostile non-ASCII filename must be ignored without slicing at a UTF-8 boundary.
        store.write(&format!("{name}/{}{}.json", "1".repeat(19), "é"), "bad").unwrap();
        let r = recover(&*store, &name).unwrap();
        assert!(r.warning.as_deref().unwrap().contains("remaining journal was preserved"));
        let mut restored = Session::new();
        restored.execute("file.open", json!({"path":r.path})).unwrap();
        assert_eq!(*restored.project, *good);
        assert_eq!(store.read(&bad_entry).as_deref(), Some("{torn"));
        store.remove(&bad_entry).unwrap();
        let gap = format!("{name}/{:020}.json", s.recovery_journal.sequence + 2);
        store.write(&gap, "{}").unwrap();
        assert!(recover(&*store, &name).unwrap().warning.is_some());
    }

    #[test]
    fn atomic_batches_and_continuous_gestures_recover_the_final_history_topology() {
        let store = Store::new();
        let mut s = session(store.clone());
        let before = s.recovery_journal.sequence;
        s.execute(
            "engine.batch",
            json!({"label":"Two layers", "steps":[
                {"command":"layer.newNull", "params":{"name":"One"}},
                {"command":"layer.newNull", "params":{"name":"Two"}}
            ]}),
        )
        .unwrap();
        assert_eq!(s.recovery_journal.sequence, before + 1);
        assert_eq!(s.history.undo.last().unwrap().0, "Two layers");
        for value in 1..=10 {
            s.edit("Comment", Some("comment"), |p, _| {
                p.settings.comment = value.to_string();
                Ok(())
            })
            .unwrap();
        }
        let good = s.project.clone();
        let good_graph = graph(&s);
        assert!(
            s.execute(
                "engine.batch",
                json!({"steps":[
                    {"command":"layer.newNull"}, {"command":"no.such.command"}
                ]})
            )
            .is_err()
        );
        assert_eq!(*s.project, *good);
        let restored = restored(&store, s.recovery_journal.name.as_deref().unwrap());
        assert_eq!(*restored.project, *good);
        assert_eq!(graph(&restored), good_graph);
        drop(s);
        let mut reopened = Session { config: Some(store.clone()), ..Default::default() };
        assert!(reopened.begin_recovery().unwrap().warning.is_none(), "a rolled-back batch's acknowledged revision is stored too");
    }

    #[test]
    fn read_only_commands_do_not_write_or_grow_recovery_and_clean_exit_releases_records() {
        let store = Store::new();
        let mut s = session(store.clone());
        let writes = store.writes.load(Ordering::Relaxed);
        for _ in 0..20 {
            s.execute("project.summary", json!({})).unwrap();
        }
        assert_eq!(writes, store.writes.load(Ordering::Relaxed));
        let name = s.recovery_journal.name.clone().unwrap();
        s.end_recovery();
        assert!(store.read(crate::autosave::SENTINEL).is_none());
        assert!(store.list(&name).is_empty());
    }

    #[test]
    fn one_layer_edit_records_a_small_patch_and_reuses_all_unchanged_layers() {
        let mut s = Session::new();
        s.execute("comp.new", json!({"width":320,"height":180})).unwrap();
        for _ in 0..100 {
            s.execute("layer.newNull", json!({})).unwrap();
        }
        let old = s.project.clone();
        let comp = s.state.active_comp.unwrap();
        s.edit("Rename", None, |p, _| {
            p.comp_mut(comp).unwrap().layers[50].name = "Renamed".into();
            Ok(())
        })
        .unwrap();
        let delta = ProjectChange::between(&old, &s.project).unwrap();
        let text = serde_json::to_string(&delta).unwrap();
        assert!(text.len() < 6000, "a layer rename must not copy the hundred-layer property tree: {} bytes", text.len());
        let delta: ProjectChange = serde_json::from_str(&text).unwrap();
        let restored = delta.apply(&old).unwrap();
        assert_eq!(restored, *s.project);
        let a = old.comp(comp).unwrap();
        let b = restored.comp(comp).unwrap();
        assert!(std::ptr::eq(&*a.layers[0].props, &*b.layers[0].props));
    }

    #[test]
    fn unsupported_journal_names_are_rejected_without_file_access() {
        let store = Store::new();
        for name in ["../outside", "EditRecovery-../../outside", "C:/elsewhere", "EditRecovery-💥"] {
            assert!(recover(&*store, name).is_err());
            remove(&*store, name);
        }
        assert_eq!(store.writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_newer_timer_checkpoint_beats_a_journal_that_stopped_on_storage_failure() {
        let store = Store::new();
        let mut s = session(store.clone());
        s.execute("layer.newNull", json!({"name":"Journal safe"})).unwrap();
        store.fail.store(true, Ordering::Relaxed);
        s.execute("layer.newNull", json!({"name":"Journal write failed"})).unwrap();
        store.fail.store(false, Ordering::Relaxed);
        let newer = s.autosave_now().unwrap();
        let expected = s.project.clone();
        drop(s);
        let mut reopened = Session { config: Some(store.clone()), ..Default::default() };
        let offered = reopened.begin_recovery().unwrap();
        assert!(!offered.completed_edits);
        assert_eq!(offered.autosave.as_deref(), Some(newer.as_str()));
        reopened.execute("file.open", json!({"path": newer})).unwrap();
        assert_eq!(*reopened.project, *expected);
    }

    #[test]
    fn deleting_the_last_acknowledged_record_is_reported_during_recovery() {
        let store = Store::new();
        let mut s = session(store.clone());
        s.execute("layer.newNull", json!({})).unwrap();
        let name = s.recovery_journal.name.as_deref().unwrap();
        store.remove(&format!("{name}/{:020}.json", s.recovery_journal.sequence)).unwrap();
        drop(s);
        let mut reopened = Session { config: Some(store.clone()), ..Default::default() };
        let offered = reopened.begin_recovery().unwrap();
        assert!(offered.warning.as_deref().unwrap().contains("latest acknowledged record"));
    }

    #[test]
    fn oversized_and_non_ascii_record_names_do_not_allocate_unbounded_recovery_data() {
        let store = Store::new();
        let s = session(store.clone());
        let name = s.recovery_journal.name.as_deref().unwrap();
        let entry = format!("{name}/{:020}.json", s.recovery_journal.sequence + 1);
        let file = std::fs::File::create(store.dir.dir.join(&entry)).unwrap();
        file.set_len(u64::try_from(MAX_RECORD_BYTES).unwrap() + 1).unwrap();
        let recovered = recover(&*store, name).unwrap();
        assert!(recovered.warning.as_deref().unwrap().contains("recovery size limit"));
        assert!(!record_name("1111111111111111111é.json"));
        assert!(store.dir.read_required_bounded(&entry, MAX_RECORD_BYTES).is_err());
    }

    #[test]
    fn a_poisoned_configuration_lock_returns_a_write_error_instead_of_false_success() {
        let store = Arc::new(crate::config::MemoryConfig::default());
        let worker_store = store.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = worker_store.files.lock().unwrap();
                panic!("synthetic configuration failure");
            })
            .join()
            .is_err()
        );
        assert!(store.write("checkpoint.json", "{}").unwrap_err().to_string().contains("poisoned"));
        assert!(store.remove("checkpoint.json").is_err());
    }

    #[test]
    fn byte_and_record_limits_rotate_only_after_a_checked_replacement_is_published() {
        for records in [false, true] {
            let store = Store::new();
            let mut s = session(store.clone());
            s.execute("layer.newNull", json!({"name":"Before rotation"})).unwrap();
            let name = s.recovery_journal.name.clone().unwrap();
            let good = s.project.clone();
            let revision = s.recovery_journal.saved_revision;
            if records {
                s.recovery_journal.sequence = MAX_RECORDS;
            } else {
                s.recovery_journal.bytes = MAX_JOURNAL_BYTES;
            }
            store.fail.store(true, Ordering::Relaxed);
            s.execute("layer.newNull", json!({"name":"Pending rotation"})).unwrap();
            assert!(s.recovery_journal.error.is_some());
            assert_eq!(s.recovery_journal.saved_revision, revision);
            assert_eq!(*restored(&store, &name).project, *good);
            store.fail.store(false, Ordering::Relaxed);
            s.retry_edit_recovery().unwrap();
            assert!(s.recovery_journal.error.is_none());
            assert_eq!(s.recovery_journal.sequence, 0);
            assert!(s.recovery_journal.bytes <= MAX_CHECKPOINT_BYTES);
            let new = s.recovery_journal.name.as_deref().unwrap();
            assert_ne!(name, new);
            assert_eq!(*restored(&store, new).project, *s.project);
            assert!(store.read(&format!("{name}/checkpoint.json")).is_none());
        }
    }

    #[test]
    fn simultaneous_native_sessions_isolate_journals_and_rotating_slots() {
        let store = Store::new();
        let mut first = session(store.clone());
        let mut second = session(store.clone());
        let path = store.dir.dir.join("Shared.ecproj");
        first.execute("file.saveAs", json!({"path":path})).unwrap();
        second.execute("file.saveAs", json!({"path":path})).unwrap();
        first.execute("layer.newNull", json!({"name":"First window"})).unwrap();
        second.execute("layer.newNull", json!({"name":"Second window"})).unwrap();
        let first_auto = first.autosave_now().unwrap();
        let second_auto = second.autosave_now().unwrap();
        assert_ne!(first_auto, second_auto, "one run cannot rotate another run's checkpoint slots");
        first.execute("layer.newNull", json!({"name":"First window latest"})).unwrap();
        let expected = first.project.clone();
        let expected_graph = graph(&first);
        let first_marker = first.autosave.run.name.clone().unwrap();
        let first_journal = first.recovery_journal.name.clone().unwrap();
        second.end_recovery();
        assert!(store.read(&first_marker).is_some());
        assert!(store.read(&format!("{first_journal}/checkpoint.json")).is_some());
        drop(first); // Crash releases the OS lease without deleting its marker.
        let mut reopened = Session { config: Some(store.clone()), ..Default::default() };
        let recovery = reopened.begin_recovery().unwrap();
        assert!(recovery.completed_edits && recovery.warning.is_none());
        let recovered_path = recovery.autosave.unwrap();
        reopened.execute("file.open", json!({"path": recovered_path})).unwrap();
        assert_eq!(*reopened.project, *expected);
        assert_eq!(graph(&reopened), expected_graph);
        assert!(reopened.resolve_recovery(false).unwrap().is_none());
        assert!(store.read(&first_marker).is_none());
        assert!(Path::new(&recovered_path).is_file(), "resolving a choice never removes its recovered project");
    }

    #[test]
    fn multiple_crashed_sessions_remain_pending_until_each_is_recovered() {
        let store = Store::new();
        let mut first = session(store.clone());
        let mut second = session(store.clone());
        first.execute("layer.newNull", json!({"name":"Recovered A"})).unwrap();
        second.execute("layer.newNull", json!({"name":"Recovered B"})).unwrap();
        let expected = [first.project.clone(), second.project.clone()];
        let markers = [first.autosave.run.name.clone().unwrap(), second.autosave.run.name.clone().unwrap()];
        drop((first, second));
        let mut reopened = Session { config: Some(store.clone()), ..Default::default() };
        let offered = reopened.begin_recovery().unwrap();
        assert_eq!(reopened.autosave.run.pending.len(), 1);
        assert!(reopened.resolve_recovery(false).is_err(), "an unopened recovery must stay available");
        assert!(markers.iter().all(|m| store.read(m).is_some()));
        let mut restored = vec![];
        reopened.execute("file.open", json!({"path": offered.autosave.unwrap()})).unwrap();
        restored.push(reopened.project.clone());
        let next = reopened.execute("file.resolveRecovery", json!({})).unwrap()["next"].clone();
        reopened.execute("file.open", json!({"path": next["autosave"]})).unwrap();
        restored.push(reopened.project.clone());
        assert!(reopened.resolve_recovery(false).unwrap().is_none());
        assert!(expected.iter().all(|p| restored.iter().any(|r| **p == **r)));
        assert!(markers.iter().all(|m| store.read(m).is_none()));
    }

    #[test]
    fn nonfinite_changed_values_and_invalid_redo_never_acknowledge_an_unreadable_recovery() {
        let store = Store::new();
        let mut s = session(store.clone());
        s.execute("layer.newNull", json!({})).unwrap();
        let comp = s.state.active_comp.unwrap();
        let last_good = s.project.clone();
        let revision = s.recovery_journal.saved_revision;
        let name = s.recovery_journal.name.clone().unwrap();
        s.edit("Invalid internal edit", None, |p, _| {
            p.comp_mut(comp).unwrap().layers[0].props.prop_mut("transform/opacity").unwrap().value = effectcraft_project::Value::Scalar(f64::NAN);
            Ok(())
        })
        .unwrap();
        assert!(s.recovery_journal.error.is_some());
        assert_eq!(s.recovery_journal.saved_revision, revision);
        assert_eq!(*restored(&store, &name).project, *last_good);
        assert!(s.undo());
        let retained_graph = graph(&s);
        assert!(s.retry_edit_recovery().is_err(), "retry must report that the retained redo state is unreadable");
        assert!(s.recovery_journal.error.is_some(), "an invalid redo state cannot become an acknowledged checkpoint");
        assert_eq!(s.recovery_journal.saved_revision, revision);
        assert_eq!(graph(&s), retained_graph, "rejecting the checkpoint must preserve the retained history");
        assert_eq!(*restored(&store, &name).project, *last_good);
    }
}

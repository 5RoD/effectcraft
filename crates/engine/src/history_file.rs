//! Portable branching history in project and recovery files. Unchanged items are stored once;
//! successive versions of an item use the engine's lossless structural JSON patches.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use effectcraft_project::{Item, ItemId, ItemKind, Layer, LayerId, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::history::{Branch, History};
use crate::{EngineError, Result, remote};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
enum Delta<T> {
    Full(T),
    Patch { base: usize, ops: Vec<remote::Op> },
}

#[derive(Serialize, Deserialize)]
struct StoredItem {
    data: Delta<Item>,
    layers: Option<Vec<usize>>,
}

#[derive(Serialize, Deserialize)]
struct StoredState {
    // Metadata only: items refer to the shared item table.
    project: Project,
    items: BTreeMap<ItemId, usize>,
}

#[derive(Serialize, Deserialize)]
struct StoredBranch {
    parent: usize,
    steps: Vec<(String, usize)>,
}

#[derive(Serialize, Deserialize)]
struct Archive {
    version: u32,
    states: Vec<StoredState>,
    items: Vec<StoredItem>,
    layers: Vec<Delta<Layer>>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    branches: Vec<StoredBranch>,
}

#[derive(Deserialize)]
struct FileMetadata {
    #[serde(default, rename = "savedBy")]
    saved_by: Option<Value>,
    #[serde(default, rename = "editHistory", deserialize_with = "present_value")]
    history: Option<Value>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

fn invalid(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("Saved edit history: {e}"))
}

/// Current project plus its branching history. Old EffectCraft builds can read the current
/// project, but ignore (and discard on saving) the optional `editHistory` extension.
pub fn encode(project: &Arc<Project>, history: &History, compact: bool) -> Result<String> {
    let text = project.to_file_json()?;
    if history.undo.is_empty() && history.redo.is_empty() && history.branches.is_empty() {
        return if compact {
            let v: Value = serde_json::from_str(&text).map_err(invalid)?;
            serde_json::to_string(&v).map_err(invalid)
        } else {
            Ok(text)
        };
    }
    let mut archive = Archive { version: VERSION, states: vec![], items: vec![], layers: vec![], undo: vec![], redo: vec![], branches: vec![] };
    let mut states = HashMap::new();
    let mut identities: HashMap<(ItemId, usize), Vec<usize>> = HashMap::new();
    let mut representatives: Vec<Item> = vec![];
    let mut previous: HashMap<ItemId, (usize, Value)> = HashMap::new();
    let mut layer_candidates: HashMap<(LayerId, usize), Vec<usize>> = HashMap::new();
    let mut layer_content: HashMap<(LayerId, u64), Vec<usize>> = HashMap::new();
    let mut layer_representatives: Vec<Layer> = vec![];
    let mut previous_layers: HashMap<LayerId, (usize, Value)> = HashMap::new();
    let mut add = |p: &Arc<Project>| -> Result<usize> {
        let address = Arc::as_ptr(p) as usize;
        if let Some(&i) = states.get(&address) {
            return Ok(i);
        }
        let mut refs = BTreeMap::new();
        for (id, item) in &p.items {
            let pointer = match &item.kind {
                ItemKind::Comp(c) => Arc::as_ptr(c) as usize,
                _ => 0,
            };
            let candidates = identities.entry((*id, pointer)).or_default();
            let found = candidates.iter().copied().find(|&i| representatives.get(i).is_some_and(|old| old == item));
            let i = match found {
                Some(i) => i,
                None => {
                    let mut metadata = item.clone();
                    let layers = if let ItemKind::Comp(c) = &mut metadata.kind {
                        let mut refs = Vec::with_capacity(c.layers.len());
                        // Layer order is separate from layer content. Adding/removing a layer
                        // must not serialize every unchanged layer again at every edit.
                        let ItemKind::Comp(original) = &item.kind else { return Err(invalid("invalid composition")) };
                        for layer in &original.layers {
                            let pointer = std::ptr::from_ref(&*layer.props) as usize;
                            let candidates = layer_candidates.entry((layer.id, pointer)).or_default();
                            let found = candidates.iter().copied().find(|&i| layer_representatives.get(i).is_some_and(|old| *old == *layer));
                            let index = if let Some(index) = found {
                                index
                            } else {
                                let value = serde_json::to_value(layer).map_err(invalid)?;
                                // Hash only a previously unseen property tree. Unchanged layers
                                // take the pointer path above; content buckets avoid scanning all
                                // earlier versions of an edited layer. Equality checks collisions.
                                let mut hash = std::collections::hash_map::DefaultHasher::new();
                                value.hash(&mut hash);
                                let content = layer_content.entry((layer.id, hash.finish())).or_default();
                                if let Some(index) = content.iter().copied().find(|&i| layer_representatives.get(i).is_some_and(|old| *old == *layer)) {
                                    candidates.push(index);
                                    refs.push(index);
                                    continue;
                                }
                                let entry = match previous_layers.get(&layer.id) {
                                    Some((base, old)) => Delta::Patch { base: *base, ops: remote::diff(old, &value) },
                                    None => Delta::Full(layer.clone()),
                                };
                                let index = archive.layers.len();
                                archive.layers.push(entry);
                                previous_layers.insert(layer.id, (index, value));
                                layer_representatives.push(layer.clone());
                                candidates.push(index);
                                content.push(index);
                                index
                            };
                            refs.push(index);
                        }
                        Arc::make_mut(c).layers.clear();
                        Some(refs)
                    } else {
                        None
                    };
                    let value = serde_json::to_value(&metadata).map_err(invalid)?;
                    let entry = match previous.get(id) {
                        Some((base, old)) => Delta::Patch { base: *base, ops: remote::diff(old, &value) },
                        None => Delta::Full(metadata),
                    };
                    let i = archive.items.len();
                    archive.items.push(StoredItem { data: entry, layers });
                    previous.insert(*id, (i, value));
                    representatives.push(item.clone());
                    candidates.push(i);
                    i
                }
            };
            refs.insert(*id, i);
        }
        let mut metadata = (**p).clone();
        metadata.items.clear();
        let i = archive.states.len();
        archive.states.push(StoredState { project: metadata, items: refs });
        states.insert(address, i);
        Ok(i)
    };
    add(project)?; // state zero is the main file's current project
    for (label, p) in &history.undo {
        archive.undo.push((label.clone(), add(p)?));
    }
    for (label, p) in &history.redo {
        archive.redo.push((label.clone(), add(p)?));
    }
    for branch in &history.branches {
        let parent = add(&branch.parent)?;
        let steps = branch.steps.iter().map(|(label, p)| Ok((label.clone(), add(p)?))).collect::<Result<Vec<_>>>()?;
        archive.branches.push(StoredBranch { parent, steps });
    }
    let mut doc: Value = serde_json::from_str(&text).map_err(invalid)?;
    let object = doc.as_object_mut().ok_or_else(|| invalid("project must be an object"))?;
    object.insert("editHistory".into(), serde_json::to_value(archive).map_err(invalid)?);
    if compact { serde_json::to_string(&doc).map_err(invalid) } else { serde_json::to_string_pretty(&doc).map_err(invalid) }
}

/// Restore all shared snapshots before mutating the live session. Damaged history never stops
/// a valid current project from opening; the caller reports the error and keeps that project.
pub fn decode(text: &str, current: &Arc<Project>) -> Result<Option<History>> {
    read_metadata(text, current).1
}

/// Read only the file metadata and optional archive. Serde skips the composition fields
/// without allocating a second whole-project JSON tree. Opening also obtains `savedBy`
/// here, avoiding a separate metadata scan.
pub(crate) fn read_metadata(text: &str, current: &Arc<Project>) -> (Option<String>, Result<Option<History>>) {
    match serde_json::from_str::<FileMetadata>(text) {
        Ok(metadata) => {
            let saved_by = metadata.saved_by.and_then(|v| v.as_str().map(str::to_string));
            (saved_by, decode_archive(text, metadata.history, current))
        }
        Err(e) => (effectcraft_project::saved_by(text), Err(invalid(e))),
    }
}

fn decode_archive(text: &str, history: Option<Value>, current: &Arc<Project>) -> Result<Option<History>> {
    let Some(value) = history else { return Ok(None) };
    let archive: Archive = serde_json::from_value(value).map_err(invalid)?;
    if archive.version != VERSION || archive.states.is_empty() {
        return Err(invalid("unsupported version or state count"));
    }
    let mut items: Vec<Item> = Vec::with_capacity(archive.items.len());
    let mut layers: Vec<Layer> = Vec::with_capacity(archive.layers.len());
    for stored in archive.layers {
        let layer = match stored {
            Delta::Full(layer) => layer,
            Delta::Patch { base, ops } => {
                let parent = layers.get(base).ok_or_else(|| invalid("invalid layer reference"))?;
                let mut value = serde_json::to_value(parent).map_err(invalid)?;
                remote::patch(&mut value, &ops).map_err(invalid)?;
                serde_json::from_value(value).map_err(invalid)?
            }
        };
        layers.push(layer);
    }
    let mut item_values: Vec<Value> = Vec::with_capacity(archive.items.len());
    for stored in archive.items {
        let mut item = match stored.data {
            Delta::Full(item) => item,
            Delta::Patch { base, ops } => {
                let mut value = item_values.get(base).ok_or_else(|| invalid("invalid item reference"))?.clone();
                remote::patch(&mut value, &ops).map_err(invalid)?;
                serde_json::from_value(value).map_err(invalid)?
            }
        };
        item_values.push(serde_json::to_value(&item).map_err(invalid)?);
        match (&mut item.kind, stored.layers) {
            (ItemKind::Comp(comp), Some(refs)) if comp.layers.is_empty() => {
                Arc::make_mut(comp).layers =
                    refs.into_iter().map(|i| layers.get(i).cloned().ok_or_else(|| invalid("invalid composition layer"))).collect::<Result<Vec<_>>>()?;
            }
            (ItemKind::Comp(_), _) | (_, Some(_)) => return Err(invalid("invalid composition metadata")),
            _ => {}
        }
        items.push(item);
    }
    // Reuse the main reader's current items too, including their shared composition Arcs.
    if let Some(first) = archive.states.first() {
        for (id, &index) in &first.items {
            let item = current.items.get(id).ok_or_else(|| invalid("missing current item"))?;
            let stored = items.get_mut(index).ok_or_else(|| invalid("invalid current item reference"))?;
            if *stored == *item {
                *stored = item.clone();
            }
        }
    }
    let mut states = Vec::with_capacity(archive.states.len());
    for mut stored in archive.states {
        if !stored.project.items.is_empty() {
            return Err(invalid("unexpected inline items"));
        }
        for (id, index) in stored.items {
            let item = items.get(index).filter(|i| i.id == id).ok_or_else(|| invalid("invalid state item"))?;
            stored.project.items.insert(id, item.clone());
        }
        if stored.project.schema > effectcraft_project::SCHEMA_VERSION {
            return Err(invalid("unsupported project schema"));
        }
        // Keep the shared composition Arcs: reparsing each snapshot duplicates unchanged comps.
        stored.project.fix_next_id();
        stored.project.validate_id_space().map_err(invalid)?;
        states.push(Arc::new(stored.project));
    }
    if states.first().is_none_or(|p| **p != **current) {
        // The normal engine reader upgrades older effect instances. Validate the archive
        // against the original document before migrating its snapshots the same way, so a
        // newly added effect control doesn't discard a project's entire saved history.
        let original = Project::from_json(text).map_err(invalid)?;
        if states.first().is_none_or(|p| **p != original) {
            return Err(invalid("current project does not match history"));
        }
        let mut upgraded = original;
        crate::upgrade_effects(&mut upgraded);
        if upgraded != **current {
            return Err(invalid("upgraded current project does not match history"));
        }
    }
    let mut cache = HashMap::new();
    if let Some(first) = states.first() {
        for (id, item) in &first.items {
            if let ItemKind::Comp(old) = &item.kind
                && let Some(Item { kind: ItemKind::Comp(now), .. }) = current.items.get(id)
            {
                cache.insert(Arc::as_ptr(old) as usize, (now.clone(), current.next_id));
            }
        }
    }
    // Also upgrade effects that survive only on a redo line or an abandoned branch.
    // Cache by the shared composition Arc so old states keep sharing unchanged comps.
    for state in states.iter_mut().skip(1) {
        crate::upgrade_effects_cached(Arc::make_mut(state), &mut cache);
    }
    if let Some(first) = states.first_mut() {
        *first = current.clone();
    }
    let get = |i| states.get(i).cloned().ok_or_else(|| invalid("invalid state reference"));
    let steps = |v: Vec<(String, usize)>| v.into_iter().map(|(label, i)| Ok((label, get(i)?))).collect::<Result<Vec<_>>>();
    let mut history = History { undo: steps(archive.undo)?, redo: steps(archive.redo)?, ..Default::default() };
    for branch in archive.branches {
        history.branches.push(Branch { parent: get(branch.parent)?, steps: steps(branch.steps)? });
    }
    history.prune(current);
    Ok(Some(history))
}

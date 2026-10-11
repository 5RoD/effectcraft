//! Branching history: undo then a new step keeps the undone line as a branch; the tree lists
//! every state; jumping to any state (on any branch) restores it and keeps everything else.

use std::sync::Arc;

use effectcraft_project::Project;
use serde_json::{Value, json};

use crate::Session;
use crate::history::{Branch, History};

fn names(s: &Session) -> Vec<String> {
    s.project.comps().next().map(|(_, c)| c.layers.iter().map(|l| l.name.clone()).collect()).unwrap_or_default()
}

fn solid(s: &mut Session, name: &str) {
    s.execute("layer.newSolid", json!({"name": name, "color": "#ff0000", "width": 10, "height": 10})).unwrap();
}

fn list(s: &mut Session) -> Vec<Value> {
    s.execute("edit.history.list", json!({})).unwrap()["states"].as_array().cloned().unwrap()
}

fn index_of(states: &[Value], label: &str, nth: usize) -> u64 {
    states.iter().filter(|n| n["label"] == label).nth(nth).unwrap_or_else(|| panic!("no state {label} in {states:?}"))["index"].as_u64().unwrap()
}

#[test]
fn saved_history_restores_branches_redo_and_all_edits() {
    use std::sync::Arc;
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Persistent", "width": 32, "height": 32})).unwrap();
    for i in 0..110 {
        solid(&mut s, &format!("Layer {i}"));
    }
    assert_eq!(s.history.undo.len(), 111, "the old 32/99 step limits must not discard edits");
    s.undo();
    s.undo();
    solid(&mut s, "Alternative");
    solid(&mut s, "Later");
    s.undo();
    let before = s.history_tree();
    let text = crate::history_file::encode(&s.project, &s.history, true).unwrap();
    assert!(text.len() < s.project.to_json_compact().len() * 4, "saving 110 layer additions must not duplicate the full growing composition at every step");
    let current = Arc::new(effectcraft_project::Project::from_json(&text).unwrap());
    let history = crate::history_file::decode(&text, &current).unwrap().unwrap();
    let mut reopened = Session { project: current, history, ..Default::default() };
    let after = reopened.history_tree();
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(&after) {
        assert_eq!((&a.label, a.parent, a.current, a.future, a.depth), (&b.label, b.parent, b.current, b.future, b.depth));
    }
    assert!(reopened.redo());
    assert_eq!(names(&reopened)[0], "Later");
    let old = reopened.history_tree().into_iter().rfind(|n| n.depth > 0).unwrap().id;
    assert!(reopened.goto_history(&old));
    assert_eq!(names(&reopened)[0], "Layer 109");
    let root = reopened.history_tree()[0].id.clone();
    assert!(reopened.goto_history(&root));
    assert!(reopened.project.items.is_empty());
}

#[test]
fn saved_history_shares_unchanged_compositions() {
    use effectcraft_project::ItemKind;
    use std::sync::Arc;
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Unchanged", "width": 32, "height": 32})).unwrap();
    let id = s.state.active_comp.unwrap();
    s.execute("comp.new", json!({"name": "Working", "width": 32, "height": 32})).unwrap();
    solid(&mut s, "One");
    solid(&mut s, "Two");
    let text = crate::history_file::encode(&s.project, &s.history, true).unwrap();
    let current = Arc::new(effectcraft_project::Project::from_json(&text).unwrap());
    let h = crate::history_file::decode(&text, &current).unwrap().unwrap();
    let ItemKind::Comp(now) = &current.items[&id].kind else { panic!() };
    for (_, p) in h.undo.iter().skip(1) {
        let ItemKind::Comp(old) = &p.items[&id].kind else { panic!() };
        assert!(Arc::ptr_eq(now, old), "reopening must not duplicate unchanged comps for each edit");
    }
}

#[test]
fn damaged_history_does_not_damage_the_current_project() {
    use std::sync::Arc;
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Safe", "width": 32, "height": 32})).unwrap();
    solid(&mut s, "Keep me");
    let text = crate::history_file::encode(&s.project, &s.history, false).unwrap();
    let mut doc: Value = serde_json::from_str(&text).unwrap();
    doc["editHistory"]["undo"][0][1] = json!(999999);
    let damaged = serde_json::to_string(&doc).unwrap();
    let current = Arc::new(effectcraft_project::Project::from_json(&damaged).unwrap());
    assert_eq!(*current, *s.project);
    assert!(crate::history_file::decode(&damaged, &current).is_err());
    doc.as_object_mut().unwrap().remove("editHistory");
    assert!(crate::history_file::decode(&serde_json::to_string(&doc).unwrap(), &current).unwrap().is_none());
}

#[test]
fn history_metadata_skips_project_fields_and_reads_escaped_keys() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "editHistory is ordinary text", "width": 32, "height": 32})).unwrap();
    let plain = s.project.to_file_json().unwrap();
    let (version, history) = crate::history_file::read_metadata(&plain, &s.project);
    assert_eq!(version.as_deref(), Some(effectcraft_project::APP_VERSION));
    assert!(history.unwrap().is_none());

    let with_history = crate::history_file::encode(&s.project, &s.history, true).unwrap();
    let escaped = with_history.replace("\"editHistory\":", "\"edit\\u0048istory\":").replace("\"savedBy\":", "\"saved\\u0042y\":");
    let (version, history) = crate::history_file::read_metadata(&escaped, &s.project);
    assert_eq!(version.as_deref(), Some(effectcraft_project::APP_VERSION));
    assert_eq!(history.unwrap().unwrap().undo.len(), s.history.undo.len());
}

#[test]
fn null_history_reports_damage_but_bad_version_metadata_does_not_discard_history() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"width": 32, "height": 32})).unwrap();
    let mut doc: Value = serde_json::from_str(&crate::history_file::encode(&s.project, &s.history, true).unwrap()).unwrap();
    doc["savedBy"] = json!({"invalid": true});
    let text = serde_json::to_string(&doc).unwrap();
    let (version, history) = crate::history_file::read_metadata(&text, &s.project);
    assert!(version.is_none());
    assert!(history.unwrap().is_some());
    doc["editHistory"] = Value::Null;
    let (version, history) = crate::history_file::read_metadata(&serde_json::to_string(&doc).unwrap(), &s.project);
    assert!(version.is_none());
    assert!(history.is_err(), "a present but malformed archive must produce a recovery warning");
}

#[test]
fn saved_history_deduplicates_equal_layers_with_distinct_property_storage() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"width":32,"height":32})).unwrap();
    s.execute("layer.newNull", json!({})).unwrap();
    for value in [10, 20, 10, 20, 10] {
        s.execute("prop.set", json!({"path":"transform/opacity","value":value})).unwrap();
    }
    let text = crate::history_file::encode(&s.project, &s.history, true).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["editHistory"]["layers"].as_array().unwrap().len(), 3, "only the original, 10% and 20% variants belong in the layer table");
    let current = std::sync::Arc::new(effectcraft_project::Project::from_json(&text).unwrap());
    let restored = crate::history_file::decode(&text, &current).unwrap().unwrap();
    assert_eq!(restored.undo.len(), s.history.undo.len());
    for ((_, old), (_, new)) in s.history.undo.iter().zip(restored.undo) {
        assert_eq!(**old, *new);
    }
}

#[test]
fn effect_control_upgrades_preserve_saved_history() {
    use effectcraft_project::Node;
    use std::sync::Arc;
    let mut s = Session::default();
    s.execute("comp.new", json!({"width": 32, "height": 32})).unwrap();
    solid(&mut s, "Old effect");
    let layer = s.state.selected_layers[0];
    s.execute("effect.apply", json!({"layer": layer.0, "effect": "ec.blur.gaussian"})).unwrap();
    let cid = s.active_comp_id().unwrap();
    s.edit("Older controls", None, |project, _| {
        let effects = project.comp_mut(cid).unwrap().layer_mut(layer).unwrap().props.sub_mut("effects").unwrap();
        let blur = effects
            .children
            .iter_mut()
            .find_map(|n| match n {
                Node::Group(g) if g.match_id == "ec.blur.gaussian" => Some(g),
                _ => None,
            })
            .unwrap();
        blur.children.retain(|n| !matches!(n, Node::Prop(p) if p.match_id == "repeatEdge"));
        Ok(())
    })
    .unwrap();
    s.execute("prop.set", json!({"layer": layer.0, "path": "transform/opacity", "value": 60})).unwrap();
    s.undo();
    let expected = s.history_tree().len();
    let text = crate::history_file::encode(&s.project, &s.history, true).unwrap();
    let mut reopened = Session::default();
    reopened.replace_project(effectcraft_project::Project::from_json(&text).unwrap(), None);
    assert_ne!(*reopened.project, *s.project, "the main reader added the newer control");
    reopened.history = crate::history_file::decode(&text, &reopened.project).unwrap().unwrap();
    assert_eq!(reopened.history_tree().len(), expected);
    assert!(reopened.redo());
    let restored = reopened.project.comp(cid).unwrap().layer(layer).unwrap();
    let blur = restored.effects().unwrap().groups().find(|g| g.match_id == "ec.blur.gaussian").unwrap();
    assert!(blur.get("repeatEdge").is_some());
    assert!(reopened.undo());
    assert!(Arc::ptr_eq(&reopened.project, &reopened.saved_project));

    // The old instance can live only in a future; the current file then needs no upgrade.
    assert!(s.undo());
    assert!(s.undo());
    assert!(s.project.comp(cid).unwrap().layer(layer).unwrap().effects().unwrap().children.is_empty());
    let text = crate::history_file::encode(&s.project, &s.history, true).unwrap();
    let current = Arc::new(effectcraft_project::Project::from_json(&text).unwrap());
    let history = crate::history_file::decode(&text, &current).unwrap().unwrap();
    let mut reopened = Session { project: current, history, ..Default::default() };
    assert!(reopened.redo());
    assert!(reopened.redo());
    let blur = reopened.project.comp(cid).unwrap().layer(layer).unwrap().effects().unwrap().groups().find(|g| g.match_id == "ec.blur.gaussian").unwrap();
    assert!(blur.get("repeatEdge").is_some(), "a legacy instance stored only in redo is upgraded too");
}

#[test]
fn saved_pixels_with_a_new_future_still_need_a_history_save() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"width": 32, "height": 32})).unwrap();
    s.mark_saved();
    solid(&mut s, "Keep this future");
    assert!(s.undo());
    assert!(!s.is_dirty(), "document is at the saved snapshot");
    assert!(s.needs_save(), "the new redo state still needs persisting");
    s.mark_saved();
    assert!(!s.needs_save());
    assert!(s.redo());
    assert!(s.needs_save());
    assert!(s.undo());
    assert!(!s.needs_save(), "the saved cursor and complete history are restored");
    let failed: crate::Result<()> = crate::commands::app_more::grouped(&mut s, "Failed", |s| {
        solid(s, "Temporary");
        Err(crate::EngineError::Other("cancelled".into()))
    });
    assert!(failed.is_err());
    assert!(!s.needs_save(), "failed grouped edits leave the saved history intact");
    s.execute("edit.purgeUndo", json!({})).unwrap();
    assert!(s.needs_save(), "explicitly purged history must also be saved");
}

#[test]
fn grouped_commands_preserve_branches_and_restore_history_on_failure() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Groups", "width": 32, "height": 32})).unwrap();
    solid(&mut s, "A");
    solid(&mut s, "B");
    s.undo();
    crate::commands::app_more::grouped(&mut s, "Two layers", |s| {
        solid(s, "C");
        solid(s, "D");
        Ok(())
    })
    .unwrap();
    assert_eq!(s.history.branches.len(), 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Two layers");
    assert!(s.undo());
    assert_eq!(names(&s), ["A"]);
    let before = s.history_tree();
    let result: crate::Result<()> = crate::commands::app_more::grouped(&mut s, "Failure", |s| {
        solid(s, "Temporary");
        Err(crate::EngineError::Other("cancelled".into()))
    });
    assert!(result.is_err());
    assert_eq!(s.history_tree(), before, "failed actions must not discard redo or branches");
    assert_eq!(names(&s), ["A"]);
}

#[test]
fn undo_then_new_step_keeps_a_branch() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    solid(&mut s, "A");
    solid(&mut s, "B");
    solid(&mut s, "C");
    // Linear so far: Original, New Composition, 3 solids.
    let st = list(&mut s);
    assert_eq!(st.len(), 5, "{st:?}");
    assert!(st.iter().all(|n| n["depth"] == 0));
    assert_eq!(st.last().unwrap()["current"], true);
    // Undo twice (B and C undone), then a new step: AE would forget B and C.
    assert!(s.undo() && s.undo());
    let st = list(&mut s);
    assert_eq!(st.iter().filter(|n| n["future"] == true).count(), 2);
    solid(&mut s, "D");
    assert!(s.history.redo.is_empty(), "the new step clears Redo, as in After Effects");
    assert_eq!(s.history.branches.len(), 1);
    assert_eq!(names(&s), ["D", "A"]);
    let st = list(&mut s);
    assert_eq!(st.len(), 6, "the undone states are still listed: {st:?}");
    let branch: Vec<&Value> = st.iter().filter(|n| n["depth"] == 1).collect();
    assert_eq!(branch.len(), 2, "B and C sit on a branch: {st:?}");
    // Jump to C on the old branch.
    let c = st.iter().rfind(|n| n["depth"] == 1).unwrap()["index"].as_u64().unwrap();
    let r = s.execute("edit.history.goto", json!({"index": c})).unwrap();
    assert_eq!(r["label"], "New Solid", "{r}");
    assert_eq!(names(&s), ["C", "B", "A"]);
    // Now D is the branch, and Undo walks the B/C line.
    assert_eq!(s.history.branches.len(), 1);
    assert_eq!(s.history.branches[0].steps.len(), 1);
    assert!(s.undo());
    assert_eq!(names(&s), ["B", "A"]);
    assert!(s.redo());
    // ...and back to D by id.
    let st = list(&mut s);
    let d = st.iter().find(|n| n["depth"] == 1).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("edit.history.goto", json!({"id": d})).unwrap();
    assert_eq!(names(&s), ["D", "A"]);
    let st = list(&mut s);
    assert_eq!(st.len(), 6);
    assert_eq!(st.iter().filter(|n| n["current"] == true).count(), 1);
    // The original state.
    let root = index_of(&st, "Original", 0);
    s.execute("edit.history.goto", json!({"index": root})).unwrap();
    assert!(s.project.comps().next().is_none());
    assert!(s.history.undo.is_empty());
    assert_eq!(list(&mut s).len(), 6, "nothing is lost by jumping");
    // Relative steps along the line.
    s.execute("edit.history.goto", json!({"steps": 2})).unwrap();
    assert_eq!(names(&s), ["A"]);
    assert!(s.execute("edit.history.goto", json!({"id": "nope"})).is_err());
    assert!(s.execute("edit.history.goto", json!({})).is_err());
}

#[test]
fn branches_of_branches_and_purge() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    solid(&mut s, "A");
    solid(&mut s, "B");
    s.undo();
    solid(&mut s, "C"); // branch 1: B
    s.undo();
    solid(&mut s, "D"); // branch 2: C
    let st = list(&mut s);
    assert_eq!(st.len(), 6, "{st:?}");
    // Go to B, undo to A, make E: D's line becomes a branch, and a branch of the new line.
    let b = st.iter().find(|n| n["depth"] == 1 && n["label"] == "New Solid").unwrap()["index"].as_u64().unwrap();
    s.execute("edit.history.goto", json!({"index": b})).unwrap();
    let which = names(&s);
    assert!(which == ["B", "A"] || which == ["C", "A"], "{which:?}");
    s.undo();
    solid(&mut s, "E");
    let st = list(&mut s);
    assert_eq!(st.len(), 7, "{st:?}");
    // Every state is reachable and restores its own layers.
    let mut seen = std::collections::BTreeSet::new();
    for i in 0..st.len() {
        s.execute("edit.history.goto", json!({"index": i})).unwrap();
        seen.insert(names(&s).join(","));
        assert_eq!(list(&mut s).len(), 7);
    }
    for want in ["", "A", "B,A", "C,A", "D,A", "E,A"] {
        assert!(seen.contains(want), "{want:?} not reachable: {seen:?}");
    }
    s.execute("edit.purgeUndo", json!({})).unwrap();
    assert_eq!(list(&mut s).len(), 1);
}

#[test]
fn undo_levels_trim_branches() {
    let mut s = Session::default();
    s.prefs.general.keep_all_history = false;
    s.prefs.general.undo_levels = 3;
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    for n in ["A", "B", "C", "D", "E"] {
        solid(&mut s, n);
    }
    assert_eq!(s.history.undo.len(), 3);
    s.undo();
    s.undo();
    solid(&mut s, "F");
    // The branch (D, E) grew from a state still kept.
    assert_eq!(s.history.branch_states(), 2);
    for n in ["G", "H", "I"] {
        solid(&mut s, n);
    }
    // Its parent fell off the end of the undo levels: the branch goes too.
    assert_eq!(s.history.branch_states(), 0);
    assert_eq!(list(&mut s).len(), 4);
}

#[test]
fn modified_mark_follows_undo_and_no_op_edits_record_nothing() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    solid(&mut s, "A");
    let path = std::env::temp_dir().join(format!("ec-dirty-{}.ecproj", std::process::id()));
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    assert!(!s.is_dirty());
    // Setting a value to what it already is: no undo step, still saved.
    let steps = s.history.undo.len();
    s.execute("prop.set", json!({"layer": "A", "path": "transform/opacity", "value": 100})).unwrap();
    assert_eq!(s.history.undo.len(), steps);
    assert!(!s.is_dirty());
    // A real change, then undo back to the saved state: not modified; redo: modified again.
    s.execute("prop.set", json!({"layer": "A", "path": "transform/opacity", "value": 50})).unwrap();
    assert!(s.is_dirty());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.is_dirty(), "undo back to the saved state");
    s.execute("edit.redo", json!({})).unwrap();
    assert!(s.is_dirty());
    // History jumps: the saved state is the parent of the current (changed) one.
    let states = list(&mut s);
    let cur = states.iter().find(|n| n["current"] == true).unwrap();
    let (changed, saved) = (cur["index"].as_u64().unwrap(), cur["parent"].as_u64().unwrap());
    s.execute("edit.history.goto", json!({"index": saved})).unwrap();
    assert!(!s.is_dirty(), "jumped to the saved state");
    s.execute("edit.history.goto", json!({"index": changed})).unwrap();
    assert!(s.is_dirty());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.is_dirty());
    let r = s.execute(
        "engine.batch",
        json!({"atomic": true, "steps": [
            {"command": "prop.set", "params": {"layer": "A", "path": "transform/opacity", "value": 10}},
            {"command": "no.suchCommand", "params": {}}
        ]}),
    );
    assert!(r.is_err());
    assert!(!s.is_dirty(), "rolled back");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_session_restored_with_unsaved_changes_stays_modified() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    let p = (*s.project).clone();
    // The web app reopens the last visit's project, which had unsaved changes.
    s.replace_project(p, None);
    assert!(!s.is_dirty());
    s.mark_unsaved();
    assert!(s.is_dirty());
    solid(&mut s, "A");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.is_dirty(), "undo doesn't reach a saved state");
}

#[test]
fn dropped_redo_parent_does_not_discard_reachable_branches() {
    let state = || Arc::new(Project::default());
    let current = state();
    let futures: Vec<_> = (0..5).map(|_| state()).collect();
    let reachable = state();
    let mut history = History {
        redo: futures.iter().rev().map(|p| ("future".into(), p.clone())).collect(),
        branches: vec![
            Branch { parent: current.clone(), steps: vec![("reachable".into(), reachable.clone())] },
            Branch { parent: futures[4].clone(), steps: vec![("orphan1".into(), state()), ("orphan2".into(), state())] },
        ],
        ..Default::default()
    };
    history.trim(2, &current);
    assert_eq!(history.redo.len(), 2);
    assert_eq!(history.branches.len(), 1);
    assert!(Arc::ptr_eq(&history.branches[0].steps[0].1, &reachable));
    assert_eq!(history.branch_states(), 1);
    assert!(history.tree(&current).iter().any(|node| node.label == "reachable"));
}

#[test]
fn evicted_branch_parent_does_not_discard_other_reachable_branches() {
    let state = || Arc::new(Project::default());
    let current = state();
    let parent = state();
    let reachable = state();
    let mut history = History {
        branches: vec![
            Branch { parent: current.clone(), steps: vec![("parent".into(), parent.clone())] },
            Branch { parent: current.clone(), steps: vec![("reachable".into(), reachable.clone())] },
            Branch { parent, steps: vec![("orphan1".into(), state()), ("orphan2".into(), state())] },
        ],
        ..Default::default()
    };
    history.trim(2, &current);
    assert_eq!(history.branches.len(), 1);
    assert!(Arc::ptr_eq(&history.branches[0].steps[0].1, &reachable));
    assert_eq!(history.branch_states(), 1);
    assert_eq!(history.tree(&current).len(), 2);
}

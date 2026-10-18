//! Manual saves use the existing task machinery, with at most one writer per session.
//! The worker owns immutable document/history snapshots; completion never marks later edits saved.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

pub(crate) const TASK_KIND: &str = "projectSave";

impl Session {
    /// A manual save is still serialising, writing, or awaiting its completion notification.
    pub fn is_saving(&self) -> bool {
        self.tasks.iter().any(|task| task.kind == TASK_KIND)
    }
}

pub(crate) fn check_idle(session: &Session) -> Result<()> {
    if session.is_saving() {
        return Err(EngineError::Other("A project save is already in progress; wait for it to finish before saving again.".into()));
    }
    Ok(())
}

pub(crate) fn start(session: &mut Session, path: &str) -> Result<Value> {
    check_idle(session)?;
    let project = session.project.clone();
    let history = session.history.clone();
    let view = session.saved_view();
    let revision = session.revision;
    let epoch = session.script_ui.project_epoch;
    let origin = session.path.clone();
    let services = session.services.clone();
    let path = path.to_string();
    session.spawn_task(TASK_KIND, format!("Saving {path}"), false, move |ctl| {
        crate::guard::guarded("background project save", || {
            ctl.message("Serialising project and complete edit history");
            let json = crate::history_file::encode_with_view(&project, &history, false, &view)?;
            let text = if crate::xml_project::is_xml_path(&path) { crate::xml_project::to_xml(&json).map_err(EngineError::Other)? } else { json };
            ctl.message("Writing project");
            services.write_file(&path, text.as_bytes()).map_err(|e| EngineError::Other(format!("cannot write {path}: {e}")))?;
            let bytes = text.len();
            let apply: crate::jobs::Apply = Box::new(move |session| {
                // New/Open may have replaced the document while its immutable snapshot saved.
                if session.script_ui.project_epoch == epoch && session.path == origin {
                    session.path = Some(path.clone());
                    session.saved_revision = if Arc::ptr_eq(&session.project, &project) { session.revision } else { revision };
                    session.saved_project = project;
                    session.saved_history = history;
                    session.refresh_history_dirty();
                    session.note_project_path_async(&path);
                    // Keep the durable edit journal: resetting it would serialise all history
                    // again on the UI thread and could discard protection for newer edits.
                }
                Ok(json!({"path": path, "bytes": bytes, "toast": format!("Saved {path}")}))
            });
            Ok(apply)
        })
        .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    #[derive(Default)]
    struct Files {
        gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
        written: Mutex<Vec<(String, Vec<u8>)>>,
        fail: bool,
        panic: bool,
    }
    impl crate::Services for Files {
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            if let Some((started, release)) = self.gate.lock().unwrap().take() {
                started.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(5)).map_err(std::io::Error::other)?;
            }
            assert!(!self.panic, "synthetic broken storage plugin");
            if self.fail {
                return Err(std::io::Error::other("synthetic full disk"));
            }
            self.written.lock().unwrap().push((path.into(), data.to_vec()));
            Ok(())
        }
    }

    fn session(files: Arc<Files>) -> Session {
        let mut session = Session { services: files, ..Default::default() };
        session.execute("comp.new", json!({"name": "Save fixture", "width": 64, "height": 64})).unwrap();
        session
    }
    fn block(files: &Files) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (started, received) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        *files.gate.lock().unwrap() = Some((started, waiting));
        (received, release)
    }
    fn written(files: &Files) -> Value {
        serde_json::from_slice(&files.written.lock().unwrap()[0].1).unwrap()
    }

    #[test]
    fn background_save_preserves_new_edits_and_every_captured_history_state() {
        let files = Arc::new(Files::default());
        let mut session = session(files.clone());
        let snapshot = session.project.clone();
        let archive: Value = serde_json::from_str(&crate::history_file::encode(&snapshot, &session.history, false).unwrap()).unwrap();
        let (started, release) = block(&files);
        let response = session.execute("file.saveAs", json!({"path": "saved.ecproj", "background": true})).unwrap();
        assert!(response["started"].as_bool().unwrap());
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(session.is_saving() && session.path.is_none() && session.needs_save());
        session.execute("layer.newNull", json!({"name": "edit while saving"})).unwrap();
        release.send(()).unwrap();
        session.wait_jobs();
        assert_eq!(session.job_log.last().unwrap().status, "done");
        assert_eq!(written(&files)["editHistory"], archive["editHistory"]);
        assert!(Arc::ptr_eq(&session.saved_project, &snapshot));
        assert!(session.needs_save(), "later edits must remain unsaved");
        session.execute("edit.undo", json!({})).unwrap();
        assert!(!session.is_dirty());
        assert!(session.needs_save(), "the new redo future must still be saved");
        session.execute("file.save", json!({"background": true})).unwrap();
        session.wait_jobs();
        assert!(!session.needs_save());
    }

    #[test]
    fn background_save_is_single_writer_and_cannot_be_cancelled_after_writing() {
        let files = Arc::new(Files::default());
        let mut session = session(files.clone());
        let (started, release) = block(&files);
        let result = session.execute("file.saveAs", json!({"path": "first.ecproj", "background": true})).unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        for background in [true, false] {
            assert!(
                session
                    .execute("file.saveAs", json!({"path": "second.ecproj", "background": background}))
                    .unwrap_err()
                    .to_string()
                    .contains("already in progress")
            );
        }
        assert!(!session.cancel_job(result["job"].as_str().unwrap()));
        assert!(!session.cancel_job("all"));
        assert!(!session.jobs()[0].cancellable);
        release.send(()).unwrap();
        session.wait_jobs();
        assert_eq!(files.written.lock().unwrap().len(), 1);
        assert_eq!(session.path.as_deref(), Some("first.ecproj"));
    }

    #[test]
    fn background_save_survives_project_switch_without_joining_or_marking_new_project_saved() {
        let files = Arc::new(Files::default());
        let mut session = session(files.clone());
        let (started, release) = block(&files);
        session.execute("file.saveAs", json!({"path": "old.ecproj", "background": true})).unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        session.execute("file.newProject", json!({})).unwrap();
        session.execute("comp.new", json!({"name": "New project"})).unwrap();
        let new_project = session.project.clone();
        release.send(()).unwrap();
        session.wait_jobs();
        assert_eq!(session.job_log.last().unwrap().status, "done");
        assert!(session.path.is_none() && session.needs_save());
        assert!(Arc::ptr_eq(&session.project, &new_project));
        assert_eq!(files.written.lock().unwrap()[0].0, "old.ecproj");
    }

    #[test]
    fn background_save_storage_errors_and_panics_leave_document_and_saved_state_intact() {
        for panic in [false, true] {
            let files = Arc::new(Files { fail: !panic, panic, ..Default::default() });
            let mut session = session(files.clone());
            let current = session.project.clone();
            let saved = session.saved_project.clone();
            session.execute("file.saveAs", json!({"path": "fail.ecproj", "background": true})).unwrap();
            session.wait_jobs();
            let record = session.job_log.last().unwrap();
            assert_eq!(record.status, "failed");
            assert!(record.message.contains(if panic { "synthetic broken storage plugin" } else { "synthetic full disk" }));
            assert!(Arc::ptr_eq(&session.project, &current) && Arc::ptr_eq(&session.saved_project, &saved));
            assert!(session.path.is_none() && session.needs_save() && !session.is_saving());
            assert!(files.written.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn background_save_xml_keeps_complete_branching_history() {
        let files = Arc::new(Files::default());
        let mut session = session(files.clone());
        session.execute("layer.newNull", json!({})).unwrap();
        session.execute("edit.undo", json!({})).unwrap();
        session.execute("layer.newNull", json!({"name": "alternative"})).unwrap();
        let original: Value = serde_json::from_str(&crate::history_file::encode(&session.project, &session.history, false).unwrap()).unwrap();
        session.execute_checked("file.saveAs", json!({"path": "history.ecprojx", "background": true})).unwrap();
        session.wait_jobs();
        let xml = String::from_utf8(files.written.lock().unwrap()[0].1.clone()).unwrap();
        let restored: Value = serde_json::from_str(&crate::xml_project::from_xml(&xml).unwrap()).unwrap();
        assert_eq!(restored["editHistory"], original["editHistory"]);
        assert!(!session.needs_save());
    }

    #[test]
    fn background_save_completion_does_not_join_a_pending_autosave() {
        let files = Arc::new(Files::default());
        let mut session = session(files);
        let (release, waiting) = mpsc::channel();
        session.autosave.pending = Some(
            crate::autosave::PendingWrite::spawn("old-auto-save.ecproj".into(), 1, session.revision, None, session.script_ui.project_epoch, move || {
                waiting.recv_timeout(Duration::from_secs(5)).map_err(std::io::Error::other)
            })
            .unwrap(),
        );
        session.execute("file.saveAs", json!({"path": "new.ecproj", "background": true})).unwrap();
        session.wait_jobs();
        assert_eq!(session.job_log.last().unwrap().status, "done");
        assert!(session.autosave.pending.is_some());
        assert_eq!(session.path.as_deref(), Some("new.ecproj"));
        release.send(()).unwrap();
        session.autosave_wait().unwrap();
        assert!(session.autosave.last_path.is_none(), "the old checkpoint must not be published under the new path");
    }
}

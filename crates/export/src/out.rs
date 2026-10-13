//! Output files: the file system, or an in-memory [`Sink`] chosen per job (the web app, which
//! offers every written file as a download; tests).

use std::io::{BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{ExportError, Result, io};

/// Receives each finished output file (path as resolved, complete contents) instead of the file
/// system. Called once per file; image sequences call it from the render threads.
pub type Sink = dyn Fn(&str, Vec<u8>) + Send + Sync;

/// One output file being written.
pub(crate) enum Out<'a> {
    File { file: FileOut, cleanup: Option<&'a Mutex<Vec<String>>> },
    Mem { path: String, cur: Cursor<Vec<u8>>, sink: &'a Sink },
}

/// Create `path` (parent directories included) or an in-memory file for `sink`.
pub(crate) fn create<'a>(sink: Option<&'a Sink>, path: &str) -> Result<Out<'a>> {
    match sink {
        Some(sink) => Ok(Out::Mem { path: path.to_string(), cur: Cursor::new(Vec::new()), sink }),
        None => FileOut::create(Path::new(path)).map(|file| Out::File { file, cleanup: None }),
    }
}

/// Whether `path` already exists (never for a sink: it has no memory of earlier jobs).
pub(crate) fn exists(sink: Option<&Sink>, path: &str) -> bool {
    sink.is_none() && std::path::Path::new(path).exists()
}

impl<'a> Out<'a> {
    /// Cancellation owns only successfully published, newly created destinations. Failed
    /// encoders own their provisional file, whose guard removes it without touching the target.
    pub(crate) fn track_cleanup(&mut self, destinations: &'a Mutex<Vec<String>>) {
        if let Out::File { cleanup, .. } = self {
            *cleanup = Some(destinations);
        }
    }

    /// Flush and close; a sink receives the contents now. Returns the file size.
    pub(crate) fn finish(self) -> Result<u64> {
        match self {
            Out::File { file, cleanup } => {
                let destination = (!file.target_existed).then(|| file.target.to_string_lossy().into_owned());
                let bytes = file.finish()?;
                if let (Some(cleanup), Some(destination)) = (cleanup, destination) {
                    cleanup.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(destination);
                }
                Ok(bytes)
            }
            Out::Mem { path, cur, sink } => {
                let data = cur.into_inner();
                let n = data.len() as u64;
                sink(&path, data);
                Ok(n)
            }
        }
    }
}

impl Write for Out<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::File { file, .. } => file.writer()?.write(buf),
            Out::Mem { cur, .. } => cur.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::File { file, .. } => file.writer()?.flush(),
            Out::Mem { .. } => Ok(()),
        }
    }
}

impl Seek for Out<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match self {
            Out::File { file, .. } => file.writer()?.seek(pos),
            Out::Mem { cur, .. } => cur.seek(pos),
        }
    }
}

/// An adjacent, exclusively created temporary file. Only a completed export replaces the
/// destination; dropping a cancelled/failed encoder removes its unfinished temporary file.
pub(crate) struct FileOut {
    writer: Option<BufWriter<std::fs::File>>,
    temp: PathBuf,
    target: PathBuf,
    target_existed: bool,
    committed: bool,
}

impl FileOut {
    fn create(target: &Path) -> Result<Self> {
        let target_existed = target.try_exists().map_err(io)?;
        let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(io)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
            let temp = parent.join(format!(".effectcraft-export-{}-{nonce}.tmp", std::process::id()));
            match std::fs::OpenOptions::new().write(true).read(true).create_new(true).open(&temp) {
                Ok(file) => return Ok(Self { writer: Some(BufWriter::new(file)), temp, target: target.to_path_buf(), target_existed, committed: false }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io(error)),
            }
        }
        Err(ExportError::Io("cannot create an exclusive temporary export file; remove stale .effectcraft-export-*.tmp files or choose another folder".into()))
    }

    fn writer(&mut self) -> std::io::Result<&mut BufWriter<std::fs::File>> {
        self.writer.as_mut().ok_or_else(|| std::io::Error::other("export file is already closed"))
    }

    fn finish(mut self) -> Result<u64> {
        let mut writer = self.writer.take().ok_or_else(|| ExportError::Io("export file is already closed".into()))?;
        writer.flush().map_err(io)?;
        let file = writer.into_inner().map_err(|error| ExportError::Io(error.to_string()))?;
        file.sync_all().map_err(io)?;
        let size = file.metadata().map_err(io)?.len();
        // Windows cannot rename an open file. The same-directory rename replaces the old
        // destination only after all encoding, flushing and sync steps have succeeded.
        drop(file);
        std::fs::rename(&self.temp, &self.target).map_err(io)?;
        self.committed = true;
        Ok(size)
    }
}

impl Drop for FileOut {
    fn drop(&mut self) {
        drop(self.writer.take());
        if !self.committed
            && let Err(error) = std::fs::remove_file(&self.temp)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("cannot remove unfinished export {}: {error}", self.temp.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("ec-export-atomic-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cancellation_tracks_only_new_destinations_after_successful_publication() {
        let dir = dir();
        let cleanup = Mutex::new(Vec::new());
        let existing = dir.join("previous.mov");
        std::fs::write(&existing, b"previous completed output").unwrap();
        let new = dir.join("new.mov");
        for target in [&existing, &new] {
            let mut output = create(None, target.to_str().unwrap()).unwrap();
            output.track_cleanup(&cleanup);
            output.write_all(b"unfinished movie").unwrap();
            drop(output);
            assert!(cleanup.lock().unwrap().is_empty());
        }
        assert_eq!(std::fs::read(&existing).unwrap(), b"previous completed output");
        assert!(!new.exists());
        for target in [&existing, &new] {
            let mut output = create(None, target.to_str().unwrap()).unwrap();
            output.track_cleanup(&cleanup);
            output.write_all(b"completed movie").unwrap();
            output.finish().unwrap();
        }
        assert_eq!(*cleanup.lock().unwrap(), vec![new.to_string_lossy().into_owned()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unfinished_exports_preserve_the_existing_destination_and_remove_temp() {
        let dir = dir();
        let target = dir.join("movie.gif");
        std::fs::write(&target, b"previous completed export").unwrap();
        let mut output = create(None, target.to_str().unwrap()).unwrap();
        output.write_all(b"incomplete new movie").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"previous completed export");
        drop(output);
        assert_eq!(std::fs::read(&target).unwrap(), b"previous completed export");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let mut output = create(None, target.to_str().unwrap()).unwrap();
        output.write_all(b"finished new movie").unwrap();
        assert_eq!(output.finish().unwrap(), 18);
        assert_eq!(std::fs::read(&target).unwrap(), b"finished new movie");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_final_rename_does_not_publish_or_leave_an_unfinished_file() {
        let dir = dir();
        let target = dir.join("existing-directory");
        std::fs::create_dir(&target).unwrap();
        let mut output = create(None, target.to_str().unwrap()).unwrap();
        output.write_all(b"finished bytes").unwrap();
        assert!(output.finish().is_err());
        assert!(target.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sinks_receive_only_finished_files() {
        let files = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received = files.clone();
        let sink = move |path: &str, bytes: Vec<u8>| received.lock().unwrap().push((path.to_string(), bytes));
        let mut output = create(Some(&sink), "movie.gif").unwrap();
        output.write_all(b"unfinished").unwrap();
        drop(output);
        assert!(files.lock().unwrap().is_empty());
        let mut output = create(Some(&sink), "movie.gif").unwrap();
        output.write_all(b"complete").unwrap();
        assert_eq!(output.finish().unwrap(), 8);
        assert_eq!(*files.lock().unwrap(), vec![("movie.gif".to_owned(), b"complete".to_vec())]);
    }
}

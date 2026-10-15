//! Persistent configuration storage (Settings, keyboard shortcut presets, the crash-recovery
//! sentinel). The engine only sees the [`ConfigStore`] trait; frontends choose where it lives:
//! the desktop app uses a [`DirConfig`] in the platform config directory, the web app backs it
//! with the browser's Origin Private File System (IndexedDB fallback), tests and headless runs use
//! [`MemoryConfig`] (or none at all).
//!
//! Auto-saves are project files, not settings, but they live wherever the store says: the store's
//! [`FileOps`] (the file system by default; the browser's storage on the web).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Named text blobs (`prefs.json`, `shortcuts.json`, `session.lock`).
pub trait ConfigStore: Send + Sync {
    fn read(&self, name: &str) -> Option<String>;
    /// Read a bounded UTF-8 entry, distinguishing missing entries from read errors.
    /// The compatible default checks an existing store's returned string. Stores that read
    /// from disk should override this to enforce the limit while reading.
    fn read_bounded(&self, name: &str, max_bytes: usize) -> std::io::Result<Option<String>> {
        let text = self.read(name);
        if text.as_ref().is_some_and(|text| text.len() > max_bytes) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "configuration entry exceeds its size limit"));
        }
        Ok(text)
    }
    /// A required bounded entry for recovery, where absence is a contextual read error.
    fn read_required_bounded(&self, name: &str, max_bytes: usize) -> std::io::Result<String> {
        self.read_bounded(name, max_bytes)?.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "configuration file is unavailable"))
    }
    fn write(&self, name: &str, data: &str) -> std::io::Result<()>;
    fn remove(&self, name: &str) -> std::io::Result<()>;
    /// Names of the entries directly in `dir` (a `/`-separated name prefix such as `Scripts`),
    /// sorted. Stores that can't list return nothing.
    fn list(&self, dir: &str) -> Vec<String> {
        let _ = dir;
        vec![]
    }
    /// Bound directory enumeration before recovery allocates/deserializes its entries.
    fn list_bounded(&self, dir: &str, limit: usize) -> std::io::Result<Vec<String>> {
        let names = self.list(dir);
        if names.len() > limit {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "recovery directory exceeds the entry limit"));
        }
        Ok(names)
    }
    /// A nonblocking native lease. Closing the returned handle releases it, including after
    /// process termination. `None` means another live run or recovery dialog owns the lease.
    fn try_lease(&self, name: &str) -> std::io::Result<Option<std::fs::File>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let relative = Path::new(name);
            if relative.as_os_str().is_empty() || relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid recovery lease name"));
            }
            let base =
                self.native_recovery_dir().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Unsupported, "native recovery leases are unavailable"))?;
            let path = base.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(path)?;
            match file.try_lock() {
                Ok(()) => Ok(Some(file)),
                Err(std::fs::TryLockError::WouldBlock) => Ok(None),
                Err(std::fs::TryLockError::Error(e)) => Err(e),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = name;
            Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "native recovery leases are unavailable"))
        }
    }
    /// The folder next to the configuration (default Auto-Save folder for untitled projects),
    /// in the namespace of [`ConfigStore::files`]; `None` when there is none.
    fn dir(&self) -> Option<PathBuf> {
        None
    }
    /// Physical filesystem storage for native edit journals and session leases. A virtual
    /// namespace returned by `dir()` (for example browser storage) must not opt in.
    fn native_recovery_dir(&self) -> Option<PathBuf> {
        None
    }
    /// Where auto-saves are listed, written and found (default: the file system).
    fn files(&self) -> &dyn FileOps {
        &StdFiles
    }
}

/// The file operations auto-save and crash recovery need.
pub trait FileOps: Send + Sync {
    fn read_bounded(&self, path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
        let _ = (path, limit);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "file reading is unavailable"))
    }
    /// Files directly in `dir`: (file name, modified time).
    fn list(&self, dir: &Path) -> Vec<(String, std::time::SystemTime)>;
    /// Write `data` to `path` atomically, creating its folder.
    fn write(&self, path: &Path, data: &[u8]) -> std::io::Result<()>;
    fn remove(&self, path: &Path) -> std::io::Result<()>;
    fn is_file(&self, path: &Path) -> bool;
}

/// [`FileOps`] on the file system.
pub struct StdFiles;

impl FileOps for StdFiles {
    fn read_bounded(&self, path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
        read_file_bounded(path, limit)
    }
    fn list(&self, dir: &Path) -> Vec<(String, std::time::SystemTime)> {
        let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
        rd.flatten()
            .filter_map(|e| {
                let m = e.metadata().ok().filter(|m| m.is_file())?.modified().ok()?;
                Some((e.file_name().to_string_lossy().to_string(), m))
            })
            .collect()
    }
    fn write(&self, path: &Path, data: &[u8]) -> std::io::Result<()> {
        if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d)?;
        }
        atomic_write(path, data)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// In-memory store (tests, headless sessions).
#[derive(Default)]
pub struct MemoryConfig {
    pub files: Mutex<BTreeMap<String, String>>,
}

impl ConfigStore for MemoryConfig {
    fn read(&self, name: &str) -> Option<String> {
        self.files.lock().ok()?.get(name).cloned()
    }
    fn write(&self, name: &str, data: &str) -> std::io::Result<()> {
        self.files.lock().map_err(|_| std::io::Error::other("configuration lock is poisoned"))?.insert(name.to_string(), data.to_string());
        Ok(())
    }
    fn remove(&self, name: &str) -> std::io::Result<()> {
        self.files.lock().map_err(|_| std::io::Error::other("configuration lock is poisoned"))?.remove(name);
        Ok(())
    }
    fn list(&self, dir: &str) -> Vec<String> {
        let pre = format!("{}/", dir.trim_end_matches('/'));
        let Ok(m) = self.files.lock() else { return vec![] };
        m.keys().filter_map(|k| k.strip_prefix(&pre)).filter(|n| !n.is_empty() && !n.contains('/')).map(str::to_string).collect()
    }
}

/// Files in one directory, written atomically.
pub struct DirConfig {
    pub dir: PathBuf,
}

impl DirConfig {
    pub fn new(dir: impl Into<PathBuf>) -> DirConfig {
        DirConfig { dir: dir.into() }
    }
}

impl ConfigStore for DirConfig {
    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(name)).ok()
    }
    fn read_bounded(&self, name: &str, max_bytes: usize) -> std::io::Result<Option<String>> {
        let bytes = match read_file_bounded(&self.dir.join(name), max_bytes) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        String::from_utf8(bytes).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
    fn write(&self, name: &str, data: &str) -> std::io::Result<()> {
        let path = self.dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(&self.dir))?;
        atomic_write(&path, data.as_bytes())
    }
    fn list(&self, dir: &str) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.dir.join(dir)) else { return vec![] };
        let mut v: Vec<String> = rd.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }
    fn list_bounded(&self, dir: &str, limit: usize) -> std::io::Result<Vec<String>> {
        let rd = match std::fs::read_dir(self.dir.join(dir)) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut names = vec![];
        for entry in rd {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if names.len() >= limit {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "recovery directory exceeds the entry limit"));
                }
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        Ok(names)
    }
    fn remove(&self, name: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.dir.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
    fn dir(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }
    fn native_recovery_dir(&self) -> Option<PathBuf> {
        (!cfg!(target_arch = "wasm32")).then(|| self.dir.clone())
    }
}

fn read_file_bounded(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let maximum = u64::try_from(limit).map_err(std::io::Error::other)?;
    if length > maximum {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "file exceeds the recovery size limit"));
    }
    let mut data = Vec::new();
    data.try_reserve(usize::try_from(length).map_err(std::io::Error::other)?).map_err(std::io::Error::other)?;
    file.take(maximum.saturating_add(1)).read_to_end(&mut data)?;
    if data.len() > limit {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "file exceeds the recovery size limit"));
    }
    Ok(data)
}

/// A new, empty temporary file next to `path` for an atomic write of it, and its path. The name
/// is unique to this process and this write (`.effectcraft-<pid>-<n>.tmp`) and the file is created
/// exclusively, so two writers never share one and nothing already at that name (a file, a
/// symbolic link) is opened or followed (#1233). It ends in `.tmp`, so a file left by a hard
/// kill never counts as an auto-save. Its bounded name does not include the destination's name,
/// so a valid long destination name still fits the filesystem's component limit.
fn create_temp(path: &Path) -> std::io::Result<(std::fs::File, PathBuf)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut last = None;
    for _ in 0..16 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_file_name(format!(".effectcraft-{}-{n}.tmp", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((f, tmp)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temporary file name")))
}

/// Write `data` to `path` atomically: write a temporary file next to it, flush it to disk, then
/// rename it over `path`. A crash at any point leaves either the old file or the new one, never a
/// torn file, and concurrent writers each publish a whole file of their own.
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    atomic_write_with(path, data, |f, data| {
        use std::io::Write;
        f.write_all(data)?;
        f.sync_all()
    })
}

/// [`atomic_write`] with an injectable temp-file writer (tests simulate a crash mid-write); it is
/// given the temporary file, already created. A failed write or rename removes the temporary
/// file and leaves `path` untouched.
pub fn atomic_write_with(path: &Path, data: &[u8], write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>) -> std::io::Result<()> {
    let (mut f, tmp) = create_temp(path)?;
    let written = write(&mut f, data);
    // Closed before the rename (Windows can't rename an open file).
    drop(f);
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

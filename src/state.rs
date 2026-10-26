//! Per-session state files: `${CLAUDE_PLUGIN_DATA}/state/<session_id>.json`.
//!
//! - Directories are 0700 and owner-checked; a group/world-writable or foreign
//!   directory is refused.
//! - Files are opened with `O_NOFOLLOW`; writes go to a random `O_EXCL` temp
//!   file, are fsynced, then renamed into place.
//! - Every read-modify-write holds an exclusive `flock` on `<id>.lock`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::contract::{SessionState, STATE_VERSION};

/// State files are small; anything bigger is treated as corrupt.
const STATE_MAX_BYTES: u64 = 1 << 20;

/// Wall-clock seconds since the epoch.
pub fn now() -> i64 {
    now_ms() / 1000
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `${CLAUDE_PLUGIN_DATA}`, if set and non-empty.
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_PLUGIN_DATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `^[A-Za-z0-9_-]{1,128}$`
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Random lowercase hex string of `2 * bytes` characters.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // Extremely unlikely; fall back to time + pid so ids stay distinct.
        let seed = (now_ms() as u64) ^ ((std::process::id() as u64) << 32);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (seed >> ((i % 8) * 8)) as u8 ^ (i as u8);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Check that `dir` is a real directory owned by us and not group- or
/// world-writable.
pub fn check_private_dir(dir: &Path) -> io::Result<()> {
    let md = std::fs::symlink_metadata(dir)?;
    if !md.file_type().is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not_a_dir"));
    }
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if md.uid() != uid || md.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "insecure_dir",
        ));
    }
    Ok(())
}

/// Create (0700) or verify `parent/name`. `parent` must already exist: this
/// never recreates a deleted data directory.
pub fn private_subdir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = parent.join(name);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    check_private_dir(&dir)?;
    Ok(dir)
}

/// Create `${CLAUDE_PLUGIN_DATA}` itself if missing (hooks and CLI only).
pub fn ensure_data_dir() -> io::Result<PathBuf> {
    let dir = data_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no_data_dir"))?;
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
    }
    Ok(dir)
}

/// Open a file for reading without following symlinks; refuse non-regular
/// files and anything larger than `max`.
pub fn read_nofollow(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let md = f.metadata()?;
    if !md.file_type().is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not_a_file"));
    }
    if md.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too_large"));
    }
    let mut buf = Vec::with_capacity(md.len() as usize);
    f.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too_large"));
    }
    Ok(buf)
}

/// Atomically replace `path` with `bytes` (mode 0600): random `O_EXCL` temp
/// file in the same directory, fsync, rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no_parent"))?;
    let tmp = dir.join(format!(".tmp-{}", random_hex(8)));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// An exclusive `flock` held until drop.
pub struct FileLock {
    _file: File,
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

impl FileLock {
    /// Block until the lock is held.
    pub fn acquire(path: &Path) -> io::Result<FileLock> {
        let file = open_lock_file(path)?;
        loop {
            // SAFETY: fd is valid for the lifetime of `file`.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc == 0 {
                return Ok(FileLock { _file: file });
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Take the lock without blocking; `Ok(None)` if someone else holds it.
    pub fn try_acquire(path: &Path) -> io::Result<Option<FileLock>> {
        let file = open_lock_file(path)?;
        Ok(try_flock(&file)?.then_some(FileLock { _file: file }))
    }

    /// Wait at most `timeout` for the lock; `ErrorKind::TimedOut` after that.
    pub fn acquire_timeout(path: &Path, timeout: Duration) -> io::Result<FileLock> {
        let file = open_lock_file(path)?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if try_flock(&file)? {
                return Ok(FileLock { _file: file });
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "lock_timeout"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Non-blocking exclusive `flock`; `Ok(false)` if someone else holds it.
fn try_flock(file: &File) -> io::Result<bool> {
    // SAFETY: fd is valid for the lifetime of `file`.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.kind() {
        io::ErrorKind::WouldBlock => Ok(false),
        io::ErrorKind::Interrupted => Ok(false),
        _ => Err(err),
    }
}

/// Result of reading a session's state.
#[derive(Debug)]
pub enum Loaded {
    Missing,
    Ok(Box<SessionState>),
    /// Unreadable or unparseable: the session state is `unknown`.
    Corrupt,
}

/// The `state/` directory.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Open the store, creating `state/` (0700) if needed. With `create_data`,
    /// `${CLAUDE_PLUGIN_DATA}` itself is created when missing.
    pub fn open(create_data: bool) -> io::Result<Store> {
        let data = if create_data {
            ensure_data_dir()?
        } else {
            data_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no_data_dir"))?
        };
        if !create_data && !data.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no_data_dir"));
        }
        let dir = private_subdir(&data, "state")?;
        Ok(Store { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, sid: &str) -> PathBuf {
        self.dir.join(format!("{sid}.json"))
    }

    /// `${CLAUDE_PLUGIN_DATA}/sweeper.lock`. It lives outside `state/`, so it
    /// can never collide with a session's `<id>.lock`.
    pub fn sweeper_lock_path(&self) -> PathBuf {
        self.dir.parent().unwrap_or(&self.dir).join("sweeper.lock")
    }

    fn lock_path(&self, sid: &str) -> PathBuf {
        debug_assert!(valid_session_id(sid));
        self.dir.join(format!("{sid}.lock"))
    }

    /// Exclusive lock for one session's read-modify-write.
    pub fn lock(&self, sid: &str) -> io::Result<FileLock> {
        FileLock::acquire(&self.lock_path(sid))
    }

    /// Like [`Store::lock`], but gives up after `timeout`, so a stuck lock
    /// never runs a hook into Claude Code's hook timeout.
    pub fn lock_timeout(&self, sid: &str, timeout: Duration) -> io::Result<FileLock> {
        FileLock::acquire_timeout(&self.lock_path(sid), timeout)
    }

    pub fn load(&self, sid: &str) -> Loaded {
        match read_nofollow(&self.path(sid), STATE_MAX_BYTES) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Loaded::Missing,
            Err(_) => Loaded::Corrupt,
            Ok(bytes) => match serde_json::from_slice::<SessionState>(&bytes) {
                Ok(s) if s.version == STATE_VERSION && s.session_id == sid => {
                    Loaded::Ok(Box::new(s))
                }
                _ => Loaded::Corrupt,
            },
        }
    }

    pub fn save(&self, state: &SessionState) -> io::Result<()> {
        let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
        write_atomic(&self.path(&state.session_id), &bytes)
    }

    pub fn remove(&self, sid: &str) -> io::Result<()> {
        match std::fs::remove_file(self.path(sid)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Session ids that have a state file.
    pub fn sessions(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let sid = name.strip_suffix(".json")?;
                valid_session_id(sid).then(|| sid.to_string())
            })
            .collect();
        out.sort();
        out
    }

    /// Delete state and stray temp files not modified for `max_age`, except
    /// those of `keep`. A `.lock` file is deleted only when its `.json` is
    /// gone and the lock itself is older than `max_age`: unlinking a live
    /// session's lock would let two holders lock different inodes.
    pub fn prune(&self, max_age: Duration, keep: &str) {
        let cutoff = SystemTime::now().checked_sub(max_age).unwrap_or(UNIX_EPOCH);
        let old = |p: &Path| {
            std::fs::symlink_metadata(p)
                .and_then(|m| m.modified())
                .map(|t| t < cutoff)
                .unwrap_or(false)
        };
        let names = || -> Vec<String> {
            std::fs::read_dir(&self.dir)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        // Pass 1: old state files (only while their lock is free) and temps.
        for name in names() {
            let path = self.dir.join(&name);
            if let Some(sid) = name.strip_suffix(".json") {
                if sid == keep || !valid_session_id(sid) || !old(&path) {
                    continue;
                }
                if let Ok(Some(_l)) = FileLock::try_acquire(&self.lock_path(sid)) {
                    if old(&path) {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            } else if name.starts_with(".tmp-") && old(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
        // Pass 2: orphaned locks only.
        for name in names() {
            let Some(sid) = name.strip_suffix(".lock") else {
                continue;
            };
            if sid == keep || !valid_session_id(sid) || self.path(sid).exists() {
                continue;
            }
            let path = self.dir.join(&name);
            if old(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// A fresh state for `sid` in project `project`.
pub fn new_state(sid: &str, project: &str) -> SessionState {
    SessionState {
        version: STATE_VERSION,
        session_id: sid.to_string(),
        project: project.to_string(),
        claude: None,
        pending_tools: Vec::new(),
        episode: None,
        updated_at: now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_validation() {
        assert!(valid_session_id("abc-DEF_123"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("../etc"));
        assert!(!valid_session_id("a b"));
        assert!(!valid_session_id(&"a".repeat(129)));
        assert!(valid_session_id(&"a".repeat(128)));
    }

    #[test]
    fn random_hex_is_distinct() {
        let a = random_hex(8);
        assert_eq!(a.len(), 16);
        assert_ne!(a, random_hex(8));
    }
}

//! Exclusive workspace lock held by mutating commands like `gasp sync`.
//!
//! Uses an OS-level file lock on `.workspace/lock` via `fs2`. The lock
//! file's contents (PID and hostname of the holder) are written for
//! diagnostic purposes only; the OS releases the lock when the holding
//! process dies, so stale lock files are not a problem.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{Error, Result};

const LOCK_FILE: &str = "lock";

/// RAII guard. The lock is released when this is dropped.
#[derive(Debug)]
pub struct WorkspaceLock {
    file: File,
    path: PathBuf,
}

impl WorkspaceLock {
    /// Try to acquire the lock without blocking. Errors if another
    /// process holds it; the error includes their PID+hostname.
    pub fn acquire(dot_dir: &Path) -> Result<Self> {
        let path = dot_dir.join(LOCK_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| Error::Io {
                operation: "open lock file".into(),
                path: path.clone(),
                source,
            })?;

        if file.try_lock_exclusive().is_err() {
            // Lock held by another process; read the existing holder for
            // a useful error message.
            let mut holder = String::new();
            file.read_to_string(&mut holder).ok();
            let holder = holder.trim();
            let holder = if holder.is_empty() {
                "another gasp process".to_string()
            } else {
                holder.to_string()
            };
            return Err(Error::WorkspaceLocked {
                path: path.clone(),
                holder,
            });
        }

        // Write our identification for whoever might wait on us.
        let payload = format!(
            "pid={} host={}\n",
            std::process::id(),
            hostname::get()
                .ok()
                .and_then(|s| s.into_string().ok())
                .unwrap_or_else(|| "unknown".into()),
        );
        file.set_len(0).ok();
        file.seek(SeekFrom::Start(0)).ok();
        file.write_all(payload.as_bytes()).ok();
        file.flush().ok();

        Ok(Self { file, path })
    }

    /// Lock-file path, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        // Releasing the OS lock is automatic on file close, but be
        // explicit so the lock isn't held longer than necessary if the
        // File somehow outlives us via dup. Errors are swallowed —
        // best-effort cleanup.
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_then_release_lets_next_caller_in() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _g = WorkspaceLock::acquire(dir.path()).unwrap();
        }
        // Previous guard dropped; should be free again.
        let _g = WorkspaceLock::acquire(dir.path()).unwrap();
    }

    #[test]
    fn second_concurrent_acquire_fails() {
        let dir = tempfile::tempdir().unwrap();
        let g1 = WorkspaceLock::acquire(dir.path()).unwrap();
        let err = WorkspaceLock::acquire(dir.path()).unwrap_err();
        assert!(matches!(err, Error::WorkspaceLocked { .. }));
        drop(g1);
    }

    #[test]
    fn lock_file_contains_pid() {
        let dir = tempfile::tempdir().unwrap();
        let _g = WorkspaceLock::acquire(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap();
        assert!(body.contains(&format!("pid={}", std::process::id())));
    }
}

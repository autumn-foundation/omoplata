//! Multi-writer safety primitives for `.omoplata/` (ADR-0008).
//!
//! Object writes are already concurrency-safe (content-addressed, staged to a
//! temp file and `rename`d into place, idempotent). The *mutable* state — the
//! refs and the append-only operation log folded out of `oplog.jsonl` — is not:
//! every writer runs an unguarded `load -> mutate -> save` cycle, so two
//! concurrent `omo` processes lose updates and can leave a torn file. This
//! module provides the two mechanisms ADR-0008 decided on:
//!
//! * [`RepoLock`] — an RAII guard over an **exclusive `flock(2)`** on
//!   `.omoplata/lock`, giving mutual exclusion across processes. The lock is
//!   released when the guard is dropped and — inherently — when the process
//!   dies (the kernel drops `flock` when the owning file descriptor closes), so
//!   there is no stale-lock bookkeeping.
//! * [`atomic_write`] — a **crash-atomic** file write (temp file -> `fsync` ->
//!   `rename` -> directory `fsync`) so the op log is never observed torn and a
//!   crash mid-write cannot corrupt it.
//!
//! See `docs/adr/0008-multi-writer-concurrency.md` for the race analysis and the
//! reasoning behind `flock` over an `O_EXCL` PID lockfile.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// Aliased so calls resolve to fs4's advisory-lock trait rather than the
// inherent `std::fs::File::{lock, try_lock, unlock}` methods that shadow the
// same names on recent toolchains. Every lock call below is written as
// `Flock::method(&file)` to keep the fs4 implementation unambiguous.
use fs4::FileExt as Flock;

use crate::io;
use crate::StoreError;

/// An exclusive advisory lock over a repository's mutable state (ADR-0008).
///
/// While a `RepoLock` is alive, no other process can hold the exclusive lock on
/// the same `.omoplata/lock` file, so a writer can run its whole
/// `load -> mutate -> save` critical section atomically with respect to every
/// other `omo` process. This is the mechanism that closes the ref-update
/// read-modify-write race: the second writer blocks in
/// [`Repository::lock`](crate::Repository::lock) until the first has saved and
/// dropped its guard, and therefore always folds its `old` target against the
/// first writer's committed state.
///
/// # Crash and liveness behaviour
///
/// The lock is an advisory `flock(2)`. It is released when this guard is dropped
/// **and** automatically by the kernel if the process exits or is killed while
/// holding it — including a `kill -9` or a power loss. There is consequently no
/// stale lock to detect or clean up, unlike an `O_EXCL` PID lockfile.
///
/// # Scope
///
/// `flock` serializes *processes*. It does not mediate two threads of a single
/// process that share the same open lock file; omoplata's model is one repo
/// mutation per `omo` invocation, so cross-process exclusion is exactly what is
/// required.
#[derive(Debug)]
pub struct RepoLock {
    /// The open, locked file. Held so the descriptor — and thus the `flock` —
    /// stays alive for the guard's lifetime; released on drop.
    file: File,
    /// The lock file's path, retained for diagnostics on unlock failure.
    path: PathBuf,
}

impl RepoLock {
    /// Open (creating if absent) `lock_path` and take an **exclusive**, blocking
    /// advisory lock, returning the held guard.
    ///
    /// Blocks until the lock is acquired. See [`try_acquire`](Self::try_acquire)
    /// for the non-blocking variant.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] if the lock file cannot be opened or the lock cannot be
    /// acquired.
    pub(crate) fn acquire(lock_path: PathBuf) -> Result<Self, StoreError> {
        let file = Self::open(&lock_path)?;
        Flock::lock(&file).map_err(io(&lock_path))?;
        Ok(Self {
            file,
            path: lock_path,
        })
    }

    /// Open (creating if absent) `lock_path` and try to take an **exclusive**
    /// advisory lock **without blocking**.
    ///
    /// Returns `Ok(Some(guard))` if the lock was free and is now held, or
    /// `Ok(None)` if another process currently holds it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] if the lock file cannot be opened, or the lock attempt
    /// fails for a reason other than the lock being held.
    pub(crate) fn try_acquire(lock_path: PathBuf) -> Result<Option<Self>, StoreError> {
        let file = Self::open(&lock_path)?;
        match Flock::try_lock(&file) {
            Ok(()) => Ok(Some(Self {
                file,
                path: lock_path,
            })),
            Err(fs4::TryLockError::WouldBlock) => Ok(None),
            Err(fs4::TryLockError::Error(source)) => Err(StoreError::Io {
                path: lock_path,
                source,
            }),
        }
    }

    /// Open the lock file for read+write, creating it if it does not exist.
    fn open(lock_path: &Path) -> Result<File, StoreError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(io(lock_path))
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        // Dropping the file descriptor already releases the `flock`; unlocking
        // explicitly first makes the release eager and observable. Errors here
        // cannot be surfaced from `drop` and do not affect correctness (the
        // kernel releases the lock on close regardless), so they are ignored —
        // `self.path` is retained only to aid debugging if this is ever logged.
        let _ = Flock::unlock(&self.file);
        let _ = &self.path;
    }
}

/// Atomically and durably write `bytes` to `path` (ADR-0008 crash-safety).
///
/// The write is staged so that a concurrent reader — even one that does not hold
/// the repository lock — observes **either** the complete previous contents
/// **or** the complete new contents of `path`, never a torn or truncated mixture,
/// and so that a crash at any point cannot corrupt the file. The steps, in order:
///
/// 1. write `bytes` to a uniquely-named temporary sibling of `path`;
/// 2. `fsync` the temporary file, so its data is durable before it is exposed;
/// 3. `rename` the temporary file over `path` — atomic within a filesystem, so
///    the swap is all-or-nothing;
/// 4. `fsync` the parent directory, so the rename itself survives a crash.
///
/// A crash before step 3 leaves the complete old file; a crash after step 3
/// leaves the complete new file. The temporary name embeds the process id and a
/// per-process counter, so concurrent writers never collide on it.
///
/// The parent-directory `fsync` is best-effort: some platforms (notably Windows)
/// do not permit opening a directory as a file. The atomicity of the rename does
/// not depend on it; only the durability of the rename across a crash does.
///
/// # Errors
///
/// [`StoreError::Io`] if any of the staging, sync, or rename steps fails.
pub fn atomic_write(path: impl AsRef<Path>, bytes: &[u8]) -> Result<(), StoreError> {
    let path = path.as_ref();
    let tmp = temp_sibling(path);

    // Steps 1-2: write and fsync the staged copy before it is ever linked to the
    // real name.
    {
        let mut file = File::create(&tmp).map_err(io(&tmp))?;
        file.write_all(bytes).map_err(io(&tmp))?;
        file.sync_all().map_err(io(&tmp))?;
    }

    // Step 3: atomically publish the new contents.
    fs::rename(&tmp, path).map_err(io(path))?;

    // Step 4: make the rename durable. Best-effort (see the doc comment).
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(dir_file) = File::open(dir) {
            let _ = dir_file.sync_all();
        }
    }

    Ok(())
}

/// A unique temporary sibling path for `path`, of the form
/// `.<filename>.tmp.<pid>.<counter>`, so concurrent writers never share a temp
/// name.
fn temp_sibling(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();

    let mut name = std::ffi::OsString::from(".");
    if let Some(file_name) = path.file_name() {
        name.push(file_name);
    } else {
        name.push("omoplata");
    }
    name.push(format!(".tmp.{pid}.{n}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn atomic_write_creates_then_replaces() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.txt");
        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        atomic_write(&path, b"second-longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second-longer");
        // A shorter follow-up write fully replaces (no trailing bytes from the
        // longer previous contents).
        atomic_write(&path, b"x").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"x");
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("oplog.jsonl");
        atomic_write(&path, b"data").unwrap();
        let stray: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(stray.is_empty(), "temp files left behind: {stray:?}");
    }

    #[test]
    fn temp_sibling_names_are_unique() {
        let path = Path::new("/repo/.omoplata/oplog.jsonl");
        let a = temp_sibling(path);
        let b = temp_sibling(path);
        assert_ne!(a, b);
        assert_eq!(a.parent(), path.parent());
    }
}

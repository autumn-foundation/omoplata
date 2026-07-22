//! Resolving a user-supplied path to the git directory that actually holds the
//! objects and refs.
//!
//! Users naturally point `omo git verify`/`import` at a *worktree root* (the
//! directory a repo was cloned or `init`ed into) rather than at its `.git`
//! subdirectory. The object store, however, lives under `<root>/.git/objects`,
//! not `<root>/objects` — so walking `<root>/objects` finds nothing. Reporting
//! "found nothing" as a PASS is a silent wrong answer (it violates the
//! project's "no silent wrong answers" theorem); the first half of the fix is
//! to auto-descend into `.git`, and the second half (in [`crate::gate`] and
//! [`crate::import`]) is to refuse when even the resolved directory is empty.

use std::path::{Path, PathBuf};

use crate::error::GitError;

/// Resolve `path` to the git directory that holds `objects/` and `refs/`.
///
/// Handles the ways a user points `omo git` at a repository:
/// - **Worktree root** — `<path>/.git` is a directory: returns `<path>/.git`.
///   This is the natural (and previously silently-wrong) invocation, where the
///   objects live under `<path>/.git/objects`, not `<path>/objects`.
/// - **Already a git directory** — `<path>` itself has `objects/` **and**
///   `refs/`, or a `HEAD` file plus `objects/`: returns `<path>` unchanged.
///   Covers an explicit `.../.git` argument and a bare repository.
/// - **Linked-worktree `.git` file** — `<path>/.git` is a *file* (it contains
///   `gitdir: <path>` pointing at the real git dir): **not** resolved in v1.
///   Rather than silently walking a directory with no `objects/`, this returns
///   [`GitError::NotARepository`]; full linked-worktree support is a follow-up.
/// - Anything else: [`GitError::NotARepository`].
///
/// Resolution is idempotent on an already-resolved git directory: given
/// `<root>/.git`, `<root>/.git/.git` does not exist, and `<root>/.git` has
/// `objects/`+`refs/`, so it is returned as-is. Callers may therefore resolve
/// defensively without fear of descending twice.
///
/// # Errors
/// Returns [`GitError::NotARepository`] if `path` is neither a worktree root
/// with a `.git` directory nor a git directory itself (including the
/// unsupported linked-worktree `.git`-file case).
pub fn resolve_git_dir(path: &Path) -> Result<PathBuf, GitError> {
    let dot_git = path.join(".git");
    if dot_git.is_dir() {
        // Worktree root handed in — descend into its `.git` directory.
        return Ok(dot_git);
    }
    if dot_git.is_file() {
        // Linked worktree: `.git` is a file (`gitdir: <path>`). Not resolved in
        // v1 — refuse clearly instead of walking an object-less directory.
        return Err(GitError::NotARepository(path.to_path_buf()));
    }
    if looks_like_git_dir(path) {
        // Already a `.git` (or bare repo) — use it directly.
        return Ok(path.to_path_buf());
    }
    Err(GitError::NotARepository(path.to_path_buf()))
}

/// Whether `path` itself looks like a git directory: it has both `objects/` and
/// `refs/`, or a `HEAD` file alongside `objects/`. The `HEAD`+`objects/`
/// fallback covers bare repositories whose `refs/` may be empty/pruned.
fn looks_like_git_dir(path: &Path) -> bool {
    let has_objects = path.join("objects").is_dir();
    let has_refs = path.join("refs").is_dir();
    let has_head = path.join("HEAD").is_file();
    has_objects && (has_refs || has_head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_worktree_root_to_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A minimal worktree-shaped layout: `<root>/.git/{objects,refs}` + HEAD.
        std::fs::create_dir_all(root.join(".git").join("objects")).unwrap();
        std::fs::create_dir_all(root.join(".git").join("refs")).unwrap();
        std::fs::write(root.join(".git").join("HEAD"), b"ref: refs/heads/main\n").unwrap();

        assert_eq!(resolve_git_dir(root).unwrap(), root.join(".git"));
    }

    #[test]
    fn resolves_bare_git_dir_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let gd = dir.path();
        std::fs::create_dir_all(gd.join("objects")).unwrap();
        std::fs::create_dir_all(gd.join("refs")).unwrap();

        assert_eq!(resolve_git_dir(gd).unwrap(), gd);
    }

    #[test]
    fn resolution_is_idempotent_on_a_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let gd = root.join(".git");
        std::fs::create_dir_all(gd.join("objects")).unwrap();
        std::fs::create_dir_all(gd.join("refs")).unwrap();
        // Resolving the already-resolved `.git` returns it unchanged.
        assert_eq!(resolve_git_dir(&gd).unwrap(), gd);
    }

    #[test]
    fn empty_dir_is_not_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        match resolve_git_dir(dir.path()) {
            Err(GitError::NotARepository(_)) => {}
            other => panic!("expected NotARepository, got {other:?}"),
        }
    }

    #[test]
    fn dot_git_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Linked-worktree `.git` *file*, not a directory.
        std::fs::write(root.join(".git"), b"gitdir: /somewhere/.git/worktrees/wt\n").unwrap();
        match resolve_git_dir(root) {
            Err(GitError::NotARepository(_)) => {}
            other => panic!("expected NotARepository for a `.git` file, got {other:?}"),
        }
    }
}

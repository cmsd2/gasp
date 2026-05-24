//! libgit2-backed read/write operations on local repositories.
//!
//! These never touch the network; for that, see `git::remote`.

use std::path::Path;

use git2::{Repository, StatusOptions};

use crate::error::{Error, Result};

fn open(path: &Path) -> Result<Repository> {
    Repository::open(path).map_err(|source| Error::LibGit {
        operation: "open".into(),
        path: path.to_path_buf(),
        source,
    })
}

fn map_err(op: &str, path: &Path) -> impl FnOnce(git2::Error) -> Error {
    let operation = op.to_string();
    let path = path.to_path_buf();
    move |source| Error::LibGit {
        operation,
        path,
        source,
    }
}

/// Returns the full SHA-1 of HEAD.
pub fn head_sha(path: &Path) -> Result<String> {
    let repo = open(path)?;
    let head = repo.head().map_err(map_err("head", path))?;
    let oid = head.target().ok_or_else(|| Error::LibGit {
        operation: "head".into(),
        path: path.to_path_buf(),
        source: git2::Error::from_str("HEAD has no direct target (symbolic ref?)"),
    })?;
    Ok(oid.to_string())
}

/// Returns the short name of the currently checked-out branch, or `None`
/// if HEAD is detached.
pub fn current_branch(path: &Path) -> Result<Option<String>> {
    let repo = open(path)?;
    let head = repo.head().map_err(map_err("head", path))?;
    if !head.is_branch() {
        return Ok(None);
    }
    Ok(head.shorthand().map(|s| s.to_string()))
}

/// Returns true if the working tree or index has any uncommitted changes
/// (including untracked files).
pub fn is_dirty(path: &Path) -> Result<bool> {
    let repo = open(path)?;
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).include_ignored(false);
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(map_err("status", path))?;
    Ok(!statuses.is_empty())
}

/// Resolve `revision` to a SHA-1 in the local repo, given the remote name
/// to use for branch lookup. Resolution order:
///   1. `<remote>/<revision>` (treat as remote-tracking branch)
///   2. `refs/tags/<revision>`
///   3. raw revision (sha, local branch, anything `revparse_single` accepts)
///
/// Returns `Ok(None)` if the revision can't be resolved (e.g. branch not
/// yet fetched, tag not present), distinct from other libgit2 errors.
pub fn resolve_revision(path: &Path, revision: &str, remote: &str) -> Result<Option<String>> {
    let repo = open(path)?;

    let candidates = [
        format!("{remote}/{revision}"),
        format!("refs/tags/{revision}"),
        revision.to_string(),
    ];

    for spec in &candidates {
        if let Ok(obj) = repo.revparse_single(spec) {
            return Ok(Some(obj.id().to_string()));
        }
    }
    Ok(None)
}

/// True if the working tree at `path` looks like a git repository.
pub fn is_repo(path: &Path) -> bool {
    Repository::open(path).is_ok()
}

/// True if the currently-checked-out branch has an upstream tracking
/// branch configured. Detached HEAD returns `false`. Used to avoid
/// calling `git pull` in repos where it would just fail with "no
/// tracking information".
pub fn has_upstream(path: &Path) -> Result<bool> {
    let repo = open(path)?;
    let head = repo.head().map_err(map_err("head", path))?;
    if !head.is_branch() {
        return Ok(false);
    }
    let Some(refname) = head.name() else {
        return Ok(false);
    };
    match repo.branch_upstream_name(refname) {
        Ok(_) => Ok(true),
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(false),
        Err(source) => Err(Error::LibGit {
            operation: "branch_upstream_name".into(),
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(cwd: &Path, args: &[&str]) {
        let s = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap();
        assert!(s.success(), "git {args:?} failed");
    }

    fn fresh_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q", "-b", "main", "."]);
        std::fs::write(dir.path().join("f"), "1\n").unwrap();
        run(dir.path(), &["add", "-A"]);
        run(
            dir.path(),
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        dir
    }

    #[test]
    fn head_sha_is_40_hex() {
        let d = fresh_repo();
        let sha = head_sha(d.path()).unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn current_branch_returns_branch_name() {
        let d = fresh_repo();
        assert_eq!(current_branch(d.path()).unwrap().as_deref(), Some("main"));
    }

    #[test]
    fn current_branch_none_when_detached() {
        let d = fresh_repo();
        let sha = head_sha(d.path()).unwrap();
        run(d.path(), &["checkout", "-q", "--detach", &sha]);
        assert_eq!(current_branch(d.path()).unwrap(), None);
    }

    #[test]
    fn is_dirty_false_for_clean_tree() {
        let d = fresh_repo();
        assert!(!is_dirty(d.path()).unwrap());
    }

    #[test]
    fn is_dirty_true_for_uncommitted_change() {
        let d = fresh_repo();
        std::fs::write(d.path().join("f"), "2\n").unwrap();
        assert!(is_dirty(d.path()).unwrap());
    }

    #[test]
    fn is_dirty_true_for_untracked_file() {
        let d = fresh_repo();
        std::fs::write(d.path().join("new"), "x\n").unwrap();
        assert!(is_dirty(d.path()).unwrap());
    }

    #[test]
    fn resolve_revision_finds_tag() {
        let d = fresh_repo();
        run(d.path(), &["tag", "v1"]);
        let sha = head_sha(d.path()).unwrap();
        assert_eq!(
            resolve_revision(d.path(), "v1", "origin").unwrap(),
            Some(sha)
        );
    }

    #[test]
    fn resolve_revision_finds_sha() {
        let d = fresh_repo();
        let sha = head_sha(d.path()).unwrap();
        assert_eq!(
            resolve_revision(d.path(), &sha, "origin").unwrap(),
            Some(sha.clone())
        );
    }

    #[test]
    fn resolve_revision_returns_none_for_unknown() {
        let d = fresh_repo();
        assert_eq!(
            resolve_revision(d.path(), "nonexistent-branch", "origin").unwrap(),
            None
        );
    }

    #[test]
    fn is_repo_true_for_init() {
        let d = fresh_repo();
        assert!(is_repo(d.path()));
    }

    #[test]
    fn is_repo_false_for_plain_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(!is_repo(d.path()));
    }
}

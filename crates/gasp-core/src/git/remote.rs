use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// Clone `url` into `dest`. If `revision` is `Some`, check it out after
/// cloning. Creates parent directories as needed. Shells out to `git`.
pub fn clone(url: &str, dest: &Path, revision: Option<&str>) -> Result<()> {
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            operation: "create parent directory for clone".into(),
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut cmd = Command::new("git");
    cmd.arg("clone").arg(url).arg(dest);
    run_git(&mut cmd, "clone", url)?;

    if let Some(rev) = revision {
        run_git(&mut git_in(dest, ["checkout", rev]), "checkout", rev)?;
    }
    Ok(())
}

/// `git -C <repo> fetch --prune <remote>`. `--prune` removes stale
/// remote-tracking branches so resolve_revision doesn't keep finding
/// branches the upstream has deleted.
pub fn fetch(repo: &Path, remote: &str) -> Result<()> {
    run_git(
        &mut git_in(repo, ["fetch", "--prune", remote]),
        "fetch",
        remote,
    )
}

/// `git -C <repo> merge --ff-only <target>`. Fails if the merge would
/// not be a fast-forward.
pub fn merge_ff_only(repo: &Path, target: &str) -> Result<()> {
    run_git(
        &mut git_in(repo, ["merge", "--ff-only", target]),
        "merge --ff-only",
        target,
    )
}

/// `git -C <repo> rebase <onto>`. Leaves the rebase in progress on
/// conflict — git's own error message is surfaced.
pub fn rebase(repo: &Path, onto: &str) -> Result<()> {
    run_git(&mut git_in(repo, ["rebase", onto]), "rebase", onto)
}

/// `git -C <repo> reset --hard <target>`. Destroys local commits and
/// working-tree changes.
pub fn reset_hard(repo: &Path, target: &str) -> Result<()> {
    run_git(
        &mut git_in(repo, ["reset", "--hard", target]),
        "reset --hard",
        target,
    )
}

/// `git -C <repo> pull --ff-only`. Fetches origin and fast-forwards the
/// current branch; fails if a fast-forward isn't possible.
pub fn pull_ff_only(repo: &Path) -> Result<()> {
    run_git(
        &mut git_in(repo, ["pull", "--ff-only"]),
        "pull --ff-only",
        "origin",
    )
}

fn git_in<I, S>(repo: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    cmd
}

fn run_git(cmd: &mut Command, operation: &str, target: &str) -> Result<()> {
    let output = cmd.output().map_err(|source| Error::GitSpawn { source })?;
    if !output.status.success() {
        return Err(Error::GitFailed {
            operation: operation.to_string(),
            target: target.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}

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
            path: parent.to_path_buf(),
            source,
        })?;
    }

    run_git(
        Command::new("git").arg("clone").arg(url).arg(dest),
        "clone",
        url,
    )?;

    if let Some(rev) = revision {
        run_git(
            Command::new("git")
                .arg("-C")
                .arg(dest)
                .arg("checkout")
                .arg(rev),
            "checkout",
            rev,
        )?;
    }

    Ok(())
}

/// `git -C <repo> fetch <remote>`.
pub fn fetch(repo: &Path, remote: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("fetch")
            .arg("--prune")
            .arg(remote),
        "fetch",
        remote,
    )
}

/// `git -C <repo> merge --ff-only <target>`. Fails if the merge would not
/// be a fast-forward.
pub fn merge_ff_only(repo: &Path, target: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("merge")
            .arg("--ff-only")
            .arg(target),
        "merge --ff-only",
        target,
    )
}

/// `git -C <repo> rebase <onto>`. Falls back to leaving the rebase in
/// progress on conflict — git's own error message is surfaced.
pub fn rebase(repo: &Path, onto: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("rebase")
            .arg(onto),
        "rebase",
        onto,
    )
}

/// `git -C <repo> reset --hard <target>`. Destroys local commits and
/// working-tree changes.
pub fn reset_hard(repo: &Path, target: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("reset")
            .arg("--hard")
            .arg(target),
        "reset --hard",
        target,
    )
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

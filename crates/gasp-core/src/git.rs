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

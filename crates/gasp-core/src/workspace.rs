use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;
use crate::manifest::Manifest;

/// Name of the marker directory at the workspace root.
pub const DOT_DIR: &str = ".workspace";

/// Filename of the manifest inside the marker directory.
pub const MANIFEST_FILE: &str = "workspace.toml";

/// Subdirectory of `.workspace/` that holds the cloned manifest repo
/// when the workspace was initialized from a git URL.
pub const MANIFEST_REPO_DIR: &str = "manifest";

/// Where the manifest physically lives in a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestMode {
    /// Loose file at `.workspace/workspace.toml`.
    Loose,
    /// Checked-out git repo at `.workspace/manifest/`; manifest is the
    /// `workspace.toml` inside it.
    Cloned,
}

/// Outcome of `Workspace::update_manifest`. `None` is returned for
/// Loose-mode workspaces (nothing to update).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestUpdate {
    /// Already at the latest commit on the tracked branch.
    UpToDate { sha: String },
    /// Fast-forwarded from `from` → `to`.
    Advanced { from: String, to: String },
    /// Skipped because the manifest repo has uncommitted changes.
    SkippedDirty { sha: String },
    /// Skipped because the current branch has no upstream tracking
    /// configured (detached HEAD, or `branch.<name>.remote` unset).
    /// Carries the current branch name (if any) and HEAD sha so the
    /// CLI can suggest a fix.
    SkippedNoUpstream { sha: String, branch: Option<String> },
}

/// A discovered or freshly initialized gasp workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Walk up from `start` looking for the first directory containing a
    /// `.workspace/` subdirectory.
    pub fn discover(start: &Path) -> Result<Self> {
        let start = start.canonicalize().map_err(|source| Error::Io {
            operation: "canonicalize start path".into(),
            path: start.to_path_buf(),
            source,
        })?;
        for ancestor in start.ancestors() {
            if ancestor.join(DOT_DIR).is_dir() {
                return Ok(Self {
                    root: ancestor.to_path_buf(),
                });
            }
        }
        Err(Error::WorkspaceNotFound(start))
    }

    /// Initialize a new workspace at `root` by creating `.workspace/` and
    /// copying `manifest_src` into it as `workspace.toml`. Validates that
    /// the source parses as a manifest before doing anything on disk.
    pub fn init(root: &Path, manifest_src: &Path) -> Result<Self> {
        let dot = root.join(DOT_DIR);
        if dot.exists() {
            return Err(Error::WorkspaceExists(dot));
        }

        // Validate the source manifest before touching the filesystem.
        let _ = Manifest::load(manifest_src)?;

        std::fs::create_dir_all(&dot).map_err(|source| Error::Io {
            operation: "create .workspace directory".into(),
            path: dot.clone(),
            source,
        })?;
        let dest = dot.join(MANIFEST_FILE);
        std::fs::copy(manifest_src, &dest).map_err(|source| Error::Io {
            operation: "copy manifest into .workspace".into(),
            path: dest.clone(),
            source,
        })?;

        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Initialize a new workspace at `root` by cloning a manifest
    /// repository into `.workspace/manifest/`. The cloned repo must
    /// contain a parseable `workspace.toml` at its top level; the whole
    /// `.workspace/` directory is removed on validation failure.
    pub fn init_from_url(root: &Path, url: &str) -> Result<Self> {
        let dot = root.join(DOT_DIR);
        if dot.exists() {
            return Err(Error::WorkspaceExists(dot));
        }
        std::fs::create_dir_all(&dot).map_err(|source| Error::Io {
            operation: "create .workspace directory".into(),
            path: dot.clone(),
            source,
        })?;

        let manifest_repo = dot.join(MANIFEST_REPO_DIR);
        let clone_result = git::clone(url, &manifest_repo, None);
        if let Err(e) = clone_result {
            // Roll back the empty .workspace/ we created.
            std::fs::remove_dir_all(&dot).ok();
            return Err(e);
        }

        let manifest_path = manifest_repo.join(MANIFEST_FILE);
        if let Err(e) = Manifest::load(&manifest_path) {
            std::fs::remove_dir_all(&dot).ok();
            return Err(e);
        }

        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dot_dir(&self) -> PathBuf {
        self.root.join(DOT_DIR)
    }

    /// Path to the cloned-manifest repo directory (whether it exists or not).
    pub fn manifest_repo_dir(&self) -> PathBuf {
        self.dot_dir().join(MANIFEST_REPO_DIR)
    }

    /// Whether this workspace stores the manifest as a loose file or as
    /// a checked-out git repo.
    pub fn manifest_mode(&self) -> ManifestMode {
        if self.manifest_repo_dir().join(".git").exists() {
            ManifestMode::Cloned
        } else {
            ManifestMode::Loose
        }
    }

    pub fn manifest_path(&self) -> PathBuf {
        match self.manifest_mode() {
            ManifestMode::Cloned => self.manifest_repo_dir().join(MANIFEST_FILE),
            ManifestMode::Loose => self.dot_dir().join(MANIFEST_FILE),
        }
    }

    /// Read and parse the manifest stored in this workspace.
    pub fn load_manifest(&self) -> Result<Manifest> {
        Manifest::load(&self.manifest_path())
    }

    /// Absolute on-disk path for a repo, given its (possibly relative)
    /// path from the manifest.
    pub fn repo_path(&self, repo_path: &Path) -> PathBuf {
        if repo_path.is_absolute() {
            repo_path.to_path_buf()
        } else {
            self.root.join(repo_path)
        }
    }

    /// Fetch + fast-forward the cloned manifest repo. Returns `None`
    /// for Loose-mode workspaces. Skips (without erroring) when the
    /// manifest repo has uncommitted changes or when the current
    /// branch has no upstream tracking — both are user-owned states.
    pub fn update_manifest(&self) -> Result<Option<ManifestUpdate>> {
        if self.manifest_mode() != ManifestMode::Cloned {
            return Ok(None);
        }
        let repo = self.manifest_repo_dir();
        let before = crate::git::local::head_sha(&repo)?;

        if crate::git::local::is_dirty(&repo)? {
            return Ok(Some(ManifestUpdate::SkippedDirty { sha: before }));
        }

        // Pre-flight via libgit2 so we never run `git pull` only to
        // hit "no tracking information" — a noisy failure that the
        // user can do nothing about until they configure upstream.
        if !crate::git::local::has_upstream(&repo)? {
            return Ok(Some(ManifestUpdate::SkippedNoUpstream {
                sha: before,
                branch: crate::git::local::current_branch(&repo)?,
            }));
        }

        crate::git::remote::pull_ff_only(&repo)?;
        let after = crate::git::local::head_sha(&repo)?;
        Ok(Some(if before == after {
            ManifestUpdate::UpToDate { sha: after }
        } else {
            ManifestUpdate::Advanced {
                from: before,
                to: after,
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write_manifest(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
    }

    const MIN_MANIFEST: &str = "version = 1\n";

    #[test]
    fn init_creates_dot_workspace_with_manifest() {
        let dir = tmpdir();
        let src = dir.path().join("seed.toml");
        write_manifest(&src, MIN_MANIFEST);

        let ws = Workspace::init(dir.path(), &src).unwrap();
        assert_eq!(ws.root(), dir.path());
        assert!(ws.dot_dir().is_dir());
        assert!(ws.manifest_path().is_file());

        let copied = fs::read_to_string(ws.manifest_path()).unwrap();
        assert_eq!(copied, MIN_MANIFEST);
    }

    #[test]
    fn init_refuses_if_already_initialized() {
        let dir = tmpdir();
        let src = dir.path().join("seed.toml");
        write_manifest(&src, MIN_MANIFEST);

        Workspace::init(dir.path(), &src).unwrap();
        let err = Workspace::init(dir.path(), &src).unwrap_err();
        assert!(matches!(err, Error::WorkspaceExists(_)));
    }

    #[test]
    fn init_rejects_invalid_manifest_without_touching_disk() {
        let dir = tmpdir();
        let src = dir.path().join("seed.toml");
        write_manifest(&src, "version = 99\n");

        let err = Workspace::init(dir.path(), &src).unwrap_err();
        assert!(matches!(err, Error::UnsupportedVersion { .. }));
        assert!(!dir.path().join(DOT_DIR).exists());
    }

    #[test]
    fn discover_finds_workspace_from_root() {
        let dir = tmpdir();
        fs::create_dir(dir.path().join(DOT_DIR)).unwrap();
        let ws = Workspace::discover(dir.path()).unwrap();
        assert_eq!(
            ws.root().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn discover_walks_up_from_subdirectory() {
        let dir = tmpdir();
        fs::create_dir(dir.path().join(DOT_DIR)).unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();

        let ws = Workspace::discover(&nested).unwrap();
        assert_eq!(
            ws.root().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn discover_errors_when_no_workspace_above() {
        let dir = tmpdir();
        let err = Workspace::discover(dir.path()).unwrap_err();
        assert!(matches!(err, Error::WorkspaceNotFound(_)));
    }

    /// Build a bare git repo whose working contents are `manifest_text`
    /// at `workspace.toml`. Returns the bare repo path, suitable for use
    /// as a clone URL.
    fn bare_manifest_repo(parent: &Path, manifest_text: &str) -> PathBuf {
        use std::process::Command;
        let src = parent.join("manifest-src");
        std::fs::create_dir_all(&src).unwrap();
        let g = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&src)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        g(&["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("workspace.toml"), manifest_text).unwrap();
        g(&["add", "-A"]);
        g(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "manifest",
        ]);
        let bare = parent.join("manifest.git");
        assert!(
            std::process::Command::new("git")
                .args([
                    "clone",
                    "--bare",
                    "-q",
                    src.to_str().unwrap(),
                    bare.to_str().unwrap(),
                ])
                .status()
                .unwrap()
                .success()
        );
        bare
    }

    #[test]
    fn init_from_url_clones_manifest_repo() {
        let dir = tmpdir();
        let parent = dir.path().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let bare = bare_manifest_repo(&parent, MIN_MANIFEST);

        let target = dir.path().join("ws");
        std::fs::create_dir(&target).unwrap();
        let ws = Workspace::init_from_url(&target, bare.to_str().unwrap()).unwrap();

        assert_eq!(ws.manifest_mode(), ManifestMode::Cloned);
        assert!(ws.manifest_repo_dir().join(".git").exists());
        assert!(ws.manifest_path().is_file());
        assert_eq!(
            ws.manifest_path(),
            ws.dot_dir().join("manifest").join("workspace.toml")
        );
    }

    #[test]
    fn init_from_url_rolls_back_if_manifest_invalid() {
        let dir = tmpdir();
        let parent = dir.path().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let bare = bare_manifest_repo(&parent, "version = 99\n");

        let target = dir.path().join("ws");
        std::fs::create_dir(&target).unwrap();
        let err = Workspace::init_from_url(&target, bare.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, Error::UnsupportedVersion { .. }));
        // .workspace/ was created during clone — must be cleaned up
        assert!(!target.join(DOT_DIR).exists());
    }

    #[test]
    fn init_from_url_rolls_back_if_repo_has_no_manifest() {
        let dir = tmpdir();
        // Build a bare repo with no workspace.toml inside.
        use std::process::Command;
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let g = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&src)
                .status()
                .unwrap()
        };
        g(&["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("README"), "no manifest\n").unwrap();
        g(&["add", "-A"]);
        g(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "x",
        ]);
        let bare = dir.path().join("bare.git");
        Command::new("git")
            .args([
                "clone",
                "--bare",
                "-q",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ])
            .status()
            .unwrap();

        let target = dir.path().join("ws");
        std::fs::create_dir(&target).unwrap();
        let err = Workspace::init_from_url(&target, bare.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, Error::ManifestNotFound(_)));
        assert!(!target.join(DOT_DIR).exists());
    }

    #[test]
    fn manifest_mode_defaults_to_loose() {
        let dir = tmpdir();
        let src = dir.path().join("seed.toml");
        write_manifest(&src, MIN_MANIFEST);
        let ws = Workspace::init(dir.path(), &src).unwrap();
        assert_eq!(ws.manifest_mode(), ManifestMode::Loose);
    }
}

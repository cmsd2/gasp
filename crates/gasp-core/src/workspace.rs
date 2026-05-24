use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::manifest::Manifest;

/// Name of the marker directory at the workspace root.
pub const DOT_DIR: &str = ".workspace";

/// Filename of the manifest inside the marker directory.
pub const MANIFEST_FILE: &str = "workspace.toml";

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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dot_dir(&self) -> PathBuf {
        self.root.join(DOT_DIR)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.dot_dir().join(MANIFEST_FILE)
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
}

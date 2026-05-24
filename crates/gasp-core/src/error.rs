use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("manifest file not found: {0}")]
    ManifestNotFound(PathBuf),

    #[error("failed to read manifest {path}: {source}")]
    ManifestRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse manifest {path}: {source}")]
    ManifestParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("unsupported manifest version {found} (this build supports version {expected})")]
    UnsupportedVersion { found: u32, expected: u32 },

    #[error("repo '{0}' is declared more than once in the manifest")]
    DuplicateRepoName(String),

    #[error("repo name must not be empty")]
    EmptyRepoName,

    #[error("repo '{name}' has an empty URL")]
    EmptyRepoUrl { name: String },

    #[error("repo '{name}' has invalid URL '{url}': {reason}")]
    InvalidRepoUrl {
        name: String,
        url: String,
        reason: String,
    },

    #[error("not inside a gasp workspace (no .workspace/ directory found in any parent of {0})")]
    WorkspaceNotFound(PathBuf),

    #[error("workspace already initialized at {0}")]
    WorkspaceExists(PathBuf),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to invoke git: {source}")]
    GitSpawn {
        #[source]
        source: std::io::Error,
    },

    #[error("git {operation} failed for {target}: {stderr}")]
    GitFailed {
        operation: String,
        target: String,
        stderr: String,
    },

    #[error("libgit2 error during {operation} at {path}: {source}")]
    LibGit {
        operation: String,
        path: PathBuf,
        #[source]
        source: git2::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

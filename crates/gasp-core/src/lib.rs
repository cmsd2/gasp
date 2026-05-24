pub mod error;
pub mod git;
pub mod manifest;
pub mod url;
pub mod workspace;

pub use error::{Error, Result};
pub use manifest::{Defaults, Manifest, Repo, RepoSpec};
pub use workspace::Workspace;

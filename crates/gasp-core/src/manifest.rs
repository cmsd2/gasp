use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

pub const SUPPORTED_VERSION: u32 = 1;
pub const DEFAULT_REMOTE: &str = "origin";
pub const DEFAULT_HOST: &str = "github.com";

/// Raw deserialized form of `workspace.toml`. Field defaults have not been
/// applied and URLs have not been normalized; call [`Manifest::resolve`]
/// to get a list of [`Repo`] values ready for use.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "repos")]
    pub repos: Vec<RepoSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Defaults {
    pub revision: Option<String>,
    pub remote: Option<String>,
    pub host: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoSpec {
    pub name: String,
    pub url: String,
    pub revision: Option<String>,
    pub path: Option<PathBuf>,
    pub remote: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

/// A repo with all defaults applied. URL is the raw string from the
/// manifest and still needs normalization (see `crate::url`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub url: String,
    /// Target revision. `None` means "use whatever the remote's default
    /// branch is" (i.e. plain `git clone` with no `--branch`).
    pub revision: Option<String>,
    pub path: PathBuf,
    pub remote: String,
    pub groups: Vec<String>,
}

impl Manifest {
    /// Read and parse a manifest from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::ManifestNotFound(path.to_path_buf())
            } else {
                Error::ManifestRead {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })?;
        Self::from_str_at(&text, path)
    }

    /// Parse a manifest from a string. `path` is used only for error
    /// reporting.
    pub fn from_str_at(text: &str, path: &Path) -> Result<Self> {
        let manifest: Manifest = toml::from_str(text).map_err(|source| Error::ManifestParse {
            path: path.to_path_buf(),
            source,
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.version != SUPPORTED_VERSION {
            return Err(Error::UnsupportedVersion {
                found: self.version,
                expected: SUPPORTED_VERSION,
            });
        }

        let mut seen = HashSet::new();
        for repo in &self.repos {
            if repo.name.is_empty() {
                return Err(Error::EmptyRepoName);
            }
            if repo.url.is_empty() {
                return Err(Error::EmptyRepoUrl {
                    name: repo.name.clone(),
                });
            }
            if !seen.insert(repo.name.as_str()) {
                return Err(Error::DuplicateRepoName(repo.name.clone()));
            }
        }
        Ok(())
    }

    /// Apply defaults to every [`RepoSpec`] and normalize URLs, returning
    /// a list of resolved [`Repo`] values.
    pub fn resolve(&self) -> Result<Vec<Repo>> {
        let host = self.effective_host();
        self.repos
            .iter()
            .map(|spec| {
                let url = crate::url::normalize(&spec.url, host, &spec.name)?;
                Ok(Repo {
                    name: spec.name.clone(),
                    url,
                    revision: spec
                        .revision
                        .clone()
                        .or_else(|| self.defaults.revision.clone()),
                    path: spec
                        .path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(&spec.name)),
                    remote: spec
                        .remote
                        .clone()
                        .or_else(|| self.defaults.remote.clone())
                        .unwrap_or_else(|| DEFAULT_REMOTE.to_string()),
                    groups: spec.groups.clone(),
                })
            })
            .collect()
    }

    /// The effective host used to expand `owner/repo` shorthand URLs.
    pub fn effective_host(&self) -> &str {
        self.defaults.host.as_deref().unwrap_or(DEFAULT_HOST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Manifest> {
        Manifest::from_str_at(text, Path::new("workspace.toml"))
    }

    #[test]
    fn minimal_manifest_parses() {
        let m = parse("version = 1\n").unwrap();
        assert_eq!(m.version, 1);
        assert!(m.repos.is_empty());
        assert!(m.defaults.revision.is_none());
    }

    #[test]
    fn full_manifest_parses_and_resolves() {
        let text = r#"
version = 1

[defaults]
revision = "main"
remote   = "upstream"
host     = "gitlab.example.com"

[[repos]]
name     = "frontend"
url      = "acme/frontend"
groups   = ["web"]

[[repos]]
name     = "backend"
url      = "acme/backend"
revision = "v2.3.1"
path     = "services/backend"
remote   = "origin"
"#;
        let m = parse(text).unwrap();
        assert_eq!(m.effective_host(), "gitlab.example.com");

        let repos = m.resolve().unwrap();
        assert_eq!(repos.len(), 2);

        assert_eq!(repos[0].name, "frontend");
        assert_eq!(repos[0].url, "https://gitlab.example.com/acme/frontend.git");
        assert_eq!(repos[0].revision.as_deref(), Some("main"));
        assert_eq!(repos[0].path, PathBuf::from("frontend"));
        assert_eq!(repos[0].remote, "upstream");
        assert_eq!(repos[0].groups, vec!["web".to_string()]);

        assert_eq!(repos[1].revision.as_deref(), Some("v2.3.1"));
        assert_eq!(repos[1].path, PathBuf::from("services/backend"));
        assert_eq!(repos[1].remote, "origin");
    }

    #[test]
    fn missing_revision_with_no_default_is_none() {
        let text = r#"
version = 1
[[repos]]
name = "lib"
url  = "acme/lib"
"#;
        let m = parse(text).unwrap();
        let repos = m.resolve().unwrap();
        assert_eq!(repos[0].revision, None);
    }

    #[test]
    fn host_defaults_to_github_com() {
        let m = parse("version = 1\n").unwrap();
        assert_eq!(m.effective_host(), "github.com");
    }

    #[test]
    fn remote_defaults_to_origin() {
        let text = r#"
version = 1
[[repos]]
name = "lib"
url  = "acme/lib"
"#;
        let m = parse(text).unwrap();
        assert_eq!(m.resolve().unwrap()[0].remote, "origin");
    }

    #[test]
    fn rejects_unsupported_version() {
        let err = parse("version = 2\n").unwrap_err();
        assert!(matches!(
            err,
            Error::UnsupportedVersion {
                found: 2,
                expected: 1
            }
        ));
    }

    #[test]
    fn rejects_duplicate_repo_names() {
        let text = r#"
version = 1
[[repos]]
name = "lib"
url  = "acme/lib"
[[repos]]
name = "lib"
url  = "other/lib"
"#;
        let err = parse(text).unwrap_err();
        assert!(matches!(err, Error::DuplicateRepoName(ref n) if n == "lib"));
    }

    #[test]
    fn rejects_empty_repo_name() {
        let text = r#"
version = 1
[[repos]]
name = ""
url  = "acme/lib"
"#;
        let err = parse(text).unwrap_err();
        assert!(matches!(err, Error::EmptyRepoName));
    }

    #[test]
    fn rejects_empty_repo_url() {
        let text = r#"
version = 1
[[repos]]
name = "lib"
url  = ""
"#;
        let err = parse(text).unwrap_err();
        assert!(matches!(err, Error::EmptyRepoUrl { ref name } if name == "lib"));
    }

    #[test]
    fn rejects_malformed_toml() {
        let err = parse("version = \n").unwrap_err();
        assert!(matches!(err, Error::ManifestParse { .. }));
    }

    #[test]
    fn rejects_missing_version() {
        let text = r#"
[[repos]]
name = "lib"
url  = "acme/lib"
"#;
        let err = parse(text).unwrap_err();
        // serde will reject this as a missing field
        assert!(matches!(err, Error::ManifestParse { .. }));
    }
}

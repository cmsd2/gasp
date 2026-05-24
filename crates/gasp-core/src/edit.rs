//! Mutating edits to `workspace.toml` that preserve comments and
//! formatting via `toml_edit`.

use std::path::Path;

use serde::de::Error as _;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use crate::error::{Error, Result};
use crate::manifest::Manifest;

pub struct AddArgs<'a> {
    pub name: &'a str,
    pub url: &'a str,
    pub revision: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub groups: &'a [String],
}

/// Append a `[[repos]]` block to the manifest file.
pub fn add_repo(manifest_path: &Path, args: &AddArgs<'_>) -> Result<()> {
    let mut doc = load_doc(manifest_path)?;
    let repos = repos_array_mut(&mut doc);

    if repos
        .iter()
        .any(|t| t.get("name").and_then(|v| v.as_str()) == Some(args.name))
    {
        return Err(Error::RepoAlreadyExists(args.name.to_string()));
    }

    let mut t = Table::new();
    t["name"] = value(args.name);
    t["url"] = value(args.url);
    if let Some(rev) = args.revision {
        t["revision"] = value(rev);
    }
    if let Some(p) = args.path {
        t["path"] = value(p.display().to_string());
    }
    if !args.groups.is_empty() {
        let mut arr = Array::new();
        for g in args.groups {
            arr.push(g.as_str());
        }
        t["groups"] = value(arr);
    }
    repos.push(t);

    write_doc(manifest_path, &doc)
}

/// Remove the `[[repos]]` block whose `name` matches.
pub fn remove_repo(manifest_path: &Path, name: &str) -> Result<()> {
    let mut doc = load_doc(manifest_path)?;
    let repos = repos_array_mut(&mut doc);

    let idx = repos
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .ok_or_else(|| Error::RepoNotFound(name.to_string()))?;
    repos.remove(idx);

    write_doc(manifest_path, &doc)
}

fn load_doc(path: &Path) -> Result<DocumentMut> {
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
    text.parse::<DocumentMut>()
        .map_err(|source| Error::ManifestParse {
            path: path.to_path_buf(),
            source: toml::de::Error::custom(source.to_string()),
        })
}

fn repos_array_mut(doc: &mut DocumentMut) -> &mut ArrayOfTables {
    doc.entry("repos")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .expect("repos must be an array of tables")
}

fn write_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    let text = doc.to_string();
    // Validate that what we're about to write is still a parseable
    // manifest. This catches mistakes like leaving the doc in a broken
    // state before they hit disk.
    Manifest::from_str_at(&text, path)?;
    std::fs::write(path, &text).map_err(|source| Error::Io {
        operation: "write manifest".into(),
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_manifest(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn add_appends_repo_to_empty_manifest() {
        let (_d, path) = tmp_manifest("version = 1\n");
        add_repo(
            &path,
            &AddArgs {
                name: "alpha",
                url: "acme/alpha",
                revision: Some("main"),
                path: None,
                groups: &[],
            },
        )
        .unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("[[repos]]"));
        assert!(body.contains("name = \"alpha\""));
        assert!(body.contains("url = \"acme/alpha\""));
        assert!(body.contains("revision = \"main\""));
    }

    #[test]
    fn add_rejects_duplicate_name() {
        let (_d, path) =
            tmp_manifest("version = 1\n\n[[repos]]\nname = \"alpha\"\nurl = \"acme/alpha\"\n");
        let err = add_repo(
            &path,
            &AddArgs {
                name: "alpha",
                url: "other/alpha",
                revision: None,
                path: None,
                groups: &[],
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::RepoAlreadyExists(ref n) if n == "alpha"));
    }

    #[test]
    fn add_preserves_top_level_comments() {
        let original = "# top comment\nversion = 1\n# trailing comment\n";
        let (_d, path) = tmp_manifest(original);
        add_repo(
            &path,
            &AddArgs {
                name: "alpha",
                url: "acme/alpha",
                revision: None,
                path: None,
                groups: &[],
            },
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("# top comment"));
        assert!(body.contains("# trailing comment"));
    }

    #[test]
    fn add_with_path_and_groups() {
        let (_d, path) = tmp_manifest("version = 1\n");
        add_repo(
            &path,
            &AddArgs {
                name: "beta",
                url: "acme/beta",
                revision: None,
                path: Some(Path::new("services/beta")),
                groups: &["api".into(), "web".into()],
            },
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("path = \"services/beta\""));
        assert!(body.contains("groups = [\"api\", \"web\"]"));
    }

    #[test]
    fn remove_deletes_named_repo() {
        let original = r#"version = 1

[[repos]]
name = "alpha"
url = "acme/alpha"

[[repos]]
name = "beta"
url = "acme/beta"
"#;
        let (_d, path) = tmp_manifest(original);
        remove_repo(&path, "alpha").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("\"alpha\""));
        assert!(body.contains("name = \"beta\""));
    }

    #[test]
    fn remove_errors_when_repo_missing() {
        let (_d, path) = tmp_manifest("version = 1\n");
        let err = remove_repo(&path, "ghost").unwrap_err();
        assert!(matches!(err, Error::RepoNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn round_trip_add_then_parse() {
        let (_d, path) = tmp_manifest("version = 1\n");
        add_repo(
            &path,
            &AddArgs {
                name: "alpha",
                url: "acme/alpha",
                revision: Some("main"),
                path: None,
                groups: &[],
            },
        )
        .unwrap();
        let m = Manifest::load(&path).unwrap();
        assert_eq!(m.repos.len(), 1);
        assert_eq!(m.repos[0].name, "alpha");
    }
}

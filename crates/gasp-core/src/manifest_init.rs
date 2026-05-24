//! "Graduate" a Loose-mode workspace into a Cloned-mode workspace by
//! turning `.workspace/workspace.toml` into a local git repository at
//! `.workspace/manifest/`, complete with a templated README. Optionally
//! sets up an `origin` remote and pushes.

use std::path::Path;
use std::process::Command;

use minijinja::{Environment, context};

use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::workspace::{MANIFEST_FILE, ManifestMode, Workspace};

const README_TEMPLATE: &str = r#"# {{ name }} — gasp workspace

This repository defines a [gasp](https://github.com/cmsd2/gasp) workspace
that brings together {{ repos|length }} repositor{% if repos|length == 1 %}y{% else %}ies{% endif %}.

## Usage

```sh
gasp init <this-repo-url>
gasp sync
```

## Repositories

{% if repos|length == 0 -%}
*(none yet — add some with `gasp add` and commit.)*
{% else -%}
| Name | URL | Revision |
| --- | --- | --- |
{% for r in repos -%}
| `{{ r.name }}` | `{{ r.url }}` | {% if r.revision %}`{{ r.revision }}`{% else %}—{% endif %} |
{% endfor -%}
{% endif %}
"#;

pub struct InitOpts<'a> {
    /// Display name for the README template. Defaults to the workspace
    /// root directory name.
    pub name: Option<&'a str>,
    /// Add this URL as the `origin` remote.
    pub remote: Option<&'a str>,
    /// Push after committing. Requires `remote`.
    pub push: bool,
}

#[derive(Debug)]
pub struct InitOutcome {
    pub manifest_repo: std::path::PathBuf,
    pub remote_set: bool,
    pub pushed: bool,
}

pub fn init(workspace: &Workspace, opts: &InitOpts<'_>) -> Result<InitOutcome> {
    if workspace.manifest_mode() == ManifestMode::Cloned {
        return Err(Error::ManifestAlreadyCloned(workspace.manifest_repo_dir()));
    }

    let loose_manifest = workspace.dot_dir().join(MANIFEST_FILE);
    if !loose_manifest.is_file() {
        return Err(Error::ManifestNotFound(loose_manifest));
    }

    // Validate (and resolve) the manifest before we touch anything on
    // disk, so we can render the README from accurate data.
    let manifest = Manifest::load(&loose_manifest)?;
    let repos = manifest.resolve()?;

    let name = match opts.name {
        Some(n) => n.to_string(),
        None => workspace
            .root()
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("workspace")
            .to_string(),
    };

    let readme = render_readme(&name, &repos)?;

    // Move workspace.toml into the new repo dir.
    let manifest_repo = workspace.manifest_repo_dir();
    std::fs::create_dir_all(&manifest_repo).map_err(|source| Error::Io {
        operation: "create .workspace/manifest".into(),
        path: manifest_repo.clone(),
        source,
    })?;
    let new_manifest = manifest_repo.join(MANIFEST_FILE);
    std::fs::rename(&loose_manifest, &new_manifest).map_err(|source| Error::Io {
        operation: "move workspace.toml into .workspace/manifest".into(),
        path: new_manifest.clone(),
        source,
    })?;

    let readme_path = manifest_repo.join("README.md");
    std::fs::write(&readme_path, readme).map_err(|source| Error::Io {
        operation: "write README.md".into(),
        path: readme_path.clone(),
        source,
    })?;

    // Initialize the git repo, commit both files.
    run_git_in(&manifest_repo, &["init", "-q", "-b", "main"])?;
    run_git_in(&manifest_repo, &["add", "workspace.toml", "README.md"])?;
    run_git_in(
        &manifest_repo,
        &["commit", "-q", "-m", "initial gasp manifest"],
    )?;

    let mut remote_set = false;
    let mut pushed = false;
    if let Some(url) = opts.remote {
        run_git_in(&manifest_repo, &["remote", "add", "origin", url])?;
        remote_set = true;
        if opts.push {
            run_git_in(&manifest_repo, &["push", "-q", "-u", "origin", "main"])?;
            pushed = true;
        }
    }

    Ok(InitOutcome {
        manifest_repo,
        remote_set,
        pushed,
    })
}

fn render_readme(name: &str, repos: &[crate::manifest::Repo]) -> Result<String> {
    #[derive(serde::Serialize)]
    struct RepoCtx<'a> {
        name: &'a str,
        url: &'a str,
        revision: Option<&'a str>,
    }
    let ctx_repos: Vec<RepoCtx<'_>> = repos
        .iter()
        .map(|r| RepoCtx {
            name: &r.name,
            url: &r.url,
            revision: r.revision.as_deref(),
        })
        .collect();

    let mut env = Environment::new();
    env.add_template("README", README_TEMPLATE)
        .map_err(|e| Error::Template(e.to_string()))?;
    let tpl = env
        .get_template("README")
        .map_err(|e| Error::Template(e.to_string()))?;
    tpl.render(context! { name, repos => ctx_repos })
        .map_err(|e| Error::Template(e.to_string()))
}

fn run_git_in(repo: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|source| Error::GitSpawn { source })?;
    if !output.status.success() {
        return Err(Error::GitFailed {
            operation: format!("git {}", args.join(" ")),
            target: repo.display().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn make_loose_workspace(dir: &Path, manifest_text: &str) -> Workspace {
        let seed = dir.join("seed.toml");
        std::fs::write(&seed, manifest_text).unwrap();
        Workspace::init(dir, &seed).unwrap()
    }

    #[test]
    fn graduates_loose_workspace_to_cloned() {
        let dir = tempfile::tempdir().unwrap();
        let ws = make_loose_workspace(
            dir.path(),
            "version = 1\n[[repos]]\nname = \"alpha\"\nurl = \"acme/alpha\"\n",
        );

        let outcome = init(
            &ws,
            &InitOpts {
                name: Some("my-workspace"),
                remote: None,
                push: false,
            },
        )
        .unwrap();

        assert!(outcome.manifest_repo.join(".git").exists());
        assert!(outcome.manifest_repo.join("workspace.toml").is_file());
        assert!(outcome.manifest_repo.join("README.md").is_file());
        // Loose manifest is gone.
        assert!(!ws.dot_dir().join("workspace.toml").exists());
        // Mode flipped.
        assert_eq!(ws.manifest_mode(), ManifestMode::Cloned);
        assert!(!outcome.remote_set);
        assert!(!outcome.pushed);

        // README rendered with manifest data.
        let readme = std::fs::read_to_string(outcome.manifest_repo.join("README.md")).unwrap();
        assert!(readme.contains("my-workspace"));
        assert!(readme.contains("alpha"));
        assert!(readme.contains("1 repository"), "{readme}");

        // The committed manifest is what we expect.
        let log = Command::new("git")
            .args([
                "-C",
                outcome.manifest_repo.to_str().unwrap(),
                "log",
                "--oneline",
            ])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout);
        assert!(log.contains("initial gasp manifest"), "{log}");
    }

    #[test]
    fn errors_if_already_cloned() {
        let dir = tempfile::tempdir().unwrap();
        let ws = make_loose_workspace(dir.path(), "version = 1\n");
        init(
            &ws,
            &InitOpts {
                name: None,
                remote: None,
                push: false,
            },
        )
        .unwrap();

        let err = init(
            &ws,
            &InitOpts {
                name: None,
                remote: None,
                push: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::ManifestAlreadyCloned(_)));
    }

    #[test]
    fn defaults_name_to_workspace_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        // Create a named subdirectory so file_name() has something to find.
        let ws_root = dir.path().join("cool-project");
        std::fs::create_dir(&ws_root).unwrap();
        let ws = make_loose_workspace(&ws_root, "version = 1\n");
        let outcome = init(
            &ws,
            &InitOpts {
                name: None,
                remote: None,
                push: false,
            },
        )
        .unwrap();
        let readme = std::fs::read_to_string(outcome.manifest_repo.join("README.md")).unwrap();
        assert!(readme.contains("cool-project"), "{readme}");
    }

    #[test]
    fn sets_origin_remote_when_provided() {
        let dir = tempfile::tempdir().unwrap();
        let ws = make_loose_workspace(dir.path(), "version = 1\n");

        let outcome = init(
            &ws,
            &InitOpts {
                name: None,
                remote: Some("git@example.com:foo/bar.git"),
                push: false,
            },
        )
        .unwrap();
        assert!(outcome.remote_set);
        assert!(!outcome.pushed);

        let remotes = Command::new("git")
            .args([
                "-C",
                outcome.manifest_repo.to_str().unwrap(),
                "remote",
                "-v",
            ])
            .output()
            .unwrap();
        let remotes = String::from_utf8_lossy(&remotes.stdout);
        assert!(remotes.contains("git@example.com:foo/bar.git"), "{remotes}");
    }
}

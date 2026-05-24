//! Read-only inspection of a workspace's repos against its manifest.
//!
//! Used by `gasp status`, and (later) by `gasp sync` as the planning
//! step that decides what action — if any — each repo needs.

use std::path::PathBuf;

use git2::{Oid, Repository};

use crate::error::{Error, Result};
use crate::git;
use crate::manifest::Repo;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    pub name: String,
    /// Path as written in the manifest (may be relative).
    pub path: PathBuf,
    pub state: RepoState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoState {
    /// The repo's path does not exist on disk.
    Missing,
    /// Path exists but isn't a git repository.
    NotARepo,
    /// Repository present; carries details and a comparison to the target.
    Present(RepoInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub head: String,
    /// Current branch, or `None` if HEAD is detached.
    pub branch: Option<String>,
    pub dirty: bool,
    pub target: TargetState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetState {
    /// Manifest didn't specify a revision.
    Unspecified,
    /// Manifest specified a revision but we couldn't resolve it locally
    /// (e.g. branch not yet fetched, tag not present).
    Unresolved { revision: String },
    /// Target resolved; carries a graph comparison with HEAD.
    Resolved {
        revision: String,
        sha: String,
        comparison: HeadCompare,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadCompare {
    OnTarget,
    Ahead {
        commits: usize,
    },
    Behind {
        commits: usize,
    },
    Diverged {
        ahead: usize,
        behind: usize,
    },
    /// Couldn't compute graph distances (e.g. shallow clone, partial history).
    Unknown,
}

/// Inspect a single repo against the workspace and produce its status.
pub fn inspect(workspace: &Workspace, repo: &Repo) -> Result<RepoStatus> {
    let abs = workspace.repo_path(&repo.path);

    let state = if !abs.exists() {
        RepoState::Missing
    } else if !git::local::is_repo(&abs) {
        RepoState::NotARepo
    } else {
        let head = git::local::head_sha(&abs)?;
        let branch = git::local::current_branch(&abs)?;
        let dirty = git::local::is_dirty(&abs)?;
        let target = build_target_state(&abs, repo, &head)?;
        RepoState::Present(RepoInfo {
            head,
            branch,
            dirty,
            target,
        })
    };

    Ok(RepoStatus {
        name: repo.name.clone(),
        path: repo.path.clone(),
        state,
    })
}

fn build_target_state(repo_path: &std::path::Path, repo: &Repo, head: &str) -> Result<TargetState> {
    let Some(revision) = &repo.revision else {
        return Ok(TargetState::Unspecified);
    };
    let Some(target_sha) = git::local::resolve_revision(repo_path, revision, &repo.remote)? else {
        return Ok(TargetState::Unresolved {
            revision: revision.clone(),
        });
    };
    let comparison = compare(repo_path, head, &target_sha)?;
    Ok(TargetState::Resolved {
        revision: revision.clone(),
        sha: target_sha,
        comparison,
    })
}

fn compare(repo_path: &std::path::Path, head: &str, target: &str) -> Result<HeadCompare> {
    if head == target {
        return Ok(HeadCompare::OnTarget);
    }
    let repo = Repository::open(repo_path).map_err(|source| Error::LibGit {
        operation: "open".into(),
        path: repo_path.to_path_buf(),
        source,
    })?;
    let head_oid = parse_oid(head, repo_path)?;
    let target_oid = parse_oid(target, repo_path)?;
    match repo.graph_ahead_behind(head_oid, target_oid) {
        Ok((ahead, behind)) => Ok(match (ahead, behind) {
            (0, 0) => HeadCompare::OnTarget,
            (a, 0) => HeadCompare::Ahead { commits: a },
            (0, b) => HeadCompare::Behind { commits: b },
            (a, b) => HeadCompare::Diverged {
                ahead: a,
                behind: b,
            },
        }),
        Err(_) => Ok(HeadCompare::Unknown),
    }
}

fn parse_oid(s: &str, repo_path: &std::path::Path) -> Result<Oid> {
    Oid::from_str(s).map_err(|source| Error::LibGit {
        operation: format!("parse oid '{s}'"),
        path: repo_path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::path::Path;
    use std::process::Command;

    fn run(cwd: &Path, args: &[&str]) {
        let s = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap();
        assert!(s.success(), "git {args:?} failed");
    }

    /// Set up a workspace whose single repo lives at `<root>/r`, sourced
    /// from a bare repo at `<root>/r.git`.
    struct Fix {
        _root: tempfile::TempDir,
        workspace: Workspace,
        bare: PathBuf,
        repo: PathBuf,
    }

    fn fix() -> Fix {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        let bare = root.path().join("r.git");
        std::fs::create_dir_all(&src).unwrap();

        run(&src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("f"), "1\n").unwrap();
        run(&src, &["add", "-A"]);
        run(
            &src,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "c1",
            ],
        );
        run(
            root.path(),
            &[
                "clone",
                "--bare",
                "-q",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );

        let ws_root = root.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let manifest_src = ws_root.join("seed.toml");
        std::fs::write(
            &manifest_src,
            format!(
                "version = 1\n[[repos]]\nname = \"r\"\nurl = \"{}\"\nrevision = \"main\"\n",
                bare.display()
            ),
        )
        .unwrap();
        let workspace = Workspace::init(&ws_root, &manifest_src).unwrap();

        // Clone to populate the repo at <ws_root>/r
        let repo = ws_root.join("r");
        run(
            &ws_root,
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );

        Fix {
            _root: root,
            workspace,
            bare,
            repo,
        }
    }

    fn manifest(f: &Fix) -> Manifest {
        f.workspace.load_manifest().unwrap()
    }

    fn first_repo(m: &Manifest) -> Repo {
        m.resolve().unwrap().into_iter().next().unwrap()
    }

    #[test]
    fn missing_repo_state() {
        let f = fix();
        std::fs::remove_dir_all(&f.repo).unwrap();
        let s = inspect(&f.workspace, &first_repo(&manifest(&f))).unwrap();
        assert_eq!(s.state, RepoState::Missing);
    }

    #[test]
    fn not_a_repo_state() {
        let f = fix();
        std::fs::remove_dir_all(&f.repo).unwrap();
        std::fs::create_dir(&f.repo).unwrap();
        let s = inspect(&f.workspace, &first_repo(&manifest(&f))).unwrap();
        assert_eq!(s.state, RepoState::NotARepo);
    }

    #[test]
    fn clean_on_target() {
        let f = fix();
        let s = inspect(&f.workspace, &first_repo(&manifest(&f))).unwrap();
        let RepoState::Present(info) = s.state else {
            panic!("expected Present");
        };
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert!(!info.dirty);
        match info.target {
            TargetState::Resolved { comparison, .. } => {
                assert_eq!(comparison, HeadCompare::OnTarget);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn dirty_working_tree() {
        let f = fix();
        std::fs::write(f.repo.join("f"), "modified\n").unwrap();
        let s = inspect(&f.workspace, &first_repo(&manifest(&f))).unwrap();
        let RepoState::Present(info) = s.state else {
            panic!("expected Present");
        };
        assert!(info.dirty);
    }

    #[test]
    fn behind_target_after_remote_advances() {
        let f = fix();
        // Add a commit upstream and refresh origin in the local clone.
        let src = f._root.path().join("src");
        std::fs::write(src.join("f"), "2\n").unwrap();
        run(&src, &["add", "-A"]);
        run(
            &src,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "c2",
            ],
        );
        // Push the new commit into the bare, then fetch in the workspace clone.
        run(&src, &["push", "-q", f.bare.to_str().unwrap(), "main"]);
        run(&f.repo, &["fetch", "-q", "origin"]);

        let s = inspect(&f.workspace, &first_repo(&manifest(&f))).unwrap();
        let RepoState::Present(info) = s.state else {
            panic!();
        };
        match info.target {
            TargetState::Resolved {
                comparison: HeadCompare::Behind { commits },
                ..
            } => {
                assert_eq!(commits, 1);
            }
            other => panic!("expected Behind, got {other:?}"),
        }
    }

    #[test]
    fn ahead_of_target() {
        let f = fix();
        std::fs::write(f.repo.join("f"), "local\n").unwrap();
        run(&f.repo, &["add", "-A"]);
        run(
            &f.repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "local",
            ],
        );

        let s = inspect(&f.workspace, &first_repo(&manifest(&f))).unwrap();
        let RepoState::Present(info) = s.state else {
            panic!();
        };
        match info.target {
            TargetState::Resolved {
                comparison: HeadCompare::Ahead { commits },
                ..
            } => {
                assert_eq!(commits, 1);
            }
            other => panic!("expected Ahead, got {other:?}"),
        }
    }

    #[test]
    fn detached_head_off_target_is_diverged_or_unknown() {
        let f = fix();
        // Create a new commit on a side branch, then detach there.
        run(&f.repo, &["checkout", "-q", "-b", "side"]);
        std::fs::write(f.repo.join("f"), "side\n").unwrap();
        run(&f.repo, &["add", "-A"]);
        run(
            &f.repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "side",
            ],
        );
        let sha = git::local::head_sha(&f.repo).unwrap();
        run(&f.repo, &["checkout", "-q", "--detach", &sha]);

        let s = inspect(&f.workspace, &first_repo(&manifest(&f))).unwrap();
        let RepoState::Present(info) = s.state else {
            panic!();
        };
        assert_eq!(info.branch, None);
        match info.target {
            TargetState::Resolved {
                comparison: HeadCompare::Ahead { .. },
                ..
            } => {}
            other => panic!("expected Ahead, got {other:?}"),
        }
    }

    #[test]
    fn unresolved_target() {
        let f = fix();
        // Build a manifest whose revision points at a tag/branch that
        // doesn't exist in this workspace.
        let m_text = format!(
            "version = 1\n[[repos]]\nname = \"r\"\nurl = \"{}\"\nrevision = \"does-not-exist\"\n",
            f.bare.display()
        );
        let m = Manifest::from_str_at(&m_text, Path::new("x.toml")).unwrap();
        let repo = m.resolve().unwrap().into_iter().next().unwrap();

        let s = inspect(&f.workspace, &repo).unwrap();
        let RepoState::Present(info) = s.state else {
            panic!();
        };
        assert!(matches!(info.target, TargetState::Unresolved { .. }));
    }
}

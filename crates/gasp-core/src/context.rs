//! Cross-repo agent-context aggregation.
//!
//! Given a workspace whose `workspace.toml` opts in via a `[context]`
//! section, `sync` walks each child repo for matching instruction files,
//! renders them through a jinja template into a single workspace-root
//! file, and symlinks matching skill files into a workspace-local
//! directory. Everything stays inside the workspace — `~/.claude/` is
//! never touched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use minijinja::{Environment, context};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::manifest::{ContextConfig, Repo};
use crate::workspace::Workspace;

const DEFAULT_TEMPLATE: &str = include_str!("../templates/context.md.j2");

/// HTML-comment markers that fence the gasp-managed section of the
/// output file. Markdown renderers treat them as comments, so they're
/// invisible in rendered output but easy to find in source. The user
/// can put anything they want above the BEGIN or below the END marker;
/// `gasp context sync` only touches the bytes between them.
const BEGIN_MARKER: &str = "<!-- BEGIN gasp:context — do not edit -->";
const END_MARKER: &str = "<!-- END gasp:context -->";

/// Summary of a context-sync run; primarily for CLI display.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub output_path: PathBuf,
    pub instructions_files: usize,
    pub repos_contributing: usize,
    pub skills_dir: PathBuf,
    pub skills_linked: usize,
    pub skills_relinked: usize,
}

/// Run a full context sync. No-op when the workspace has no `[context]`
/// section (returns `Ok(None)`).
pub fn sync(workspace: &Workspace) -> Result<Option<SyncReport>> {
    let manifest = workspace.load_manifest()?;
    let Some(cfg) = manifest.context.as_ref() else {
        return Ok(None);
    };
    let repos = manifest.resolve()?;

    let mut report = SyncReport {
        output_path: workspace.root().join(cfg.output_or_default()),
        skills_dir: workspace.root().join(cfg.skills_dir_or_default()),
        ..Default::default()
    };

    let collected = collect(workspace, &repos, cfg)?;
    report.instructions_files = collected.repos.iter().map(|r| r.instructions.len()).sum();
    report.repos_contributing = collected
        .repos
        .iter()
        .filter(|r| !r.instructions.is_empty())
        .count();
    report.skills_linked = collected.skills.len();

    write_instructions(workspace, cfg, &collected, &report.output_path)?;
    report.skills_relinked = link_skills(&collected.skills, &report.skills_dir)?;

    Ok(Some(report))
}

#[derive(Serialize)]
struct CollectedRepo {
    name: String,
    url: String,
    path: String,
    kind: Option<String>,
    instructions: Vec<CollectedInstruction>,
}

#[derive(Serialize)]
struct RepoGroup<'a> {
    kind: String,
    repos: Vec<&'a CollectedRepo>,
}

#[derive(Serialize)]
struct CollectedInstruction {
    /// Path relative to the repo root.
    path: String,
    content: String,
}

#[derive(Debug)]
struct CollectedSkill {
    name: String,
    source: PathBuf,
}

struct Collected {
    repos: Vec<CollectedRepo>,
    skills: Vec<CollectedSkill>,
}

fn collect(workspace: &Workspace, repos: &[Repo], cfg: &ContextConfig) -> Result<Collected> {
    let default_include = cfg.include_or_default();
    let default_skills_include = cfg.skills_include_or_default();

    let mut out_repos = Vec::with_capacity(repos.len());
    let mut out_skills: Vec<CollectedSkill> = Vec::new();
    let mut skill_index: BTreeMap<String, PathBuf> = BTreeMap::new();

    for repo in repos {
        let repo_abs = workspace.repo_path(&repo.path);
        if !repo_abs.is_dir() {
            // Missing or not-yet-cloned repo — silently skip. `gasp
            // sync` will have already complained.
            continue;
        }

        // Per-repo overrides; `Some("")` opts the repo out entirely.
        let include = repo.context_include.as_deref().unwrap_or(default_include);
        let skills_include = repo
            .context_skills_include
            .as_deref()
            .unwrap_or(default_skills_include);

        let instructions = if include.is_empty() {
            Vec::new()
        } else {
            find_matches(&repo_abs, include)
                .into_iter()
                .map(|abs| {
                    let rel = abs
                        .strip_prefix(&repo_abs)
                        .unwrap_or(&abs)
                        .display()
                        .to_string();
                    let content = std::fs::read_to_string(&abs).map_err(|source| Error::Io {
                        operation: "read instructions file".into(),
                        path: abs.clone(),
                        source,
                    })?;
                    Ok::<_, Error>(CollectedInstruction { path: rel, content })
                })
                .collect::<Result<Vec<_>>>()?
        };

        let skill_matches = if skills_include.is_empty() {
            Vec::new()
        } else {
            find_matches(&repo_abs, skills_include)
        };
        for skill_abs in skill_matches {
            let name = skill_abs
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            if let Some(prev) = skill_index.get(&name) {
                return Err(Error::ContextSkillCollision {
                    name,
                    first: prev.clone(),
                    second: skill_abs,
                });
            }
            skill_index.insert(name.clone(), skill_abs.clone());
            out_skills.push(CollectedSkill {
                name,
                source: skill_abs,
            });
        }

        out_repos.push(CollectedRepo {
            name: repo.name.clone(),
            url: repo.url.clone(),
            path: repo.path.display().to_string(),
            kind: repo.kind.clone(),
            instructions,
        });
    }

    out_skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Collected {
        repos: out_repos,
        skills: out_skills,
    })
}

/// Resolve a glob pattern relative to a repo root and return matching
/// files (directories filtered out). Silently returns empty on a bad
/// pattern.
fn find_matches(repo_root: &Path, pattern: &str) -> Vec<PathBuf> {
    let abs = repo_root.join(pattern);
    let s = abs.to_string_lossy();
    match glob::glob(&s) {
        Ok(it) => it
            .filter_map(std::result::Result::ok)
            .filter(|p| p.is_file())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn write_instructions(
    workspace: &Workspace,
    cfg: &ContextConfig,
    collected: &Collected,
    output_path: &Path,
) -> Result<()> {
    let template_src: String = match cfg.template.as_deref() {
        Some(rel) => {
            let abs = workspace.root().join(rel);
            std::fs::read_to_string(&abs).map_err(|source| Error::Io {
                operation: "read context template".into(),
                path: abs,
                source,
            })?
        }
        None => DEFAULT_TEMPLATE.to_string(),
    };

    let workspace_name = workspace
        .root()
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_string();

    let skill_names: Vec<&str> = collected.skills.iter().map(|s| s.name.as_str()).collect();

    // Group repos by kind (defaulting None → "code") so the template
    // doesn't have to do grouping itself.
    let mut by_kind: BTreeMap<String, Vec<&CollectedRepo>> = BTreeMap::new();
    for r in &collected.repos {
        let k = r.kind.clone().unwrap_or_else(|| "code".to_string());
        by_kind.entry(k).or_default().push(r);
    }
    let groups: Vec<RepoGroup<'_>> = by_kind
        .into_iter()
        .map(|(kind, repos)| RepoGroup { kind, repos })
        .collect();

    let mut env = Environment::new();
    env.add_template("context", &template_src)
        .map_err(|e| Error::Template(e.to_string()))?;
    let tpl = env
        .get_template("context")
        .map_err(|e| Error::Template(e.to_string()))?;

    let rendered = tpl
        .render(context! {
            workspace_name => workspace_name,
            output => cfg.output_or_default().display().to_string(),
            skills_dir => cfg.skills_dir_or_default().display().to_string(),
            skills => skill_names,
            repos => &collected.repos,
            groups => groups,
        })
        .map_err(|e| Error::Template(e.to_string()))?;

    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            operation: "create output parent directory".into(),
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let final_text = splice_into_existing(output_path, &rendered)?;
    std::fs::write(output_path, final_text).map_err(|source| Error::Io {
        operation: "write context output".into(),
        path: output_path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Splice freshly-rendered content into the existing output file
/// between BEGIN/END markers, preserving anything above the BEGIN or
/// below the END. If the file doesn't exist (or doesn't have a marker
/// pair), the result is just the markers wrapping the new content,
/// appended to any pre-existing text.
fn splice_into_existing(output_path: &Path, rendered: &str) -> Result<String> {
    let managed = format!(
        "{begin}\n{body}\n{end}\n",
        begin = BEGIN_MARKER,
        body = rendered.trim_end(),
        end = END_MARKER,
    );

    let existing = match std::fs::read_to_string(output_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(managed),
        Err(source) => {
            return Err(Error::Io {
                operation: "read existing context output".into(),
                path: output_path.to_path_buf(),
                source,
            });
        }
    };

    let (Some(begin), Some(end)) = (existing.find(BEGIN_MARKER), existing.find(END_MARKER)) else {
        // No marker pair. Append a managed block to whatever's already
        // there, separated by a blank line.
        let sep = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return Ok(format!("{existing}{sep}\n{managed}"));
    };

    if end < begin {
        // Markers out of order; treat as malformed and append.
        return Ok(format!("{existing}\n\n{managed}"));
    }

    let end_inclusive = end + END_MARKER.len();
    let mut after = &existing[end_inclusive..];
    // Eat exactly one trailing newline so we don't accumulate blanks.
    if let Some(rest) = after.strip_prefix('\n') {
        after = rest;
    }
    Ok(format!(
        "{before}{managed}{after}",
        before = &existing[..begin]
    ))
}

/// Symlink each collected skill into `skills_dir`. Existing symlinks
/// at the target paths are replaced (so the directory stays in sync
/// across re-runs). Non-symlink files at target paths are left alone
/// to avoid clobbering user data — collision is reported via
/// [`Error::ContextSkillCollision`] earlier in collect().
fn link_skills(skills: &[CollectedSkill], skills_dir: &Path) -> Result<usize> {
    if skills.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(skills_dir).map_err(|source| Error::Io {
        operation: "create skills directory".into(),
        path: skills_dir.to_path_buf(),
        source,
    })?;

    let mut relinked = 0usize;
    for s in skills {
        let target = skills_dir.join(&s.name);

        // If the target already exists and IS a symlink, remove it so
        // we can overwrite. If it exists and ISN'T a symlink, leave it
        // — user-owned data.
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                std::fs::remove_file(&target).map_err(|source| Error::Io {
                    operation: "remove existing skill symlink".into(),
                    path: target.clone(),
                    source,
                })?;
                relinked += 1;
            }
            Ok(_) => continue, // not a symlink — don't touch
            Err(_) => {}       // doesn't exist
        }

        symlink(&s.source, &target).map_err(|source| Error::Io {
            operation: "create skill symlink".into(),
            path: target.clone(),
            source,
        })?;
    }
    Ok(relinked)
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn fixture_workspace_with(manifest_text: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let seed = dir.path().join("seed.toml");
        std::fs::write(&seed, manifest_text).unwrap();
        let ws = Workspace::init(dir.path(), &seed).unwrap();
        (dir, ws)
    }

    fn populate_repo(ws_root: &Path, repo_name: &str, files: &[(&str, &str)]) {
        let repo_dir = ws_root.join(repo_name);
        for (rel, body) in files {
            let p = repo_dir.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, body).unwrap();
        }
    }

    #[test]
    fn no_context_section_is_noop() {
        let (_d, ws) = fixture_workspace_with("version = 1\n");
        let report = sync(&ws).unwrap();
        assert!(report.is_none());
    }

    #[test]
    fn aggregates_instructions_and_links_skills() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1

[context]

[[repos]]
name = "backend"
url  = "acme/backend"
kind = "code"

[[repos]]
name = "tools"
url  = "acme/tools"
kind = "skills"
"#,
        );
        populate_repo(
            ws.root(),
            "backend",
            &[
                ("CLAUDE.md", "# Backend\n\nUse pytest for tests.\n"),
                (".claude/skills/lint.md", "# lint skill\n"),
            ],
        );
        populate_repo(
            ws.root(),
            "tools",
            &[
                ("CLAUDE.md", "# Tools\n\nVendored helper scripts.\n"),
                (".claude/skills/format.md", "# format skill\n"),
            ],
        );

        let report = sync(&ws).unwrap().unwrap();
        assert_eq!(report.instructions_files, 2);
        assert_eq!(report.repos_contributing, 2);
        assert_eq!(report.skills_linked, 2);

        let claude = std::fs::read_to_string(ws.root().join("CLAUDE.md")).unwrap();
        assert!(claude.contains("Backend"));
        assert!(claude.contains("Use pytest for tests"));
        assert!(claude.contains("Tools"));
        assert!(claude.contains("`lint.md`"));
        assert!(claude.contains("`format.md`"));

        // Skills are symlinks pointing back at source files.
        let lint_link = ws.root().join(".claude/skills/lint.md");
        let meta = std::fs::symlink_metadata(&lint_link).unwrap();
        assert!(meta.file_type().is_symlink());
        let resolved = std::fs::read_to_string(&lint_link).unwrap();
        assert_eq!(resolved, "# lint skill\n");
    }

    #[test]
    fn preserves_user_content_outside_markers() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
[[repos]]
name = "a"
url  = "acme/a"
"#,
        );
        populate_repo(ws.root(), "a", &[("CLAUDE.md", "# A\n")]);

        // Pre-populate the output file with custom content that should
        // survive the sync.
        std::fs::write(
            ws.root().join("CLAUDE.md"),
            "# My handwritten preamble\n\nKeep this around.\n",
        )
        .unwrap();

        sync(&ws).unwrap();

        let body = std::fs::read_to_string(ws.root().join("CLAUDE.md")).unwrap();
        assert!(body.contains("My handwritten preamble"));
        assert!(body.contains("Keep this around"));
        assert!(body.contains("BEGIN gasp:context"));
        assert!(body.contains("END gasp:context"));
        assert!(body.contains("# A"));
    }

    #[test]
    fn second_run_replaces_only_managed_block() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
[[repos]]
name = "a"
url  = "acme/a"
"#,
        );
        populate_repo(ws.root(), "a", &[("CLAUDE.md", "version one\n")]);
        sync(&ws).unwrap();

        // Add user content above and below the managed block.
        let body = std::fs::read_to_string(ws.root().join("CLAUDE.md")).unwrap();
        let injected = format!("# Top\n\n{body}\n# Bottom\n");
        std::fs::write(ws.root().join("CLAUDE.md"), &injected).unwrap();

        // Change the source content.
        std::fs::write(ws.root().join("a/CLAUDE.md"), "version two\n").unwrap();
        sync(&ws).unwrap();

        let final_body = std::fs::read_to_string(ws.root().join("CLAUDE.md")).unwrap();
        assert!(final_body.starts_with("# Top\n"));
        assert!(final_body.contains("version two"));
        assert!(!final_body.contains("version one"));
        assert!(final_body.contains("# Bottom"));
        // Only one BEGIN marker (no duplicate managed blocks).
        assert_eq!(final_body.matches("BEGIN gasp:context").count(), 1);
    }

    #[test]
    fn rerun_replaces_stale_symlinks() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
[[repos]]
name = "tools"
url  = "acme/tools"
"#,
        );
        populate_repo(ws.root(), "tools", &[(".claude/skills/x.md", "v1\n")]);
        sync(&ws).unwrap();

        // Replace skill source content and re-run.
        std::fs::write(ws.root().join("tools/.claude/skills/x.md"), "v2\n").unwrap();
        sync(&ws).unwrap();

        let resolved = std::fs::read_to_string(ws.root().join(".claude/skills/x.md")).unwrap();
        assert_eq!(resolved, "v2\n");
    }

    #[test]
    fn collision_in_skill_names_is_an_error() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
[[repos]]
name = "a"
url  = "acme/a"
[[repos]]
name = "b"
url  = "acme/b"
"#,
        );
        populate_repo(ws.root(), "a", &[(".claude/skills/lint.md", "from a\n")]);
        populate_repo(ws.root(), "b", &[(".claude/skills/lint.md", "from b\n")]);

        let err = sync(&ws).unwrap_err();
        assert!(matches!(err, Error::ContextSkillCollision { ref name, .. } if name == "lint.md"));
    }

    #[test]
    fn custom_template_path_is_used() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
template = "tpl.j2"
[[repos]]
name = "a"
url  = "acme/a"
"#,
        );
        std::fs::write(
            ws.root().join("tpl.j2"),
            "MARKER {{ workspace_name }} repos:{{ repos|length }}\n",
        )
        .unwrap();
        populate_repo(ws.root(), "a", &[("CLAUDE.md", "ignored\n")]);

        sync(&ws).unwrap();
        let out = std::fs::read_to_string(ws.root().join("CLAUDE.md")).unwrap();
        // The marker pair wraps the rendered content; the custom
        // template appears inside.
        assert!(out.contains("BEGIN gasp:context"));
        assert!(out.contains("MARKER"));
        assert!(out.contains("repos:1"));
    }

    #[test]
    fn per_repo_overrides_change_includes() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
[[repos]]
name = "a"
url  = "acme/a"
# uses defaults: CLAUDE.md
[[repos]]
name = "b"
url  = "acme/b"
context_include = "docs/AGENTS.md"        # different file per-repo
context_skills_include = ""                # opt out of skills
"#,
        );
        populate_repo(
            ws.root(),
            "a",
            &[
                ("CLAUDE.md", "from a\n"),
                (".claude/skills/a-skill.md", "a skill\n"),
            ],
        );
        populate_repo(
            ws.root(),
            "b",
            &[
                ("CLAUDE.md", "should be ignored\n"),
                ("docs/AGENTS.md", "from b custom\n"),
                (".claude/skills/b-skill.md", "ignored skill\n"),
            ],
        );

        let report = sync(&ws).unwrap().unwrap();
        // a contributes CLAUDE.md + a-skill; b contributes docs/AGENTS.md, no skills
        assert_eq!(report.instructions_files, 2);
        assert_eq!(report.skills_linked, 1);

        let body = std::fs::read_to_string(ws.root().join("CLAUDE.md")).unwrap();
        assert!(body.contains("from a"));
        assert!(body.contains("from b custom"));
        assert!(!body.contains("should be ignored"));
        assert!(!ws.root().join(".claude/skills/b-skill.md").exists());
        assert!(ws.root().join(".claude/skills/a-skill.md").exists());
    }

    #[test]
    fn missing_repos_are_skipped_silently() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
[[repos]]
name = "ghost"
url  = "acme/ghost"
"#,
        );
        // No "ghost" directory created.
        let report = sync(&ws).unwrap().unwrap();
        assert_eq!(report.instructions_files, 0);
        assert_eq!(report.repos_contributing, 0);
    }

    #[test]
    fn custom_output_and_skills_dir_paths_honored() {
        let (_d, ws) = fixture_workspace_with(
            r#"
version = 1
[context]
output = "docs/AGENTS.md"
skills_dir = "agent-skills"
[[repos]]
name = "a"
url  = "acme/a"
"#,
        );
        populate_repo(
            ws.root(),
            "a",
            &[("CLAUDE.md", "# A\n"), (".claude/skills/x.md", "x\n")],
        );
        let report = sync(&ws).unwrap().unwrap();
        assert!(report.output_path.ends_with("docs/AGENTS.md"));
        assert!(ws.root().join("docs/AGENTS.md").is_file());
        assert!(ws.root().join("agent-skills/x.md").exists());
    }

    // Round-trip sanity: a freshly-parsed Manifest exposes context.
    #[test]
    fn context_config_threads_through_resolve() {
        let m = Manifest::from_str_at(
            r#"
version = 1
[context]
output = "X.md"
"#,
            Path::new("workspace.toml"),
        )
        .unwrap();
        assert!(m.context.is_some());
    }
}

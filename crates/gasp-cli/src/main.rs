use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gasp_core::status::{HeadCompare, RepoState, TargetState};
use gasp_core::sync::{Action, ConflictMode};
use gasp_core::{Workspace, status, sync};

mod table;
use table::Table;

/// CLI-side parallel to [`ConflictMode`] so we can derive `ValueEnum`
/// without taking a clap dependency in `gasp-core`.
#[derive(Clone, Copy, ValueEnum)]
enum CliConflictMode {
    /// Skip repos that can't be fast-forwarded (default).
    Refuse,
    /// Rebase local commits onto the target.
    Rebase,
    /// Hard-reset to the target. Destructive.
    Reset,
}

impl From<CliConflictMode> for ConflictMode {
    fn from(m: CliConflictMode) -> Self {
        match m {
            CliConflictMode::Refuse => ConflictMode::Refuse,
            CliConflictMode::Rebase => ConflictMode::Rebase,
            CliConflictMode::Reset => ConflictMode::Reset,
        }
    }
}

#[derive(Parser)]
#[command(name = "gasp", version, about = "Multi-repo workspace manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a skeleton workspace.toml ready to be filled in.
    New {
        /// Where to write the manifest. Defaults to ./workspace.toml.
        path: Option<PathBuf>,
    },

    /// Initialize a workspace from a manifest file.
    Init {
        /// Path to the workspace.toml manifest to use.
        manifest: PathBuf,
    },

    /// Clone missing repos and update existing ones to match the manifest.
    Sync {
        /// Behavior when an existing repo can't be fast-forwarded.
        #[arg(
            long = "on-conflict",
            value_enum,
            default_value_t = CliConflictMode::Refuse,
            value_name = "MODE",
        )]
        on_conflict: CliConflictMode,
        /// Restrict to repos in the given group(s).
        #[arg(long = "group", value_name = "GROUP")]
        groups: Vec<String>,
    },

    /// Show per-repo state vs the manifest.
    Status {
        /// Exit non-zero if any repo is missing, dirty, or off-target.
        /// Useful in CI.
        #[arg(long)]
        strict: bool,
    },

    /// List repos in the manifest.
    List,

    /// Run a shell command in every repo.
    Foreach {
        /// Command and arguments to run.
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    /// Write a new manifest pinning the current resolved shas.
    Freeze {
        /// Output path. Defaults to workspace.frozen.toml in the workspace root.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },

    /// Add a repo to the manifest.
    Add {
        /// Logical name for the repo.
        name: String,
        /// URL (owner/repo shorthand or full git URL).
        url: String,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long = "group", value_name = "GROUP")]
        groups: Vec<String>,
    },

    /// Remove a repo from the manifest.
    Remove {
        /// Logical name of the repo to remove.
        name: String,
    },

    /// Check that the local environment can reach the manifest's hosts.
    Doctor,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::New { path } => cmd_new(path.as_deref()).map(|()| ExitCode::SUCCESS),
        Command::Init { manifest } => cmd_init(&manifest).map(|()| ExitCode::SUCCESS),
        Command::List => cmd_list().map(|()| ExitCode::SUCCESS),
        Command::Sync {
            on_conflict,
            groups,
        } => cmd_sync(&groups, on_conflict.into()),
        Command::Status { strict } => cmd_status(strict),
        Command::Foreach { .. } => not_implemented("foreach"),
        Command::Freeze { .. } => not_implemented("freeze"),
        Command::Add { .. } => not_implemented("add"),
        Command::Remove { .. } => not_implemented("remove"),
        Command::Doctor => not_implemented("doctor"),
    }
}

const SKELETON: &str = r#"# gasp workspace manifest.
# See https://github.com/cmsd2/gasp for documentation.

version = 1

# Defaults applied to every repo unless overridden.
[defaults]
revision = "main"
remote   = "origin"
host     = "github.com"

# Example repo entries. Replace with your own:
#
# [[repos]]
# name     = "frontend"
# url      = "acme/frontend"          # owner/repo shorthand → defaults.host
# revision = "main"                   # branch, tag, or sha
# # path   = "services/frontend"      # default = repo name
# # remote = "origin"                 # default = defaults.remote
# # groups = ["web"]                  # for `gasp sync --group`
#
# [[repos]]
# name = "shared-lib"
# url  = "git@gitlab.example.com:platform/shared.git"
"#;

fn cmd_new(path: Option<&std::path::Path>) -> Result<()> {
    let default = PathBuf::from("workspace.toml");
    let target = path.unwrap_or(&default);

    if target.exists() {
        anyhow::bail!("refusing to overwrite existing file: {}", target.display());
    }
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        anyhow::bail!("parent directory does not exist: {}", parent.display());
    }

    std::fs::write(target, SKELETON).with_context(|| format!("writing {}", target.display()))?;
    println!("Wrote skeleton manifest to {}", target.display());
    println!("Next: edit it, then run `gasp init {}`.", target.display());
    Ok(())
}

fn cmd_init(manifest: &std::path::Path) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::init(&cwd, manifest)?;
    println!("Initialized workspace at {}", ws.root().display());
    Ok(())
}

fn cmd_list() -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;
    let manifest = ws.load_manifest()?;
    let repos = manifest.resolve()?;

    if repos.is_empty() {
        println!("(no repos in manifest)");
        return Ok(());
    }

    let mut t = Table::new(&["NAME", "REVISION", "PATH", "URL"]);
    for r in &repos {
        t.row([
            r.name.clone(),
            r.revision.clone().unwrap_or_else(|| "-".into()),
            r.path.display().to_string(),
            r.url.clone(),
        ]);
    }
    t.print();
    Ok(())
}

fn cmd_status(strict: bool) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;
    let manifest = ws.load_manifest()?;
    let repos = manifest.resolve()?;

    if repos.is_empty() {
        println!("(no repos in manifest)");
        return Ok(ExitCode::SUCCESS);
    }

    let rows: Vec<StatusRow> = repos
        .iter()
        .map(|r| {
            let s = status::inspect(&ws, r)?;
            Ok(StatusRow::from(&s))
        })
        .collect::<Result<_>>()?;

    let mut t = Table::new(&["NAME", "STATE", "HEAD", "BRANCH", "DETAIL"]);
    for r in &rows {
        t.row([
            r.name.clone(),
            r.state.clone(),
            r.head.clone(),
            r.branch.clone(),
            r.detail.clone(),
        ]);
    }
    t.print();

    if strict && rows.iter().any(|r| !r.is_clean) {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

struct StatusRow {
    name: String,
    state: String,
    head: String,
    branch: String,
    detail: String,
    /// True if the repo is in a state `--strict` considers acceptable:
    /// present, clean, and either on target or with no target specified.
    is_clean: bool,
}

impl StatusRow {
    fn from(s: &status::RepoStatus) -> Self {
        let (state, head, branch, detail, is_clean) = match &s.state {
            RepoState::Missing => (
                "missing".to_string(),
                "-".to_string(),
                "-".to_string(),
                "not cloned".to_string(),
                false,
            ),
            RepoState::NotARepo => (
                "not-git".to_string(),
                "-".to_string(),
                "-".to_string(),
                "path exists but is not a git repo".to_string(),
                false,
            ),
            RepoState::Present(info) => {
                let head_short = short_sha(&info.head);
                let branch = info.branch.clone().unwrap_or_else(|| "(detached)".into());
                let (state, detail) = classify(info);
                let is_clean = !info.dirty
                    && matches!(
                        info.target,
                        TargetState::Unspecified
                            | TargetState::Resolved {
                                comparison: HeadCompare::OnTarget,
                                ..
                            }
                    );
                (state, head_short, branch, detail, is_clean)
            }
        };
        StatusRow {
            name: s.name.clone(),
            state,
            head,
            branch,
            detail,
            is_clean,
        }
    }
}

fn classify(info: &gasp_core::status::RepoInfo) -> (String, String) {
    if info.dirty {
        return ("dirty".into(), describe_target_brief(&info.target));
    }
    match &info.target {
        TargetState::Unspecified => ("clean".into(), "no target revision".into()),
        TargetState::Unresolved { revision } => (
            "unknown".into(),
            format!("target {revision} not resolvable locally"),
        ),
        TargetState::Resolved {
            revision,
            sha,
            comparison,
        } => match comparison {
            HeadCompare::OnTarget => ("clean".into(), format!("on target {revision}")),
            HeadCompare::Ahead { commits } => (
                "ahead".into(),
                format!("target {} ({}), ahead {commits}", revision, short_sha(sha)),
            ),
            HeadCompare::Behind { commits } => (
                "behind".into(),
                format!("target {} ({}), behind {commits}", revision, short_sha(sha)),
            ),
            HeadCompare::Diverged { ahead, behind } => (
                "diverged".into(),
                format!(
                    "target {} ({}), ahead {ahead} behind {behind}",
                    revision,
                    short_sha(sha)
                ),
            ),
            HeadCompare::Unknown => (
                "unknown".into(),
                format!("target {} ({}), graph unknown", revision, short_sha(sha)),
            ),
        },
    }
}

fn describe_target_brief(t: &TargetState) -> String {
    match t {
        TargetState::Unspecified => "uncommitted changes".into(),
        TargetState::Unresolved { revision } => {
            format!("uncommitted changes; target {revision} not resolved")
        }
        TargetState::Resolved { revision, .. } => {
            format!("uncommitted changes; target {revision}")
        }
    }
}

fn short_sha(s: &str) -> String {
    s.chars().take(7).collect()
}

fn cmd_sync(group_filter: &[String], mode: ConflictMode) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;
    let manifest = ws.load_manifest()?;
    let mut repos = manifest.resolve()?;

    if !group_filter.is_empty() {
        repos.retain(|r| r.groups.iter().any(|g| group_filter.contains(g)));
    }

    if repos.is_empty() {
        println!("(no repos match)");
        return Ok(ExitCode::SUCCESS);
    }

    let mut counts = SyncCounts::default();
    let mut failed: Vec<(String, gasp_core::Error)> = Vec::new();

    for repo in &repos {
        let dest = ws.repo_path(&repo.path);

        // Fetch first for existing repos so planning sees up-to-date refs.
        if dest.exists()
            && let Err(err) = sync::fetch_remote(&dest, &repo.remote)
        {
            println!("  fetch  {} ... FAILED", repo.name);
            failed.push((repo.name.clone(), err));
            continue;
        }

        let status = match status::inspect(&ws, repo) {
            Ok(s) => s,
            Err(err) => {
                println!("  status {} ... FAILED", repo.name);
                failed.push((repo.name.clone(), err));
                continue;
            }
        };

        let action = sync::plan_action(&status, mode);
        let (label, detail) = describe_action(&action);
        print!("  {label:<6} {} {} ... ", repo.name, detail);
        std::io::Write::flush(&mut std::io::stdout()).ok();

        match sync::execute(&ws, repo, &action) {
            Ok(()) => {
                println!("ok");
                counts.bump(&action);
            }
            Err(err) => {
                println!("FAILED");
                failed.push((repo.name.clone(), err));
            }
        }
    }

    println!();
    println!(
        "Summary: {} cloned, {} updated, {} reset, {} rebased, {} unchanged, {} skipped, {} failed",
        counts.cloned,
        counts.fast_forwarded,
        counts.reset,
        counts.rebased,
        counts.unchanged,
        counts.skipped,
        failed.len(),
    );

    if !failed.is_empty() {
        println!("\nFailures:");
        for (name, err) in &failed {
            println!("  {name}: {err}");
        }
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Default)]
struct SyncCounts {
    cloned: usize,
    fast_forwarded: usize,
    reset: usize,
    rebased: usize,
    unchanged: usize,
    skipped: usize,
}

impl SyncCounts {
    fn bump(&mut self, action: &Action) {
        match action {
            Action::Clone => self.cloned += 1,
            Action::FastForward { .. } => self.fast_forwarded += 1,
            Action::Reset { .. } => self.reset += 1,
            Action::Rebase { .. } => self.rebased += 1,
            Action::AlreadyOnTarget | Action::NoTarget => self.unchanged += 1,
            Action::Skip { .. } => self.skipped += 1,
        }
    }
}

fn describe_action(action: &Action) -> (&'static str, String) {
    match action {
        Action::Clone => ("clone", String::new()),
        Action::AlreadyOnTarget => ("ok", "(on target)".into()),
        Action::NoTarget => ("ok", "(no target)".into()),
        Action::FastForward { to, .. } => ("ff", format!("→ {}", short_sha(to))),
        Action::Reset { to } => ("reset", format!("→ {}", short_sha(to))),
        Action::Rebase { onto } => ("rebase", format!("onto {}", short_sha(onto))),
        Action::Skip { reason } => ("skip", format!("({reason})")),
    }
}

fn not_implemented(cmd: &str) -> Result<ExitCode> {
    eprintln!("error: `{cmd}` is not implemented yet");
    Ok(ExitCode::FAILURE)
}

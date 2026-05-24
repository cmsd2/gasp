use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gasp_core::status::{HeadCompare, RepoState, TargetState};
use gasp_core::{Workspace, git, status};

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
        /// Refuse to modify repos that aren't fast-forwardable (default).
        #[arg(long, conflicts_with_all = ["rebase", "reset"])]
        refuse: bool,
        /// Rebase local commits onto the target revision on conflict.
        #[arg(long, conflicts_with_all = ["refuse", "reset"])]
        rebase: bool,
        /// Hard-reset to the target revision on conflict. Destructive.
        #[arg(long, conflicts_with_all = ["refuse", "rebase"])]
        reset: bool,
        /// Restrict to repos in the given group(s).
        #[arg(long = "group", value_name = "GROUP")]
        groups: Vec<String>,
    },

    /// Show per-repo state vs the manifest.
    Status,

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
        Command::Sync { groups, .. } => cmd_sync(&groups),
        Command::Status => cmd_status(),
        Command::Foreach { .. } => not_implemented("foreach").map(|()| ExitCode::SUCCESS),
        Command::Freeze { .. } => not_implemented("freeze").map(|()| ExitCode::SUCCESS),
        Command::Add { .. } => not_implemented("add").map(|()| ExitCode::SUCCESS),
        Command::Remove { .. } => not_implemented("remove").map(|()| ExitCode::SUCCESS),
        Command::Doctor => not_implemented("doctor").map(|()| ExitCode::SUCCESS),
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

    let name_w = repos.iter().map(|r| r.name.len()).max().unwrap_or(0).max(4);
    let path_w = repos
        .iter()
        .map(|r| r.path.display().to_string().len())
        .max()
        .unwrap_or(0)
        .max(4);
    let rev_w = repos
        .iter()
        .map(|r| r.revision.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0)
        .max(8);

    println!(
        "{:<nw$}  {:<rw$}  {:<pw$}  URL",
        "NAME",
        "REVISION",
        "PATH",
        nw = name_w,
        rw = rev_w,
        pw = path_w,
    );
    for r in &repos {
        println!(
            "{:<nw$}  {:<rw$}  {:<pw$}  {}",
            r.name,
            r.revision.as_deref().unwrap_or("-"),
            r.path.display(),
            r.url,
            nw = name_w,
            rw = rev_w,
            pw = path_w,
        );
    }
    Ok(())
}

fn cmd_status() -> Result<ExitCode> {
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

    let name_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(0).max(4);
    let state_w = rows.iter().map(|r| r.state.len()).max().unwrap_or(0).max(5);
    let head_w = rows.iter().map(|r| r.head.len()).max().unwrap_or(0).max(7);
    let branch_w = rows
        .iter()
        .map(|r| r.branch.len())
        .max()
        .unwrap_or(0)
        .max(6);

    println!(
        "{:<nw$}  {:<sw$}  {:<hw$}  {:<bw$}  DETAIL",
        "NAME",
        "STATE",
        "HEAD",
        "BRANCH",
        nw = name_w,
        sw = state_w,
        hw = head_w,
        bw = branch_w,
    );
    for r in &rows {
        println!(
            "{:<nw$}  {:<sw$}  {:<hw$}  {:<bw$}  {}",
            r.name,
            r.state,
            r.head,
            r.branch,
            r.detail,
            nw = name_w,
            sw = state_w,
            hw = head_w,
            bw = branch_w,
        );
    }
    Ok(ExitCode::SUCCESS)
}

struct StatusRow {
    name: String,
    state: String,
    head: String,
    branch: String,
    detail: String,
}

impl StatusRow {
    fn from(s: &status::RepoStatus) -> Self {
        let (state, head, branch, detail) = match &s.state {
            RepoState::Missing => (
                "missing".to_string(),
                "-".to_string(),
                "-".to_string(),
                "not cloned".to_string(),
            ),
            RepoState::NotARepo => (
                "not-git".to_string(),
                "-".to_string(),
                "-".to_string(),
                "path exists but is not a git repo".to_string(),
            ),
            RepoState::Present(info) => {
                let head_short = short_sha(&info.head);
                let branch = info.branch.clone().unwrap_or_else(|| "(detached)".into());
                let (state, detail) = classify(info);
                (state, head_short, branch, detail)
            }
        };
        StatusRow {
            name: s.name.clone(),
            state,
            head,
            branch,
            detail,
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

fn cmd_sync(group_filter: &[String]) -> Result<ExitCode> {
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

    let mut cloned = 0usize;
    let mut skipped = 0usize;
    let mut failed: Vec<(String, gasp_core::Error)> = Vec::new();

    for repo in &repos {
        let dest = ws.repo_path(&repo.path);
        if dest.exists() {
            println!("  skip   {} (already present)", repo.name);
            skipped += 1;
            continue;
        }

        print!("  clone  {} ... ", repo.name);
        std::io::Write::flush(&mut std::io::stdout()).ok();

        match git::clone(&repo.url, &dest, repo.revision.as_deref()) {
            Ok(()) => {
                println!("ok");
                cloned += 1;
            }
            Err(err) => {
                println!("FAILED");
                failed.push((repo.name.clone(), err));
            }
        }
    }

    println!();
    println!(
        "Summary: {cloned} cloned, {skipped} skipped, {} failed",
        failed.len()
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

fn not_implemented(cmd: &str) -> Result<()> {
    println!("{cmd}: not implemented");
    Ok(())
}

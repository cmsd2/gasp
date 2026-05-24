use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gasp_core::{Workspace, git};

#[derive(Parser)]
#[command(name = "gasp", version, about = "Multi-repo workspace manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
        Command::Init { manifest } => cmd_init(&manifest).map(|()| ExitCode::SUCCESS),
        Command::List => cmd_list().map(|()| ExitCode::SUCCESS),
        Command::Sync { groups, .. } => cmd_sync(&groups),
        Command::Status => not_implemented("status").map(|()| ExitCode::SUCCESS),
        Command::Foreach { .. } => not_implemented("foreach").map(|()| ExitCode::SUCCESS),
        Command::Freeze { .. } => not_implemented("freeze").map(|()| ExitCode::SUCCESS),
        Command::Add { .. } => not_implemented("add").map(|()| ExitCode::SUCCESS),
        Command::Remove { .. } => not_implemented("remove").map(|()| ExitCode::SUCCESS),
        Command::Doctor => not_implemented("doctor").map(|()| ExitCode::SUCCESS),
    }
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

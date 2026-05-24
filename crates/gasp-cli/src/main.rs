use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gasp_core::context;
use gasp_core::edit::AddArgs;
use gasp_core::lock::WorkspaceLock;
use gasp_core::manifest::Repo;
use gasp_core::manifest_init::{self, InitOpts};
use gasp_core::status::{HeadCompare, RepoState, TargetState};
use gasp_core::sync::{Action, ConflictMode};
use gasp_core::workspace::ManifestUpdate;
use gasp_core::{Workspace, edit, freeze, status, sync};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

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
    /// Initialize a workspace from a manifest source.
    ///
    /// The source can be:
    /// * a path to a local workspace.toml,
    /// * a git URL (https://..., ssh://..., git@host:path) to a
    ///   manifest repository, or
    /// * an `owner/repo` GitHub shorthand for a manifest repository.
    Init {
        /// Manifest source: local file path, git URL, or owner/repo shorthand.
        source: String,
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
        /// Number of repos to process in parallel. Defaults to min(8, ncpu).
        #[arg(long, short = 'j', value_name = "N")]
        jobs: Option<usize>,
        /// Skip updating the cloned manifest repo before syncing.
        #[arg(long)]
        no_update_manifest: bool,
        /// Skip running `context sync` after syncing.
        #[arg(long)]
        no_update_context: bool,
    },

    /// Show per-repo state vs the manifest.
    Status {
        /// Exit non-zero if any repo is missing, dirty, or off-target.
        /// Useful in CI.
        #[arg(long)]
        strict: bool,
        /// Also report on the cloned manifest repo (HEAD, branch,
        /// uncommitted changes). No-op in loose-file mode.
        #[arg(long = "show-manifest")]
        show_manifest: bool,
    },

    /// List repos in the manifest.
    List,

    /// Run a shell command in every repo.
    Foreach {
        /// Restrict to repos in the given group(s).
        #[arg(long = "group", value_name = "GROUP")]
        groups: Vec<String>,
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
    ///
    /// If <URL> is omitted and a git repo already exists at the target
    /// path (default: `<name>` under the workspace root), the URL is
    /// inferred from that repo's `origin` remote.
    Add {
        /// Logical name for the repo.
        name: String,
        /// URL (owner/repo shorthand or full git URL). Optional — see
        /// command description for the inference rules.
        url: Option<String>,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long = "group", value_name = "GROUP")]
        groups: Vec<String>,
        /// Freeform classification (e.g. `code`, `skills`, `adrs`,
        /// `data`, `docs`). Used by `gasp context sync` to group repos.
        #[arg(long)]
        kind: Option<String>,
    },

    /// Remove a repo from the manifest.
    Remove {
        /// Logical name of the repo to remove.
        name: String,
    },

    /// Check that the local environment can reach the manifest's hosts.
    Doctor,

    /// Manage the cloned manifest repository.
    Manifest {
        #[command(subcommand)]
        cmd: ManifestCmd,
    },

    /// Manage cross-repo agent context (aggregated instructions and
    /// symlinked skills).
    Context {
        #[command(subcommand)]
        cmd: ContextCmd,
    },
}

#[derive(Subcommand)]
enum ContextCmd {
    /// Aggregate per-repo instructions files into the workspace-root
    /// output, and refresh skill symlinks. No-op if the manifest has
    /// no `[context]` section.
    Sync,
}

#[derive(Subcommand)]
enum ManifestCmd {
    /// Graduate a loose `.workspace/workspace.toml` into a fresh local
    /// git repo at `.workspace/manifest/`, with a templated README.
    Init {
        /// Display name used in the README template. Defaults to the
        /// workspace root directory name.
        #[arg(long)]
        name: Option<String>,
        /// URL to set as the `origin` remote.
        #[arg(long, value_name = "URL")]
        remote: Option<String>,
        /// Push to origin after committing. Requires `--remote`.
        #[arg(long, requires = "remote")]
        push: bool,
    },
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
        Command::Init { source } => cmd_init(&source).map(|()| ExitCode::SUCCESS),
        Command::List => cmd_list().map(|()| ExitCode::SUCCESS),
        Command::Sync {
            on_conflict,
            groups,
            jobs,
            no_update_manifest,
            no_update_context,
        } => cmd_sync(
            &groups,
            on_conflict.into(),
            jobs,
            !no_update_manifest,
            !no_update_context,
        ),
        Command::Status {
            strict,
            show_manifest,
        } => cmd_status(strict, show_manifest),
        Command::Foreach { groups, command } => cmd_foreach(&groups, &command),
        Command::Freeze { output } => cmd_freeze(output.as_deref()).map(|()| ExitCode::SUCCESS),
        Command::Add {
            name,
            url,
            revision,
            path,
            groups,
            kind,
        } => cmd_add(
            &name,
            url.as_deref(),
            revision.as_deref(),
            path.as_deref(),
            &groups,
            kind.as_deref(),
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Remove { name } => cmd_remove(&name).map(|()| ExitCode::SUCCESS),
        Command::Doctor => cmd_doctor(),
        Command::Manifest { cmd } => match cmd {
            ManifestCmd::Init { name, remote, push } => {
                cmd_manifest_init(name.as_deref(), remote.as_deref(), push)
                    .map(|()| ExitCode::SUCCESS)
            }
        },
        Command::Context { cmd } => match cmd {
            ContextCmd::Sync => cmd_context_sync().map(|()| ExitCode::SUCCESS),
        },
    }
}

fn cmd_init(source: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = match classify_init_source(source) {
        InitSource::Path(p) => Workspace::init(&cwd, &p)?,
        InitSource::Url(u) => {
            println!("Cloning manifest repository {u}...");
            Workspace::init_from_url(&cwd, &u)?
        }
    };
    println!("Initialized workspace at {}", ws.root().display());
    Ok(())
}

enum InitSource {
    Path(PathBuf),
    Url(String),
}

/// Decide whether `source` is a local file path, a clone URL, or an
/// `owner/repo` shorthand. Tried in order: existing file → loose-file
/// init; existing directory → clone (local bare repos are directories);
/// URL with scheme → clone; SCP-style SSH → clone; `owner/repo` shape →
/// clone (shorthand → GitHub URL); otherwise loose-file init
/// (`Workspace::init` will report the missing file).
fn classify_init_source(source: &str) -> InitSource {
    let trimmed = source.trim();
    let path = std::path::Path::new(trimmed);

    if path.is_file() {
        return InitSource::Path(PathBuf::from(trimmed));
    }
    if path.is_dir() {
        return InitSource::Url(trimmed.to_string());
    }
    if trimmed.contains("://") || is_scp_style(trimmed) {
        return InitSource::Url(trimmed.to_string());
    }
    if let Ok(url) = gasp_core::url::normalize(trimmed, "github.com", "manifest")
        && url != trimmed
    {
        return InitSource::Url(url);
    }
    InitSource::Path(PathBuf::from(trimmed))
}

fn is_scp_style(s: &str) -> bool {
    // user@host:path — has an '@' followed later by a ':'.
    s.find('@')
        .and_then(|at| s.get(at + 1..))
        .is_some_and(|rest| rest.contains(':'))
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

fn cmd_status(strict: bool, show_manifest: bool) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;

    if show_manifest && let Some(m) = status::inspect_manifest(&ws)? {
        let branch_label = match (&m.branch, m.has_upstream) {
            (Some(b), true) => format!("{b} ↑"),
            (Some(b), false) => format!("{b} (no upstream)"),
            (None, _) => "(detached)".to_string(),
        };
        let state = if m.dirty { "dirty" } else { "clean" };
        let notes = match (m.dirty, m.has_upstream) {
            (true, _) => "uncommitted changes",
            (false, true) => "no local changes",
            (false, false) => "no local changes; no upstream tracking",
        };
        println!(
            "manifest: {} {} on {} ({})",
            short_sha(&m.head),
            state,
            branch_label,
            notes,
        );
        println!();
    }

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
                // Annotate the branch column with an upstream indicator
                // so users can see at a glance which branches `gasp
                // sync` can fast-forward.
                let branch = match (&info.branch, info.has_upstream) {
                    (Some(b), true) => format!("{b} ↑"),
                    (Some(b), false) => format!("{b} (no upstream)"),
                    (None, _) => "(detached)".to_string(),
                };
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

fn cmd_sync(
    group_filter: &[String],
    mode: ConflictMode,
    jobs: Option<usize>,
    update_manifest: bool,
    update_context: bool,
) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;
    let _lock = WorkspaceLock::acquire(&ws.dot_dir())?;

    if update_manifest
        && let Some(outcome) = ws
            .update_manifest()
            .context("updating the cloned manifest repo (use --no-update-manifest to skip)")?
    {
        match outcome {
            ManifestUpdate::UpToDate { sha } => {
                println!("manifest: up to date ({})", short_sha(&sha));
            }
            ManifestUpdate::Advanced { from, to } => {
                println!(
                    "manifest: advanced {} → {}",
                    short_sha(&from),
                    short_sha(&to)
                );
            }
            ManifestUpdate::SkippedDirty { sha } => {
                println!(
                    "manifest: skipped update ({}, uncommitted changes)",
                    short_sha(&sha)
                );
            }
            ManifestUpdate::SkippedNoUpstream { sha, branch } => {
                let suggest = match branch {
                    Some(b) => format!(
                        "  hint: git -C {} branch --set-upstream-to=origin/{b} {b}",
                        ws.manifest_repo_dir().display()
                    ),
                    None => "  hint: HEAD is detached; check out a branch first".into(),
                };
                println!(
                    "manifest: skipped update ({}, no upstream tracking)",
                    short_sha(&sha)
                );
                println!("{suggest}");
            }
        }
    }

    let manifest = ws.load_manifest()?;
    let mut repos = manifest.resolve()?;

    if !group_filter.is_empty() {
        repos.retain(|r| r.groups.iter().any(|g| group_filter.contains(g)));
    }

    if repos.is_empty() {
        println!("(no repos match)");
        return Ok(ExitCode::SUCCESS);
    }

    let num_jobs = jobs.unwrap_or_else(|| num_cpus::get().min(8)).max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_jobs)
        .build()
        .context("building rayon pool")?;

    let pb = ProgressBar::new(repos.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("[{pos}/{len}] {wide_msg}")
            .expect("static template")
            .progress_chars("=> "),
    );

    let outcomes: Vec<SyncOutcome> = pool.install(|| {
        repos
            .par_iter()
            .map(|repo| {
                let outcome = sync_one(&ws, repo, mode);
                // println! holds the stdout lock per call, so per-repo
                // lines from different workers won't interleave inside
                // a line. They will be in nondeterministic order across
                // lines.
                println!("{}", outcome.line());
                pb.set_message(repo.name.clone());
                pb.inc(1);
                outcome
            })
            .collect()
    });

    pb.finish_and_clear();

    let mut counts = SyncCounts::default();
    let mut failed: Vec<&SyncOutcome> = Vec::new();
    for o in &outcomes {
        match &o.result {
            SyncResult::Done(action) => counts.bump(action),
            SyncResult::Failed(_) => failed.push(o),
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
        for o in &failed {
            if let SyncResult::Failed(err) = &o.result {
                println!("  {}: {err}", o.name);
            }
        }
        return Ok(ExitCode::FAILURE);
    }

    // Refresh agent context after repos are up-to-date. Skips silently
    // if the manifest has no [context] section, and only runs after a
    // fully successful sync so it never operates on an inconsistent
    // workspace.
    if update_context {
        run_context_sync(&ws).context("syncing agent context (use --no-update-context to skip)")?;
    }
    Ok(ExitCode::SUCCESS)
}

struct SyncOutcome {
    name: String,
    result: SyncResult,
}

enum SyncResult {
    Done(Action),
    Failed(gasp_core::Error),
}

impl SyncOutcome {
    fn line(&self) -> String {
        match &self.result {
            SyncResult::Done(action) => {
                let (label, detail) = describe_action(action);
                format!("  {label:<6} {} {detail} ... ok", self.name)
            }
            SyncResult::Failed(_) => {
                format!("  error  {} ... FAILED", self.name)
            }
        }
    }
}

/// Sync a single repo: fetch (if exists) → inspect → plan → execute.
/// All errors are folded into `SyncResult::Failed` so the parallel
/// driver can collect outcomes without propagating Results.
fn sync_one(ws: &Workspace, repo: &Repo, mode: ConflictMode) -> SyncOutcome {
    let dest = ws.repo_path(&repo.path);

    if dest.exists()
        && let Err(err) = sync::fetch_remote(&dest, &repo.remote)
    {
        return SyncOutcome {
            name: repo.name.clone(),
            result: SyncResult::Failed(err),
        };
    }

    let status = match status::inspect(ws, repo) {
        Ok(s) => s,
        Err(err) => {
            return SyncOutcome {
                name: repo.name.clone(),
                result: SyncResult::Failed(err),
            };
        }
    };

    let action = sync::plan_action(&status, mode);

    match sync::execute(ws, repo, &action) {
        Ok(()) => SyncOutcome {
            name: repo.name.clone(),
            result: SyncResult::Done(action),
        },
        Err(err) => SyncOutcome {
            name: repo.name.clone(),
            result: SyncResult::Failed(err),
        },
    }
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

fn cmd_context_sync() -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;
    run_context_sync(&ws)
}

/// Shared between the explicit `gasp context sync` command and the
/// auto-run at the end of `gasp sync`. Prints a one-line summary or
/// silently no-ops when the manifest has no `[context]` section.
fn run_context_sync(ws: &Workspace) -> Result<()> {
    match context::sync(ws)? {
        None => Ok(()),
        Some(r) => {
            println!(
                "context: wrote {} ({} file{} from {} repo{}, {} skill{} linked)",
                r.output_path.display(),
                r.instructions_files,
                if r.instructions_files == 1 { "" } else { "s" },
                r.repos_contributing,
                if r.repos_contributing == 1 { "" } else { "s" },
                r.skills_linked,
                if r.skills_linked == 1 { "" } else { "s" },
            );
            Ok(())
        }
    }
}

fn cmd_manifest_init(name: Option<&str>, remote: Option<&str>, push: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    // Bootstrap path: if no workspace yet, create an empty .workspace/
    // here so manifest_init can drop the default template into it.
    let ws = match Workspace::discover(&cwd) {
        Ok(ws) => ws,
        Err(gasp_core::Error::WorkspaceNotFound(_)) => {
            let dot = cwd.join(".workspace");
            std::fs::create_dir_all(&dot).with_context(|| format!("creating {}", dot.display()))?;
            eprintln!(
                "No workspace found here — creating one at {}",
                dot.display()
            );
            Workspace::discover(&cwd)?
        }
        Err(e) => return Err(e.into()),
    };
    let outcome = manifest_init::init(&ws, &InitOpts { name, remote, push })?;
    if outcome.bootstrapped {
        println!("Wrote default workspace.toml template.");
    }
    println!(
        "Created manifest repo at {}",
        outcome.manifest_repo.display()
    );
    if outcome.remote_set {
        println!(
            "  origin → {}",
            remote.expect("remote_set implies --remote was given")
        );
    }
    if outcome.pushed {
        println!("  pushed main to origin");
    } else if outcome.remote_set {
        println!("  (run `git -C .workspace/manifest push -u origin main` when ready)");
    }
    Ok(())
}

fn cmd_doctor() -> Result<ExitCode> {
    let mut all_ok = true;

    // 1. git is installed.
    match std::process::Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("OK    {v}");
        }
        Ok(_) | Err(_) => {
            println!("FAIL  git is not on PATH or failed to run");
            all_ok = false;
        }
    }

    // 2. Workspace-aware checks: per-host gh auth (github.com) or
    // git ls-remote (everything else).
    let cwd = std::env::current_dir().context("reading current directory")?;
    match Workspace::discover(&cwd) {
        Ok(ws) => {
            let manifest = ws.load_manifest()?;
            let repos = manifest.resolve()?;
            let host_to_url = collect_hosts_with_urls(&repos);

            if host_to_url.is_empty() {
                println!("OK    manifest has no remote hosts to check");
            }

            for (host, sample_url) in &host_to_url {
                if host == "github.com" {
                    all_ok &= check_gh_auth(host);
                } else {
                    all_ok &= check_ls_remote(host, sample_url);
                }
            }
        }
        Err(_) => {
            println!("INFO  no workspace in scope; skipping per-host checks");
            // Still try gh auth status overall, since it's commonly the
            // thing users want to verify.
            all_ok &= check_gh_auth_overall();
        }
    }

    println!();
    if all_ok {
        println!("All checks passed.");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("Some checks failed.");
        Ok(ExitCode::FAILURE)
    }
}

fn collect_hosts_with_urls(repos: &[Repo]) -> Vec<(String, String)> {
    use std::collections::BTreeMap;
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for r in repos {
        if let Some(host) = gasp_core::url::host_of(&r.url) {
            seen.entry(host).or_insert_with(|| r.url.clone());
        }
    }
    seen.into_iter().collect()
}

fn check_gh_auth(hostname: &str) -> bool {
    let out = std::process::Command::new("gh")
        .args(["auth", "status", "--hostname", hostname])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            println!("OK    gh authenticated to {hostname}");
            true
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            println!(
                "FAIL  gh not authenticated to {hostname}: {}",
                stderr.lines().next().unwrap_or("(no detail)")
            );
            false
        }
        Err(_) => {
            println!("FAIL  gh CLI not installed (needed for {hostname})");
            false
        }
    }
}

fn check_gh_auth_overall() -> bool {
    let out = std::process::Command::new("gh")
        .args(["auth", "status"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            println!("OK    gh CLI authenticated");
            true
        }
        Ok(_) => {
            println!("WARN  gh CLI present but no active auth");
            true // not a hard failure outside a workspace
        }
        Err(_) => {
            println!("WARN  gh CLI not installed");
            true
        }
    }
}

fn check_ls_remote(host: &str, sample_url: &str) -> bool {
    let out = std::process::Command::new("git")
        .args(["ls-remote", "--exit-code", sample_url, "HEAD"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            println!("OK    {host} reachable (probed {sample_url})");
            true
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            println!(
                "FAIL  {host} unreachable ({sample_url}): {}",
                stderr.lines().next().unwrap_or("(no detail)")
            );
            false
        }
        Err(e) => {
            println!("FAIL  could not spawn git ls-remote: {e}");
            false
        }
    }
}

fn cmd_foreach(group_filter: &[String], command: &[String]) -> Result<ExitCode> {
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

    let (program, args) = command
        .split_first()
        .expect("clap requires at least one arg");

    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut missing = 0usize;

    for repo in &repos {
        let dest = ws.repo_path(&repo.path);
        if !dest.exists() {
            println!("=== {} (skipped: not cloned) ===", repo.name);
            missing += 1;
            continue;
        }

        println!("=== {} ===", repo.name);
        let status = std::process::Command::new(program)
            .args(args)
            .current_dir(&dest)
            .status();
        match status {
            Ok(s) if s.success() => succeeded += 1,
            Ok(s) => {
                eprintln!("    (exit {})", s.code().unwrap_or(-1));
                failed += 1;
            }
            Err(e) => {
                eprintln!("    failed to spawn: {e}");
                failed += 1;
            }
        }
    }

    println!();
    println!("Summary: {succeeded} succeeded, {failed} failed, {missing} missing");

    if failed > 0 || missing > 0 {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_add(
    name: &str,
    url: Option<&str>,
    revision: Option<&str>,
    path: Option<&std::path::Path>,
    groups: &[String],
    kind: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;

    let target_path = match path {
        Some(p) => ws.repo_path(p),
        None => ws.repo_path(std::path::Path::new(name)),
    };
    let inferred = infer_from_disk(&target_path)?;

    let url_owned: String = match (url, inferred.url) {
        (Some(u), _) => u.to_string(),
        (None, Some(u)) => {
            eprintln!("inferred url from {}: {u}", target_path.display());
            u
        }
        (None, None) => {
            if !target_path.is_dir() {
                anyhow::bail!(
                    "no URL provided and no clone at {} to infer from",
                    target_path.display()
                );
            }
            if !gasp_core::git::local::is_repo(&target_path) {
                anyhow::bail!(
                    "no URL provided and {} is not a git repository",
                    target_path.display()
                );
            }
            anyhow::bail!(
                "no URL provided and {} has no 'origin' remote",
                target_path.display()
            );
        }
    };

    let rev_owned: Option<String> = match (revision, inferred.revision) {
        (Some(r), _) => Some(r.to_string()),
        (None, Some(r)) => {
            eprintln!("inferred revision from {}: {r}", target_path.display());
            Some(r)
        }
        (None, None) => None,
    };

    edit::add_repo(
        &ws.manifest_path(),
        &AddArgs {
            name,
            url: &url_owned,
            revision: rev_owned.as_deref(),
            path,
            groups,
            kind,
        },
    )?;
    println!("Added '{name}' to manifest.");
    Ok(())
}

struct InferredFromDisk {
    url: Option<String>,
    revision: Option<String>,
}

/// Probe an on-disk repo for fields a user might not have typed
/// explicitly. Returns `None`-everywhere if the path isn't a repo so
/// the caller can decide whether the missing pieces are fatal.
fn infer_from_disk(target_path: &std::path::Path) -> Result<InferredFromDisk> {
    if !target_path.is_dir() || !gasp_core::git::local::is_repo(target_path) {
        return Ok(InferredFromDisk {
            url: None,
            revision: None,
        });
    }
    Ok(InferredFromDisk {
        url: gasp_core::git::local::remote_url(target_path, "origin")?,
        revision: gasp_core::git::local::current_branch(target_path)?,
    })
}

fn cmd_remove(name: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;
    edit::remove_repo(&ws.manifest_path(), name)?;
    println!("Removed '{name}' from manifest.");
    Ok(())
}

fn cmd_freeze(output: Option<&std::path::Path>) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let ws = Workspace::discover(&cwd)?;
    let body = freeze::freeze(&ws)?;

    // `-` means stdout. The manifest is the only thing on stdout in
    // this mode so the command can be piped.
    if output == Some(std::path::Path::new("-")) {
        use std::io::Write as _;
        std::io::stdout()
            .write_all(body.as_bytes())
            .context("writing manifest to stdout")?;
        return Ok(());
    }

    let default_path = ws.root().join("workspace.frozen.toml");
    let target = output.unwrap_or(&default_path);

    if target.exists() {
        anyhow::bail!("refusing to overwrite existing file: {}", target.display());
    }
    std::fs::write(target, &body).with_context(|| format!("writing {}", target.display()))?;
    // Status message → stderr, so it never pollutes stdout pipelines.
    eprintln!("Wrote frozen manifest to {}", target.display());
    Ok(())
}

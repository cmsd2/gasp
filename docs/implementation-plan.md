# Implementation Plan — v1

Plan for the first usable version of the workspace tool. Assumes the
decisions captured in `design-questions.md`:

- Rust, distributed as a single binary.
- `.workspace/` marker directory at the workspace root.
- `workspace.toml` manifest (loose-file in v1).
- No lockfile; `freeze` command for on-demand snapshots.
- libgit2 (`git2` crate) for local-only ops; shell out to `git` for all
  remote ops.
- `gh auth login` + `gh auth setup-git` (or another credential helper)
  assumed pre-configured by the user; tool does not manage credentials.
- `sync` fast-forwards by default; `--refuse` / `--rebase` / `--reset`
  for conflict handling.

---

## Crate layout

Two-crate cargo workspace from the start — keeps the core testable
independently of the CLI.

```
gasp/
├── Cargo.toml              # workspace
├── crates/
│   ├── gasp-core/          # library: manifest, workspace, git, sync
│   └── gasp-cli/           # binary: clap + command wiring (produces `gasp`)
├── docs/
└── tests/                  # integration tests (real git, real fs)
```

Project and binary name: `gasp`.

## Module breakdown — `gasp-core`

| Module | Responsibility |
| --- | --- |
| `manifest` | Parse / serialize `workspace.toml` via `toml` (read) and `toml_edit` (mutations). Schema types: `Manifest`, `Repo`, `Defaults`, `RevSpec`. URL normalization for GitHub shorthand. |
| `workspace` | Discover workspace root (walk up looking for `.workspace/`). Manage `.workspace/` layout: paths, state file, lock file, logs. |
| `git::local` | libgit2 wrapper for read-only + local ops: `head_sha`, `is_dirty`, `current_branch`, `can_fast_forward`, `checkout`, `reset_hard`. |
| `git::remote` | Shell-out wrapper for `clone`, `fetch`, `pull --ff-only`, `rebase`. Captures stderr for error reporting. |
| `sync` | The engine. Two phases: **plan** (diff manifest vs disk → list of actions) and **execute** (run actions in parallel, collect results). |
| `lock` | File-lock at `.workspace/lock` with PID + hostname, stale detection. |
| `doctor` | Environment checks: `git` present, `gh auth status`, `git ls-remote` probes per host. |
| `error` | `thiserror`-based error types. |

## Module breakdown — `gasp-cli`

| Module | Responsibility |
| --- | --- |
| `cli` | `clap` derive definitions for all subcommands. |
| `commands::*` | One file per subcommand (`init`, `sync`, `status`, `list`, `foreach`, `freeze`, `add`, `remove`, `doctor`). Each thin — translate clap args into `gasp-core` calls, format output. |
| `output` | Progress display (indicatif), result formatting, exit codes. |
| `main` | Wire logging, run the dispatcher. |

## Dependencies

```toml
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
toml = "0.8"               # read
toml_edit = "0.22"         # mutate while preserving formatting
git2 = "0.18"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "fs"] }
indicatif = "0.17"
thiserror = "1"
anyhow = "1"               # cli only — for top-level error formatting
tracing = "0.1"
tracing-subscriber = "0.3"
url = "2"
fs2 = "0.4"                # file locking
```

## Key types (rough shape)

```rust
// manifest
pub struct Manifest {
    pub version: u32,
    pub defaults: Defaults,
    pub repos: Vec<Repo>,
}

pub struct Repo {
    pub name: String,           // logical name, used as default path
    pub url: Url,               // normalized from shorthand or explicit
    pub revision: RevSpec,      // Branch / Tag / Sha
    pub path: Option<PathBuf>,  // override of default
    pub remote: String,         // default "origin"
    pub groups: Vec<String>,
}

pub enum RevSpec { Branch(String), Tag(String), Sha(String) }

// sync planning
pub enum RepoState {
    Missing,
    Clean { head: Oid, on_target: bool },
    Dirty { head: Oid },
    Diverged { head: Oid, target: Oid },
}

pub enum Action {
    Clone(Repo),
    FastForward { repo: Repo, from: Oid, to: Oid },
    Checkout   { repo: Repo, to: Oid },
    Rebase     { repo: Repo, onto: Oid },
    Reset      { repo: Repo, to: Oid },
    Skip       { repo: Repo, reason: String },
}

pub struct SyncPlan {
    pub actions: Vec<Action>,
    pub conflicts: Vec<(Repo, ConflictKind)>,
}
```

The `plan → execute` split is important: it makes `sync --dry-run` free,
makes testing the engine possible without a filesystem, and gives a
natural point for confirmation prompts later.

## Example manifest

```toml
version = 1

[defaults]
revision = "main"
remote   = "origin"
host     = "github.com"

[[repos]]
name     = "frontend"
url      = "acme/frontend"          # shorthand → github.com/acme/frontend
revision = "main"
groups   = ["web"]

[[repos]]
name     = "backend"
url      = "acme/backend"
revision = "v2.3.1"                 # tag pin
groups   = ["api"]

[[repos]]
name     = "shared-lib"
url      = "git@gitlab.example.com:platform/shared.git"
revision = "8f3a1c..."              # sha pin
path     = "lib/shared"
```

## Implementation milestones

### M0 — Scaffolding (~½ day)
- Workspace cargo layout, CI (`cargo test`, `cargo clippy`, `cargo fmt
  --check`), license, README stub.
- Empty clap commands that print "not implemented."

### M1 — Manifest + workspace discovery (~1–2 days)
- `Manifest` parsing with serde + `toml`, comprehensive parse-error tests.
- GitHub shorthand `owner/repo` → URL normalization, respecting
  `defaults.host`.
- `workspace init` creates `.workspace/`, places the manifest inside.
- `workspace list` reads and prints. No git involved yet.

### M2 — Clone happy path (~2 days)
- `workspace sync` for repos that don't exist on disk.
- Shell out to `git clone` per repo, sequential, sync I/O.
- Capture and surface git's stderr on failure.
- Continue-on-error, summary report, non-zero exit on any failure.

### M3 — Status + inspection via libgit2 (~1–2 days)
- `git::local` module: `head_sha`, `is_dirty`, `current_branch`.
- `workspace status` shows per-repo state vs manifest.
- The plan/execute split lands here — `status` is "plan only, don't
  execute."

### M4 — Update existing repos (~3 days)
- Fast-forward by default; classify each repo into `RepoState`.
- `--refuse` (default on conflict), `--rebase`, `--reset` flags.
- Detached HEAD for sha-pinned repos.
- Bulk of the design from section 3 of design-questions — needs thorough
  tests covering each `RepoState × flag` combination.

### M5 — Parallelism + progress + locking (~2 days)
- Move execution to tokio, bounded concurrency (`--jobs`, default
  `min(8, ncpu)`).
- `indicatif` multi-progress display.
- `.workspace/lock` via `fs2`, with stale-lock detection (PID + hostname).

### M6 — Remaining commands (~2–3 days)
- `foreach` — run shell command per repo, collect results.
- `add` / `remove` — edit manifest TOML preserving formatting and
  comments via `toml_edit`.
- `freeze` — write current shas as a new manifest (or sidecar).
- `doctor` — `git --version`, `gh auth status`, per-host `git ls-remote`
  probe.

### M7 — Distribution + polish (~2 days)
- `cargo-dist` for release binaries (macOS / Linux / Windows).
- Homebrew tap.
- Error message pass — every error should tell the user what to do next.
- Manual testing on a real multi-repo workspace.

**Estimated total:** ~2–3 weeks of focused work for v1.

## Order rationale

- M1 + M2 give a usable tool fast (clone a workspace from a manifest),
  even if dumb.
- M3 introduces libgit2 *before* M4 needs it for fast-forward decisions,
  so the integration is isolated.
- M5 is deferred until correctness is proven sequentially — parallelism
  on top of buggy sync is a debugging nightmare.
- M6 commands are independent; could be reordered or parallelized.

## Test strategy

- Unit tests in `gasp-core` for manifest parsing, URL normalization, and
  the plan phase of `sync` (no filesystem).
- Integration tests in `tests/` that build temporary git repos
  programmatically via `git2` in `tempfile::TempDir`s, then drive the
  CLI binary. Covers the execute phase end-to-end without needing a
  network or real remotes.
- One smoke test that hits a real public GitHub repo, gated behind a
  feature flag so CI can opt in.

## Open items to settle before coding

1. ~~**Binary name**~~ — **decided:** `gasp` (project and binary).
2. **Manifest schema details** — the example above is a starting point.
   Worth a sit-down to lock the exact field names and shorthand rules
   before writing the parser.
3. **`foreach` output format** — interleaved vs grouped per-repo;
   prefix lines with repo name?
4. **Exit codes** — convention beyond "0 success, non-zero failure"?
   e.g. distinct codes for "manifest error" vs "git error" vs "partial
   failure."

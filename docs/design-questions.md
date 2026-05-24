# Workspace Tool — Design Questions

A walkthrough of the open design questions for a workspace tool that clones
GitHub repositories into a workspace based on a `workspace.toml` manifest.
Inspired by Zephyr's `west`, `git-ws`, Google's `repo`, and similar tools.

This document captures the questions, the options on the table, and the
tradeoffs — not final decisions. Decisions should land as ADRs once made.

---

## 1. Workspace model

### Where does `workspace.toml` live?

Three common shapes:

- **Sibling file** (`git-ws` style): `workspace.toml` sits next to the cloned
  repos. Root contains the manifest + N repo directories.
- **Parent directory with `.workspace/` marker** (`west` style): root has
  `.west/` (or `.workspace/`) holding state + a pointer to the manifest repo;
  repos clone as siblings.
- **Manifest-repo-is-root** (`repo`/Android style): one of the cloned repos
  *is* the manifest repo, and others sit alongside.

Tradeoffs:

- Sibling file is simplest and easiest to bootstrap from a local file. But
  upgrading later to "manifest stored in git" is awkward — the file is no
  longer the source of truth, just a cache.
- A `.workspace/` directory pattern gives you a clean place for state,
  lockfile, config, logs, and a checked-out copy of the manifest repo. It
  scales cleanly from local-file → remote-manifest because the local
  `workspace.toml` just lives inside `.workspace/` either way.
- Manifest-repo-is-root couples the manifest's git history to one specific
  project — fine for monoliths-pretending-to-be-polyrepos, awkward otherwise.

**Decided:** `.workspace/` marker directory at the workspace root.
`workspace.toml` can live either as a loose file at
`.workspace/workspace.toml` (when initialized from a local file) or
inside a cloned git repo at `.workspace/manifest/workspace.toml` (when
initialized from a URL). Cloned repos sit as siblings of `.workspace/`
under the workspace root.

**Both modes are first-class** as of the M6+remote-manifest work:
`gasp init` dispatches based on the argument shape — existing file →
loose mode; existing directory, `://` URL, SCP-style SSH, or
`owner/repo` shorthand → cloned mode. `Workspace::manifest_mode()`
returns `Loose` or `Cloned` based on whether `.workspace/manifest/.git`
exists.

### Nested workspaces and imports

Two separate questions:

- **Imports** (one manifest pulls in another by reference): very useful, but
  opens a can of worms — conflict resolution when two imports name the same
  repo at different revisions, cycles, transitive depth limits. West has
  this; it's powerful and confusing.
- **Nesting** (a cloned repo is itself a workspace): no by default. Tools
  should not recurse into child workspaces automatically; if you want that,
  it's a separate `workspace foreach --recursive` behavior.

**Lean:** ship without imports in v1. Add them once there's real demand and
the conflict-resolution rules can be designed with concrete cases in hand.

---

## 2. Manifest schema

### Repo coordinates

Options, roughly increasing in flexibility:

- `owner/repo` shorthand (GitHub-only, terse)
- Full URL (works for any git host)
- Structured `{ host, owner, name }` (verbose but parseable)

**Lean:** accept all three, normalize to URL internally. Shorthand
`owner/repo` assumes GitHub; full URLs override.

### SSH vs HTTPS

Don't bake this into the manifest. The manifest names the repo; the *user's
local config* decides transport. Otherwise contributors with SSH keys are
forced to HTTPS or vice versa. Provide a workspace-level default + per-user
override.

### Per-repo fields for v1

- `revision`: branch, tag, or sha. One field, type-detected, or three
  separate fields? Prefer one field with explicit `type:` only when
  ambiguous (rare). Default branch if omitted.
- `path`: destination relative to workspace root. Default to repo name.
- `remote`: name for the git remote. Default `origin`.
- `groups` / `tags`: for selective clone (`workspace sync --group frontend`).
  Cheap to add, valuable early.

### Defer to v2

- Submodules handling (recursive, on-demand, ignore) — pick a default,
  expose later
- Shallow / depth — useful but adds complexity to update semantics
- Sparse checkout — niche, big surface area

### Defaults block

Top-level `defaults:` for `revision`, `remote`, `host`. Keeps repetition out
of long manifests.

### Schema versioning

`version: 1` at the top of `workspace.toml` from day one. Non-negotiable.
The cost is one line; the cost of *not* having it is breaking every
existing workspace when the schema changes.

---

## 3. Commands / UX

### Minimum verbs

- `init` — create a workspace from a manifest (local path or git URL)
- `sync` (or `update`) — make the on-disk state match the manifest
- `status` — show per-repo state vs manifest
- `foreach` — run a command in every repo
- `list` — print repos, paths, revisions

### Worth adding early

- `add` / `remove` — edit the manifest without hand-editing TOML
  (use `toml_edit` to preserve formatting and comments)
- `freeze` — write a lockfile pinning current shas

### `sync` is the hard one

Three orthogonal cases:

1. Repo doesn't exist locally → clone. Easy.
2. Repo exists, on the right revision, clean → fetch + fast-forward (or
   no-op if pinned to sha).
3. Repo exists, but checked out on a different branch / has local changes /
   has diverged.

For case 3:

- **Refuse** and report. Safe, annoying.
- **Refuse if dirty, fast-forward otherwise.** What most tools do well.
- **Reset hard.** Convenient, occasionally career-ending.

**Decided:** fast-forward by default. If a fast-forward isn't possible
(dirty working tree, diverged history, wrong branch checked out), behavior
is selected by an explicit flag:

- `--refuse` (or no flag → default to refuse on conflict): report and skip
  the repo, continue with the rest, exit non-zero with a summary.
- `--rebase`: rebase local commits onto the target revision.
- `--reset`: hard-reset to the target revision. Discards local commits and
  uncommitted changes in tracked files. Never implied by `--force` or any
  other flag — must be typed.

Open sub-question: what's the default on conflict — refuse, or prompt? Lean
refuse (non-interactive is the right default for a tool that gets scripted).

### Partial failure

Continue on error, collect failures, exit non-zero with a summary. Do *not*
roll back successful clones — users will be confused when their disk state
doesn't match what the tool just said it did. Print a clear "3 of 10
failed, here they are, run `workspace sync` to retry."

---

## 4. Update semantics & pinning

### Pin vs track

The manifest expresses *intent* per repo:

- A branch name → track the branch, fast-forward on sync.
- A tag → check out the tag, no updates.
- A sha → pin exactly, detached HEAD.

Mixing is fine and expected: most repos track a branch during active
development; a few may be pinned to a sha while debugging or stabilizing.

### Why no lockfile in v1

Lockfiles win when dependencies are immutable artifacts you consume. Here
the subprojects are repos you're actively editing and committing to, which
breaks the model:

- Lockfile is stale the moment anyone pushes a commit to a tracked repo.
- Every sync forces a "do I update the lockfile?" decision.
- Lockfile merge conflicts become routine when two devs work in different
  repos simultaneously.
- The manifest already supports sha pinning when reproducibility matters,
  so the lockfile is duplicative for the cases that need it.

`west` operates without a lockfile and the dev workflow is fine.

**Decided:** no lockfile in v1. The manifest is the single source of truth.

### `freeze` command

For the cases where a snapshot *is* useful (release tagging, bug repro,
onboarding at a known-good state), provide a `workspace freeze` command
that writes the current resolved shas — either as a new manifest file or
a sidecar — on demand. Not invoked by `sync`.

### Reconsider later if

- Workspace gets used in CI in a way that needs pinned state across runs.
- Multi-team coordination grows a "what version of all 20 repos are we
  shipping" question that the manifest alone can't answer ergonomically.

### Divergence

For tracked branches, default to fast-forward only. For pinned shas, `sync`
should detach HEAD at that sha (and warn loudly if local commits would be
orphaned).

---

## 5. Auth & transport

### Scope

GitHub-only is tempting (simpler auth story, shorthand coordinates) but
limiting. Git-generic with GitHub as the well-supported default is barely
more work and keeps the door open for GHE, GitLab, internal hosts.

### Auth — delegated, not managed

**Decided:** the tool does not manage credentials. It assumes the user has
already configured auth such that plain `git` commands work:

- For GitHub: `gh auth login` + `gh auth setup-git` is the recommended
  path. `gh` installs itself as a git credential helper for github.com.
- For non-GitHub hosts: whatever the user already uses — ssh-agent for
  SSH URLs, a git credential helper (OS keychain, `git-credential-store`,
  etc.) for HTTPS.

Rationale: every credential mechanism we'd reinvent already exists in the
git ecosystem and is what the user already trusts. We don't read tokens,
don't store them, don't prompt for them.

### Private repos

Just Work as long as the above is configured. `workspace doctor` verifies:

- `gh auth status` for any GitHub host in the manifest.
- A best-effort `git ls-remote` probe for non-GitHub hosts to confirm
  auth is reachable, with a clear message pointing at git credential
  helper docs if not.

---

## 6. State & concurrency

### State location

`.workspace/` at root. Inside:

- `workspace.toml` — the manifest (loose-file mode)
- `state.json` — last sync time, last manifest sha, etc.
- `manifest/` — checked-out manifest repo, when graduated from loose file
- `logs/` — per-sync logs (rotate aggressively)

### Parallel operations

Clones and fetches parallelize trivially and the speedup is large. Default
to something like `min(8, ncpu)`, expose `--jobs`. Single progress display
that doesn't interleave garbage — pick a TTY library that handles this.

### Locking

A simple file lock in `.workspace/lock` to prevent two concurrent `sync`
runs from racing. Stale lock detection (PID + hostname). Not negotiable —
users *will* run two terminals.

---

## 7. Language & distribution

### Language

Real options:

- **Go** — single static binary, great git libraries (`go-git`) or shell
  out to `git`, easy cross-platform release. Best fit for this kind of
  tool.
- **Rust** — similar binary story, more ceremony, less mature git
  ecosystem (gitoxide is getting there).
- **Python** — fast to write, slow to distribute well. `west` is Python
  and users feel it.
- **TypeScript / Node** — fine if the audience already has Node; otherwise
  distribution is painful.

**Decided:** Rust. Single static binary, strong type system for the
manifest / state modeling, good async story for parallel clones (tokio),
and the ecosystem covers what's needed (`toml` / `toml_edit` for the
manifest, clap for CLI, indicatif for progress, tokio for parallelism,
`git2` for local git ops).

Tradeoffs accepted:

- More upfront ceremony than Go (lifetimes, error types, async coloring).
- `git2` crate brings a C build dependency (libgit2). Acceptable — the
  surface area we use is small and well-trodden.

### Git operations: libgit2 vs shell-out

Split by operation type:

| Operation class | Tool | Why |
| --- | --- | --- |
| Local-only (HEAD sha, dirty check, branch list, local fast-forward, checkout, reset, log inspection) | **libgit2** (via `git2` crate) | No process spawn, programmatic API, fast and safe for cheap reads run per-repo across a large workspace. |
| All remote ops (clone, fetch, push) — GitHub and non-GitHub | **shell out to `git`** | Relies on the user's existing git credential setup. No new auth code from us. |

The C build dependency from `git2` (libgit2) is accepted; the crate is
well-maintained and the local-op surface area is small and stable.

### Auth — assumed pre-configured

**Assumption:** the user has already run `gh auth login` and `gh auth
setup-git` (or has otherwise configured a git credential helper). Under
that assumption, plain `git` commands authenticate transparently for both
GitHub and non-GitHub remotes, and the tool never has to touch tokens.

Consequences:

- `gh` is a *setup* dependency, not a runtime dependency. The tool
  doesn't invoke `gh` on the hot path.
- `workspace doctor` checks: `gh auth status` for GitHub hosts in the
  manifest, and a basic credential-helper sanity check for non-GitHub
  hosts. Reports clearly what's missing rather than trying to fix it.
- If auth isn't configured, `git` operations fail with git's own error
  messages — clear enough, and the doctor command points the way.

### Distribution

Homebrew tap + GitHub releases with prebuilt binaries for macOS / Linux /
Windows (use `cargo-dist` or similar to automate the matrix). `cargo
install` for the Rust-native crowd. Skip package managers (apt, etc.)
until there's demand.

---

## Open questions to resolve next

- ~~Lock in the workspace layout (section 1)~~ — **decided:** `.workspace/`
  marker directory at the root.
- ~~Decide on `sync` divergence behavior (section 3)~~ — **decided:**
  fast-forward by default, `--refuse` / `--rebase` / `--reset` opt-ins.
- ~~Confirm lockfile in v1 (section 4)~~ — **decided:** no lockfile;
  manifest is source of truth; `freeze` command for on-demand snapshots.
- ~~Pick language (section 7)~~ — **decided:** Rust. Git ops split:
  libgit2 (`git2` crate) for local-only, shell out to `git` for all
  remote ops. Auth is the user's responsibility (assume `gh auth login`
  + `gh auth setup-git` for GitHub, credential helper for others);
  `workspace doctor` verifies. Distribute via Homebrew + GitHub releases.

All four gating decisions are now made. Next step is to lift these into a
spec / ADRs and start scoping v1 implementation.

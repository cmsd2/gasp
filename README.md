# gasp

A multi-repo workspace manager. One `workspace.toml` lists the git
repositories your project spans; `gasp` keeps them cloned and in sync,
and (optionally) aggregates per-repo AI-agent instructions and skills
into the workspace root. Inspired by Zephyr's `west` and `git-ws`.

A workspace can be defined either as a local file or as a git repository
of its own — share the URL with collaborators and `gasp init <url>` is
all they need.

## Install

No prebuilt binaries yet; build from source. Requires Rust 1.85+
(edition 2024) and a working `git` on PATH.

```sh
git clone https://github.com/cmsd2/gasp.git
cd gasp
cargo install --path crates/gasp-cli
```

`gh` is recommended for GitHub auth (`gh auth login && gh auth setup-git`
once); for other hosts, your normal SSH key or git credential helper.

## Quickstart

**Start a new workspace from scratch:**

```sh
mkdir my-project && cd my-project
gasp manifest init                     # creates .workspace/manifest/
gasp add backend  acme/backend  --revision main --kind code
gasp add frontend acme/frontend --revision main --kind code
gasp sync                              # clones backend/ and frontend/
```

**Join an existing workspace:**

```sh
mkdir my-project && cd my-project
gasp init git@github.com:acme/my-project-workspace.git
gasp sync
```

**Use a local manifest file (loose mode):**

```sh
gasp init ./my-workspace.toml
gasp sync
```

## Commands

| Command | What it does |
| --- | --- |
| `gasp init <source>` | Initialize a workspace. `<source>` is a path to a local `workspace.toml`, or a git URL / `owner/repo` shorthand for a manifest repository. |
| `gasp manifest init [--name X] [--remote URL] [--push]` | Create a fresh manifest repo. Works from an empty directory (bootstraps a default template) or graduates an existing loose-file workspace. |
| `gasp manifest push` | Push commits from the cloned manifest repo to its `origin`. Auto-sets the upstream on first push. |
| `gasp sync [--on-conflict refuse\|rebase\|reset] [--group X] [-j N] [--no-update-manifest] [--no-update-context]` | Clone missing repos; update existing ones. Fast-forwards by default. In cloned-manifest mode, pulls the manifest first; if `[context]` is configured, refreshes the aggregated instructions and skill symlinks afterward. |
| `gasp context sync` | Run the agent-context aggregation explicitly. No-op when the manifest has no `[context]` section. |
| `gasp status [--show-manifest] [--strict]` | Show per-repo state (clean / dirty / ahead / behind / diverged / missing) with upstream-tracking annotations. `--show-manifest` adds a line for the cloned-manifest repo. `--strict` exits non-zero on any issue (CI-friendly). |
| `gasp list` | Print the resolved repos in the manifest. |
| `gasp add <name> <url> [--revision X] [--path X] [--group ...]` | Append a `[[repos]]` entry to the manifest (preserves comments / formatting via `toml_edit`). |
| `gasp remove <name>` | Delete a `[[repos]]` entry. |
| `gasp foreach [--group X] -- <cmd> [args...]` | Run a shell command in every repo, sectioned output. |
| `gasp freeze [-o PATH\|-]` | Write a pinned manifest with current shas as revisions. `-o -` streams to stdout (status messages go to stderr) for piping. |
| `gasp doctor` | Verify `git`, `gh` auth, and per-host reachability. |

Run `gasp <cmd> --help` for the full flag list of any command.

## The manifest

```toml
version = 1

# Defaults applied to every repo unless overridden.
[defaults]
revision = "main"
remote   = "origin"
host     = "github.com"

# Optional: aggregate per-repo agent instructions + symlink skills.
# Absence of this section makes `gasp context sync` a no-op.
[context]
output         = "CLAUDE.md"             # generated aggregated file
include        = "CLAUDE.md"             # per-repo file(s) to aggregate
skills_dir     = ".claude/skills"        # workspace-local symlink target
skills_include = ".claude/skills/*.md"   # per-repo skills to link
# template     = ".workspace/context.j2" # optional custom jinja template

[[repos]]
name     = "frontend"
url      = "acme/frontend"          # owner/repo shorthand → defaults.host
revision = "main"                   # branch, tag, or sha
kind     = "code"                   # used to group repos in context output
groups   = ["web"]                  # for `gasp sync --group web`

[[repos]]
name     = "backend"
url      = "acme/backend"
revision = "v2.3.1"                 # tag pin
path     = "services/backend"       # default = repo name
kind     = "code"

[[repos]]
name = "architecture"
url  = "acme/architecture-decisions"
kind = "adrs"                       # ADR records the agent should consult

[[repos]]
name = "shared-lib"
url  = "git@gitlab.example.com:platform/shared.git"
# Per-repo override of the top-level context globs; "" opts the repo out.
context_include        = "docs/AGENTS.md"
context_skills_include = ""
```

**URL forms accepted:** `owner/repo` shorthand (expanded with
`defaults.host`), full HTTPS, full SSH (`ssh://...`), SCP-style
(`git@host:path`), and local filesystem paths.

**Revision:** branch name (tracked, fast-forwarded), tag, or full SHA
(checks out detached HEAD). If omitted, falls back to `defaults.revision`
or the remote's default branch.

**Paths:** must be relative and inside the workspace; `..` and absolute
paths are rejected.

**`kind`:** freeform classification (`"code"`, `"skills"`, `"adrs"`,
`"data"`, `"docs"`, etc.). Used by `gasp context sync` to group repos
in the generated agent-instructions file. No validation — pick what
makes sense for your project.

## Operating modes

A workspace is either **loose** (manifest is a plain file at
`.workspace/workspace.toml`) or **cloned** (manifest lives inside a git
repo at `.workspace/manifest/`). Every command works the same way in
both — `gasp` figures out which mode it's in.

```
.workspace/
├── workspace.toml      ← loose mode
├── manifest/           ← cloned mode (a git repo)
│   ├── .git/
│   ├── workspace.toml
│   └── README.md
└── lock                ← workspace lock (held during sync)
```

Switch from loose to cloned at any time with `gasp manifest init`. The
loose `workspace.toml` is moved into the new repo and a README is
generated. Pass `--remote URL --push` to wire up origin and push in
one step.

**Cloned mode benefits:**
- Collaborators get the workspace with `gasp init <url>`.
- `gasp sync` automatically pulls the latest manifest before updating
  child repos, so newly added entries appear without a manual step.
  (Skipped cleanly when the manifest branch has no upstream — fixable
  with `git -C .workspace/manifest branch --set-upstream-to=origin/...`.)
- The manifest's git history shows who added/removed/repinned what.

## Agent context

When the manifest has a `[context]` section, `gasp` will aggregate
agent-relevant files from each child repo:

- **Instructions:** every file matching `[context].include` in each repo
  (default `CLAUDE.md`) is concatenated through a Jinja template into a
  single workspace-root file (default `CLAUDE.md`). Repos are grouped
  by `kind` so the agent sees a structured layout.
- **Skills:** every file matching `[context].skills_include`
  (default `.claude/skills/*.md`) is **symlinked** into a workspace-local
  directory (default `.claude/skills/`). Updates to the source files
  are reflected immediately; no regeneration needed.

The aggregated instructions file is **splice-managed** between markers:

```
<!-- BEGIN gasp:context — do not edit -->
... generated content ...
<!-- END gasp:context -->
```

Anything you write above the BEGIN marker or below the END marker is
preserved across re-syncs — useful for a personal preamble or extra
project-level notes.

`gasp sync` runs the aggregation automatically after a successful repo
sync; pass `--no-update-context` to skip. `gasp context sync` runs it
on demand. Nothing is ever written to `~/.claude/` — everything stays
inside the workspace.

Per-repo `context_include` / `context_skills_include` fields override
the workspace-level globs (use `""` to opt a repo out).

To use a custom template, set `[context].template = "path/to/your.j2"`
(relative to the workspace root). The built-in template is the default
when this field is unset.

## Branches and worktrees

Each child repo is just a normal git checkout — you can branch, switch,
and worktree-add as usual, and `gasp` will tell you what state things
are in.

**Switching branches within a repo:**

```sh
cd backend
git switch -c feature-x
# edit ...
git push -u origin feature-x
```

`gasp status` will report `feature-x ↑` for that repo (the `↑` flags an
upstream is configured). When you're ready, `gasp sync` will fast-forward
it on the next sync if the manifest's target revision moves and the
branch can reach it. If the branch can't ff (you've diverged), use
`--on-conflict rebase` or `--on-conflict reset` per the section below.

**Pinning a repo to a specific branch via the manifest:**

```toml
[[repos]]
name     = "backend"
url      = "acme/backend"
revision = "main"      # or "release/v2", a tag, or a sha
```

Update the field, commit (if in cloned-manifest mode), and the next
`gasp sync` will move the repo to that revision.

**Git worktrees (parallel checkouts of the same repo):**

```sh
cd backend
git worktree add ../backend-feature -b feature-x
```

`gasp status` lists each worktree under its parent repo:

```
NAME                STATE  HEAD     BRANCH      DETAIL
backend             clean  abc1234  main ↑      on target main
  ↳ backend-feature dirty  def5678  feature-x   worktree at .../backend-feature
```

Worktrees aren't independently managed by `gasp` — they live alongside
the parent and inherit its remote configuration. Treat them as
informational: `gasp status` makes them visible so you don't forget
about unfinished work in a sibling checkout. Drop one with `git -C
backend worktree remove ../backend-feature` when you're done; `gasp
status` will stop reporting it.

## Sync conflict modes

When `gasp sync` updates an existing repo, it always fast-forwards if
possible. When a repo is ahead, diverged, or dirty:

- `--on-conflict refuse` *(default)* — skip and report.
- `--on-conflict rebase` — rebase local commits onto the target.
- `--on-conflict reset` — hard-reset to the target. **Destructive** —
  discards local commits and uncommitted changes.

## Auth

`gasp` does not manage credentials. It assumes:

- For GitHub: `gh auth login` + `gh auth setup-git` (one-time).
- For other hosts: your existing SSH key, git credential helper, or
  OS keychain.

Plain `git` runs from `gasp` will pick those up. Run `gasp doctor` to
verify the chain.

## Examples

**Run a command everywhere:**
```sh
gasp foreach -- git status -s
gasp foreach --group web -- npm install
```

**Snapshot the current state for a release:**
```sh
gasp freeze -o release-v1.toml         # writes a pinned-sha manifest
gasp freeze -o - | pbcopy              # or pipe it (data → stdout, status → stderr)
```

**CI check that the workspace is clean and on-target:**
```sh
gasp status --strict
```

**Update the manifest source, then resync:**
```sh
gasp sync                              # auto-pulls manifest, then context
gasp sync --no-update-manifest         # skip manifest pull (use cached)
gasp sync --no-update-context          # skip the context-sync step
```

**Just refresh agent context, without touching repos:**
```sh
gasp context sync
```

**Inspect the workspace including the manifest repo:**
```sh
$ gasp status --show-manifest
manifest: c4f1a2b clean on main ↑ (no local changes)

NAME       STATE   HEAD     BRANCH            DETAIL
backend    clean   abc1234  main ↑            on target main
frontend   behind  def5678  main ↑            target main (1a2b3c4), behind 1
adrs       clean   9876543  main (no upstream) on target main
```

The `↑` annotation flags branches with upstream tracking configured;
`(no upstream)` flags ones without (which `gasp sync` will skip rather
than failing on `git pull`).

## Layout once cloned

```
my-project/
├── .workspace/         ← gasp's metadata + the manifest repo
├── CLAUDE.md           ← aggregated agent instructions (if [context] is set)
├── .claude/skills/     ← symlinks to per-repo skill files
├── backend/            ← cloned per [[repos]]
│   ├── CLAUDE.md        ← per-repo instructions (source of truth)
│   └── .claude/skills/
│       └── lint.md
├── frontend/
└── services/
    └── shared/         ← path override
```

Repos sit as siblings of `.workspace/`. Anything that's not in the
manifest is left alone, so the workspace can coexist with other tools.
Generated files (`CLAUDE.md`, `.claude/skills/`, `workspace.frozen.toml`)
should be added to your top-level `.gitignore` if you don't want to
commit them.

## Design and roadmap

- [`docs/design-questions.md`](docs/design-questions.md) — decisions
  and rationale.
- [`docs/implementation-plan.md`](docs/implementation-plan.md) —
  crate layout, modules, milestones.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

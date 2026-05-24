# gasp

A multi-repo workspace manager. One `workspace.toml` lists the git
repositories your project spans; `gasp` keeps them cloned and in sync.
Inspired by Zephyr's `west` and `git-ws`.

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
gasp add backend  acme/backend  --revision main
gasp add frontend acme/frontend --revision main
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
| `gasp sync [--on-conflict refuse\|rebase\|reset] [--group X] [-j N] [--no-update-manifest]` | Clone missing repos; update existing ones. Fast-forwards by default. In cloned-manifest mode, pulls the manifest first. |
| `gasp status [--show-manifest] [--strict]` | Show per-repo state (clean / dirty / ahead / behind / diverged / missing). `--strict` exits non-zero on any issue (CI-friendly). |
| `gasp list` | Print the resolved repos in the manifest. |
| `gasp add <name> <url> [--revision X] [--path X] [--group ...]` | Append a `[[repos]]` entry to the manifest (preserves comments / formatting via `toml_edit`). |
| `gasp remove <name>` | Delete a `[[repos]]` entry. |
| `gasp foreach [--group X] -- <cmd> [args...]` | Run a shell command in every repo, sectioned output. |
| `gasp freeze [-o PATH\|-]` | Write a pinned manifest with current shas as revisions. `-o -` streams to stdout for piping. |
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

[[repos]]
name     = "frontend"
url      = "acme/frontend"          # owner/repo shorthand → defaults.host
revision = "main"                   # branch, tag, or sha
groups   = ["web"]                  # for `gasp sync --group web`

[[repos]]
name     = "backend"
url      = "acme/backend"
revision = "v2.3.1"                 # tag pin
path     = "services/backend"       # default = repo name

[[repos]]
name = "shared-lib"
url  = "git@gitlab.example.com:platform/shared.git"
```

**URL forms accepted:** `owner/repo` shorthand (expanded with
`defaults.host`), full HTTPS, full SSH (`ssh://...`), SCP-style
(`git@host:path`), and local filesystem paths.

**Revision:** branch name (tracked, fast-forwarded), tag, or full SHA
(checks out detached HEAD). If omitted, falls back to `defaults.revision`
or the remote's default branch.

**Paths:** must be relative and inside the workspace; `..` and absolute
paths are rejected.

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
├── state.json          ← (future) sync metadata
└── lock                ← workspace lock (held during sync)
```

Switch from loose to cloned at any time with `gasp manifest init`. The
loose `workspace.toml` is moved into the new repo and a README is
generated.

**Cloned mode benefits:**
- Collaborators get the workspace with `gasp init <url>`.
- `gasp sync` automatically pulls the latest manifest before updating
  child repos, so newly added entries appear without a manual step.
- The manifest's git history shows who added/removed/repinned what.

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
gasp freeze -o - | pbcopy              # or pipe it
```

**CI check that the workspace is clean and on-target:**
```sh
gasp status --strict
```

**Update the manifest source, then resync:**
```sh
gasp sync                              # auto-pulls manifest first
gasp sync --no-update-manifest         # skip manifest pull (use cached)
```

## Layout once cloned

```
my-project/
├── .workspace/         ← gasp's metadata + the manifest repo
├── backend/            ← cloned per [[repos]]
├── frontend/
└── services/
    └── shared/         ← path override
```

Repos sit as siblings of `.workspace/`. Anything that's not in the
manifest is left alone, so the workspace can coexist with other tools.

## Design and roadmap

- [`docs/design-questions.md`](docs/design-questions.md) — decisions
  and rationale.
- [`docs/implementation-plan.md`](docs/implementation-plan.md) —
  crate layout, modules, milestones.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

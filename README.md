# gasp

A multi-repo workspace manager. Clone and sync a collection of git
repositories from a single `workspace.toml` manifest, similar in spirit
to Zephyr's `west` or `git-ws`.

## Status

Pre-alpha. The scaffolding and design docs are in place; commands are
being implemented milestone-by-milestone — see
[`docs/implementation-plan.md`](docs/implementation-plan.md).

## Design

- [`docs/design-questions.md`](docs/design-questions.md) — design
  decisions and rationale.
- [`docs/implementation-plan.md`](docs/implementation-plan.md) — crate
  layout, modules, milestones.

## Building

```sh
cargo build
```

Requires a recent Rust toolchain (edition 2024, MSRV 1.85).

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

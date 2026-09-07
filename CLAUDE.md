# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`dotty` is a Rust CLI for managing dotfiles: it moves files/directories out of a home directory into a git repository (replacing the originals with symlinks) and can restore, sync, and update them later. Git repos passed to `add` are tracked as git submodules rather than plain files.

## Commands

```sh
cargo build                  # debug build
cargo build --release        # release build
cargo run -- <args>          # run locally, e.g. cargo run -- -r ~/.dotty init
cargo clippy -- -D warnings  # lint (must pass with zero warnings, enforced in CI)
cargo fmt                    # format
cargo fmt --check            # format check (enforced in CI)
```

There is no test suite in this repository currently.

The toolchain is pinned via `rust-toolchain.toml` (nightly-2024-12-29); `rustup` will pick it up automatically. Cargo.lock is committed (this is a binary, not a library).

## CLI structure

Entry point is [src/main.rs](src/main.rs), using `clap` derive macros. Global options (`-r/--repository`, defaulting to `~/.dotty` or `$DOTTY_REPOSITORY`; `-R/--root`, the directory dotfiles live relative to) are parsed once in `Opts`, resolved to canonical paths, then dispatched to a subcommand handler in [src/cmds.rs](src/cmds.rs). Each subcommand (`init`, `clone`, `add`, `restore`, `sync`, `update`) is a thin free function in `cmds.rs` that composes helpers from `src/utils/`.

`src/utils/` is organized by concern, not by command:
- [src/utils/path.rs](src/utils/path.rs) — path canonicalization (including `~` expansion, and canonicalizing paths that don't exist yet), computing a path's location relative to the dotfiles root, common-base-path calculation for commit messages.
- [src/utils/fs.rs](src/utils/fs.rs) — the actual move/symlink/copy/restore mechanics, including the `move_then_symlink` used by `add` and the `restore` used by `restore` (with an `OverwriteTempDir` RAII helper that stashes displaced files instead of deleting them).
- [src/utils/git.rs](src/utils/git.rs) — all `git2` usage: init/open/clone (with recursive submodule init), staging/committing, submodule add/update, and a hand-rolled fetch+merge+push flow for `sync` (fast-forward when possible, otherwise a real merge commit; conflicts abort with an error rather than being resolved automatically).
- [src/utils/string.rs](src/utils/string.rs) — trivial random string helper used for temp dir names.

Command flow worth understanding before changing `add` or `restore`:
- `add` walks the given paths (`flatten_paths_to_add` in cmds.rs), treating any directory that is itself a git repo as an opaque unit (added as a submodule) rather than descending into it. Everything else is moved into the dotty repo and symlinked back, then staged and committed in one shot (submodules are added before the rest of the paths are staged).
- `restore` does the inverse over the *dotty repo's* top-level contents (skipping `.git`/`.gitmodules`), and supports both symlink and file-copy modes; `--overwrite` moves conflicting existing files into a temp dir rather than clobbering them.
- Every fallible operation returns `Result<_, String>` (errors are pre-formatted, human-readable strings) — there's no custom error enum/`thiserror`/`anyhow` in this codebase, so keep new code consistent with that style rather than introducing a new error type.
- Logging uses the `log`/`simplelog` crates; verbosity is controlled by repeated `-v` flags mapped to log levels in `main.rs`.

## Release process (informational, not something to invoke directly)

- PR titles must follow Conventional Commits (enforced by `lint-pr.yaml`); PR type also drives auto-labeling (`label-pr.yaml`).
- Merging a PR labeled `release` (or a manual workflow dispatch) computes the next semver tag and pushes it (`tag.yaml`).
- Pushing a `MAJOR.MINOR.PATCH` tag triggers `release.yaml`, which builds darwin-arm64, darwin-amd64, and linux-amd64 binaries, publishes a GitHub release, and opens a PR against the `homebrew-dotty` tap.
- `build.yaml` runs on every push: builds all three targets plus `cargo clippy -- -D warnings` and `cargo fmt --check`.

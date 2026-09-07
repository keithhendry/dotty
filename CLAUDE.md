# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`dotty` is a Rust CLI for managing dotfiles: it moves files/directories out of a home directory into a git repository (replacing the originals with symlinks) and can restore, sync, and update them later. Git repos passed to `add` are tracked as git submodules rather than plain files.

## Commands

```sh
cargo build                                       # debug build
cargo run -- <args>                               # run locally, e.g. cargo run -- -r ~/.dotty init
cargo test                                        # unit + end-to-end tests
cargo test --test cli                             # end-to-end tests only
cargo clippy --locked --all-targets -- -D warnings  # lint, exactly as CI runs it
cargo fmt --check                                 # format check (enforced in CI)
```

The toolchain is pinned to **stable** in `rust-toolchain.toml`; `rustup` picks it up automatically. Nothing here needs nightly — if a build ever wants `-Z` flags, something has gone wrong. `Cargo.lock` is committed (this is a binary, not a library) and CI builds with `--locked`, so a dependency change must include the updated lockfile.

## Tests

Two layers, and the distinction matters:

- **Unit tests** live inline in `#[cfg(test)] mod tests` at the bottom of each source file, and call the functions directly.
- **End-to-end tests** live in [tests/cli.rs](tests/cli.rs) and run the built binary (`env!("CARGO_BIN_EXE_dotty")`) against throwaway home directories and local `file://` git repos, asserting with the **`git` command line** rather than libgit2 again.

Bugs have repeatedly hidden in the gap between them — process exit codes, `~` expansion, and anything depending on the current working directory only show up when the real binary runs. Prefer adding to both layers when changing behaviour.

Two things to know before writing tests here:

- **Never set `HOME` (or any env var) on the test process.** It is global and unsound with parallel tests. `tests/cli.rs` sets `HOME` per *child process* instead, which is also what keeps tests from touching your real home directory.
- **Canonicalize temp directories in tests.** On macOS `std::env::temp_dir()` sits behind `/var -> /private/var`, and since dotty canonicalizes the paths it is given, raw `TempDir` paths will not compare equal. Both test layers have a helper that does this.

## CLI structure

Entry point is [src/main.rs](src/main.rs), using `clap` derive macros. Global options (`-r/--repository`, defaulting to `~/.dotty` or `$DOTTY_REPOSITORY`; `-R/--root`, the directory dotfiles live relative to) are parsed once in `Opts`, resolved to canonical paths, then dispatched to a subcommand handler in [src/cmds.rs](src/cmds.rs). Each subcommand (`init`, `clone`, `add`, `restore`, `sync`, `update`, `status`) is a thin free function in `cmds.rs` that composes helpers from `src/utils/`.

### Output convention

**stdout is what the command produced; stderr is everything about how it went.**

- A command's result — the confirmation of what changed, or a report like `status` — is `println!`ed to stdout, unconditionally. It is the answer the user asked for, so it is not hidden behind `-v`.
- Everything else goes through `log` to stderr: per-path warnings, errors, and the `-v` diagnostics. The logger is configured with `TerminalMode::Stderr` so nothing it emits can contaminate stdout.

This is what keeps `dotty status > managed.txt` and `dotty restore 2>/dev/null` behaving sensibly. New commands should follow it: print the outcome, log the diagnostics.

The `///` comments on the clap structs are rendered as `--help` text, so editing them changes user-facing output.

`src/utils/` is organized by concern, not by command:
- [src/utils/path.rs](src/utils/path.rs) — path canonicalization (including `~` expansion, and canonicalizing paths that don't exist yet), computing a path's location relative to the dotfiles root, common-base-path calculation for commit messages.
- [src/utils/fs.rs](src/utils/fs.rs) — the actual move/symlink/copy/restore mechanics, including the `move_then_symlink` used by `add` and the `restore` used by `restore` (with an `OverwriteTempDir` RAII helper that stashes displaced files instead of deleting them).
- [src/utils/git.rs](src/utils/git.rs) — all `git2` usage: init/open/clone (with recursive submodule init), staging/committing, submodule add/update, and a hand-rolled fetch+merge+push flow for `sync` (fast-forward when possible, otherwise a real merge commit; conflicts abort with an error rather than being resolved automatically).
- [src/utils/string.rs](src/utils/string.rs) — trivial random string helper used for temp dir names.

Command flow worth understanding before changing `add` or `restore`:
- `add` walks the given paths (`flatten_paths_to_add` in cmds.rs), treating any directory that is itself a git repo as an opaque unit (added as a submodule) rather than descending into it. Everything else is moved into the dotty repo and symlinked back, then staged and committed in one shot (submodules are added before the rest of the paths are staged).
- Paths arrive at `move_to_dotty_repo` already canonicalized, so a path that resolves *into* the repository is one dotty already manages — that check is what stops a second `add` of the same file from burying it a level deeper.
- `restore` does the inverse over the *dotty repo's* top-level contents (skipping `.git`/`.gitmodules`), and supports both symlink and file-copy modes; `--overwrite` moves conflicting existing files into a temp dir rather than clobbering them.
- Every fallible operation returns `Result<_, String>` (errors are pre-formatted, human-readable strings) — there's no custom error enum/`thiserror`/`anyhow` in this codebase, so keep new code consistent with that style rather than introducing a new error type.
- Logging uses the `log`/`simplelog` crates; verbosity is controlled by repeated `-v` flags mapped to log levels in `main.rs`. A failure is logged **and exits non-zero**, so dotty can be scripted.

### `utils::git` path convention

Functions touching the index or submodules take paths **relative to the repository's working directory**, never absolute ones — libgit2 requires it. The trap: a relative path is only meaningful to libgit2's own index. Any plain filesystem or `Repository::open` call must resolve it against `Repository::workdir()` first, or it silently resolves against the *process's* current directory instead.

## Working with this repository

- `main` is protected and **squash-merge only**. Every commit on `main` is a squashed PR whose title is the (Conventional Commits validated) PR title — which is what `get-next-version` reads to compute releases.
- **Do not stack PRs.** Because merges are squashed, a branch built on another unmerged branch will duplicate that branch's commit content once the base is squashed into `main`, and the stacked PR then conflicts. Work one change at a time off `main`.
- Branch protection requires branches to be up to date, so after each merge the next PR needs its branch updated and CI re-run.
- The `build.yaml` **job names are the required status checks**. Renaming a job means updating the branch protection contexts in lockstep, or every PR blocks on a check that never reports.

## CI and release (informational, not something to invoke directly)

- `build.yaml` runs on every push. Each of `Test macOS arm64`, `Test macOS amd64` and `Test Linux amd64` runs the test suite *and* builds a binary for that target — sharing one set of compiled dependencies, which is why the test and build steps pass the same `--target`. `Lint` runs clippy and rustfmt. Cargo builds are cached; release builds deliberately are not.
- PR titles must follow Conventional Commits (enforced by `lint-pr.yaml`); PR type also drives auto-labeling (`label-pr.yaml`). Both must react to `synchronize` as well as `opened`, or their required checks never report on a pushed commit.
- Releasing is **manual and one run**: run `release.yaml` by hand when the commits on `main` add up to a release. It derives the next semantic version from the conventional commits since the last tag (or takes one you supply), pushes that tag, builds darwin-arm64, darwin-amd64 and linux-amd64 binaries from it, publishes a GitHub release, and opens a PR against the `homebrew-dotty` tap. Pushing a `MAJOR.MINOR.PATCH` tag by hand runs the same workflow, skipping the tagging step.
- Tagging and releasing are deliberately one workflow. The built-in `GITHUB_TOKEN` can push a tag but cannot *trigger* another workflow, so splitting them would need a deploy key or app token — a credential that expires, gets revoked, and breaks releases silently. Keep them together.
- Release asset names (`dotty-<version>-<platform>-<arch>.tar.gz`) and the uploaded artifact names are load-bearing: the Homebrew tap builds its download URLs from them. Job *display* names can change; these cannot.

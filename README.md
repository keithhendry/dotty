# dotty

**Your dotfiles, in a git repository, without moving a single file out of place.**

[![Builds](https://github.com/keithhendry/dotty/actions/workflows/build.yaml/badge.svg)](https://github.com/keithhendry/dotty/actions/workflows/build.yaml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Point dotty at a config file and it moves the file into a git repository, then
leaves a symlink where the file used to be. Vim still reads `~/.vimrc`, your
shell still reads `~/.zshrc`, and neither of them knows anything changed — but
now the real file is version controlled, and one command gets it onto your next
machine.

```console
$ dotty add ~/.zshrc ~/.config/nvim

$ ls -l ~/.zshrc
lrwxr-xr-x  1 you  staff  20 Sep  7 11:04 /Users/you/.zshrc -> /Users/you/.dotty/.zshrc
```

That's the whole idea. The repository mirrors your home directory, so what you
see in git is exactly the layout you already know.

## Why dotty

- **Plugins stay plugins.** Point dotty at a directory that is already a git
  repository — a vim plugin, a zsh theme, a tmux plugin manager — and it is
  tracked as a **submodule** instead of being swallowed whole. You get a
  pointer, not three thousand files of somebody else's history. `dotty update`
  upgrades all of them in one go.
- **One binary, nothing else.** dotty is a single static executable with git
  built in via libgit2. No Python, no Ruby, no shell framework to bootstrap, and
  nothing to install on the machine you are setting up.
- **It won't clobber your files.** Restoring onto a machine that already has a
  `.zshrc` is an error, not a silent overwrite. Ask for `--overwrite` and the
  existing files are *moved aside* to a temporary directory rather than deleted.
- **Commits happen for you.** `dotty add` moves the file, creates the symlink
  and writes a sensible commit in one step. `dotty sync` is fetch, merge and
  push together.
- **Take the files and leave.** `dotty restore --mode files` copies real files
  instead of symlinks, for a container or a machine you're only borrowing and
  don't want depending on a repository sticking around.

## Install

### macOS and Linux (Homebrew)

```sh
brew install keithhendry/dotty/dotty
```

### From source

Requires Rust 1.98 or later.

```sh
cargo install --git https://github.com/keithhendry/dotty
```

Prebuilt binaries are attached to every
[release](https://github.com/keithhendry/dotty/releases), and each one is built
and tested on the architecture it ships for:

| Platform | Architecture | Asset |
| --- | --- | --- |
| macOS | Apple silicon (`arm64`) | `dotty-<version>-darwin-arm64.tar.gz` |
| macOS | Intel (`amd64`) | `dotty-<version>-darwin-amd64.tar.gz` |
| Linux | `amd64` | `dotty-<version>-linux-amd64.tar.gz` |

> dotty uses unix symlinks, so macOS and Linux are supported. Windows is not.

## Quick start

On the machine that already has the dotfiles you care about:

```sh
dotty init                                   # create ~/.dotty
dotty add ~/.zshrc ~/.gitconfig ~/.config/nvim
dotty sync https://github.com/you/dotfiles.git   # adopt the remote and push
```

On the next machine:

```sh
dotty clone https://github.com/you/dotfiles.git  # fetch, submodules included
dotty restore                                    # symlink everything into place
```

From then on, `dotty sync` in either direction is enough to keep them together.

## Commands

| Command | What it does |
| --- | --- |
| `dotty init` | Creates an empty dotty repository. Safe to re-run. |
| `dotty clone <url>` | Clones an existing dotty repository and its submodules. Doesn't touch your files — run `restore` when you're ready. |
| `dotty add <paths...>` | Moves files into the repository, symlinks them back, and commits. Directories are expanded into their files; directories that are git repositories become submodules. |
| `dotty restore` | Puts the repository's files back onto the machine. |
| `dotty sync [url]` | Fetches, merges and pushes. Pass a URL to set or change `origin`. |
| `dotty update` | Fast-forwards every submodule to the latest commit on its default branch, and commits the result. |
| `dotty status` | Shows what the repository tracks and how each file stands on this machine. Changes nothing. |
| `dotty remove <paths...>` | Stops managing files: moves the real file back where the symlink was, and commits the removal. Nothing is deleted. |

### `dotty remove`

The inverse of `add`. The file moves out of the repository to where the symlink
was, the symlink goes, and the removal is committed — leaving the machine as
though dotty had never touched it.

```console
$ dotty remove ~/.zshrc
removed .zshrc from ~/.dotty

$ ls -l ~/.zshrc
-rw-r--r--  1 you  staff  20 Sep  7 11:04 /Users/you/.zshrc    # a real file again
```

A submodule comes back with its own history and remote intact. Nothing is ever
deleted: a path dotty doesn't manage, or one whose place is occupied by
something dotty didn't put there, is reported and skipped.

### `dotty status`

```console
$ dotty status
  linked  .config/nvim/init.lua
 missing  .gitconfig_extra
  copied  .profile
conflict  .tmux.conf  (a different file)
conflict  .vimrc  (a broken symlink)
  linked  .zshrc

6 tracked, 1 missing, 2 conflicting
```

| State | Meaning |
| --- | --- |
| `linked` | A symlink into the repository — edits on either side are the same edit. |
| `copied` | A plain copy, as `restore --mode files` leaves behind. |
| `missing` | Tracked, but not on this machine yet. `dotty restore` puts it in place. |
| `conflict` | Something else is in the way; the reason is given. `--overwrite` moves it aside. |

### `dotty restore`

| Option | Default | Description |
| --- | --- | --- |
| `-m, --mode <symlinks\|files>` | `symlinks` | Symlink back to the repository, or copy real files out of it. |
| `-o, --overwrite` | off | Move conflicting files aside into a temporary directory instead of failing. |

```sh
dotty restore                  # symlink everything into place
dotty restore --mode files     # copy instead, leaving no dependency on the repo
dotty restore --overwrite      # stash anything already there, then restore
```

### Global options

| Option | Environment variable | Default | Description |
| --- | --- | --- | --- |
| `-r, --repository <PATH>` | `DOTTY_REPOSITORY` | `~/.dotty` | Where the dotty repository lives. |
| `-R, --root <PATH>` | `DOTTY_ROOT` | the repository's parent directory | The directory your dotfiles are tracked relative to. |
| `-v, --verbose` | | warnings only | Repeatable: `-v` info, `-vv` debug, `-vvv` trace. |

## How it works

A dotfile has the same path under your home directory as it does inside the
repository, and that single fact is the whole model:

```
~/.zshrc                 ->  ~/.dotty/.zshrc
~/.config/nvim/init.lua  ->  ~/.dotty/.config/nvim/init.lua
```

`add` moves a file along that mapping and symlinks it back. `restore` walks the
mapping in the other direction. Because the repository is an ordinary git
repository, anything dotty doesn't do you can still do with `git` directly —
inspect it, rebase it, or hand it to someone else.

By default the repository lives at `~/.dotty` and the root is its parent, `~`.
Point `--repository` and `--root` somewhere else and the same rules apply, so
you can keep a repository of project configs somewhere entirely different.

**Authentication** reuses what git already has. SSH remotes
(`git@github.com:you/dotfiles.git`) authenticate with a key from your running
ssh agent, and HTTPS remotes go through your configured git credential helper.
If `git push` works for a remote, `dotty sync` should too.

**`dotty sync` refuses to run on a dirty working tree,** and stops at a merge
conflict rather than guessing — the conflict is left checked out for you to
resolve with git as usual.

## Development

```sh
cargo build                                     # build
cargo test                                      # run the test suite
cargo clippy --all-targets -- -D warnings       # lint
cargo fmt                                       # format
```

Pull request titles follow [Conventional Commits](https://www.conventionalcommits.org/),
which is what drives release labelling and versioning.

## License

[MIT](LICENSE)

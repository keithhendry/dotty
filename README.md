# dotty

A simple dotfiles utility that manages dotfiles as a git repository.

`dotty` moves the files and directories you point it at into a git repository, then
replaces the originals with symlinks back to that repository. This gives you a single
git repo you can commit, push, and clone onto other machines, while the files still
appear in their normal locations on disk. Directories that are themselves git
repositories (e.g. vim/tmux plugin managers) are tracked as git submodules instead of
being flattened.

## Installation

### macOS (Homebrew)

```sh
brew install keithhendry/dotty/dotty
```

This taps [keithhendry/homebrew-dotty](https://github.com/keithhendry/homebrew-dotty)
and installs `dotty`.

### From source

```sh
cargo install --git https://github.com/keithhendry/dotty
```

## Usage

```
dotty [OPTIONS] <SUBCOMMAND>
```

### Global options

| Option | Env var | Default | Description |
| --- | --- | --- | --- |
| `-r, --repository <PATH>` | `DOTTY_REPOSITORY` | `~/.dotty` | Path to the dotty git repository |
| `-R, --root <PATH>` | `DOTTY_ROOT` | parent directory of the repository | Root directory that dotfiles live relative to |
| `-v, --verbose` | | | Increase log verbosity, repeatable (`-v`, `-vv`, `-vvv`) |

### Subcommands

#### `init`

Initializes a new, empty dotty repository at `--repository`.

```sh
dotty init
```

#### `clone <url>`

Clones an existing dotty repository (and its submodules, recursively) from `<url>`
into `--repository`.

```sh
dotty clone git@github.com:you/dotfiles.git
```

#### `add <paths...>`

Moves the given files/directories into the dotty repository, replaces each with a
symlink pointing back into the repository, and commits the result. Paths must live
under `--root`. Directories that are git repositories are added as submodules rather
than being copied file by file.

```sh
dotty add ~/.vimrc ~/.config/nvim ~/.gitconfig
```

#### `restore`

Restores everything currently tracked in the dotty repository back into `--root`, by
default as symlinks.

```sh
dotty restore                    # create symlinks
dotty restore --mode files       # copy files instead of symlinking
dotty restore --overwrite        # replace conflicting existing files/symlinks
```

`--overwrite` moves any conflicting files aside into a temp directory rather than
deleting them.

#### `sync [url]`

Fetches, merges, and pushes the dotty repository against its `origin` remote (or
`[url]`, which is set as `origin` if given). Fails if there are unstaged changes or
merge conflicts.

```sh
dotty sync
dotty sync git@github.com:you/dotfiles.git
```

#### `update`

Fetches and fast-forwards every submodule in the dotty repository to its remote
default branch, then commits the update.

```sh
dotty update
```

## Example workflow

```sh
# On your first machine
dotty init
dotty add ~/.zshrc ~/.gitconfig ~/.config/nvim
dotty sync git@github.com:you/dotfiles.git

# On another machine
dotty clone git@github.com:you/dotfiles.git
dotty restore
```

## License

[MIT](LICENSE)

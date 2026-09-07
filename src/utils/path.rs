//! Working out where files live, and where they belong in the repository.
//!
//! Paths reach dotty from three places — the command line, the `DOTTY_*`
//! environment variables and the contents of the repository itself — in all
//! sorts of shapes: relative, tilde-prefixed, symlinked or not yet existing.
//! Everything here exists to reduce them to one comparable form, because the
//! rest of the program relies on being able to strip the root prefix off a
//! path to decide where its counterpart belongs.

use std::env;
use std::path::{Component, Path, PathBuf};

/// Expands a leading `~` and resolves `path` to an absolute, symlink-free form.
///
/// Unlike [`Path::canonicalize`], `path` does not need to exist: the deepest
/// ancestor that does exist is canonicalized and the missing components are
/// appended to it. That matters because dotty is routinely handed paths that
/// have not been created yet, such as `~/.dotty` on the very first `dotty init`.
pub fn canonicalize(path: &Path) -> Result<PathBuf, String> {
    let tilde_expanded = expand_home(path)?;
    let canonical = canonicalize_missing(&tilde_expanded)?;
    if !canonical.eq(path) {
        log::trace!(
            "canonicalized form of {} is {}",
            path.display(),
            canonical.display()
        );
    }
    Ok(canonical)
}

/// Expands a leading `~` to the current user's home directory.
///
/// Only the user's own `~` is expanded. `~someone-else` is rejected rather than
/// quietly taken as a directory literally named that, which is what looking it
/// up would otherwise hide.
fn expand_home(path: &Path) -> Result<PathBuf, String> {
    expand_home_from(path, env::home_dir())
}

/// The part of [`expand_home`] that does not read the environment, so that it
/// can be tested without mutating it.
fn expand_home_from(path: &Path, home: Option<PathBuf>) -> Result<PathBuf, String> {
    let mut components = path.components();
    let first = match components.next() {
        Some(Component::Normal(first)) => first.to_str().unwrap_or_default(),
        // Anything else — a root, a prefix, `.` or `..` — cannot start with a
        // tilde, so there is nothing to expand.
        _ => return Ok(path.to_owned()),
    };

    if !first.starts_with('~') {
        return Ok(path.to_owned());
    }
    if first != "~" {
        return Err(format!(
            "cannot expand {} - only a leading ~ for your own home directory is supported",
            path.display()
        ));
    }

    match home {
        Some(home) => Ok(home.join(components.as_path())),
        None => Err(format!(
            "cannot expand ~ in {} - the home directory is unknown; set HOME or give the full path",
            path.display()
        )),
    }
}

/// Canonicalizes `path`, walking up to the first ancestor that exists.
///
/// Only the existing ancestor can be resolved by the OS, so the components
/// below it are re-joined one at a time as the recursion unwinds. They must be
/// joined by file name: joining the full (absolute) path instead would discard
/// the canonicalized prefix entirely, silently leaving symlinks unresolved.
fn canonicalize_missing(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return match path.canonicalize() {
            Ok(canonical) => Ok(canonical),
            Err(err) => {
                return Err(format!(
                    "failed to get the canonical path of {} - {}",
                    path.display(),
                    err
                ))
            }
        };
    }

    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok(canonicalize_missing(parent)?.join(name)),
        _ => Err(format!(
            "failed to get the canonical path of {} - unknown parent",
            path.display()
        )),
    }
}

/// Decides which directory dotfile paths are tracked relative to.
///
/// An explicitly configured `root` always wins. Otherwise the repository's
/// parent directory is used, which puts a repository at `~/.dotty` in charge of
/// `~` — the arrangement dotty is designed around.
pub fn get_root(root: Option<&Path>, repo: &Path) -> Result<PathBuf, String> {
    if let Some(path) = root {
        log::trace!("using specified root {}", path.display());
        return Ok(path.to_owned());
    }

    if let Some(path) = repo.parent() {
        log::trace!(
            "using repository parent directory as root {}",
            path.display()
        );
        return Ok(path.to_owned());
    }

    Err(format!(
        "cannot get parent of repository path {}",
        repo.display()
    ))
}

/// Strips `root` from `path`, yielding the location shared by both copies.
///
/// A dotfile has the same relative path under the root and under the
/// repository, so this one value locates the original file, its counterpart in
/// the repository, and its entry in git.
///
/// Both arguments must already be canonical, and `path` must live below `root`
/// and not be `root` itself — the repository cannot contain itself.
pub fn relative_from_root(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let relative = match path.strip_prefix(root) {
        Ok(sub_path) => sub_path.to_owned(),
        Err(err) => {
            return Err(format!(
                "the path {} must be a child of {} - {}",
                path.display(),
                root.display(),
                err
            ))
        }
    };

    if relative.components().next().is_none() {
        return Err(format!("cannot add repository path {}", path.display()));
    }

    Ok(relative)
}

/// Returns the deepest directory that every path in `paths` sits under.
///
/// Used to summarise a multi-file commit ("adding 12 files to .config"). Paths
/// sharing no components at all yield an empty path, and absolute paths always
/// share at least the root.
pub fn common_base_path(paths: &[PathBuf]) -> PathBuf {
    paths.iter().fold(PathBuf::new(), |accum, item| {
        if accum.as_os_str().is_empty() {
            return item.to_owned();
        }
        let mut common = PathBuf::new();
        for (left, right) in accum.components().zip(item.components()) {
            if left.eq(&right) {
                common.push(left);
            } else {
                break;
            }
        }
        common
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn expand_home_replaces_a_leading_tilde() {
        let home = Some(PathBuf::from("/home/alice"));

        assert_eq!(
            expand_home_from(Path::new("~"), home.clone()).unwrap(),
            PathBuf::from("/home/alice")
        );
        assert_eq!(
            expand_home_from(Path::new("~/.dotty"), home).unwrap(),
            PathBuf::from("/home/alice/.dotty")
        );
    }

    #[test]
    fn expand_home_leaves_other_paths_alone() {
        let home = Some(PathBuf::from("/home/alice"));

        for path in ["/etc/hosts", "relative/path", "./here", "../up"] {
            assert_eq!(
                expand_home_from(Path::new(path), home.clone()).unwrap(),
                PathBuf::from(path),
                "{path} should be untouched"
            );
        }
    }

    // A tilde only means home at the very start of a path.
    #[test]
    fn expand_home_ignores_a_tilde_further_along() {
        let home = Some(PathBuf::from("/home/alice"));

        assert_eq!(
            expand_home_from(Path::new("/etc/~weird"), home).unwrap(),
            PathBuf::from("/etc/~weird")
        );
    }

    #[test]
    fn expand_home_rejects_another_users_home() {
        let home = Some(PathBuf::from("/home/alice"));

        let err = expand_home_from(Path::new("~bob/.dotty"), home).unwrap_err();

        assert!(err.contains("only a leading ~"), "got: {err}");
    }

    #[test]
    fn expand_home_reports_an_unknown_home_directory() {
        let err = expand_home_from(Path::new("~/.dotty"), None).unwrap_err();

        assert!(err.contains("home directory is unknown"), "got: {err}");
    }

    #[test]
    fn canonicalize_resolves_existing_path() {
        let dir = tempdir().unwrap();
        let canonical = canonicalize(dir.path()).unwrap();
        assert_eq!(canonical, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_resolves_missing_nested_path() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does/not/exist");

        let canonical = canonicalize(&missing).unwrap();

        assert_eq!(
            canonical,
            dir.path().canonicalize().unwrap().join("does/not/exist")
        );
    }

    #[test]
    fn canonicalize_resolves_symlinked_ancestor_of_missing_path() {
        // On macOS, std::env::temp_dir() lives under a symlink (/var -> /private/var),
        // so this exercises canonicalizing a missing path whose *existing* ancestor
        // itself needs symlink resolution.
        let dir = tempdir().unwrap();
        let canonical_dir = dir.path().canonicalize().unwrap();
        if canonical_dir == dir.path() {
            return;
        }
        let missing = dir.path().join("does/not/exist");

        let canonical = canonicalize(&missing).unwrap();

        assert_eq!(canonical, canonical_dir.join("does/not/exist"));
    }

    #[test]
    fn get_root_prefers_explicit_root() {
        let repo = PathBuf::from("/repo");
        let root = PathBuf::from("/explicit/root");
        assert_eq!(get_root(Some(&root), &repo).unwrap(), root);
    }

    #[test]
    fn get_root_falls_back_to_repo_parent() {
        let repo = PathBuf::from("/home/user/.dotty");
        assert_eq!(get_root(None, &repo).unwrap(), PathBuf::from("/home/user"));
    }

    #[test]
    fn get_root_errors_when_repo_has_no_parent() {
        let repo = PathBuf::from("/");
        assert!(get_root(None, &repo).is_err());
    }

    #[test]
    fn relative_from_root_strips_prefix() {
        let root = PathBuf::from("/home/user");
        let path = PathBuf::from("/home/user/.vimrc");
        assert_eq!(
            relative_from_root(&root, &path).unwrap(),
            PathBuf::from(".vimrc")
        );
    }

    #[test]
    fn relative_from_root_errors_when_not_under_root() {
        let root = PathBuf::from("/home/user");
        let path = PathBuf::from("/etc/hosts");
        assert!(relative_from_root(&root, &path).is_err());
    }

    #[test]
    fn relative_from_root_errors_when_path_is_root() {
        let root = PathBuf::from("/home/user");
        assert!(relative_from_root(&root, &root).is_err());
    }

    #[test]
    fn common_base_path_finds_shared_prefix() {
        let paths = vec![
            PathBuf::from("/home/user/.config/nvim"),
            PathBuf::from("/home/user/.config/tmux"),
        ];
        assert_eq!(
            common_base_path(&paths),
            PathBuf::from("/home/user/.config")
        );
    }

    #[test]
    fn common_base_path_of_single_path_is_itself() {
        let paths = vec![PathBuf::from("/home/user/.vimrc")];
        assert_eq!(common_base_path(&paths), PathBuf::from("/home/user/.vimrc"));
    }

    #[test]
    fn common_base_path_of_empty_is_empty() {
        let paths: Vec<PathBuf> = vec![];
        assert_eq!(common_base_path(&paths), PathBuf::new());
    }

    #[test]
    fn common_base_path_with_no_shared_prefix_is_root() {
        let paths = vec![PathBuf::from("/home/alice"), PathBuf::from("/etc/hosts")];
        assert_eq!(common_base_path(&paths), PathBuf::from("/"));
    }
}

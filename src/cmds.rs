//! One function per subcommand, composing the helpers in [`crate::utils`].
//!
//! Every command takes `repo` — the dotty repository — and, where it needs to
//! know about the machine's own files, `root`, the directory dotfiles are
//! tracked relative to (usually the user's home directory). Both are absolute,
//! canonical paths by the time they arrive here; `main` resolves them once.

use crate::utils::fs;
use crate::utils::git;
use crate::utils::path;
use std::path::{Path, PathBuf};

/// Creates an empty dotty repository, or leaves an existing one alone.
pub fn init(repo: &Path) -> Result<(), String> {
    git::init_or_open(repo)?;

    println!("initialized dotty repository {}", repo.display());
    Ok(())
}

/// Clones an existing dotty repository, submodules included.
///
/// This only fetches the repository. Nothing is put in place on the machine
/// until [`restore`] is run, so a clone is safe to inspect first.
pub fn clone(repo: &Path, url: &str) -> Result<(), String> {
    git::clone_recurse(repo, url)?;
    // Check that it is a valid dotty repository
    println!("cloned {} into {}", url, repo.display());
    Ok(())
}

/// Takes the given files under dotty's management and commits them.
///
/// Each path is moved into the repository and replaced with a symlink pointing
/// back at it, so the files stay exactly where the programs that read them
/// expect. Directories are expanded into their individual files, except for
/// directories that are git repositories of their own — those become
/// submodules, which is how a vim plugin or a zsh theme is tracked without
/// absorbing its history.
///
/// A path that is already managed is skipped, and a path that cannot be added
/// is logged and stepped over rather than aborting the whole run. Everything
/// that did move lands in a single commit.
pub fn add(repo: &Path, root: &Path, paths: &Vec<PathBuf>) -> Result<(), String> {
    let flattened = flatten_paths_to_add(paths)?;

    // Everything past this point rearranges real files, so whatever can be
    // known to fail is established first. Discovering the repository does not
    // exist, or that git has no identity to commit with, used to happen after
    // the files had been moved and symlinked -- leaving them stranded, and a
    // retry doing nothing because they now looked like they were managed.
    let git_repo = git::open(repo)?;
    git::check_signature(&git_repo)?;

    let mut to_commit: Vec<PathBuf> = Vec::new();
    let mut submodules: Vec<PathBuf> = Vec::new();

    for (path, path_type) in flattened {
        // A submodule records where it came from, so one with no origin cannot
        // be added at all. Finding that out before moving it keeps it from
        // taking the rest of the run down with it.
        if path_type == PathType::GitRepo {
            if let Err(err) = git::origin_url(&path) {
                log::warn!("skipping {} - {}", path.display(), err);
                continue;
            }
        }

        match move_to_dotty_repo(&git_repo, repo, root, &path) {
            Ok(Some(relative_path)) => {
                if path_type == PathType::GitRepo {
                    submodules.push(relative_path.clone())
                }
                to_commit.push(relative_path);
            }
            Ok(None) => log::debug!("{} already added.", path.display()),
            Err(err) => log::warn!(
                "failed to add {} to repo {} - {}",
                path.display(),
                repo.display(),
                err
            ),
        }
    }

    if !to_commit.is_empty() {
        git::unstage_all(&git_repo)?;
        git::add_submodules(&git_repo, &submodules)?;
        git::stage_all_paths(&git_repo, &to_commit)?;
        git::commit(&git_repo, &build_git_message(&to_commit))?;

        println!(
            "added {} to {}",
            if to_commit.len() == 1 {
                to_commit.first().unwrap().display().to_string()
            } else {
                format!("{} paths", to_commit.len())
            },
            repo.display()
        );
    }

    Ok(())
}

/// Stops managing the given paths, putting the real files back where the
/// symlinks were.
///
/// The exact inverse of [`add`]: the file moves out of the repository to the
/// place it was taken from, the symlink goes, and the removal is committed.
/// Nothing is deleted, so the machine is left as though dotty had never
/// touched it.
///
/// A path that is not managed, or whose place on the machine is occupied by
/// something dotty did not put there, is reported and skipped rather than
/// taking the rest of the run with it.
pub fn remove(repo: &Path, root: &Path, paths: &Vec<PathBuf>) -> Result<(), String> {
    let git_repo = git::open(repo)?;
    git::check_signature(&git_repo)?;

    let mut removed: Vec<PathBuf> = Vec::new();
    let mut submodules: Vec<PathBuf> = Vec::new();

    for path in paths {
        if !path.exists() {
            log::warn!("skipping {} - it does not exist", path.display());
            continue;
        }
        let canonical = path::canonicalize(path)?;

        // A managed dotfile is a symlink into the repository, so canonicalizing
        // it lands on the repository's copy whichever of the two the user named.
        let relative_path = match canonical.strip_prefix(repo) {
            Ok(relative) => relative.to_owned(),
            Err(_) => {
                log::warn!("skipping {} - it is not managed by dotty", path.display());
                continue;
            }
        };

        if !git::is_committed(&git_repo, &relative_path) {
            log::warn!("skipping {} - it is not tracked", path.display());
            continue;
        }

        match move_out_of_dotty_repo(repo, root, &relative_path) {
            Ok(()) => {
                if git::is_submodule(&git_repo, &relative_path) {
                    submodules.push(relative_path.clone());
                }
                removed.push(relative_path);
            }
            Err(err) => log::warn!("failed to remove {} - {}", path.display(), err),
        }
    }

    if removed.is_empty() {
        return Ok(());
    }

    git::unstage_all(&git_repo)?;

    // .gitmodules is dotty's bookkeeping rather than one of the user's
    // dotfiles, so it is staged alongside them but kept out of what gets
    // reported back.
    let mut to_unstage = removed.clone();
    if !submodules.is_empty() {
        for submodule in &submodules {
            git::deregister_submodule(&git_repo, submodule)?;
        }
        let modules = PathBuf::from(".gitmodules");
        if git::tidy_gitmodules(&git_repo)? {
            git::stage_all_paths(&git_repo, &vec![modules])?;
        } else {
            to_unstage.push(modules);
        }
    }

    git::unstage_paths(&git_repo, &to_unstage)?;
    git::commit(&git_repo, &build_removal_message(&removed))?;

    println!(
        "removed {} from {}",
        if removed.len() == 1 {
            removed.first().unwrap().display().to_string()
        } else {
            format!("{} paths", removed.len())
        },
        repo.display()
    );
    Ok(())
}

/// Puts every file the repository tracks back onto the machine.
///
/// This is the other half of [`add`], and the command you run on a new machine
/// after [`clone`]. With `symlinks` the files are linked back to the
/// repository, so future edits are picked up by `dotty sync`; without it they
/// are copied, leaving a machine that does not depend on the repository
/// sticking around.
///
/// `overwrite` decides what happens to files already on the machine: with it,
/// they are moved into a temporary directory that is reported in the logs;
/// without it, a conflict is an error and nothing is touched.
pub fn restore(repo: &Path, root: &Path, symlinks: bool, overwrite: bool) -> Result<(), String> {
    let overwrite = match overwrite {
        true => Some(fs::create_overwrite_temp_dir("dotty-")?),
        false => None,
    };

    let paths_to_restore = repository_contents(repo)?;

    // One awkward file should not decide the fate of the rest. Failing on the
    // first one left the machine half configured, with no account of what had
    // and had not been put in place; `add` has always reported and carried on.
    let total = paths_to_restore.len();
    let mut failed = 0;

    for from in paths_to_restore {
        let relative_path = path::relative_from_root(repo, &from)?;
        let to = root.join(&relative_path);
        let overwrite_entry = overwrite.as_ref().map(|o| o.entry(&relative_path));
        log::debug!("restoring {} to {}", from.display(), to.display());
        if let Err(err) = fs::restore(&from, &to, overwrite_entry.as_deref(), symlinks) {
            log::warn!("{}", err);
            failed += 1;
        }
    }

    if let Some(overwrite) = overwrite.as_ref() {
        if !fs::is_empty(overwrite.path())? {
            log::warn!(
                "files already on this machine were moved to {}",
                overwrite.path().display()
            );
        }
    }

    if failed > 0 {
        return Err(format!(
            "restored {} of {} paths to {}; {} could not be restored",
            total - failed,
            total,
            root.display(),
            failed
        ));
    }

    println!(
        "restored {} paths to {} by {}",
        total,
        root.display(),
        match symlinks {
            true => "creating symlinks",
            false => "copying files",
        }
    );
    Ok(())
}

/// Reports what the repository tracks and how each dotfile stands on this
/// machine.
///
/// Reads only. Unlike the other commands this writes to stdout rather than the
/// log, because the report *is* the output rather than a note about progress.
pub fn status(repo: &Path, root: &Path) -> Result<(), String> {
    let contents = repository_contents(repo)?;
    if contents.is_empty() {
        println!("{} tracks nothing yet", repo.display());
        return Ok(());
    }

    let mut rows: Vec<(String, PathBuf, Option<String>)> = Vec::new();
    let mut missing = 0;
    let mut conflicting = 0;

    for from in contents {
        let relative_path = path::relative_from_root(repo, &from)?;
        let to = root.join(&relative_path);
        let (state, note) = match fs::placement(&from, &to)? {
            fs::Placement::Linked => ("linked", None),
            fs::Placement::Copied => ("copied", None),
            fs::Placement::Missing => {
                missing += 1;
                ("missing", None)
            }
            fs::Placement::Conflict(reason) => {
                conflicting += 1;
                ("conflict", Some(reason))
            }
        };
        rows.push((state.to_owned(), relative_path, note));
    }

    rows.sort_by(|left, right| left.1.cmp(&right.1));
    for (state, relative_path, note) in &rows {
        match note {
            Some(note) => println!("{:>8}  {}  ({})", state, relative_path.display(), note),
            None => println!("{:>8}  {}", state, relative_path.display()),
        }
    }

    println!();
    println!(
        "{} tracked, {} missing, {} conflicting",
        rows.len(),
        missing,
        conflicting
    );
    Ok(())
}

/// Fetches, merges and pushes the repository, adopting `url` as `origin` if
/// one is given.
pub fn sync(repo: &Path, url: Option<&str>) -> Result<(), String> {
    let git_repo = git::open(repo)?;
    git::sync(&git_repo, url)?;
    println!("synced {}", repo.display());
    Ok(())
}

/// Upgrades every submodule to the latest commit on its default branch, and
/// commits the new pointers.
///
/// This is how plugins and themes tracked as submodules get updated. When there
/// are no submodules there is nothing to commit, and the run is a no-op.
pub fn update(repo: &Path) -> Result<(), String> {
    let git_repo = git::open(repo)?;

    git::unstage_all(&git_repo)?;
    let outcome = git::update_submodules(&git_repo)?;

    // Committing whenever there are submodules at all, rather than when one
    // actually moved, wrote an empty commit on every run.
    if outcome.changed > 0 {
        git::commit(&git_repo, "Updated all submodules")?;
        println!("updated {} submodules", outcome.changed);
    } else if outcome.total > 0 {
        println!("all {} submodules are already up to date", outcome.total);
    } else {
        println!("there are no submodules to update");
    }

    Ok(())
}

/// Every dotfile the repository holds, as absolute paths.
///
/// git's own bookkeeping is skipped, and a submodule counts as a single path
/// rather than being walked into, matching how it was added.
fn repository_contents(repo: &Path) -> Result<Vec<PathBuf>, String> {
    let top_level: Vec<PathBuf> = fs::read_dir(repo)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| !matches!(name, ".git" | ".gitmodules"))
                .unwrap_or(true)
        })
        .collect();

    Ok(flatten_paths_to_add(&top_level)?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

/// What a path turned out to be, which decides how it gets tracked.
#[derive(PartialEq)]
enum PathType {
    /// Tracked as an ordinary file in the repository.
    File,
    /// Tracked as a submodule, keeping its own history separate.
    GitRepo,
}

/// Expands the requested paths into the individual things that will be tracked.
///
/// Directories are walked into and reduced to their files, so the repository
/// mirrors the real layout. The walk stops at any directory that is a git
/// repository: that is recorded as a whole, to become a submodule.
///
/// Returns an error if any requested path does not exist.
fn flatten_paths_to_add(paths: &Vec<PathBuf>) -> Result<Vec<(PathBuf, PathType)>, String> {
    let mut path_stack = Vec::new();

    for path in paths {
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        path_stack.push(path::canonicalize(path)?)
    }

    let mut flattened: Vec<(PathBuf, PathType)> = Vec::new();

    while let Some(path) = path_stack.pop() {
        if path.is_dir() {
            if git::check_open(&path) {
                flattened.push((path, PathType::GitRepo))
            } else {
                path_stack.append(&mut fs::read_dir(&path)?)
            }
        } else {
            flattened.push((path, PathType::File))
        }
    }

    Ok(flattened)
}

/// Moves one path into the repository and symlinks it back.
///
/// Returns the path relative to the root — the form everything downstream
/// wants — or `None` when the file was already managed and nothing moved.
fn move_to_dotty_repo(
    git_repo: &git2::Repository,
    repo: &Path,
    root: &Path,
    path: &Path,
) -> Result<Option<PathBuf>, String> {
    // `path` arrives canonicalized, so anything inside the repository is
    // already there: adding a file a second time follows the symlink left by
    // the first add straight back here. Moving it again would bury it one level
    // deeper on every run.
    if path.starts_with(repo) {
        let relative_path = path::relative_from_root(repo, path)?;
        // Being in the repository is not the same as being committed. A file
        // left behind by an add that failed before committing belongs in this
        // commit, which is what makes running the command again put things
        // right rather than silently doing nothing.
        if git::is_committed(git_repo, &relative_path) {
            log::debug!("{} is already managed", path.display());
            return Ok(None);
        }
        log::debug!(
            "{} is in the repository but was never committed",
            path.display()
        );
        return Ok(Some(relative_path));
    }

    let relative_path = path::relative_from_root(root, path)?;
    let to = repo.join(&relative_path);

    log::debug!(
        "moving {} to {} and then replacing with symlink",
        path.display(),
        to.display()
    );
    Ok(match fs::move_then_symlink(path, &to)? {
        true => Some(relative_path),
        false => None,
    })
}

/// Moves a managed file out of the repository, back to where it came from.
///
/// The symlink dotty left behind is removed first; anything else sitting in
/// that place is left alone and reported, since it is not dotty's to discard.
fn move_out_of_dotty_repo(repo: &Path, root: &Path, relative_path: &Path) -> Result<(), String> {
    let from = repo.join(relative_path);
    let to = root.join(relative_path);

    match fs::placement(&from, &to)? {
        fs::Placement::Linked => fs::remove_symlink(&to)?,
        fs::Placement::Missing => {}
        fs::Placement::Copied => {
            return Err(format!(
                "{} is a copy rather than a link; remove it by hand if you meant to",
                to.display()
            ))
        }
        fs::Placement::Conflict(reason) => return Err(format!("{} is {}", to.display(), reason)),
    }

    log::debug!("moving {} back to {}", from.display(), to.display());
    fs::move_path(&from, &to)
}

/// Builds the commit message for a `remove`.
fn build_removal_message(removed: &Vec<PathBuf>) -> String {
    match removed.len() {
        0 => String::default(),
        1 => format!("removing {}", removed.first().unwrap().display()),
        _ => {
            let mut msg = format!("removing {} files\n\n", removed.len());
            for path in removed {
                msg.push_str(&format!("- {}\n", path.display()));
            }
            msg
        }
    }
}

/// Builds the commit message for an `add`.
///
/// A single file is named directly; several are summarised by the directory
/// they share, with the full list in the commit body. Files sharing no
/// directory at all are summarised by count alone.
fn build_git_message(to_commit: &Vec<PathBuf>) -> String {
    match to_commit.len() {
        0 => String::default(),
        1 => format!("adding {}", to_commit.first().unwrap().display()),
        _ => {
            let base = path::common_base_path(to_commit);
            let mut msg = match base.as_os_str().is_empty() {
                true => format!("adding {} files\n\n", to_commit.len()),
                false => format!("adding {} files to {}\n\n", to_commit.len(), base.display()),
            };
            for path in to_commit {
                msg.push_str(&format!("- {}\n", path.display()));
            }
            msg
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::git;
    use tempfile::{tempdir, TempDir};

    // tempfile's TempDir path can itself sit behind a symlink (e.g. macOS's
    // /var -> /private/var). Real callers always canonicalize `repo`/`root` up
    // front (see main.rs), so tests mirror that instead of using raw paths.
    fn canonical_tempdir() -> (TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    }

    fn configure_signature(repo_path: &Path) {
        let repo = git::open(repo_path).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
    }

    #[test]
    fn init_creates_a_git_repository() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");

        init(&repo_dir).unwrap();

        assert!(repo_dir.join(".git").exists());
    }

    #[test]
    fn add_moves_a_file_into_the_repo_and_symlinks_it_back() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        let root_dir = dir.join("root");
        std::fs::create_dir_all(&root_dir).unwrap();
        init(&repo_dir).unwrap();
        configure_signature(&repo_dir);

        let vimrc = root_dir.join(".vimrc");
        std::fs::write(&vimrc, "content").unwrap();

        add(&repo_dir, &root_dir, &vec![vimrc.clone()]).unwrap();

        assert_eq!(std::fs::read_link(&vimrc).unwrap(), repo_dir.join(".vimrc"));
        assert_eq!(
            std::fs::read_to_string(repo_dir.join(".vimrc")).unwrap(),
            "content"
        );
        let git_repo = git::open(&repo_dir).unwrap();
        let head_commit = git_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_commit.message().unwrap(), "adding .vimrc");
    }

    #[test]
    fn add_flattens_a_plain_directory_into_individual_files() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        let root_dir = dir.join("root");
        init(&repo_dir).unwrap();
        configure_signature(&repo_dir);

        let nested = root_dir.join(".config/app");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("settings.toml"), "content").unwrap();

        add(&repo_dir, &root_dir, &vec![root_dir.join(".config")]).unwrap();

        let repo_file = repo_dir.join(".config/app/settings.toml");
        assert_eq!(std::fs::read_to_string(&repo_file).unwrap(), "content");
        assert_eq!(
            std::fs::read_link(nested.join("settings.toml")).unwrap(),
            repo_file
        );
    }

    // Regression test: add_submodules used to open the submodule at a path
    // relative to the process's cwd instead of the parent repo's workdir,
    // which broke `add` for git-repo paths whenever dotty wasn't invoked with
    // its cwd set to the dotty repository itself.
    #[test]
    fn add_tracks_a_nested_git_repository_as_a_submodule() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        let root_dir = dir.join("root");
        init(&repo_dir).unwrap();
        configure_signature(&repo_dir);

        let plugin_dir = root_dir.join(".vim/plugged/foo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        init(&plugin_dir).unwrap();
        configure_signature(&plugin_dir);
        std::fs::write(plugin_dir.join("plugin.vim"), "\" plugin").unwrap();
        let plugin_repo = git::open(&plugin_dir).unwrap();
        git::stage_all_paths(&plugin_repo, &vec![PathBuf::from("plugin.vim")]).unwrap();
        git::commit(&plugin_repo, "initial").unwrap();
        plugin_repo
            .remote("origin", &format!("file://{}", plugin_dir.display()))
            .unwrap();

        add(&repo_dir, &root_dir, &vec![plugin_dir.clone()]).unwrap();

        assert!(repo_dir.join(".gitmodules").exists());
        assert_eq!(
            std::fs::read_link(&plugin_dir).unwrap(),
            repo_dir.join(".vim/plugged/foo")
        );
    }

    // Adding a file that is already managed used to follow the symlink from the
    // first add back into the repository and move the file a level deeper,
    // leaving a nested .dotty/.dotty tracked in git.
    #[test]
    fn add_leaves_an_already_managed_file_where_it_is() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        let root_dir = dir.join("root");
        std::fs::create_dir_all(&root_dir).unwrap();
        init(&repo_dir).unwrap();
        configure_signature(&repo_dir);
        let vimrc = root_dir.join(".vimrc");
        std::fs::write(&vimrc, "content").unwrap();
        add(&repo_dir, &root_dir, &vec![vimrc.clone()]).unwrap();

        add(&repo_dir, &root_dir, &vec![vimrc.clone()]).unwrap();

        assert_eq!(std::fs::read_link(&vimrc).unwrap(), repo_dir.join(".vimrc"));
        assert!(
            !repo_dir.join("repo").exists(),
            "the repository should not contain a copy of itself"
        );
        let git_repo = git::open(&repo_dir).unwrap();
        let head = git_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            head.parent_count(),
            0,
            "the second add had nothing to do, so it should not have committed"
        );
    }

    #[test]
    fn restore_recreates_symlinks_from_the_repo_into_root() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        let root_dir = dir.join("root");
        init(&repo_dir).unwrap();
        std::fs::create_dir_all(repo_dir.join(".config")).unwrap();
        std::fs::write(repo_dir.join(".config/app.toml"), "content").unwrap();

        restore(&repo_dir, &root_dir, true, false).unwrap();

        assert_eq!(
            std::fs::read_link(root_dir.join(".config/app.toml")).unwrap(),
            repo_dir.join(".config/app.toml")
        );
    }

    #[test]
    fn restore_with_overwrite_moves_conflicting_files_aside() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        let root_dir = dir.join("root");
        init(&repo_dir).unwrap();
        std::fs::write(repo_dir.join(".vimrc"), "new content").unwrap();
        std::fs::create_dir_all(&root_dir).unwrap();
        std::fs::write(root_dir.join(".vimrc"), "old content").unwrap();

        restore(&repo_dir, &root_dir, true, true).unwrap();

        assert_eq!(
            std::fs::read_link(root_dir.join(".vimrc")).unwrap(),
            repo_dir.join(".vimrc")
        );
    }

    #[test]
    fn restore_without_overwrite_errors_on_conflicting_files() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        let root_dir = dir.join("root");
        init(&repo_dir).unwrap();
        std::fs::write(repo_dir.join(".vimrc"), "new content").unwrap();
        std::fs::create_dir_all(&root_dir).unwrap();
        std::fs::write(root_dir.join(".vimrc"), "old content").unwrap();

        assert!(restore(&repo_dir, &root_dir, true, false).is_err());
    }

    #[test]
    fn update_is_a_noop_when_there_are_no_submodules() {
        let (_dir, dir) = canonical_tempdir();
        let repo_dir = dir.join("repo");
        init(&repo_dir).unwrap();
        configure_signature(&repo_dir);

        update(&repo_dir).unwrap();

        let git_repo = git::open(&repo_dir).unwrap();
        assert!(git_repo.head().is_err());
    }

    #[test]
    fn build_git_message_for_a_single_file() {
        let paths = vec![PathBuf::from(".vimrc")];
        assert_eq!(build_git_message(&paths), "adding .vimrc");
    }

    #[test]
    fn build_git_message_for_multiple_files() {
        let paths = vec![PathBuf::from(".config/a"), PathBuf::from(".config/b")];
        let message = build_git_message(&paths);
        assert!(message.starts_with("adding 2 files to .config"));
        assert!(message.contains("- .config/a\n"));
        assert!(message.contains("- .config/b\n"));
    }

    #[test]
    fn build_git_message_for_no_files_is_empty() {
        assert_eq!(build_git_message(&vec![]), "");
    }

    #[test]
    fn build_git_message_omits_base_path_when_there_is_none() {
        let paths = vec![PathBuf::from(".zshrc"), PathBuf::from(".config/nvim")];
        let message = build_git_message(&paths);

        assert!(message.starts_with("adding 2 files\n"));
        assert!(message.contains("- .zshrc\n"));
        assert!(message.contains("- .config/nvim\n"));
    }
}

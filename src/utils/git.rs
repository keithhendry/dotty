//! The git half of dotty, wrapping [`git2`].
//!
//! Everything git does for dotty happens through libgit2 rather than by
//! shelling out, so there is no dependency on a `git` binary being installed.
//! Authentication with remotes is delegated to the user's configured git
//! credential helper, which is why an existing `git push` to GitHub is enough
//! to make `dotty sync` work.
//!
//! # Path conventions
//!
//! Functions that touch the index or submodules take paths **relative to the
//! repository's working directory**, never absolute ones. libgit2 requires it,
//! and it is also the natural form here, since a dotfile has the same relative
//! path under the root and under the repository.
//!
//! The catch is that a relative path is only meaningful to libgit2's index. Any
//! plain filesystem or [`Repository::open`] call has to resolve it against
//! [`Repository::workdir`] first, or it will silently be interpreted relative to
//! the current process's working directory instead.

use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{
    AnnotatedCommit, AutotagOption, Commit, Config, Cred, CredentialType, ErrorCode, FetchOptions,
    Index, Oid, PushOptions, Reference, Remote, RemoteCallbacks, RemoteUpdateFlags, Repository,
    ResetType, SubmoduleUpdateOptions,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Opens the repository at `path`, creating it first if it is not one yet.
///
/// This makes `dotty init` idempotent: running it against an existing
/// repository is a no-op rather than an error.
pub fn init_or_open(path: &Path) -> Result<Repository, String> {
    git_helper(
        || {
            if !check_open(path) {
                log::debug!("initializing git repository {}", path.display());
                Repository::init(path)
            } else {
                log::debug!("opening git repository {}", path.display());
                Repository::open(path)
            }
        },
        |err| {
            format!(
                "failed to initialize git repository {} - {}",
                path.display(),
                err
            )
        },
    )
}

/// Clones `url` into `path`, then checks out every submodule recursively.
///
/// The recursive step matters for dotfiles: plugin managers and prompt themes
/// are tracked as submodules, and a clone without them leaves a repository full
/// of empty directories.
pub fn clone_recurse(path: &Path, url: &str) -> Result<Repository, String> {
    git_helper(
        || {
            log::debug!("cloning git repository {} into {}", url, path.display());

            let mut fetch_opts = FetchOptions::new();
            fetch_opts.remote_callbacks(create_callbacks());

            let mut builder = RepoBuilder::new();
            builder.fetch_options(fetch_opts);

            let repo = builder.clone(url, path)?;

            log::debug!("initializing submodules in {}", path.display());

            let mut checkout_builder = CheckoutBuilder::new();
            checkout_builder.force();

            let mut fetch_opts = FetchOptions::new();
            fetch_opts.remote_callbacks(create_callbacks());

            let mut opts = SubmoduleUpdateOptions::new();
            opts.checkout(checkout_builder);
            opts.fetch(fetch_opts);
            opts.allow_fetch(true);

            update_submodules_recursive(&repo, true, &mut opts)?;

            Ok(repo)
        },
        |err| {
            format!(
                "failed to clone git repository {} into {} - {}",
                url,
                path.display(),
                err
            )
        },
    )
}

/// Opens an existing repository, failing if `path` is not one.
pub fn open(path: &Path) -> Result<Repository, String> {
    log::trace!("opening git repository {}", path.display());
    git_helper(
        || Repository::open(path),
        |err| format!("failed to open git repository {} - {}", path.display(), err),
    )
}

/// Reports whether `path` is a git repository.
///
/// This is what decides that a directory should become a submodule rather than
/// being walked file by file when it is added.
pub fn check_open(path: &Path) -> bool {
    match Repository::open(path) {
        Ok(_) => {
            log::trace!("{} is a git repository", path.display());
            true
        }
        Err(_) => false,
    }
}

/// Resets the index to `HEAD`, leaving the working tree untouched.
///
/// Commands stage their own paths and then commit, so they start from a clean
/// index to avoid sweeping up unrelated changes a user had staged by hand. On a
/// repository with no commits yet there is nothing to reset, and this succeeds.
pub fn unstage_all(repo: &Repository) -> Result<(), String> {
    git_helper(
        || {
            if let Some(latest_commit) = find_last_commit(repo)? {
                log::debug!(
                    "resetting git repository {} with commit {}",
                    repo.path().display(),
                    latest_commit.id()
                );
                repo.reset(&latest_commit.into_object(), ResetType::Mixed, None)?;
            }
            Ok(())
        },
        |err| {
            format!(
                "failed to reset (mixed) git repository {} - {}",
                repo.path().display(),
                err
            )
        },
    )
}

/// Stages every path in `paths`, descending into plain directories.
///
/// Paths are relative to the repository's working directory (see the [module
/// documentation](self#path-conventions)). A path that is itself a git
/// repository is staged as a single submodule entry rather than being walked.
pub fn stage_all_paths(repo: &Repository, paths: &Vec<PathBuf>) -> Result<(), String> {
    log::debug!(
        "staging {} paths in git repository {}",
        paths.len(),
        repo.path().display()
    );
    git_helper(
        || {
            let mut index = repo.index()?;
            for path in paths {
                log::trace!("staging path {}", path.display());
                stage_path_recursive(&mut index, path)?;
            }
            index.write()
        },
        |err| {
            format!(
                "failed to stage {} paths in git repository {} - {}",
                paths.len(),
                repo.path().display(),
                err
            )
        },
    )
}

/// Commits whatever is currently staged, on top of `HEAD`.
///
/// Handles the unborn-branch case, so the first commit in a fresh repository
/// works without special casing. The author and committer come from the user's
/// git configuration, so dotty commits look like any other.
pub fn commit(repo: &Repository, message: &str) -> Result<Oid, String> {
    log::debug!(
        "creating commit in git repository {} with message {}",
        repo.path().display(),
        message
    );
    git_helper(
        || {
            let mut index = repo.index()?;
            let signature = repo.signature()?;
            let oid = index.write_tree()?;
            let tree = repo.find_tree(oid)?;
            let maybe_parent = find_last_commit(repo)?;
            let parents = match maybe_parent {
                Some(ref parent) => vec![parent],
                None => vec![],
            };
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parents,
            )
        },
        |err| {
            format!(
                "failed to commit changes in git repository {} - {}",
                repo.path().display(),
                err
            )
        },
    )
}

/// Registers already-cloned git repositories as submodules of `repo`.
///
/// Each path is relative to the repository's working directory (see the [module
/// documentation](self#path-conventions)) and must already hold the submodule's
/// own repository — dotty moves the directory into place before calling this.
/// The submodule's `origin` URL is reused as the submodule URL, so a repository
/// without an `origin` remote cannot be added.
pub fn add_submodules(repo: &Repository, submodules: &Vec<PathBuf>) -> Result<(), String> {
    log::debug!(
        "adding {} submodules to git repository {}",
        submodules.len(),
        repo.path().display()
    );
    let workdir = match repo.workdir() {
        Some(workdir) => workdir,
        None => {
            return Err(format!(
                "git repository {} has no working directory",
                repo.path().display()
            ))
        }
    };
    for path in submodules {
        log::trace!("adding submodule {}", path.display());
        // `path` is relative to the repo's working directory (as required by
        // `Repository::submodule` below), so it must be resolved against that
        // directory rather than opened as-is, which would instead resolve
        // relative to the current process's working directory.
        let submodule_repo = open(&workdir.join(path))?;
        let url = get_origin_url(&submodule_repo)?;
        if let Err(err) = repo
            .submodule(&url, path, true)
            .and_then(|mut submodule| submodule.add_finalize())
        {
            return Err(format!(
                "failed to add git submodule {} with url {} - {}",
                path.display(),
                url,
                err
            ));
        }
    }
    Ok(())
}

/// Fetches, merges and pushes the current branch, in that order.
///
/// Passing a `url` points `origin` at it, creating or updating the remote as
/// needed, which is how the first `dotty sync <url>` adopts a remote.
///
/// Refuses to run when the working tree is dirty, and stops at a merge conflict
/// rather than trying to resolve it — leaving the conflict checked out for the
/// user to sort out with git directly. A fast-forward is taken where possible,
/// otherwise a merge commit is created.
pub fn sync(repo: &Repository, url: Option<&str>) -> Result<(), String> {
    git_helper(
        || {
            if !repo.statuses(None)?.is_empty() {
                return Err(git2::Error::from_str(&format!(
                    "there are unstaged changes in {}",
                    repo.path().display()
                )));
            }
            let branch_name = get_branch_name(repo)?;
            let mut remote = get_remote(repo, url)?;

            log::debug!(
                "fetching branch {} from remote {}",
                branch_name,
                remote.url().unwrap_or("unknown")
            );

            let mut fetch_opts = FetchOptions::new();
            fetch_opts.remote_callbacks(create_callbacks());
            remote.fetch(&[&branch_name], Some(&mut fetch_opts), None)?;

            if let Ok(fetch_head) = repo.find_reference("FETCH_HEAD") {
                let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;
                log::debug!("merging remote commit {}", fetch_commit.id());
                merge(
                    repo,
                    &branch_name,
                    fetch_commit,
                    remote.url().unwrap_or("unknown"),
                )?;
            }

            log::debug!("pushing branch {}", branch_name);

            let mut push_opts = PushOptions::new();
            push_opts.remote_callbacks(create_callbacks());
            remote.push(
                &[format!("refs/heads/{0}:refs/heads/{0}", branch_name)],
                Some(&mut push_opts),
            )?;

            remote.disconnect()?;

            remote.update_tips(
                None,
                RemoteUpdateFlags::UPDATE_FETCHHEAD,
                AutotagOption::Unspecified,
                None,
            )?;

            Ok(())
        },
        |err| {
            format!(
                "failed to sync changes in git repository {} - {}",
                repo.path().display(),
                err
            )
        },
    )
}

/// Fast-forwards every submodule to its remote's default branch.
///
/// This is how a plugin or theme tracked as a submodule gets upgraded. The
/// updated submodule pointers are staged but not committed; the caller decides
/// whether there was anything worth committing, based on the returned count.
pub fn update_submodules(repo: &Repository) -> Result<i32, String> {
    git_helper(
        || {
            let mut updated: i32 = 0;

            for mut submodule in repo.submodules()? {
                log::debug!(
                    "updating submodule {}",
                    submodule.name().unwrap_or("unknown")
                );

                let submodule_repo = submodule.open()?;
                let mut remote = get_remote(&submodule_repo, None)?;

                remote.connect(git2::Direction::Fetch)?;

                let mut fetch_opts = FetchOptions::new();
                fetch_opts.remote_callbacks(create_callbacks());

                let default_branch_buf = remote.default_branch()?;
                let default_branch_ref_name =
                    default_branch_buf.as_str().unwrap_or("refs/heads/main");

                log::trace!(
                    "using branch {} for submodule {}",
                    default_branch_ref_name,
                    submodule.name().unwrap_or("unknown")
                );

                remote.fetch(
                    &[default_branch_ref_name] as &[&str],
                    Some(&mut fetch_opts),
                    None,
                )?;

                let fetch_head = submodule_repo.find_reference("FETCH_HEAD")?;
                let fetch_commit = submodule_repo.reference_to_annotated_commit(&fetch_head)?;

                let mut branch_reference =
                    submodule_repo.find_reference(default_branch_ref_name)?;
                fast_forward(&submodule_repo, &mut branch_reference, &fetch_commit)?;

                submodule.add_to_index(false)?;
                updated += 1;

                remote.disconnect()?;

                remote.update_tips(
                    None,
                    RemoteUpdateFlags::UPDATE_FETCHHEAD,
                    AutotagOption::Unspecified,
                    None,
                )?;
            }

            if updated > 0 {
                repo.index()?.write()?;
            }

            Ok(updated)
        },
        |err| {
            format!(
                "failed to update submodules in git repository {} - {}",
                repo.path().display(),
                err
            )
        },
    )
}

/// Runs a libgit2 operation, converting any [`git2::Error`] into the module's
/// `String` error via `err_func`.
///
/// Keeps the error-formatting boilerplate in one place, and keeps the happy
/// path readable by letting the body use `?` on `git2` results.
fn git_helper<G, E, A>(git_func: G, err_func: E) -> Result<A, String>
where
    G: FnOnce() -> Result<A, git2::Error>,
    E: FnOnce(git2::Error) -> String,
{
    git_func().map_err(err_func)
}

/// Returns the URL of the repository's `origin` remote.
fn get_origin_url(repo: &Repository) -> Result<String, String> {
    let remote = match repo.find_remote("origin") {
        Ok(remote) => remote.url().map(|p| p.to_owned()),
        Err(err) => {
            return Err(format!(
                "failed to get remotes for git repository {} - {}",
                repo.path().display(),
                err
            ))
        }
    };
    match remote {
        Ok(remote) => Ok(remote),
        Err(_) => Err(format!(
            "remote origin url was not found for {}",
            repo.path().display()
        )),
    }
}

/// Resolves the remote to work with, optionally repointing `origin` at `url`.
///
/// With no `url`, the existing `origin` is used. With one, `origin` is created
/// if missing and updated if it points somewhere else, so re-running sync
/// against a new URL moves the remote rather than failing.
fn get_remote<'a>(repo: &'a Repository, url: Option<&str>) -> Result<Remote<'a>, git2::Error> {
    match url {
        Some(url) => {
            log::trace!("using remote {}", url);
            if let Ok(remote) = repo.find_remote("origin") {
                if let Ok(remote_url) = remote.url() {
                    return match remote_url.eq(url) {
                        true => {
                            log::trace!("remotes match");
                            Ok(remote)
                        }
                        false => {
                            log::trace!("remote {} does not match; overwriting", remote_url);
                            repo.remote_set_url("origin", url)?;
                            repo.find_remote("origin")
                        }
                    };
                }
            }
            repo.remote("origin", url)
        }
        None => repo.find_remote("origin"),
    }
}

/// Builds remote callbacks that authenticate via the user's git credential
/// helper, so dotty reuses whatever credentials `git` itself already uses.
fn create_callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(
        |url: &str, username_from_url: Option<&str>, _cred: CredentialType| {
            Cred::credential_helper(&Config::open_default()?, url, username_from_url)
        },
    );
    callbacks
}

/// Returns the commit at `HEAD`, or `None` when the branch is unborn.
///
/// A repository created by `dotty init` has no commits until the first `add`,
/// so this distinguishes "no commits yet" from a genuine failure.
fn find_last_commit(repo: &Repository) -> Result<Option<Commit<'_>>, git2::Error> {
    match repo.head() {
        Ok(head) => Ok(Some(head.resolve()?.peel_to_commit()?)),
        Err(err) if err.code() == ErrorCode::UnbornBranch => Ok(None),
        Err(err) => Err(err),
    }
}

/// Returns the short name of the currently checked-out branch.
fn get_branch_name(repo: &Repository) -> Result<String, git2::Error> {
    let head = repo.head()?.resolve()?;
    match head
        .name()
        .ok()
        .and_then(|name| name.strip_prefix("refs/heads/"))
    {
        Some(name) => Ok(name.to_owned()),
        None => Err(git2::Error::from_str(&format!(
            "branch name could not be resolved in git repo {}",
            repo.path().display()
        ))),
    }
}

/// Stages `path`, descending into it if it is a plain directory.
///
/// A nested git repository is staged as a single submodule entry instead of
/// being descended into, which is what keeps a plugin's own history out of the
/// dotfiles repository.
fn stage_path_recursive(index: &mut Index, path: &Path) -> Result<(), git2::Error> {
    if path.is_dir() {
        if Repository::open(path).is_ok() {
            log::trace!("staging git submodule {}", path.display());
            index.add_path(path)?;
            return Ok(());
        }

        log::trace!("staging dir contents {}", path.display());
        match fs::read_dir(path) {
            Ok(entries) => {
                for entry_res in entries {
                    match entry_res {
                        Ok(entry) => {
                            let path = entry.path();
                            stage_path_recursive(index, &path)?;
                        }
                        Err(err) => {
                            return Err(git2::Error::from_str(&format!(
                                "could not read directory entry {} - {}",
                                path.display(),
                                err
                            )))
                        }
                    }
                }
            }
            Err(err) => {
                return Err(git2::Error::from_str(&format!(
                    "could not read directory {} - {}",
                    path.display(),
                    err
                )))
            }
        }
    } else {
        log::trace!("staging file {}", path.display());
        index.add_path(path)?;
    }
    Ok(())
}

/// Merges `fetch_commit` into the current branch, fast-forwarding when the
/// history allows it and creating a merge commit when it does not.
fn merge(
    repo: &Repository,
    branch: &str,
    fetch_commit: AnnotatedCommit,
    remote_url: &str,
) -> Result<(), git2::Error> {
    let analysis = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.0.is_fast_forward() {
        log::trace!("doing a fast forward");
        let mut reference = repo.find_reference(&format!("refs/heads/{}", branch))?;
        fast_forward(repo, &mut reference, &fetch_commit)?;
    } else if analysis.0.is_normal() {
        log::trace!("doing a normal merge");
        let head_commit = repo.reference_to_annotated_commit(&repo.head()?)?;
        normal_merge(repo, &head_commit, &fetch_commit, remote_url)?;
    } else {
        log::trace!("no merge needed");
    }
    Ok(())
}

/// Moves `lb` straight to `rc` and checks the result out.
fn fast_forward(
    repo: &Repository,
    lb: &mut Reference,
    rc: &AnnotatedCommit,
) -> Result<(), git2::Error> {
    let name = match lb.name() {
        Ok(name) => name.to_string(),
        Err(_) => String::from_utf8_lossy(lb.name_bytes()).to_string(),
    };
    log::debug!("fast-forward {} to id {}", name, rc.id());
    lb.set_target(
        rc.id(),
        &format!("Fast-Forward: Setting {} to id: {}", name, rc.id()),
    )?;
    repo.set_head(&name)?;
    repo.checkout_head(Some(CheckoutBuilder::new().force()))?;
    Ok(())
}

/// Creates a merge commit joining the local and remote commits.
///
/// Conflicts are checked out into the working tree and reported as an error;
/// dotty deliberately leaves resolving them to the user and git.
fn normal_merge(
    repo: &Repository,
    local: &AnnotatedCommit,
    remote: &AnnotatedCommit,
    remote_url: &str,
) -> Result<(), git2::Error> {
    log::debug!("merge {} into {}", remote.id(), local.id());
    let local_commit = repo.find_commit(local.id())?;
    let remote_commit = repo.find_commit(remote.id())?;
    let ancestor = repo
        .find_commit(repo.merge_base(local.id(), remote.id())?)?
        .tree()?;
    let mut idx = repo.merge_trees(
        &ancestor,
        &local_commit.tree()?,
        &remote_commit.tree()?,
        None,
    )?;
    if idx.has_conflicts() {
        repo.checkout_index(Some(&mut idx), None)?;
        return Err(git2::Error::from_str("merge conficts detected"));
    }
    let result_tree = repo.find_tree(idx.write_tree_to(repo)?)?;
    let sig = repo.signature()?;
    let _merge_commit = repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("Merge {}", remote_url),
        &result_tree,
        &[&local_commit, &remote_commit],
    )?;
    repo.checkout_head(Some(CheckoutBuilder::new().force()))?;
    Ok(())
}

/// Checks out every submodule, and every submodule of those, breadth first.
fn update_submodules_recursive(
    repo: &Repository,
    init: bool,
    opts: &mut SubmoduleUpdateOptions,
) -> Result<(), git2::Error> {
    fn add_subrepos(
        repo: &Repository,
        repos: &mut Vec<Repository>,
        init: bool,
        opts: &mut SubmoduleUpdateOptions,
    ) -> Result<(), git2::Error> {
        for mut subm in repo.submodules()? {
            log::trace!(
                "updating submodule {} (init: {}, in: {})",
                subm.name().unwrap_or("unknown"),
                init,
                repo.path().display(),
            );
            subm.update(init, Some(opts))?;
            repos.push(subm.open()?);
        }
        Ok(())
    }

    let mut repos = Vec::new();
    add_subrepos(repo, &mut repos, init, opts)?;
    while let Some(repo) = repos.pop() {
        add_subrepos(&repo, &mut repos, init, opts)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    // tempfile's TempDir path can itself sit behind a symlink (e.g. macOS's
    // /var -> /private/var), which breaks index/staging calls that require
    // paths relative to the repo's (canonical) workdir. Real callers always
    // canonicalize `repo`/`root` up front (see main.rs), so tests mirror that.
    fn canonical_tempdir() -> (TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    }

    fn configure_signature(repo: &Repository) {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
    }

    // stage_all_paths (like the real add/update command flows) is always called
    // with paths relative to the repo's workdir, never absolute ones.
    fn commit_file(repo: &Repository, dir: &Path, name: &str, contents: &str, message: &str) {
        fs::write(dir.join(name), contents).unwrap();
        stage_all_paths(repo, &vec![PathBuf::from(name)]).unwrap();
        commit(repo, message).unwrap();
    }

    #[test]
    fn init_or_open_creates_new_repository() {
        let (_dir, dir) = canonical_tempdir();

        init_or_open(&dir).unwrap();

        assert!(dir.join(".git").exists());
    }

    #[test]
    fn init_or_open_opens_an_existing_repository() {
        let (_dir, dir) = canonical_tempdir();
        init_or_open(&dir).unwrap();

        let repo = init_or_open(&dir).unwrap();

        assert_eq!(repo.path(), Repository::open(&dir).unwrap().path());
    }

    #[test]
    fn open_fails_for_a_non_repository() {
        let (_dir, dir) = canonical_tempdir();
        assert!(open(&dir).is_err());
    }

    #[test]
    fn check_open_detects_git_repositories() {
        let (_dir, dir) = canonical_tempdir();
        assert!(!check_open(&dir));

        init_or_open(&dir).unwrap();

        assert!(check_open(&dir));
    }

    #[test]
    fn stage_commit_and_unstage_round_trip() {
        let (_dir, dir) = canonical_tempdir();
        let repo = init_or_open(&dir).unwrap();
        configure_signature(&repo);

        commit_file(&repo, &dir, "file.txt", "hello", "add file.txt");

        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_commit.message().unwrap(), "add file.txt");
        assert_eq!(fs::read_to_string(dir.join("file.txt")).unwrap(), "hello");

        fs::write(dir.join("file2.txt"), "world").unwrap();
        stage_all_paths(&repo, &vec![PathBuf::from("file2.txt")]).unwrap();
        unstage_all(&repo).unwrap();

        let statuses = repo.statuses(None).unwrap();
        assert!(statuses
            .iter()
            .any(|s| s.path().ok() == Some("file2.txt") && s.status().is_wt_new()));
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            head_commit.id()
        );
    }

    // `add_submodules` is always called (by cmds::add) with paths relative to
    // the parent repo's workdir, and the submodule's own git repo has already
    // been placed at that location inside the parent repo's tree - mirror that
    // layout here rather than a sibling directory.
    #[test]
    fn add_submodules_registers_submodule_from_its_origin_remote() {
        let (_root, root) = canonical_tempdir();

        let repo_dir = root.join("repo");
        fs::create_dir_all(&repo_dir).unwrap();
        let repo = init_or_open(&repo_dir).unwrap();
        configure_signature(&repo);

        let sub_dir = repo_dir.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();
        let sub_repo = init_or_open(&sub_dir).unwrap();
        configure_signature(&sub_repo);
        commit_file(&sub_repo, &sub_dir, "plugin.vim", "\" plugin", "initial");
        let origin_url = format!("file://{}", sub_dir.display());
        sub_repo.remote("origin", &origin_url).unwrap();

        add_submodules(&repo, &vec![PathBuf::from("sub")]).unwrap();

        let gitmodules = fs::read_to_string(repo_dir.join(".gitmodules")).unwrap();
        assert!(gitmodules.contains(&origin_url));
    }

    #[test]
    fn add_submodules_errors_when_submodule_has_no_origin_remote() {
        let (_root, root) = canonical_tempdir();

        let repo_dir = root.join("repo");
        fs::create_dir_all(&repo_dir).unwrap();
        let repo = init_or_open(&repo_dir).unwrap();
        configure_signature(&repo);

        let sub_dir = repo_dir.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();
        let sub_repo = init_or_open(&sub_dir).unwrap();
        configure_signature(&sub_repo);
        commit_file(&sub_repo, &sub_dir, "plugin.vim", "\" plugin", "initial");

        let err = add_submodules(&repo, &vec![PathBuf::from("sub")]).unwrap_err();
        assert!(err.contains("failed to get remotes"));
    }

    #[test]
    fn sync_pushes_new_commits_to_a_fresh_remote() {
        let (_root, root) = canonical_tempdir();

        let remote_dir = root.join("remote");
        Repository::init_bare(&remote_dir).unwrap();

        let local_dir = root.join("local");
        fs::create_dir_all(&local_dir).unwrap();
        let local_repo = init_or_open(&local_dir).unwrap();
        configure_signature(&local_repo);
        commit_file(&local_repo, &local_dir, "file.txt", "hello", "initial");

        sync(
            &local_repo,
            Some(&format!("file://{}", remote_dir.display())),
        )
        .unwrap();

        let remote_repo = Repository::open_bare(&remote_dir).unwrap();
        let branch = get_branch_name(&local_repo).unwrap();
        let remote_commit = remote_repo
            .find_reference(&format!("refs/heads/{}", branch))
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(remote_commit.message().unwrap(), "initial");
    }

    #[test]
    fn sync_fast_forwards_local_from_remote() {
        let (_root, root) = canonical_tempdir();

        let remote_dir = root.join("remote");
        Repository::init_bare(&remote_dir).unwrap();

        let seed_dir = root.join("seed");
        fs::create_dir_all(&seed_dir).unwrap();
        let seed_repo = init_or_open(&seed_dir).unwrap();
        configure_signature(&seed_repo);
        commit_file(&seed_repo, &seed_dir, "file.txt", "hello", "initial");
        sync(
            &seed_repo,
            Some(&format!("file://{}", remote_dir.display())),
        )
        .unwrap();

        let local_dir = root.join("local");
        let local_repo =
            clone_recurse(&local_dir, &format!("file://{}", remote_dir.display())).unwrap();
        configure_signature(&local_repo);

        commit_file(&seed_repo, &seed_dir, "file2.txt", "world", "second");
        sync(&seed_repo, None).unwrap();

        sync(&local_repo, None).unwrap();

        assert_eq!(
            fs::read_to_string(local_dir.join("file2.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn update_submodules_fast_forwards_to_remote_default_branch() {
        let (_root, root) = canonical_tempdir();

        let sub_remote_dir = root.join("sub-remote");
        Repository::init_bare(&sub_remote_dir).unwrap();

        let sub_seed_dir = root.join("sub-seed");
        fs::create_dir_all(&sub_seed_dir).unwrap();
        let sub_seed_repo = init_or_open(&sub_seed_dir).unwrap();
        configure_signature(&sub_seed_repo);
        commit_file(&sub_seed_repo, &sub_seed_dir, "plugin.vim", "v1", "initial");
        sync(
            &sub_seed_repo,
            Some(&format!("file://{}", sub_remote_dir.display())),
        )
        .unwrap();

        let sub_dir = root.join("repo/sub");
        let sub_repo =
            clone_recurse(&sub_dir, &format!("file://{}", sub_remote_dir.display())).unwrap();
        configure_signature(&sub_repo);

        let repo_dir = root.join("repo");
        let repo = init_or_open(&repo_dir).unwrap();
        configure_signature(&repo);
        add_submodules(&repo, &vec![PathBuf::from("sub")]).unwrap();
        stage_all_paths(&repo, &vec![PathBuf::from("sub")]).unwrap();
        commit(&repo, "add submodule").unwrap();

        commit_file(&sub_seed_repo, &sub_seed_dir, "plugin.vim", "v2", "update");
        sync(&sub_seed_repo, None).unwrap();

        let updated = update_submodules(&repo).unwrap();

        assert_eq!(updated, 1);
        assert_eq!(
            fs::read_to_string(sub_dir.join("plugin.vim")).unwrap(),
            "v2"
        );
    }
}

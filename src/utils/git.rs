use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{
    AnnotatedCommit, AutotagOption, Commit, Config, Cred, CredentialType, ErrorCode, FetchOptions,
    Index, Oid, PushOptions, Reference, Remote, RemoteCallbacks, RemoteUpdateFlags, Repository,
    ResetType, SubmoduleUpdateOptions,
};
use std::fs;
use std::path::{Path, PathBuf};

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

pub fn open(path: &Path) -> Result<Repository, String> {
    log::trace!("opening git repository {}", path.display());
    git_helper(
        || Repository::open(path),
        |err| format!("failed to open git repository {} - {}", path.display(), err),
    )
}

pub fn check_open(path: &Path) -> bool {
    match Repository::open(path) {
        Ok(_) => {
            log::trace!("{} is a git repository", path.display());
            true
        }
        Err(_) => false,
    }
}

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

pub fn add_submodules(repo: &Repository, submodules: &Vec<PathBuf>) -> Result<(), String> {
    log::debug!(
        "adding {} submodules to git repository {}",
        submodules.len(),
        repo.path().display()
    );
    for path in submodules {
        log::trace!("adding submodule {}", path.display());
        let submodule_repo = open(path)?;
        let url = get_origin_url(&submodule_repo)?;
        if let Err(err) = repo
            .submodule(&url, path, true)
            .and_then(|mut submodule| submodule.add_finalize())
        {
            return Err(format!(
                "failed to add git submodule {} with url {} - {}",
                path.display(),
                &url,
                err
            ));
        }
    }
    Ok(())
}

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

fn git_helper<G, E, A>(git_func: G, err_func: E) -> Result<A, String>
where
    G: FnOnce() -> Result<A, git2::Error>,
    E: FnOnce(git2::Error) -> String,
{
    git_func().map_err(err_func)
}

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
        Some(remote) => Ok(remote),
        None => Err(format!(
            "remote origin url was not found for {}",
            repo.path().display()
        )),
    }
}

fn get_remote<'a>(repo: &'a Repository, url: Option<&str>) -> Result<Remote<'a>, git2::Error> {
    match url {
        Some(url) => {
            log::trace!("using remote {}", url);
            if let Ok(remote) = repo.find_remote("origin") {
                if let Some(remote_url) = remote.url() {
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

fn create_callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(
        |url: &str, username_from_url: Option<&str>, _cred: CredentialType| {
            Cred::credential_helper(&Config::open_default()?, url, username_from_url)
        },
    );
    callbacks
}

fn find_last_commit(repo: &Repository) -> Result<Option<Commit<'_>>, git2::Error> {
    match repo.head() {
        Ok(head) => Ok(Some(head.resolve()?.peel_to_commit()?)),
        Err(err) if err.code() == ErrorCode::UnbornBranch => Ok(None),
        Err(err) => Err(err),
    }
}

fn get_branch_name(repo: &Repository) -> Result<String, git2::Error> {
    repo.head()?
        .resolve()?
        .name()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(|name| Ok(name.to_owned()))
        .unwrap_or_else(|| {
            Err(git2::Error::from_str(&format!(
                "branch name could not be resolved in git repo {}",
                repo.path().display()
            )))
        })
}

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

fn fast_forward(
    repo: &Repository,
    lb: &mut Reference,
    rc: &AnnotatedCommit,
) -> Result<(), git2::Error> {
    let name = match lb.name() {
        Some(s) => s.to_string(),
        None => String::from_utf8_lossy(lb.name_bytes()).to_string(),
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

use git2::{
    AnnotatedCommit, Commit, Config, Cred, CredentialType, ErrorCode, FetchOptions, Index, Oid,
    PushOptions, Reference, Remote, RemoteCallbacks, Repository, ResetType, Status,
};
use std::fs;
use std::path::{Path, PathBuf};

pub fn init_or_open(path: &Path) -> Result<Repository, String> {
    git_helper(
        || {
            if !check_open(path) {
                log::trace!("initializing git repository {}", path.display());
                Repository::init(path)
            } else {
                log::trace!("opening git repository {}", path.display());
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

pub fn open(path: &Path) -> Result<Repository, String> {
    git_helper(
        || {
            log::trace!("opening git repository {}", path.display());
            Repository::open(path)
        },
        |err| format!("failed to open git repository {} - {}", path.display(), err),
    )
}

pub fn check_open(path: &Path) -> bool {
    match open(path) {
        Ok(_) => {
            log::trace!("{} is a git repository", path.display());
            true
        }
        Err(_) => false,
    }
}

pub fn clone_recurse(path: &Path, url: &str) -> Result<Repository, String> {
    git_helper(
        || {
            log::trace!("cloning git repository {} into {}", url, path.display());
            Repository::clone_recurse(url, path)
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

pub fn is_new(repo: &Repository, path: &Path) -> Result<bool, String> {
    git_helper(
        || {
            let is_new = repo.status_file(path)? == Status::WT_NEW;
            Ok(is_new)
        },
        |err| {
            format!(
                "failed to get stated status for {} in git repository {} - {}",
                path.display(),
                repo.path().display(),
                err
            )
        },
    )
}

pub fn unstage_all(repo: &Repository) -> Result<(), String> {
    git_helper(
        || {
            if let Some(latest_commit) = find_last_commit(repo)? {
                log::trace!("resetting git repository {}", repo.path().display());
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

pub fn stage_path(repo: &Repository, path: &Path) -> Result<(), String> {
    git_helper(
        || {
            let mut index = repo.index()?;
            stage_path_recursive(&mut index, path)?;
            index.write()
        },
        |err| {
            format!(
                "failed to stage {} in git repository {} - {}",
                path.display(),
                repo.path().display(),
                err
            )
        },
    )
}

fn stage_path_recursive(index: &mut Index, path: &Path) -> Result<(), git2::Error> {
    if path.is_dir() {
        if let Ok(_) = Repository::open(path) {
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

pub fn stage_all_paths(repo: &Repository, paths: &Vec<PathBuf>) -> Result<(), String> {
    git_helper(
        || {
            let mut index = repo.index()?;
            for path in paths {
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
    git_helper(
        || {
            log::trace!(
                "creating commit in git repository {} with message {}",
                repo.path().display(),
                message
            );

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

pub fn add_submodules(repo: &Repository, paths: &Vec<PathBuf>) -> Result<(), String> {
    let mut submodules = Vec::new();
    for path in paths {
        get_submodules(path, &mut submodules)?;
    }
    for (path, url) in &submodules {
        if let Err(err) = repo.submodule(&url, &path, true) {
            return Err(format!(
                "failed to add git submodule {} with url {} - {}",
                path.display(),
                &url,
                err
            ));
        }
    }
    if submodules.len() > 0 {
        stage_path(repo, Path::new(".gitmodules"))?;
    }
    Ok(())
}

fn get_submodules(path: &Path, submodules: &mut Vec<(PathBuf, String)>) -> Result<(), String> {
    if let Ok(repo) = Repository::open(path) {
        let url = get_origin(&repo)?;
        submodules.push((path.to_owned(), url));
        return Ok(());
    }

    if path.is_dir() {
        match fs::read_dir(path) {
            Ok(entries) => {
                for entry_res in entries {
                    match entry_res {
                        Ok(entry) => {
                            let path = entry.path();
                            if path.file_name().map(|f| !f.eq(".git")).unwrap_or(false) {
                                get_submodules(&entry.path(), submodules)?
                            }
                        }
                        Err(err) => log::warn!(
                            "could not read directory entry {} - {}",
                            path.display(),
                            err
                        ),
                    }
                }
            }
            Err(err) => log::warn!("could not read directory {} - {}", path.display(), err),
        }
    }
    Ok(())
}

fn get_origin(repo: &Repository) -> Result<String, String> {
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

            let mut fetch_opts = FetchOptions::new();
            fetch_opts.remote_callbacks(create_callbacks());
            remote.fetch(&[&branch_name], Some(&mut fetch_opts), None)?;

            if let Ok(fetch_head) = repo.find_reference("FETCH_HEAD") {
                let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;
                merge(
                    repo,
                    &branch_name,
                    fetch_commit,
                    remote.url().unwrap_or("unknown"),
                )?;
            }
            let mut push_opts = PushOptions::new();
            push_opts.remote_callbacks(create_callbacks());
            remote.push(
                &[format!("refs/heads/{0}:refs/heads/{0}", branch_name)],
                Some(&mut push_opts),
            )?;
            remote.disconnect()?;
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

fn git_helper<G, E, A>(git_func: G, err_func: E) -> Result<A, String>
where
    G: FnOnce() -> Result<A, git2::Error>,
    E: FnOnce(git2::Error) -> String,
{
    git_func().or_else(|err| Err(err_func(err)))
}

fn get_remote<'a>(repo: &'a Repository, url: Option<&str>) -> Result<Remote<'a>, git2::Error> {
    match url {
        Some(url) => {
            if let Ok(remote) = repo.find_remote("origin") {
                if remote
                    .url()
                    .map(|remote_url| remote_url.eq(url))
                    .unwrap_or(false)
                {
                    Ok(remote)
                } else {
                    repo.remote_set_url("origin", url)?;
                    repo.find_remote("origin")
                }
            } else {
                repo.remote("origin", url)
            }
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
        .map(|name| name.strip_prefix("refs/heads/"))
        .flatten()
        .map(|name| Ok(name.to_owned()))
        .unwrap_or(Err(git2::Error::from_str(&format!(
            "branch name could not be resolved in git repo {}",
            repo.path().display()
        ))))
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
        normal_merge(&repo, &head_commit, &fetch_commit, remote_url)?;
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
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
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
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
    Ok(())
}

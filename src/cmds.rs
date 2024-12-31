use crate::utils::fs;
use crate::utils::git;
use crate::utils::path;
use std::path::{Path, PathBuf};

pub fn init(repo: &Path) -> Result<(), String> {
    git::init_or_open(repo)?;

    log::info!(
        "successfully initialized dotty repository {}",
        repo.display()
    );
    Ok(())
}

pub fn clone(repo: &Path, url: &str) -> Result<(), String> {
    git::clone_recurse(repo, url)?;
    // Check that it is a valid dotty repository
    log::info!(
        "successfully cloned dotty repository {} from {}",
        repo.display(),
        url,
    );
    Ok(())
}

pub fn add(repo: &Path, root: &Path, paths: &Vec<PathBuf>) -> Result<(), String> {
    let mut to_commit: Vec<PathBuf> = Vec::new();
    let mut submodules: Vec<PathBuf> = Vec::new();

    for (path, path_type) in flatten_paths_to_add(paths)? {
        match move_to_dotty_repo(repo, root, &path) {
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
        let git_repo = git::open(repo)?;
        git::unstage_all(&git_repo)?;
        git::add_submodules(&git_repo, &submodules)?;
        git::stage_all_paths(&git_repo, &to_commit)?;
        git::commit(&git_repo, &build_git_message(&to_commit))?;

        log::info!(
            "successfully added {} to dotty repository {}",
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

pub fn restore(repo: &Path, root: &Path, symlinks: bool, overwrite: bool) -> Result<(), String> {
    let overwrite = match overwrite {
        true => Some(fs::create_overwrite_temp_dir("dotty-")?),
        false => None,
    };

    let top_level_repo_paths = fs::read_dir(repo)?
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .map(|f| !matches!(f, ".git" | ".gitmodules"))
                .unwrap_or(true)
        })
        .collect();
    let paths_to_restore: Vec<PathBuf> = flatten_paths_to_add(&top_level_repo_paths)?
        .into_iter()
        .map(|x| x.0)
        .collect();

    for from in paths_to_restore {
        let relative_path = path::relative_from_root(repo, &from)?;
        let to = root.join(&relative_path);
        let overwrite_entry = overwrite.as_ref().map(|o| o.entry(&relative_path));
        log::debug!("restoring {} to {}", from.display(), to.display());
        fs::restore(&from, &to, overwrite_entry.as_deref(), symlinks)?;
    }

    log::info!(
        "successfully restored dotty repository {} to {}, by {}",
        repo.display(),
        root.display(),
        match symlinks {
            true => "creating symlinks",
            false => "copying files",
        }
    );
    Ok(())
}

pub fn sync(repo: &Path, url: Option<&str>) -> Result<(), String> {
    let git_repo = git::open(repo)?;
    git::sync(&git_repo, url)?;
    log::info!("successfully synced dotty repository");
    Ok(())
}

pub fn update(repo: &Path) -> Result<(), String> {
    let git_repo = git::open(repo)?;

    git::unstage_all(&git_repo)?;
    let updated = git::update_submodules(&git_repo)?;
    if updated > 0 {
        git::commit(&git_repo, "Updated all submodules")?;
        log::info!("successfully updated {} submodules", updated);
    } else {
        log::warn!("there are no submodules to update");
    }

    Ok(())
}

#[derive(PartialEq)]
enum PathType {
    File,
    GitRepo,
}

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

fn move_to_dotty_repo(repo: &Path, root: &Path, path: &Path) -> Result<Option<PathBuf>, String> {
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

fn build_git_message(to_commit: &Vec<PathBuf>) -> String {
    match to_commit.len() {
        0 => String::default(),
        1 => format!("adding {}", to_commit.first().unwrap().display()),
        _ => {
            let mut msg = format!(
                "adding {} files to {}\n\n",
                to_commit.len(),
                path::common_base_path(to_commit).display()
            );
            for path in to_commit {
                msg.push_str(&format!("- {}\n", path.display()));
            }
            msg
        }
    }
}

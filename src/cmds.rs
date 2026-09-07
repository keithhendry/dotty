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
}

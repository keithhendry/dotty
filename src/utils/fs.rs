use super::string::random_string;
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};

pub fn move_then_symlink(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        return Err(format!("{} already exists", to.display()));
    }
    rename(from, to)?;
    symlink(to, from)?;

    Ok(())
}

pub fn restore(
    from: &Path,
    to: &Path,
    overwrite: Option<&Path>,
    symlinks: bool,
) -> Result<(), String> {
    if !from.exists() {
        return Err(format!("{} does not exist", from.display()));
    }
    if let Some(metadata) = symlink_metadata(to)? {
        log::trace!("{} already exists", to.display());

        if metadata.is_symlink() {
            let resolved_to = match fs::canonicalize(to) {
                Ok(resolved) => resolved,
                Err(err) => return Err(format!("failed to resolve {} - {}", from.display(), err)),
            };

            if resolved_to == from {
                log::trace!("{} points to {} already", to.display(), from.display());
                return Ok(());
            }

            match overwrite {
                Some(_) => {
                    log::warn!(
                        "removing existing symlink {} to {}",
                        to.display(),
                        resolved_to.display()
                    );
                    remove(to)?
                }
                None => {
                    return Err(format!(
                        "not overwriting symlink {} to {}",
                        to.display(),
                        resolved_to.display()
                    ))
                }
            }
        } else {
            match overwrite {
                Some(move_existing_to) => {
                    log::warn!(
                        "moving existing {} to {}",
                        to.display(),
                        move_existing_to.display()
                    );
                    rename(to, move_existing_to)?;
                }
                None => return Err(format!("not overwriting existing file {}", to.display())),
            }
        }
    }
    if symlinks {
        symlink(from, to)
    } else {
        rename(from, to)
    }
}

pub struct OverwriteTempDir {
    temp_dir: PathBuf,
}

impl Drop for OverwriteTempDir {
    fn drop(&mut self) {
        if let Ok(true) = is_empty(&self.temp_dir) {
            let _ = remove_dir(&self.temp_dir);
        }
    }
}

impl OverwriteTempDir {
    pub fn entry(&self, path: &Path) -> PathBuf {
        self.temp_dir.join(path)
    }
}

pub fn create_overwrite_temp_dir(prefix: &str) -> Result<OverwriteTempDir, String> {
    let name = prefix.to_owned() + &random_string(7);
    let temp_dir = env::temp_dir().join(name);
    if let Err(err) = fs::create_dir(&temp_dir) {
        return Err(format!(
            "failed to create temp dir {} - {}",
            temp_dir.display(),
            err
        ));
    }
    Ok(OverwriteTempDir { temp_dir })
}

pub fn remove_dir(dir: &Path) -> Result<(), String> {
    match fs::remove_dir(dir) {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("failed to remove {} - {}", dir.display(), err)),
    }
}

pub fn is_empty(dir: &Path) -> Result<bool, String> {
    match dir.read_dir() {
        Ok(mut read_dir) => Ok(read_dir.next().is_none()),
        Err(err) => Err(format!(
            "failed to get contents of {} - {}",
            dir.display(),
            err
        )),
    }
}

fn rename(from: &Path, to: &Path) -> Result<(), String> {
    log::trace!("rename {} to {}", from.display(), to.display());
    create_parent_dir(to)?;
    if let Err(err) = fs::rename(from, to) {
        return Err(format!(
            "failed to move {} to {} - {}",
            from.display(),
            to.display(),
            err
        ));
    }
    Ok(())
}

fn create_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            log::trace!("creating directory {} and all parents", parent.display());
            if let Err(err) = fs::create_dir_all(parent) {
                return Err(format!(
                    "failed to create directory {} - {}",
                    parent.display(),
                    err
                ));
            }
        }
    }
    Ok(())
}

fn symlink(original: &Path, link: &Path) -> Result<(), String> {
    log::trace!(
        "creating symlink {} to {}",
        link.display(),
        original.display()
    );
    create_parent_dir(link)?;
    if let Err(err) = unix_fs::symlink(original, link) {
        return Err(format!(
            "failed to create symlink {} to {} - {}",
            link.display(),
            original.display(),
            err
        ));
    };
    Ok(())
}

fn symlink_metadata(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!(
            "unable to get metadata of {} - {}",
            path.display(),
            err
        )),
    }
}

fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("unable to delete {} - {}", path.display(), err)),
    }
}

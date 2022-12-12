use home_dir::HomeDirExt;
use std::path::{Path, PathBuf};

pub fn canonicalize(path: &Path) -> Result<PathBuf, String> {
    let tilde_expanded = match path.expand_home() {
        Ok(expanded) => expanded,
        Err(err) => {
            return Err(format!(
                "failed to expand home dir {} - {}",
                path.display(),
                err
            ))
        }
    };
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

    match path.parent() {
        Some(parent) => Ok(canonicalize_missing(parent)?.join(path)),
        None => Err(format!(
            "failed to get the canonical path of {} - unknown parent",
            path.display()
        )),
    }
}

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

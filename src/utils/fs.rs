//! The filesystem half of dotty: moving files, and linking them back.
//!
//! Two operations carry the whole design. [`move_then_symlink`] relocates a
//! file into the repository and leaves a symlink in its place, so tools keep
//! finding their configuration exactly where they expect it. [`restore`] does
//! the reverse on a new machine.
//!
//! Both are deliberately conservative: anything already present that dotty did
//! not put there is an error rather than something to overwrite, unless the
//! caller supplies somewhere to move it aside to.
//!
//! Symlinks are created through `std::os::unix`, so this module is unix only —
//! matching dotty's supported targets.

use super::string::random_string;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};

/// Moves `from` into the repository at `to`, then symlinks `from` back to it.
///
/// Returns `Ok(true)` when the move happened, and `Ok(false)` when `from` is
/// already a symlink pointing at `to` — that is, when the file has been added
/// before and there is nothing to do. Any other pre-existing `to` is an error,
/// since overwriting it would discard whatever the repository already tracked.
///
/// Missing parent directories on either side are created as needed.
pub fn move_then_symlink(from: &Path, to: &Path) -> Result<bool, String> {
    if to.exists() {
        if let Some(metadata) = symlink_metadata(from)? {
            log::trace!("{} already exists", to.display());

            if metadata.is_symlink() {
                if let Ok(resolved_to) = fs::canonicalize(from) {
                    if resolved_to == to {
                        log::trace!("{} points to {} already", to.display(), from.display());
                        return Ok(false);
                    }
                }
            }
        }

        return Err(format!("{} already exists in repo", to.display()));
    }

    rename(from, to)?;
    symlink(to, from)?;

    Ok(true)
}

/// Places the repository's copy of a dotfile at `to`.
///
/// With `symlinks` set, `to` becomes a symlink pointing at `from`; otherwise
/// `from` is copied to `to`, recursively for directories. Copying is what you
/// want on a machine that should not keep depending on the repository being
/// present — a container, or a machine you are only borrowing.
///
/// Whatever is already at `to` decides the outcome:
///
/// - a symlink that already points at `from` is left alone in symlink mode, and
///   replaced in copy mode;
/// - anything else is moved to `overwrite`, or reported as an error when no
///   `overwrite` path was given, so that existing files are never silently lost.
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
            match fs::canonicalize(to) {
                Ok(resolved_to) if resolved_to == from => {
                    if symlinks {
                        log::trace!("{} correctly points to {}", to.display(), from.display());
                        return Ok(());
                    }
                    log::warn!("replacing symlink {} with {}", to.display(), from.display());
                    remove(to)?
                }
                Ok(resolved_to) if overwrite.is_some() => {
                    log::warn!(
                        "removing existing symlink {} to {}",
                        to.display(),
                        resolved_to.display()
                    );
                    remove(to)?
                }
                Ok(resolved_to) => {
                    return Err(format!(
                        "not overwriting symlink {} to {} - pass --overwrite to move it aside",
                        to.display(),
                        resolved_to.display()
                    ));
                }
                // A symlink that will not resolve is dangling: it points at
                // nothing, so replacing it cannot lose anything. Refusing here
                // used to fail the whole restore, even with --overwrite, on any
                // machine left holding stale dotfile symlinks.
                Err(_) => {
                    log::warn!("replacing broken symlink {}", to.display());
                    remove(to)?
                }
            }
        } else {
            // Copying produces a plain file at `to`, so an identical one is
            // this command's own earlier output and there is nothing to do.
            // Without this, `restore --mode files` failed the second time it ran.
            if !symlinks && same_contents(from, to)? {
                log::trace!("{} already matches {}", to.display(), from.display());
                return Ok(());
            }
            match overwrite {
                Some(move_existing_to) => {
                    log::warn!(
                        "moving existing {} to {}",
                        to.display(),
                        move_existing_to.display()
                    );
                    rename(to, move_existing_to)?;
                }
                None => {
                    return Err(format!(
                        "not overwriting existing file {} - pass --overwrite to move it aside",
                        to.display()
                    ))
                }
            }
        }
    }
    if symlinks {
        symlink(from, to)
    } else {
        copy(from, to)
    }
}

/// How a dotfile on the machine relates to the repository's copy of it.
#[derive(Debug, PartialEq)]
pub enum Placement {
    /// A symlink pointing at the repository's copy: managed, and edits to
    /// either side are the same edit.
    Linked,
    /// A plain copy of the repository's file, as `restore --mode files` leaves
    /// behind. Directories are reported this way without comparing contents.
    Copied,
    /// Nothing is there; a restore would put it in place.
    Missing,
    /// Something else is in the way, described for the reader.
    Conflict(String),
}

/// Reports what is sitting at `to`, relative to the repository's copy at
/// `from`. Reads only; nothing is moved or created.
pub fn placement(from: &Path, to: &Path) -> Result<Placement, String> {
    let metadata = match symlink_metadata(to)? {
        Some(metadata) => metadata,
        None => return Ok(Placement::Missing),
    };

    if metadata.is_symlink() {
        return Ok(match fs::canonicalize(to) {
            Ok(resolved) if resolved == from => Placement::Linked,
            Ok(resolved) => Placement::Conflict(format!("symlinked to {}", resolved.display())),
            Err(_) => Placement::Conflict("a broken symlink".to_owned()),
        });
    }

    // A submodule restored as files is a directory; comparing whole trees is
    // more than this needs to say.
    if from.is_dir() && to.is_dir() {
        return Ok(Placement::Copied);
    }

    if same_contents(from, to)? {
        return Ok(Placement::Copied);
    }

    Ok(Placement::Conflict("a different file".to_owned()))
}

/// A holding area for files displaced by `dotty restore --overwrite`.
///
/// The directory removes itself on drop, but only while it is still empty, so
/// anything actually moved aside survives for the user to recover.
pub struct BackupDir {
    dir: PathBuf,
}

impl Drop for BackupDir {
    fn drop(&mut self) {
        if let Ok(true) = is_empty(&self.dir) {
            let _ = remove_dir(&self.dir);
        }
    }
}

impl BackupDir {
    /// Returns where a dotfile with the given repository-relative path should
    /// be moved aside to. Creates nothing; it only builds the path.
    pub fn entry(&self, path: &Path) -> PathBuf {
        self.dir.join(path)
    }

    /// The directory itself, so that callers can tell the user where to look
    /// for anything that was displaced.
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

/// Creates the directory that displaced files are moved into, beside the
/// dotfiles themselves rather than under the system temp directory.
///
/// These are the user's real configuration files, moved out of the way to make
/// room; leaving them somewhere the operating system reaps on its own schedule
/// is no place for them. The name carries the time the run started, so
/// successive restores stay separate and sort into order.
pub fn create_backup_dir(root: &Path) -> Result<BackupDir, String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();

    let mut dir = root.join(format!(".dotty-backup-{seconds}"));
    if dir.exists() {
        // Two runs in the same second still get somewhere of their own.
        dir = root.join(format!(".dotty-backup-{seconds}-{}", random_string(4)));
    }

    if let Err(err) = fs::create_dir_all(&dir) {
        return Err(format!(
            "failed to create backup directory {} - {}",
            dir.display(),
            err
        ));
    }
    log::debug!("created backup directory {}", dir.display());
    Ok(BackupDir { dir })
}

/// Removes a symlink, leaving whatever it pointed at untouched.
pub fn remove_symlink(path: &Path) -> Result<(), String> {
    log::trace!("removing symlink {}", path.display());
    remove(path)
}

/// Moves a file or directory to `to`, creating parent directories as needed.
pub fn move_path(from: &Path, to: &Path) -> Result<(), String> {
    rename(from, to)
}

/// Removes an empty directory.
pub fn remove_dir(dir: &Path) -> Result<(), String> {
    match fs::remove_dir(dir) {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("failed to remove {} - {}", dir.display(), err)),
    }
}

/// Reports whether `dir` has no entries.
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

/// Lists the immediate children of `dir` as full paths, in no defined order.
pub fn read_dir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    match dir.read_dir() {
        Ok(read_dir) => {
            for entry_result in read_dir {
                match entry_result {
                    Ok(entry) => paths.push(entry.path()),
                    Err(err) => {
                        return Err(format!("Failed to read dir {}: {}", dir.display(), err))
                    }
                }
            }
        }
        Err(err) => return Err(format!("Failed to read dir {}: {}", dir.display(), err)),
    }
    Ok(paths)
}

/// Whether two paths are files holding exactly the same bytes.
///
/// Directories are never equal for this purpose: a submodule restored as a
/// directory is not something to compare byte for byte.
fn same_contents(left: &Path, right: &Path) -> Result<bool, String> {
    if !left.is_file() || !right.is_file() {
        return Ok(false);
    }
    let read = |path: &Path| {
        fs::read(path).map_err(|err| format!("failed to read {} - {}", path.display(), err))
    };
    Ok(read(left)? == read(right)?)
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

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    log::trace!("copy {} to {}", from.display(), to.display());
    create_parent_dir(to)?;
    if let Err(err) = copy_recursively(from, to) {
        return Err(format!(
            "failed to copy {} to {} - {}",
            from.display(),
            to.display(),
            err
        ));
    }
    Ok(())
}

fn copy_recursively(source: &Path, destination: &Path) -> std::io::Result<()> {
    if source.is_file() {
        fs::copy(source, destination)?;
    } else {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn create_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            return Err(format!(
                "failed to create directory {} - {}",
                parent.display(),
                err
            ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    // tempfile's TempDir path can itself sit behind a symlink (e.g. macOS's
    // /var -> /private/var), which breaks == comparisons against
    // fs::canonicalize() output. Real callers always canonicalize `repo`/`root`
    // up front (see main.rs), so tests mirror that instead of using raw paths.
    fn canonical_tempdir() -> (TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    }

    #[test]
    fn move_then_symlink_moves_file_and_links_back() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("original/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("repo/.vimrc");

        let moved = move_then_symlink(&from, &to).unwrap();

        assert!(moved);
        assert_eq!(fs::read_to_string(&to).unwrap(), "content");
        assert_eq!(fs::read_link(&from).unwrap(), to);
    }

    #[test]
    fn move_then_symlink_is_noop_if_already_linked() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("original/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("repo/.vimrc");
        assert!(move_then_symlink(&from, &to).unwrap());

        assert!(!move_then_symlink(&from, &to).unwrap());
    }

    #[test]
    fn move_then_symlink_errors_if_destination_exists() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join(".vimrc");
        fs::write(&from, "content").unwrap();
        let to = dir.join("repo/.vimrc");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::write(&to, "other content").unwrap();

        assert!(move_then_symlink(&from, &to).is_err());
    }

    #[test]
    fn restore_creates_symlink() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");

        restore(&from, &to, None, true).unwrap();

        assert_eq!(fs::read_link(&to).unwrap(), from);
    }

    #[test]
    fn restore_copies_file_when_not_symlinks() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");

        restore(&from, &to, None, false).unwrap();

        assert!(!symlink_metadata(&to).unwrap().unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&to).unwrap(), "content");
    }

    #[test]
    fn restore_copies_directories_recursively() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.config/app");
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join("settings.toml"), "content").unwrap();
        let to = dir.join("root/.config/app");

        restore(&from, &to, None, false).unwrap();

        assert_eq!(
            fs::read_to_string(to.join("settings.toml")).unwrap(),
            "content"
        );
    }

    #[test]
    fn restore_errors_when_source_missing() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        let to = dir.join("root/.vimrc");

        assert!(restore(&from, &to, None, true).is_err());
    }

    #[test]
    fn restore_is_noop_when_already_correctly_linked() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");
        restore(&from, &to, None, true).unwrap();

        restore(&from, &to, None, true).unwrap();

        assert_eq!(fs::read_link(&to).unwrap(), from);
    }

    #[test]
    fn restore_replaces_stale_symlink_when_not_using_symlink_mode() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");
        restore(&from, &to, None, true).unwrap();

        restore(&from, &to, None, false).unwrap();

        assert!(!symlink_metadata(&to).unwrap().unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&to).unwrap(), "content");
    }

    #[test]
    fn restore_errors_on_conflicting_symlink_without_overwrite() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        let elsewhere = dir.join("elsewhere");
        fs::write(&elsewhere, "other").unwrap();
        symlink(&elsewhere, &to).unwrap();

        assert!(restore(&from, &to, None, true).is_err());
    }

    #[test]
    fn restore_replaces_conflicting_symlink_with_overwrite() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        let elsewhere = dir.join("elsewhere");
        fs::write(&elsewhere, "other").unwrap();
        symlink(&elsewhere, &to).unwrap();

        restore(&from, &to, Some(&dir.join("stash/.vimrc")), true).unwrap();

        assert_eq!(fs::read_link(&to).unwrap(), from);
    }

    #[test]
    fn restore_errors_on_existing_file_without_overwrite() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::write(&to, "existing").unwrap();

        assert!(restore(&from, &to, None, true).is_err());
    }

    #[test]
    fn restore_moves_existing_file_aside_with_overwrite() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.vimrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "content").unwrap();
        let to = dir.join("root/.vimrc");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::write(&to, "existing").unwrap();
        let moved_aside = dir.join("stash/.vimrc");

        restore(&from, &to, Some(&moved_aside), true).unwrap();

        assert_eq!(fs::read_link(&to).unwrap(), from);
        assert_eq!(fs::read_to_string(&moved_aside).unwrap(), "existing");
    }

    #[test]
    fn placement_reports_a_correct_symlink_as_linked() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.zshrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "shell").unwrap();
        let to = dir.join("root/.zshrc");
        restore(&from, &to, None, true).unwrap();

        assert_eq!(placement(&from, &to).unwrap(), Placement::Linked);
    }

    #[test]
    fn placement_reports_an_identical_copy_as_copied() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.zshrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "shell").unwrap();
        let to = dir.join("root/.zshrc");
        restore(&from, &to, None, false).unwrap();

        assert_eq!(placement(&from, &to).unwrap(), Placement::Copied);
    }

    #[test]
    fn placement_reports_nothing_there_as_missing() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.zshrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "shell").unwrap();

        assert_eq!(
            placement(&from, &dir.join("root/.zshrc")).unwrap(),
            Placement::Missing
        );
    }

    #[test]
    fn placement_describes_what_is_in_the_way() {
        let (_dir, dir) = canonical_tempdir();
        let from = dir.join("repo/.zshrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "shell").unwrap();
        let to = dir.join("root/.zshrc");
        fs::create_dir_all(to.parent().unwrap()).unwrap();

        fs::write(&to, "something else").unwrap();
        assert_eq!(
            placement(&from, &to).unwrap(),
            Placement::Conflict("a different file".to_owned())
        );

        fs::remove_file(&to).unwrap();
        symlink(Path::new("/nowhere-at-all"), &to).unwrap();
        assert_eq!(
            placement(&from, &to).unwrap(),
            Placement::Conflict("a broken symlink".to_owned())
        );

        fs::remove_file(&to).unwrap();
        let elsewhere = dir.join("elsewhere");
        fs::write(&elsewhere, "other").unwrap();
        symlink(&elsewhere, &to).unwrap();
        assert!(matches!(
            placement(&from, &to).unwrap(),
            Placement::Conflict(reason) if reason.starts_with("symlinked to")
        ));
    }

    #[test]
    fn read_dir_lists_entries() {
        let (_dir, dir) = canonical_tempdir();
        fs::write(dir.join("a"), "").unwrap();
        fs::write(dir.join("b"), "").unwrap();

        let mut entries = read_dir(&dir).unwrap();
        entries.sort();

        assert_eq!(entries, vec![dir.join("a"), dir.join("b")]);
    }

    #[test]
    fn read_dir_errors_for_missing_dir() {
        let (_dir, dir) = canonical_tempdir();
        assert!(read_dir(&dir.join("missing")).is_err());
    }

    #[test]
    fn is_empty_reports_correctly() {
        let (_dir, dir) = canonical_tempdir();
        assert!(is_empty(&dir).unwrap());
        fs::write(dir.join("a"), "").unwrap();
        assert!(!is_empty(&dir).unwrap());
    }

    #[test]
    fn backup_dir_is_removed_when_empty_on_drop() {
        let (_dir, dir) = canonical_tempdir();
        let backup_path;
        {
            let backup = create_backup_dir(&dir).unwrap();
            backup_path = backup.path().to_owned();
            assert!(backup_path.exists());
        }
        assert!(!backup_path.exists());
    }

    #[test]
    fn backup_dir_is_kept_when_not_empty_on_drop() {
        let (_dir, dir) = canonical_tempdir();
        let backup_path;
        {
            let backup = create_backup_dir(&dir).unwrap();
            backup_path = backup.path().to_owned();
            fs::write(backup.entry(Path::new("file")), "content").unwrap();
        }
        assert!(backup_path.exists());
    }

    // The whole point of moving it out of the temp directory is that it sits
    // with the dotfiles, where nothing else will clear it away.
    #[test]
    fn backup_dir_is_created_beside_the_dotfiles() {
        let (_dir, dir) = canonical_tempdir();

        let backup = create_backup_dir(&dir).unwrap();

        assert_eq!(backup.path().parent().unwrap(), dir);
        assert!(backup
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(".dotty-backup-"));
    }

    #[test]
    fn backup_dirs_do_not_collide() {
        let (_dir, dir) = canonical_tempdir();

        let first = create_backup_dir(&dir).unwrap();
        let second = create_backup_dir(&dir).unwrap();

        assert_ne!(first.path(), second.path());
    }
}

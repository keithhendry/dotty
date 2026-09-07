//! End to end tests that drive the real `dotty` binary.
//!
//! The unit tests call the command functions directly, which leaves a gap:
//! argument parsing, the process exit code, `~` expansion and the environment
//! variables are only exercised when the binary is actually run. Both bugs
//! found while writing the README lived in that gap — `dotty restore` reporting
//! a failure and still exiting 0, and submodules resolving against the wrong
//! directory — so these tests run the built binary against throwaway home
//! directories and local git repositories, and check the results with the `git`
//! command line rather than with libgit2 again.
//!
//! Every test gets its own home directory, and `HOME` is set per child process
//! rather than for the test runner, so nothing here can reach the real one and
//! the tests are safe to run in parallel.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// A throwaway machine: its own home directory, git identity and dotfiles.
struct Machine {
    _tmp: TempDir,
    home: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("failed to create a temp dir");
        // macOS puts the temp dir behind a symlink (/var -> /private/var) and
        // dotty canonicalizes the paths it is given, so resolve it up front or
        // every path comparison below is off by that prefix.
        let home = tmp.path().canonicalize().unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\
             \tname = Dotty Test\n\
             \temail = test@example.com\n\
             [init]\n\
             \tdefaultBranch = main\n",
        )
        .unwrap();
        Machine { _tmp: tmp, home }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }

    /// The dotty repository this machine uses by default.
    fn repo(&self) -> PathBuf {
        self.path(".dotty")
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).unwrap()
    }

    /// Runs dotty the way a user would: from their home directory, with `HOME`
    /// pointing at it, and with no arguments beyond the ones given.
    fn dotty(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_dotty"))
            .args(args)
            .env("HOME", &self.home)
            .env_remove("DOTTY_REPOSITORY")
            .env_remove("DOTTY_ROOT")
            .current_dir(&self.home)
            .output()
            .expect("failed to run the dotty binary")
    }

    /// Creates a git repository at `relative` with one commit and an origin
    /// remote, standing in for a vim plugin or a shell theme.
    fn plugin(&self, relative: &str, origin: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).unwrap();
        let dir = path.to_str().unwrap();
        git_ok(&self.home, &["init", dir]);
        fs::write(path.join("plugin.vim"), contents).unwrap();
        git_ok(&self.home, &["-C", dir, "add", "-A"]);
        git_ok(&self.home, &["-C", dir, "commit", "-m", "initial"]);
        git_ok(&self.home, &["-C", dir, "remote", "add", "origin", origin]);
        path
    }
}

/// A bare repository standing in for a remote such as GitHub.
struct Remote {
    _tmp: TempDir,
    path: PathBuf,
}

impl Remote {
    fn new(home: &Path) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().canonicalize().unwrap().join("remote.git");
        git_ok(home, &["init", "--bare", path.to_str().unwrap()]);
        Remote { _tmp: tmp, path }
    }

    fn url(&self) -> String {
        format!("file://{}", self.path.display())
    }
}

fn git(home: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .env("HOME", home)
        .output()
        .expect("git is required to run these tests")
}

fn git_ok(home: &Path, args: &[&str]) -> String {
    let output = git(home, args);
    assert!(
        output.status.success(),
        "git {:?} failed:\n{}",
        args,
        text(&output)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[track_caller]
fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "expected success, got {:?}:\n{}",
        output.status.code(),
        text(output)
    );
}

#[track_caller]
fn assert_failed(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected a failure exit code, got {:?}:\n{}",
        output.status.code(),
        text(output)
    );
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_symlink())
        .unwrap_or(false)
}

// ---------------------------------------------------------------- init

#[test]
fn init_creates_a_repository_at_the_default_location() {
    let machine = Machine::new();

    assert_ok(&machine.dotty(&["init"]));

    assert!(machine.repo().join(".git").is_dir());
}

#[test]
fn init_can_be_run_again_without_complaint() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));

    assert_ok(&machine.dotty(&["init"]));
}

// ---------------------------------------------------------------- add

#[test]
fn add_moves_the_file_into_the_repository_and_symlinks_it_back() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "export EDITOR=nvim\n");

    assert_ok(&machine.dotty(&["add", ".zshrc"]));

    assert_eq!(
        fs::read_link(machine.path(".zshrc")).unwrap(),
        machine.repo().join(".zshrc"),
        "the original path should now be a symlink into the repository"
    );
    assert_eq!(
        fs::read_to_string(machine.repo().join(".zshrc")).unwrap(),
        "export EDITOR=nvim\n"
    );
}

#[test]
fn add_commits_the_file_it_moved() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "export EDITOR=nvim\n");

    assert_ok(&machine.dotty(&["add", ".zshrc"]));

    let repo = machine.repo();
    let repo = repo.to_str().unwrap();
    assert_eq!(
        git_ok(&machine.home, &["-C", repo, "log", "-1", "--format=%s"]),
        "adding .zshrc"
    );
    assert_eq!(git_ok(&machine.home, &["-C", repo, "ls-files"]), ".zshrc");
    assert_eq!(
        git_ok(&machine.home, &["-C", repo, "status", "--porcelain"]),
        "",
        "the repository should be left clean"
    );
}

#[test]
fn add_expands_a_directory_into_the_files_inside_it() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".config/nvim/init.lua", "-- nvim\n");
    machine.write(".config/nvim/lua/opts.lua", "-- opts\n");

    assert_ok(&machine.dotty(&["add", ".config"]));

    let tracked = git_ok(
        &machine.home,
        &["-C", machine.repo().to_str().unwrap(), "ls-files"],
    );
    let mut tracked: Vec<&str> = tracked.lines().collect();
    tracked.sort();
    assert_eq!(
        tracked,
        vec![".config/nvim/init.lua", ".config/nvim/lua/opts.lua"]
    );
    assert!(is_symlink(&machine.path(".config/nvim/init.lua")));
    assert!(is_symlink(&machine.path(".config/nvim/lua/opts.lua")));
}

#[test]
fn add_summarises_a_multiple_file_commit_by_count_when_there_is_no_shared_directory() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "shell\n");
    machine.write(".config/nvim/init.lua", "editor\n");

    assert_ok(&machine.dotty(&["add", ".zshrc", ".config"]));

    let subject = git_ok(
        &machine.home,
        &[
            "-C",
            machine.repo().to_str().unwrap(),
            "log",
            "-1",
            "--format=%s",
        ],
    );
    assert_eq!(subject, "adding 2 files");
}

// A directory that is a git repository of its own is the interesting case: it
// should become a submodule rather than having its history swallowed.
#[test]
fn add_tracks_a_nested_git_repository_as_a_submodule() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.plugin(
        ".vim/plugged/nifty",
        "https://example.com/nifty.git",
        "let g:nifty = 1\n",
    );

    assert_ok(&machine.dotty(&["add", ".vim/plugged/nifty"]));

    let repo = machine.repo();
    let repo = repo.to_str().unwrap();
    let listing = git_ok(&machine.home, &["-C", repo, "ls-files", "-s"]);
    let entry = listing
        .lines()
        .find(|line| line.ends_with(".vim/plugged/nifty"))
        .unwrap_or_else(|| panic!("the plugin is not tracked at all:\n{listing}"));
    assert!(
        entry.starts_with("160000 "),
        "expected a gitlink (mode 160000), got: {entry}"
    );

    let modules = fs::read_to_string(machine.repo().join(".gitmodules")).unwrap();
    assert!(
        modules.contains("https://example.com/nifty.git"),
        "the submodule should keep its own origin url, got: {modules}"
    );
    assert!(
        is_symlink(&machine.path(".vim/plugged/nifty")),
        "the plugin directory should be replaced by a symlink"
    );
}

#[test]
fn add_reports_a_missing_path_and_exits_non_zero() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));

    let output = machine.dotty(&["add", ".nope"]);

    assert_failed(&output);
    assert!(
        text(&output).contains("does not exist"),
        "got: {}",
        text(&output)
    );
}

#[test]
fn add_leaves_an_already_added_file_alone() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "export EDITOR=nvim\n");
    assert_ok(&machine.dotty(&["add", ".zshrc"]));

    assert_ok(&machine.dotty(&["add", ".zshrc"]));

    let repo = machine.repo();
    let count = git_ok(
        &machine.home,
        &["-C", repo.to_str().unwrap(), "rev-list", "--count", "HEAD"],
    );
    assert_eq!(count, "1", "adding the same file twice should not commit");
    assert_eq!(
        fs::read_link(machine.path(".zshrc")).unwrap(),
        repo.join(".zshrc")
    );
}

// add rearranges real files, so anything knowable in advance is checked before
// it starts. Each of these used to move the file first and fail afterwards,
// leaving it symlinked into the repository but never committed -- and a retry
// then did nothing, because the file looked like it was already managed.
#[test]
fn add_before_init_leaves_the_file_alone() {
    let machine = Machine::new();
    machine.write(".zshrc", "shell\n");

    let output = machine.dotty(&["add", ".zshrc"]);

    assert_failed(&output);
    assert!(
        !is_symlink(&machine.path(".zshrc")),
        "the file should not have been moved"
    );
    assert_eq!(machine.read(".zshrc"), "shell\n");
}

#[test]
fn add_without_a_git_identity_leaves_the_file_alone() {
    let machine = Machine::new();
    fs::remove_file(machine.path(".gitconfig")).unwrap();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "shell\n");

    let output = machine.dotty(&["add", ".zshrc"]);

    assert_failed(&output);
    assert!(
        text(&output).contains("user.name"),
        "the error should say what to configure, got: {}",
        text(&output)
    );
    assert!(!is_symlink(&machine.path(".zshrc")));
}

#[test]
fn add_skips_a_git_repository_with_no_origin_and_still_adds_the_rest() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "shell\n");
    // A plugin directory that was never cloned from anywhere.
    let plugin = machine.path(".vim/plugged/local");
    fs::create_dir_all(&plugin).unwrap();
    let dir = plugin.to_str().unwrap();
    git_ok(&machine.home, &["init", dir]);
    fs::write(plugin.join("plugin.vim"), "local\n").unwrap();
    git_ok(&machine.home, &["-C", dir, "add", "-A"]);
    git_ok(&machine.home, &["-C", dir, "commit", "-m", "local"]);

    assert_ok(&machine.dotty(&["add", ".zshrc", ".vim/plugged/local"]));

    // The unrelated file still got added...
    assert!(is_symlink(&machine.path(".zshrc")));
    assert_eq!(
        git_ok(
            &machine.home,
            &["-C", machine.repo().to_str().unwrap(), "ls-files"]
        ),
        ".zshrc"
    );
    // ...and the plugin was left exactly where it was.
    assert!(!is_symlink(&plugin));
}

// A machine stranded by an older version: the file sits in the repository,
// symlinked, but was never committed. Adding it again should finish the job.
#[test]
fn add_commits_a_file_left_in_the_repository_uncommitted() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "shell\n");
    std::os::unix::fs::symlink(machine.repo().join(".zshrc"), machine.path(".zshrc")).unwrap();

    assert_ok(&machine.dotty(&["add", ".zshrc"]));

    let repo = machine.repo();
    let repo = repo.to_str().unwrap();
    assert_eq!(git_ok(&machine.home, &["-C", repo, "ls-files"]), ".zshrc");

    // And doing it once more really is a no-op.
    let before = git_ok(&machine.home, &["-C", repo, "rev-list", "--count", "HEAD"]);
    assert_ok(&machine.dotty(&["add", ".zshrc"]));
    assert_eq!(
        git_ok(&machine.home, &["-C", repo, "rev-list", "--count", "HEAD"]),
        before
    );
}

// ---------------------------------------------------------------- restore

#[test]
fn restore_symlinks_the_repository_contents_into_place() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.config/app.toml", "port = 1\n");

    assert_ok(&machine.dotty(&["restore"]));

    assert_eq!(
        fs::read_link(machine.path(".config/app.toml")).unwrap(),
        machine.repo().join(".config/app.toml")
    );
}

#[test]
fn restore_in_files_mode_copies_real_files() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "shell\n");

    assert_ok(&machine.dotty(&["restore", "--mode", "files"]));

    assert!(!is_symlink(&machine.path(".zshrc")));
    assert_eq!(machine.read(".zshrc"), "shell\n");
}

#[test]
fn restore_refuses_to_clobber_an_existing_file() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "from the repository\n");
    machine.write(".zshrc", "already here\n");

    let output = machine.dotty(&["restore"]);

    assert_failed(&output);
    assert!(
        text(&output).contains("not overwriting"),
        "got: {}",
        text(&output)
    );
    assert_eq!(
        machine.read(".zshrc"),
        "already here\n",
        "the existing file must be left untouched"
    );
}

#[test]
fn restore_with_overwrite_moves_the_existing_file_aside() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "from the repository\n");
    machine.write(".zshrc", "already here\n");

    let output = machine.dotty(&["-v", "restore", "--overwrite"]);
    assert_ok(&output);

    assert_eq!(
        fs::read_link(machine.path(".zshrc")).unwrap(),
        machine.repo().join(".zshrc")
    );

    // The displaced file is reported rather than deleted, so it can be recovered.
    let moved_to = text(&output)
        .lines()
        .find_map(|line| line.split(" to ").nth(1).map(str::to_owned))
        .expect("expected the log to say where the file was moved");
    assert_eq!(
        fs::read_to_string(moved_to.trim()).unwrap(),
        "already here\n"
    );
}

// A machine being migrated onto dotty is exactly the one likely to be holding
// stale dotfile symlinks. Refusing to touch them failed the whole restore, even
// with --overwrite, which is the flag for "deal with whatever is in the way".
#[test]
fn restore_replaces_a_dangling_symlink() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "from the repository\n");
    std::os::unix::fs::symlink("/nonexistent/gone", machine.path(".zshrc")).unwrap();

    assert_ok(&machine.dotty(&["restore"]));

    assert_eq!(
        fs::read_link(machine.path(".zshrc")).unwrap(),
        machine.repo().join(".zshrc")
    );
}

#[test]
fn restore_in_files_mode_can_be_run_again() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "shell\n");
    assert_ok(&machine.dotty(&["restore", "--mode", "files"]));

    assert_ok(&machine.dotty(&["restore", "--mode", "files"]));

    assert_eq!(machine.read(".zshrc"), "shell\n");
}

// The copy being identical is what makes a repeat run a no-op; a real local
// edit is still something the user has to decide about.
#[test]
fn restore_in_files_mode_still_refuses_to_discard_a_local_edit() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "shell\n");
    assert_ok(&machine.dotty(&["restore", "--mode", "files"]));
    machine.write(".zshrc", "edited by hand\n");

    let output = machine.dotty(&["restore", "--mode", "files"]);

    assert_failed(&output);
    assert_eq!(machine.read(".zshrc"), "edited by hand\n");
}

#[test]
fn restore_carries_on_past_a_conflict_and_reports_what_failed() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    for name in ["a", "b", "c"] {
        machine.write(&format!(".dotty/.file-{name}"), name);
    }
    machine.write(".file-b", "already here\n");

    let output = machine.dotty(&["restore"]);

    assert_failed(&output);
    assert!(
        text(&output).contains("restored 2 of 3 paths"),
        "expected a summary, got: {}",
        text(&output)
    );
    // The two that could be restored were, rather than being abandoned.
    assert!(is_symlink(&machine.path(".file-a")));
    assert!(is_symlink(&machine.path(".file-c")));
    assert_eq!(machine.read(".file-b"), "already here\n");
}

// ---------------------------------------------------------------- sync + clone

#[test]
fn sync_pushes_to_a_newly_adopted_remote() {
    let machine = Machine::new();
    let remote = Remote::new(&machine.home);
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "shell\n");
    assert_ok(&machine.dotty(&["add", ".zshrc"]));

    assert_ok(&machine.dotty(&["sync", &remote.url()]));

    assert_eq!(
        git_ok(
            &machine.home,
            &[
                "-C",
                remote.path.to_str().unwrap(),
                "log",
                "-1",
                "--format=%s",
                "main"
            ]
        ),
        "adding .zshrc"
    );
}

#[test]
fn sync_refuses_to_run_with_uncommitted_changes() {
    let machine = Machine::new();
    let remote = Remote::new(&machine.home);
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "shell\n");
    assert_ok(&machine.dotty(&["add", ".zshrc"]));
    fs::write(machine.repo().join(".zshrc"), "edited but not committed\n").unwrap();

    let output = machine.dotty(&["sync", &remote.url()]);

    assert_failed(&output);
    assert!(
        text(&output).contains("unstaged changes"),
        "got: {}",
        text(&output)
    );
}

// The whole point of dotty: set a machine up from a repository made on another.
#[test]
fn a_second_machine_can_clone_and_restore() {
    let first = Machine::new();
    let remote = Remote::new(&first.home);
    assert_ok(&first.dotty(&["init"]));
    first.write(".zshrc", "shell\n");
    first.write(".config/nvim/init.lua", "editor\n");
    assert_ok(&first.dotty(&["add", ".zshrc", ".config"]));
    assert_ok(&first.dotty(&["sync", &remote.url()]));

    let second = Machine::new();
    assert_ok(&second.dotty(&["clone", &remote.url()]));

    // Cloning fetches, but deliberately does not touch the machine's own files.
    assert!(!second.path(".zshrc").exists());

    assert_ok(&second.dotty(&["restore"]));

    assert_eq!(
        fs::read_link(second.path(".zshrc")).unwrap(),
        second.repo().join(".zshrc")
    );
    assert_eq!(second.read(".zshrc"), "shell\n");
    assert_eq!(second.read(".config/nvim/init.lua"), "editor\n");
}

#[test]
fn sync_brings_down_changes_made_on_another_machine() {
    let first = Machine::new();
    let remote = Remote::new(&first.home);
    assert_ok(&first.dotty(&["init"]));
    first.write(".zshrc", "first\n");
    assert_ok(&first.dotty(&["add", ".zshrc"]));
    assert_ok(&first.dotty(&["sync", &remote.url()]));

    let second = Machine::new();
    assert_ok(&second.dotty(&["clone", &remote.url()]));
    assert_ok(&second.dotty(&["restore"]));

    // The first machine adds another file and pushes it.
    first.write(".gitconfig_extra", "second\n");
    assert_ok(&first.dotty(&["add", ".gitconfig_extra"]));
    assert_ok(&first.dotty(&["sync"]));

    assert_ok(&second.dotty(&["sync"]));

    assert_eq!(
        fs::read_to_string(second.repo().join(".gitconfig_extra")).unwrap(),
        "second\n"
    );
}

// ---------------------------------------------------------------- update

#[test]
fn update_fast_forwards_a_submodule_and_commits_the_new_pointer() {
    let machine = Machine::new();
    let upstream = Remote::new(&machine.home);

    // A plugin published to its own remote, then installed on this machine.
    let source = Machine::new();
    source.plugin(".src", &upstream.url(), "version 1\n");
    let source_dir = source.path(".src");
    let source_dir = source_dir.to_str().unwrap();
    git_ok(&source.home, &["-C", source_dir, "push", "origin", "main"]);

    let installed = machine.path(".vim/plugged/nifty");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    git_ok(
        &machine.home,
        &["clone", &upstream.url(), installed.to_str().unwrap()],
    );

    assert_ok(&machine.dotty(&["init"]));
    assert_ok(&machine.dotty(&["add", ".vim/plugged/nifty"]));

    // The plugin releases a new version.
    fs::write(source.path(".src/plugin.vim"), "version 2\n").unwrap();
    git_ok(&source.home, &["-C", source_dir, "commit", "-am", "v2"]);
    git_ok(&source.home, &["-C", source_dir, "push", "origin", "main"]);

    assert_ok(&machine.dotty(&["update"]));

    assert_eq!(
        fs::read_to_string(machine.repo().join(".vim/plugged/nifty/plugin.vim")).unwrap(),
        "version 2\n",
        "the submodule should have been fast forwarded"
    );
    assert_eq!(
        git_ok(
            &machine.home,
            &[
                "-C",
                machine.repo().to_str().unwrap(),
                "log",
                "-1",
                "--format=%s"
            ]
        ),
        "Updated all submodules"
    );
}

// Counting every submodule as "updated" rather than only the ones that moved
// wrote an empty commit on every single run.
#[test]
fn update_does_not_commit_when_the_submodule_is_already_current() {
    let machine = Machine::new();
    let upstream = Remote::new(&machine.home);

    let source = Machine::new();
    source.plugin(".src", &upstream.url(), "version 1\n");
    let source_dir = source.path(".src");
    let source_dir = source_dir.to_str().unwrap();
    git_ok(&source.home, &["-C", source_dir, "push", "origin", "main"]);

    let installed = machine.path(".vim/plugged/nifty");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    git_ok(
        &machine.home,
        &["clone", &upstream.url(), installed.to_str().unwrap()],
    );
    assert_ok(&machine.dotty(&["init"]));
    assert_ok(&machine.dotty(&["add", ".vim/plugged/nifty"]));

    let repo = machine.repo();
    let repo = repo.to_str().unwrap();
    let before = git_ok(&machine.home, &["-C", repo, "rev-list", "--count", "HEAD"]);

    for _ in 0..3 {
        assert_ok(&machine.dotty(&["update"]));
    }

    assert_eq!(
        git_ok(&machine.home, &["-C", repo, "rev-list", "--count", "HEAD"]),
        before,
        "updating with nothing new upstream should not commit"
    );
}

#[test]
fn update_does_nothing_when_there_are_no_submodules() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".zshrc", "shell\n");
    assert_ok(&machine.dotty(&["add", ".zshrc"]));

    let output = machine.dotty(&["update"]);
    assert_ok(&output);

    assert_eq!(
        git_ok(
            &machine.home,
            &[
                "-C",
                machine.repo().to_str().unwrap(),
                "rev-list",
                "--count",
                "HEAD"
            ]
        ),
        "1",
        "there was nothing to update, so nothing should have been committed"
    );
}

// ---------------------------------------------------------------- status

#[test]
fn status_reports_every_state_a_dotfile_can_be_in() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));

    // managed correctly
    machine.write(".zshrc", "shell\n");
    assert_ok(&machine.dotty(&["add", ".zshrc"]));
    // in the repository but not on the machine
    machine.write(".dotty/.gitconfig_extra", "git\n");
    // a copy rather than a symlink
    machine.write(".dotty/.profile", "profile\n");
    machine.write(".profile", "profile\n");
    // something else is in the way
    machine.write(".dotty/.tmux.conf", "tmux\n");
    machine.write(".tmux.conf", "not the same\n");

    let output = machine.dotty(&["status"]);
    assert_ok(&output);
    let report = text(&output);

    assert!(
        report.contains("linked") && report.contains(".zshrc"),
        "{report}"
    );
    assert!(
        report.contains("missing") && report.contains(".gitconfig_extra"),
        "{report}"
    );
    assert!(
        report.contains("copied") && report.contains(".profile"),
        "{report}"
    );
    assert!(
        report.contains("conflict") && report.contains("a different file"),
        "{report}"
    );
    assert!(
        report.contains("4 tracked, 1 missing, 1 conflicting"),
        "{report}"
    );
}

// It reports; it must not put anything right behind the user's back.
#[test]
fn status_changes_nothing() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));
    machine.write(".dotty/.zshrc", "shell\n");

    assert_ok(&machine.dotty(&["status"]));

    assert!(
        !machine.path(".zshrc").exists(),
        "status should not have restored anything"
    );
}

#[test]
fn status_on_an_empty_repository_says_so() {
    let machine = Machine::new();
    assert_ok(&machine.dotty(&["init"]));

    let output = machine.dotty(&["status"]);

    assert_ok(&output);
    assert!(
        text(&output).contains("tracks nothing yet"),
        "{}",
        text(&output)
    );
}

// ---------------------------------------------------------------- options

#[test]
fn the_repository_and_root_can_be_set_by_environment() {
    let machine = Machine::new();
    let repo = machine.path("elsewhere/repo");
    let root = machine.path("elsewhere/root");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".zshrc"), "shell\n").unwrap();

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_dotty"))
            .args(args)
            .env("HOME", &machine.home)
            .env("DOTTY_REPOSITORY", &repo)
            .env("DOTTY_ROOT", &root)
            .current_dir(&machine.home)
            .output()
            .unwrap()
    };

    assert_ok(&run(&["init"]));
    assert_ok(&run(&["add", root.join(".zshrc").to_str().unwrap()]));

    assert!(repo.join(".git").is_dir());
    assert_eq!(
        fs::read_link(root.join(".zshrc")).unwrap(),
        repo.join(".zshrc")
    );
    assert!(
        !machine.repo().exists(),
        "the default repository location should not have been used"
    );
}

#[test]
fn a_tilde_for_another_users_home_is_rejected() {
    let machine = Machine::new();

    let output = machine.dotty(&["-r", "~someone/.dotty", "init"]);

    assert_failed(&output);
    assert!(
        text(&output).contains("only a leading ~"),
        "got: {}",
        text(&output)
    );
}

#[test]
fn an_unknown_subcommand_is_rejected() {
    let machine = Machine::new();

    let output = machine.dotty(&["frobnicate"]);

    assert!(!output.status.success());
    assert!(
        text(&output).contains("unrecognized subcommand"),
        "got: {}",
        text(&output)
    );
}

#[test]
fn help_lists_every_subcommand() {
    let machine = Machine::new();

    let output = machine.dotty(&["--help"]);
    assert_ok(&output);

    let help = text(&output);
    for subcommand in [
        "init", "clone", "add", "restore", "sync", "update", "status",
    ] {
        assert!(
            help.contains(subcommand),
            "{subcommand} missing from --help"
        );
    }
}

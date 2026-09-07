//! dotty — dotfiles managed as a git repository.
//!
//! The idea is small: keep every dotfile in one git repository, and leave a
//! symlink behind wherever the file used to be. Programs go on reading
//! `~/.vimrc` without knowing anything changed, while the actual file — and its
//! history — lives somewhere you can commit and push.
//!
//! This module is the command line front end. It parses arguments, resolves the
//! repository and root directories once, and hands off to [`cmds`], where each
//! subcommand is implemented.

mod cmds;
mod utils;

use clap::{ArgAction, Parser, ValueEnum};
use cmds::{add, clone, init, remove, restore, status, sync, update};
use simplelog::*;
use std::path::PathBuf;
use std::process;
use utils::path;

#[derive(Parser)]
#[clap(about, version, author)]
struct Opts {
    /// A level of verbosity, and can be used multiple times
    #[clap(short, long, action = ArgAction::Count)]
    verbose: u8,

    /// The path to the dotty repository
    #[clap(
        short = 'r',
        long,
        env = "DOTTY_REPOSITORY",
        default_value = "~/.dotty"
    )]
    repository: PathBuf,

    /// The root path to the dot files. Default is parent directory of dotty repository
    #[clap(short = 'R', long, env = "DOTTY_ROOT")]
    root: Option<PathBuf>,

    #[clap(subcommand)]
    subcmd: SubCommand,
}

#[derive(Parser)]
enum SubCommand {
    /// Initializes a new dotty repository
    Init(Init),
    /// Clones an existing dotty repository
    Clone(Clone),
    /// Adds files or directories to dotty management
    Add(Add),
    /// Restores files to the root
    Restore(Restore),
    /// Syncs the dotty repository with the remote
    Sync(Sync),
    /// Updates the submodules in the dotty repository
    Update(Update),
    /// Shows what is tracked and how it stands on this machine
    Status(Status),
    /// Stops managing files, putting the real files back in their place
    Remove(Remove),
}

#[derive(Parser)]
struct Init {}

#[derive(Parser)]
struct Clone {
    /// The repository url to clone
    #[clap()]
    url: String,
}

#[derive(Parser)]
struct Add {
    /// The paths to the files or directories
    #[clap()]
    paths: Vec<PathBuf>,
}

#[derive(Parser)]
struct Restore {
    /// Restore mode
    #[clap(short, long, value_enum, default_value = "symlinks")]
    mode: RestoreMode,

    /// Overwrites existing files/symlinks
    #[clap(short, long, default_value = "false")]
    overwrite: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum RestoreMode {
    /// Restore symlinks
    Symlinks,

    /// Restore files
    Files,
}

#[derive(Parser)]
struct Sync {
    /// The repository url to sync to
    #[clap()]
    url: Option<String>,
}

#[derive(Parser)]
struct Update {}

#[derive(Parser)]
struct Status {}

#[derive(Parser)]
struct Remove {
    /// The paths to stop managing
    #[clap()]
    paths: Vec<PathBuf>,
}

/// Sets up terminal logging, with `-v` raising the level each time it is given:
/// warnings by default, then info, debug and trace.
///
/// Output is deliberately bare — no timestamps or thread names — because this
/// is a foreground tool whose logs are read as they scroll past, and the extra
/// columns only get in the way. From `-vv` the log target and source location
/// are added, which is what you want when you are actually debugging.
fn init_logger(opts: &Opts) {
    let level = match opts.verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        2 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };
    let config = ConfigBuilder::new()
        .set_max_level(if opts.verbose >= 2 {
            log::LevelFilter::Error
        } else {
            log::LevelFilter::Off
        })
        .set_target_level(if opts.verbose >= 3 {
            log::LevelFilter::Error
        } else {
            log::LevelFilter::Off
        })
        .set_thread_level(log::LevelFilter::Off)
        .set_time_level(log::LevelFilter::Off)
        .build();

    // Everything the logger emits is diagnostics, so it all goes to stderr and
    // leaves stdout carrying only what a command actually produced.
    if let Err(err) = TermLogger::init(level, config, TerminalMode::Stderr, ColorChoice::Auto) {
        panic!("failed to initialize logger - {}", err);
    }
}

/// Resolves the repository and root directories, then dispatches to the command.
///
/// Both paths are canonicalized here, once, so that everything downstream can
/// compare them and strip one from the other without worrying about `~`,
/// relative paths or symlinks.
fn run(opts: &Opts) -> Result<(), String> {
    let repo = path::canonicalize(&opts.repository)?;
    let root = path::canonicalize(&path::get_root(opts.root.as_deref(), &repo)?)?;
    log::debug!(
        "using dotty repo {} for root {}",
        repo.display(),
        root.display()
    );
    match &opts.subcmd {
        SubCommand::Init(_) => init(&repo),
        SubCommand::Clone(clone_cmd) => clone(&repo, &clone_cmd.url),
        SubCommand::Add(add_cmd) => add(&repo, &root, &add_cmd.paths),
        SubCommand::Restore(restore_cmd) => restore(
            &repo,
            &root,
            restore_cmd.mode == RestoreMode::Symlinks,
            restore_cmd.overwrite,
        ),
        SubCommand::Sync(sync_cmd) => sync(&repo, sync_cmd.url.as_deref()),
        SubCommand::Update(_) => update(&repo),
        SubCommand::Status(_) => status(&repo, &root),
        SubCommand::Remove(remove_cmd) => remove(&repo, &root, &remove_cmd.paths),
    }
}

/// Parses arguments, starts logging, and runs the requested command.
///
/// A failure is logged and exits non-zero, so that dotty can be used in a
/// script or chained with `&&` without a failed command looking like a success.
fn main() {
    let opts: Opts = Opts::parse();
    init_logger(&opts);
    if let Err(err) = run(&opts) {
        log::error!("{}", err);
        process::exit(1);
    }
}

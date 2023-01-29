mod config;
mod utils;

use clap::{ArgAction, Parser, ValueEnum};
use config::Config;
use simplelog::*;
use std::path::{Path, PathBuf};
use utils::fs;
use utils::git;
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
    #[clap(short, long, default_value = "true")]
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

fn main() {
    let opts: Opts = Opts::parse();
    init_logger(&opts);
    if let Err(err) = run(&opts) {
        log::error!("{}", err);
    }
}

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

    if let Err(err) = TermLogger::init(level, config, TerminalMode::Mixed, ColorChoice::Auto) {
        panic!("failed to initialize logger - {}", err);
    }
}

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
        SubCommand::Restore(restore_cmd) => {
            restore(&repo, &root, restore_cmd.mode, restore_cmd.overwrite)
        }
        SubCommand::Sync(sync_cmd) => sync(&repo, sync_cmd.url.as_deref()),
    }
}

fn init(repo: &Path) -> Result<(), String> {
    let git_repo = git::init_or_open(repo)?;
    let config = Config::init_or_open(repo)?;

    if git::is_new(&git_repo, config.repo_path())? {
        git::stage_path(&git_repo, config.repo_path())?;
        git::commit(&git_repo, "initial dotty commit")?;
    }

    log::info!(
        "successfully initialized dotty repository {}",
        repo.display()
    );
    Ok(())
}

fn clone(repo: &Path, url: &str) -> Result<(), String> {
    git::clone_recurse(repo, url)?;
    // Check that it is a valid dotty repository
    Config::read(repo)?;
    log::info!(
        "successfully cloned dotty repository {} from {}",
        repo.display(),
        url,
    );
    Ok(())
}

fn add(repo: &Path, root: &Path, paths: &Vec<PathBuf>) -> Result<(), String> {
    let mut config = Config::read(repo)?;
    let mut to_commit: Vec<PathBuf> = Vec::new();
    for path in paths {
        match move_to_dotty_repo(repo, root, path) {
            Ok(relative_path) => {
                config.append(&relative_path);
                to_commit.push(relative_path);
            }
            Err(err) => log::warn!(
                "failed to fully add {} to repo {} - {}",
                path.display(),
                repo.display(),
                err
            ),
        }
    }

    if !to_commit.is_empty() {
        config.write()?;

        let git_repo = git::open(repo)?;
        git::unstage_all(&git_repo)?;
        git::stage_path(&git_repo, config.repo_path())?;
        git::add_submodules(&git_repo, &to_commit)?;
        git::stage_all_paths(&git_repo, &to_commit)?;
        git::commit(&git_repo, &build_git_message(&to_commit))?;

        log::info!(
            "successfully added {} to dotty repository {}",
            if to_commit.len() == 1 {
                to_commit.get(0).unwrap().display().to_string()
            } else {
                format!("{} paths", to_commit.len())
            },
            repo.display()
        );
    }

    Ok(())
}

fn move_to_dotty_repo(repo: &Path, root: &Path, path: &Path) -> Result<PathBuf, String> {
    let from = path::canonicalize(path)?;
    if !from.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let relative_path = path::relative_from_root(root, &from)?;
    let to = repo.join(&relative_path);

    log::debug!(
        "moving {} to {} and then replacing with symlink",
        from.display(),
        to.display()
    );
    fs::move_then_symlink(&from, &to)?;
    Ok(relative_path)
}

fn build_git_message(to_commit: &Vec<PathBuf>) -> String {
    match to_commit.len() {
        0 => String::default(),
        1 => format!("adding {}", to_commit.get(0).unwrap().display()),
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

fn restore(repo: &Path, root: &Path, mode: RestoreMode, overwrite: bool) -> Result<(), String> {
    let overwrite = match overwrite {
        true => Some(fs::create_overwrite_temp_dir("dotty-")?),
        false => None,
    };
    let config = Config::read(repo)?;
    for entry in config.entries {
        let from = repo.join(&entry.path);
        let to = root.join(&entry.path);
        let overwrite_entry = overwrite.as_ref().map(|o| o.entry(&entry.path));
        log::debug!(
            "restoring {} to {} with mode {:?}",
            from.display(),
            to.display(),
            mode
        );
        fs::restore(
            &from,
            &to,
            overwrite_entry.as_deref(),
            mode == RestoreMode::Symlinks,
        )?
    }
    Ok(())
}

fn sync(repo: &Path, url: Option<&str>) -> Result<(), String> {
    let git_repo = git::open(repo)?;
    git::sync(&git_repo, url)
}

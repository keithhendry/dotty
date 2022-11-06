use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
}

pub struct Config {
    path: PathBuf,
    pub entries: Vec<Entry>,
}

const CONFIG_FILE: &str = "dotty.yaml";

impl Config {
    pub fn init_or_open(repo: &Path) -> Result<Self, String> {
        let path = repo.join(CONFIG_FILE);
        if path.exists() {
            return Config::read(repo);
        }
        log::trace!("initiating config file {}", path.display());
        let config = Config {
            path,
            entries: Vec::new(),
        };
        config.write()?;
        Ok(config)
    }

    pub fn read(repo: &Path) -> Result<Self, String> {
        let path = repo.join(CONFIG_FILE);
        log::trace!("reading config file {}", path.display());
        let config_file = match File::open(&path) {
            Ok(file) => file,
            Err(err) => {
                return Err(format!(
                    "failed to open config file {} - {}",
                    path.display(),
                    err
                ))
            }
        };

        let entries: Vec<Entry> = match serde_yaml::from_reader(config_file) {
            Ok(entries) => entries,
            Err(err) => {
                return Err(format!(
                    "failed to deserialize config file {} - {}",
                    path.display(),
                    err
                ))
            }
        };

        Ok(Config {
            path,
            entries
        })
    }

    pub fn append(&mut self, entry: &Path) {
        log::trace!(
            "adding {} to config file {}",
            entry.display(),
            self.path.display()
        );
        self.entries.push(Entry {
            path: entry.to_owned(),
        });
    }

    pub fn write(&self) -> Result<(), String> {
        log::trace!("writing config file {}", self.path.display());
        let config_file = match File::create(&self.path) {
            Ok(file) => file,
            Err(err) => {
                return Err(format!(
                    "failed to open config file for write {} - {}",
                    self.path.display(),
                    err
                ))
            }
        };

        if let Err(err) = serde_yaml::to_writer(config_file, &self.entries) {
            return Err(format!(
                "failed to write config file {} - {}",
                self.path.display(),
                err
            ));
        }
        Ok(())
    }

    pub fn repo_path(&self) -> &Path {
        Path::new(CONFIG_FILE)
    }
}

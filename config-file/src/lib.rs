//! Loads and merges site, user and named TOML settings while preserving value locations.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod error;
mod node;
mod registry;
mod secret;
mod table;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use error::ConfigError;
pub use node::Origin;
pub use registry::{RegistryAuth, registry_auth};
pub use secret::Secret;
pub use table::Table;

use node::Node;

/// The name of the config file in each folder.
pub const FILE_NAME: &str = "config.toml";

/// The folder of the site-wide config file, which every user of the machine reads.
pub const SITE_FOLDER: &str = "/etc/uwe";

/// The environment variable that names another site-wide file than `/etc/uwe/config.toml`.
/// An empty value means no site-wide file. Tests set it, so that the file of the machine
/// never changes their results.
pub const SITE_FILE_VARIABLE: &str = "UWE_SITE_CONFIG";

/// The merged settings of the config files.
#[derive(Clone, Debug)]
pub struct ConfigFile {
    /// The top-level entries, whose tables are the sections.
    entries: BTreeMap<String, Node>,
    /// Where the settings start: the first file, or nowhere without one.
    origin: Origin,
    files: Vec<PathBuf>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        ConfigFile {
            entries: BTreeMap::new(),
            origin: Origin::nowhere(),
            files: Vec::new(),
        }
    }
}

impl ConfigFile {
    /// The site-wide file and the user's file where they exist, then `named`, which must exist.
    pub fn load(named: Option<&Path>) -> Result<ConfigFile, ConfigError> {
        let mut files: Vec<PathBuf> = default_files()
            .into_iter()
            .filter(|file| file.is_file())
            .collect();
        files.extend(named.map(Path::to_owned));
        ConfigFile::read(&files)
    }

    /// Reads these files in order; a later file wins key by key.
    pub fn read(files: &[PathBuf]) -> Result<ConfigFile, ConfigError> {
        let mut config = ConfigFile::default();
        for file in files {
            let text = std::fs::read_to_string(file).map_err(|source| ConfigError::Read {
                file: file.clone(),
                source,
            })?;
            config.add(&text, file)?;
        }
        Ok(config)
    }

    /// The settings of one text, read as the file `file`.
    pub fn parse(text: &str, file: &Path) -> Result<ConfigFile, ConfigError> {
        let mut config = ConfigFile::default();
        config.add(text, file)?;
        Ok(config)
    }

    /// Merges the settings of a text over the settings so far.
    fn add(&mut self, text: &str, file: &Path) -> Result<(), ConfigError> {
        let parsed = node::parse(text, file)?;
        if self.files.is_empty() {
            self.origin = Origin::start(file);
        }
        node::merge(&mut self.entries, parsed);
        self.files.push(file.to_owned());
        Ok(())
    }

    /// The files that were read, in the order they merged.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The top level of the settings, whose tables are the sections.
    pub fn root(&self) -> Table<'_> {
        Table::root(&self.entries, &self.origin)
    }

    /// The section `name`, like `storage`; `None` when no file has it.
    pub fn section(&self, name: &str) -> Result<Option<Table<'_>>, ConfigError> {
        self.root().table(name)
    }
}

/// The site-wide file and the user's file, in the order they merge.
pub fn default_files() -> Vec<PathBuf> {
    site_file().into_iter().chain(user_file()).collect()
}

/// The site-wide file: `/etc/uwe/config.toml`, or the file that `UWE_SITE_CONFIG`
/// names, or none when that is empty.
pub fn site_file() -> Option<PathBuf> {
    site_file_named(std::env::var_os(SITE_FILE_VARIABLE))
}

fn site_file_named(variable: Option<OsString>) -> Option<PathBuf> {
    match variable {
        Some(file) if file.is_empty() => None,
        Some(file) => Some(PathBuf::from(file)),
        None => Some(Path::new(SITE_FOLDER).join(FILE_NAME)),
    }
}

/// The user's file: `$XDG_CONFIG_HOME/uwe/config.toml`, else
/// `~/.config/uwe/config.toml`.
pub fn user_file() -> Option<PathBuf> {
    let folder = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|folder| !folder.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(folder.join("uwe").join(FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_site_file() {
        assert_eq!(
            site_file_named(None),
            Some(PathBuf::from("/etc/uwe/config.toml"))
        );
        assert_eq!(
            site_file_named(Some("/srv/site.toml".into())),
            Some(PathBuf::from("/srv/site.toml"))
        );
        assert_eq!(site_file_named(Some(OsString::new())), None);
    }
}

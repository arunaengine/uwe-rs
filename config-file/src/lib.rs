//! The iunctus config file. Settings come from up to three TOML files, merged in this order,
//! where a later file wins key by key: the site-wide file, the user's file, and the file that a
//! command-line option names. Without any file, every setting has its default. Each value
//! remembers its file and line, so that errors say where to look. A secret may be a reference
//! to an environment variable instead of the secret itself, so that the file can be shared.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod error;
mod node;
mod table;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use error::ConfigError;
pub use node::Origin;
pub use table::Table;

use node::Node;

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

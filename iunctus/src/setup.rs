//! The config file of iunctus (decision 20): the site-wide file, the user's file and the file
//! of `-iunctus-config`, read once per command, with the storage providers and the store that
//! they name. Without any file, everything works with its defaults.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use config_file::{ConfigError, ConfigFile};
use content_store::Store;
use storage::{Storage, StoreLocation};

use crate::arguments::Arguments;
use crate::{Failure, failure};

/// The sections of the config file. Another name is an error, so that a typing mistake does
/// not go unnoticed. `executor` holds the settings of executors.
const SECTIONS: &[&str] = &["store", "storage", "executor"];

/// What the config files set up for a command.
pub struct Setup {
    /// The storage providers, one client each for the whole process.
    pub storage: Storage,
}

impl Setup {
    /// Reads the config files.
    pub fn load(arguments: &Arguments) -> Result<Setup, Failure> {
        let failed = |error: ConfigError| failure(format!("iunctus: {error}"));
        let config = ConfigFile::load(arguments.config_file.as_deref()).map_err(failed)?;
        config.root().only(SECTIONS).map_err(failed)?;
        let storage = Storage::from_config(&config).map_err(failed)?;
        Ok(Setup { storage })
    }

    /// Where the cache store lives: `-store`, else `location` of `[store]`, else `store` in
    /// the cache directory.
    pub fn store_location(&self, arguments: &Arguments) -> String {
        (arguments.store.clone())
            .or_else(|| self.storage.store_location().map(str::to_owned))
            .unwrap_or_else(|| {
                let folder = crate::cache_directory().join("store");
                folder.to_string_lossy().into_owned()
            })
    }

    /// The cache store: a folder, or a place of a storage provider that can write there.
    pub fn store(&self, arguments: &Arguments) -> Result<Store, Failure> {
        let location = self.store_location(arguments);
        let failed = |message: String| failure(format!("iunctus: {message}"));
        let place = (self.storage.store(&location)).map_err(|error| failed(error.to_string()))?;
        let store = match place {
            StoreLocation::Folder(folder) => Store::local(&folder),
            StoreLocation::Objects(operator) => Store::remote(operator, &location),
        };
        store.map_err(|error| failed(format!("cannot open the store at {location}: {error}")))
    }
}

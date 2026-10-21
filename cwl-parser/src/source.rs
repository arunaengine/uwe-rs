//! Reads documents by URL and keeps each text and parsed tree for reuse.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use storage::{Storage, StorageError};

use crate::error::Error;
use crate::iri;
use crate::node::{Location, Node};
use crate::yaml;

/// Where the loader reads documents from.
pub trait Source {
    /// The text of the document at the URL, which has no fragment.
    fn read(&self, url: &str) -> Result<String, String>;

    /// Whether the document or file at the URL exists; used to check links such as `location`.
    fn exists(&self, url: &str) -> bool;
}

/// Reads `file:` URLs from the local file system; other schemes are refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileSystem;

impl Source for FileSystem {
    fn read(&self, url: &str) -> Result<String, String> {
        let path = iri::file_path(url).ok_or("only file URLs can be read")?;
        std::fs::read_to_string(&path).map_err(|error| error.to_string())
    }

    /// Remote URLs count as present, since the file system cannot check them.
    fn exists(&self, url: &str) -> bool {
        match iri::Parts::parse(url).scheme {
            Some("file") => iri::file_path(url).is_some_and(|path| path.exists()),
            Some("http" | "https" | "mailto") => true,
            _ => false,
        }
    }
}

/// Where documents and files at addresses come from: the storage providers, so that the
/// config file's providers and secrets apply to CWL too, and the folder that keeps what they
/// download.
#[derive(Clone, Debug)]
pub struct Remote {
    pub storage: Storage,
    downloads: Downloads,
}

/// The folder of downloads: given by the caller, or private and made on first use.
#[derive(Clone, Debug)]
enum Downloads {
    Given(PathBuf),
    /// Removed when the last clone is dropped.
    Private(Arc<OnceLock<(PathBuf, Option<tempfile::TempDir>)>>),
}

impl Remote {
    pub fn new(storage: &Storage, downloads: PathBuf) -> Remote {
        Remote {
            storage: storage.clone(),
            downloads: Downloads::Given(downloads),
        }
    }

    /// The folder of downloads; a private one is made here the first time.
    pub fn downloads(&self) -> PathBuf {
        let private = match &self.downloads {
            Downloads::Given(folder) => return folder.clone(),
            Downloads::Private(private) => private,
        };
        let (folder, _) = private.get_or_init(|| {
            let made = tempfile::Builder::new()
                .prefix("iunctus-downloads-")
                .tempdir();
            // Without a temporary folder, `fetch` reports the error once it needs the folder.
            match made {
                Ok(folder) => (folder.path().to_owned(), Some(folder)),
                Err(_) => (std::env::temp_dir().join("iunctus-downloads"), None),
            }
        });
        folder.clone()
    }
}

/// Without storage providers of a config file, and with a private temporary folder of
/// downloads, made only when something is downloaded, for callers that give none.
impl Default for Remote {
    fn default() -> Self {
        Remote {
            storage: Storage::default(),
            downloads: Downloads::Private(Arc::default()),
        }
    }
}

/// Reads `file:` URLs from the local file system and `http:` and `https:` URLs through the
/// storage providers, as cwltool reads `run` and `$import` targets that are web addresses.
#[derive(Clone, Debug, Default)]
pub struct Web {
    remote: Remote,
}

impl Web {
    pub fn new(remote: Remote) -> Web {
        Web { remote }
    }
}

impl Source for Web {
    fn read(&self, url: &str) -> Result<String, String> {
        match iri::Parts::parse(url).scheme {
            Some("http" | "https") => {
                let downloads = self.remote.downloads();
                let copy =
                    (self.remote.storage.blocking().fetch(url, &downloads)).map_err(reason)?;
                std::fs::read_to_string(&copy).map_err(|error| error.to_string())
            }
            _ => FileSystem.read(url),
        }
    }

    fn exists(&self, url: &str) -> bool {
        FileSystem.exists(url)
    }
}

/// Why a document could not be fetched, without the address that the caller names already.
fn reason(error: StorageError) -> String {
    match error {
        StorageError::Failed {
            provider, message, ..
        } => format!("{message} ({provider})"),
        StorageError::Missing { provider, .. } => format!("it does not exist ({provider})"),
        error => error.to_string(),
    }
}

/// Documents held in memory by URL, for tests and for callers that read files themselves.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    files: HashMap<String, String>,
}

impl Memory {
    pub fn insert(&mut self, url: &str, text: &str) {
        self.files.insert(url.to_owned(), text.to_owned());
    }
}

impl Source for Memory {
    fn read(&self, url: &str) -> Result<String, String> {
        let found = self.files.get(url).cloned();
        found.ok_or_else(|| "no such document".to_owned())
    }

    /// Remote URLs count as present, as they do for the file system.
    fn exists(&self, url: &str) -> bool {
        let remote = matches!(
            iri::Parts::parse(url).scheme,
            Some("http" | "https" | "mailto")
        );
        remote || self.files.contains_key(url)
    }
}

/// The documents read so far, by URL without fragment.
pub struct Documents {
    source: Box<dyn Source>,
    texts: HashMap<String, Result<Arc<str>, String>>,
    trees: HashMap<String, Result<Node, Error>>,
}

impl Documents {
    pub fn new(source: Box<dyn Source>) -> Self {
        Self {
            source,
            texts: HashMap::new(),
            trees: HashMap::new(),
        }
    }

    pub fn exists(&self, url: &str) -> bool {
        self.source.exists(url)
    }

    /// The text of a document; `at` is where the reference to it was written.
    pub fn text(&mut self, url: &str, at: &Location) -> Result<Arc<str>, Error> {
        let source = &self.source;
        let read = self
            .texts
            .entry(url.to_owned())
            .or_insert_with(|| source.read(url).map(Arc::from));
        read.clone()
            .map_err(|reason| Error::new(at, format!("cannot read `{url}`: {reason}")))
    }

    /// The YAML tree of a document, with every location pointing into it.
    pub fn tree(&mut self, url: &str, at: &Location) -> Result<Node, Error> {
        if let Some(tree) = self.trees.get(url) {
            return tree.clone();
        }
        let text = self.text(url, at)?;
        let tree = yaml::read(&text, &Arc::from(url));
        self.trees.insert(url.to_owned(), tree.clone());
        tree
    }
}

#[cfg(test)]
mod tests {
    use super::{Downloads, Remote};

    #[test]
    fn default_makes_nothing() {
        let remote = Remote::default();
        let Downloads::Private(private) = &remote.downloads else {
            panic!("the default folder must be private");
        };
        assert!(private.get().is_none(), "made before first use");
        let folder = remote.clone().downloads();
        assert!(folder.is_dir(), "{folder:?}");
        assert_eq!(remote.downloads(), folder, "clones share the folder");
    }
}

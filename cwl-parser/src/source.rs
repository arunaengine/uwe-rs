//! Reads documents by URL and keeps each text and parsed tree for reuse.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

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

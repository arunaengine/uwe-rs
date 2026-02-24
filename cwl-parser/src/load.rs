//! Loads a CWL document and every process that its steps run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::Error;
use crate::iri::{self, split_fragment};
use crate::model::record::Context;
use crate::model::{Process, Runs, Version, check_workflows, read_process};
use crate::node::{Location, Node, Value};
use crate::salad::context::Namespaces;
use crate::salad::{Resolver, check_links, find_object};
use crate::source::{Documents, FileSystem, Source};

/// A loaded document: its main process, with every `run` loaded, and document metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub process: Arc<Process>,
    /// The `$namespaces` of the file, as prefix and IRI pairs.
    pub namespaces: Vec<(String, String)>,
    /// The `$schemas` of the file; they are not fetched.
    pub schemas: Vec<String>,
}

/// A document after Schema Salad preprocessing, with its version and metadata.
struct Prepared {
    tree: Arc<Node>,
    version: Version,
    namespaces: Vec<(String, String)>,
    schemas: Vec<String>,
}

/// Loads documents from one source; processes loaded once are shared between steps.
pub struct Loader {
    resolver: Resolver,
    prepared: HashMap<String, Result<Arc<Prepared>, Error>>,
    processes: HashMap<String, Arc<Process>>,
    loading: Vec<String>,
}

/// Loads a file, and everything it refers to, from the local file system.
pub fn load_file(path: impl AsRef<Path>) -> Result<Document, Error> {
    let mut loader = Loader::new(FileSystem);
    let path = path.as_ref().to_string_lossy();
    loader.load(&path)
}

impl Loader {
    pub fn new(source: impl Source + 'static) -> Self {
        Self {
            resolver: Resolver::new(Documents::new(Box::new(source))),
            prepared: HashMap::new(),
            processes: HashMap::new(),
            loading: Vec::new(),
        }
    }

    /// Loads the process a URL or path names; a `#fragment` selects a process of a packed file.
    pub fn load(&mut self, reference: &str) -> Result<Document, Error> {
        let url = to_url(reference);
        let file: Arc<str> = Arc::from(split_fragment(&url).0);
        let at = Location::document(&file);
        let process = self.process(&url, &at)?;
        check_workflows(&process, &[])?;
        let prepared = self.prepare(&file, &at)?;
        Ok(Document {
            process,
            namespaces: prepared.namespaces.clone(),
            schemas: prepared.schemas.clone(),
        })
    }

    /// Loads what cwltool validates for a reference: the process it names or, for a packed
    /// file without fragment and without `#main`, every process of the file.
    pub fn load_entries(&mut self, reference: &str) -> Result<Vec<Arc<Process>>, Error> {
        let url = to_url(reference);
        let (file, fragment) = split_fragment(&url);
        let at = Location::document(&Arc::from(file));
        let prepared = self.prepare(file, &at)?;
        let main = format!("{file}#main");
        let every = match prepared.tree.get("$graph") {
            Some(graph) if fragment.is_none() && find_object(graph, &main).is_none() => graph,
            _ => {
                let process = self.process(&url, &at)?;
                check_workflows(&process, &[])?;
                return Ok(vec![process]);
            }
        };
        let mut processes = Vec::new();
        for entry in every.as_list().unwrap_or_default() {
            let id = entry.get("id").and_then(Node::as_str).unwrap_or(file);
            let process = self.process(id, &entry.location)?;
            check_workflows(&process, &[])?;
            processes.push(process);
        }
        Ok(processes)
    }

    fn process(&mut self, url: &str, at: &Location) -> Result<Arc<Process>, Error> {
        if let Some(process) = self.processes.get(url) {
            return Ok(Arc::clone(process));
        }
        if self.loading.iter().any(|open| open == url) {
            return Err(Error::new(at, format!("`{url}` runs itself")));
        }
        let (file, fragment) = split_fragment(url);
        let prepared = self.prepare(file, at)?;
        let node = match (fragment, prepared.tree.get("$graph")) {
            (Some(_), _) => find_object(&prepared.tree, url),
            (None, Some(graph)) => find_object(graph, &format!("{file}#main")),
            (None, None) => Some(prepared.tree.as_ref()),
        };
        let Some(node) = node else {
            let message = match fragment {
                Some(_) => format!("`{url}` is not defined"),
                None => format!("`{file}` holds several processes but none named `#main`"),
            };
            return Err(Error::new(at, message));
        };
        self.loading.push(url.to_owned());
        let context = Context {
            version: prepared.version,
        };
        let read = read_process(node, &context, self, url);
        self.loading.pop();
        let process = Arc::new(read?);
        self.processes.insert(url.to_owned(), Arc::clone(&process));
        Ok(process)
    }

    /// Reads, resolves and link-checks a document once.
    fn prepare(&mut self, file: &str, at: &Location) -> Result<Arc<Prepared>, Error> {
        if let Some(prepared) = self.prepared.get(file) {
            return prepared.clone();
        }
        let prepared = self.resolve(file, at).map(Arc::new);
        self.prepared.insert(file.to_owned(), prepared.clone());
        prepared
    }

    fn resolve(&mut self, file: &str, at: &Location) -> Result<Prepared, Error> {
        let raw = self.resolver.documents.tree(file, at)?;
        let version = read_version(&raw)?;
        self.resolver.legacy = version == Version::V1_0;
        let mut tree = self.resolver.document(file, at, &Namespaces::default())?;
        check_links(
            &mut tree,
            file,
            &self.resolver.ids,
            &self.resolver.documents,
        )?;
        let namespaces = match tree.get("$namespaces").and_then(Node::as_map) {
            Some(entries) => entries
                .iter()
                .filter_map(|entry| Some((entry.key.clone(), entry.value.as_str()?.to_owned())))
                .collect(),
            None => Vec::new(),
        };
        let schemas = match tree.get("$schemas") {
            Some(node) => crate::model::record::texts(node)?,
            None => Vec::new(),
        };
        Ok(Prepared {
            tree: Arc::new(tree),
            version,
            namespaces,
            schemas,
        })
    }
}

impl Runs for Loader {
    fn load(&mut self, url: &str, at: &Location) -> Result<Arc<Process>, Error> {
        self.process(url, at)
    }
}

/// The `cwlVersion` of a document, which may carry the `cwl:` prefix.
fn read_version(root: &Node) -> Result<Version, Error> {
    let Some(node) = root.get("cwlVersion") else {
        let message = "the document has no `cwlVersion`";
        return Err(Error::new(&root.location, message));
    };
    let Value::String(text) = &node.value else {
        return Err(Error::new(&node.location, "`cwlVersion` must be a string"));
    };
    let text = text
        .strip_prefix("cwl:")
        .or_else(|| text.strip_prefix("https://w3id.org/cwl/cwl#"))
        .unwrap_or(text);
    Version::parse(text).ok_or_else(|| {
        let message = format!("unsupported cwlVersion `{text}`; supported are v1.0, v1.1 and v1.2");
        Error::new(&node.location, message)
    })
}

/// A URL for a reference given as a URL or as a path relative to the working directory.
fn to_url(reference: &str) -> String {
    if iri::has_scheme(reference) {
        return reference.to_owned();
    }
    let (path, fragment) = split_fragment(reference);
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let url = iri::file_url(&absolute);
    match fragment {
        Some(fragment) => format!("{url}#{fragment}"),
        None => url,
    }
}

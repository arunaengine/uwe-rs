//! The documents a workflow can reference by name: the WorkflowTemplates and
//! ClusterWorkflowTemplates of its own file, of extra files, and of its repository, which
//! stand in for the templates a cluster would hold.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use argo_parser::model::normalize;
use argo_parser::{Node, Value, parse_documents};

/// The kinds of Argo documents that iunctus reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Workflow,
    WorkflowTemplate,
    ClusterWorkflowTemplate,
    CronWorkflow,
}

impl Kind {
    fn parse(kind: &str) -> Option<Kind> {
        Some(match kind {
            "Workflow" => Kind::Workflow,
            "WorkflowTemplate" => Kind::WorkflowTemplate,
            "ClusterWorkflowTemplate" => Kind::ClusterWorkflowTemplate,
            "CronWorkflow" => Kind::CronWorkflow,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Workflow => "Workflow",
            Kind::WorkflowTemplate => "WorkflowTemplate",
            Kind::ClusterWorkflowTemplate => "ClusterWorkflowTemplate",
            Kind::CronWorkflow => "CronWorkflow",
        }
    }
}

/// One Argo document of a file, normalized to Argo v4.
#[derive(Debug)]
pub struct Document {
    pub path: PathBuf,
    /// The position of the document among the documents of its file, from 0.
    pub index: usize,
    pub kind: Kind,
    /// `metadata.name`, or `metadata.generateName` when there is no name.
    pub name: String,
    pub node: Node,
}

impl Document {
    /// The workflow spec: `spec`, or `spec.workflowSpec` of a CronWorkflow.
    pub fn spec(&self) -> Option<&Node> {
        let spec = self.node.get("spec")?;
        match self.kind {
            Kind::CronWorkflow => spec.get("workflowSpec"),
            _ => Some(spec),
        }
    }

    /// Whether the document can run on its own: it names an entrypoint or a WorkflowTemplate
    /// to run.
    pub fn runnable(&self) -> bool {
        let Some(spec) = self.spec() else {
            return false;
        };
        let entrypoint = spec
            .get("entrypoint")
            .and_then(Node::as_str)
            .is_some_and(|entrypoint| !entrypoint.is_empty());
        entrypoint || spec.get("workflowTemplateRef").is_some()
    }

    /// The template of the spec with this name.
    pub fn template(&self, name: &str) -> Option<&Node> {
        match &self.spec()?.get("templates")?.value {
            Value::Sequence(templates) => templates
                .iter()
                .find(|template| template.get("name").and_then(Node::as_str) == Some(name)),
            _ => None,
        }
    }

    /// A label for messages, like `WorkflowTemplate/samtools`.
    pub fn label(&self) -> String {
        format!("{}/{}", self.kind.name(), self.name)
    }
}

/// Reads the Argo documents of a file. Other documents, like Kubernetes objects, are left out.
pub fn load_file(path: &Path) -> Result<Vec<Rc<Document>>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let nodes = parse_documents(&text)
        .map_err(|error| format!("{}:{}: {}", path.display(), error.position, error.message))?;
    let mut documents = Vec::new();
    for (index, mut node) in nodes.into_iter().enumerate() {
        let api = node.get("apiVersion").and_then(Node::as_str).unwrap_or("");
        let Some(kind) = node
            .get("kind")
            .and_then(Node::as_str)
            .and_then(Kind::parse)
        else {
            continue;
        };
        if !api.starts_with("argoproj.io/") {
            continue;
        }
        normalize(&mut node);
        let metadata = node.get("metadata");
        let text = |key: &str| {
            metadata
                .and_then(|metadata| metadata.get(key))
                .and_then(Node::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let name = match text("name") {
            name if name.is_empty() => text("generateName"),
            name => name,
        };
        documents.push(Rc::new(Document {
            path: path.to_owned(),
            index,
            kind,
            name,
            node,
        }));
    }
    Ok(documents)
}

/// Documents by kind and name. Directories are read on the first lookup that the documents
/// added so far cannot answer.
#[derive(Debug, Default)]
pub struct Library {
    /// Documents with their priority: lower numbers win over closer paths.
    documents: RefCell<Vec<(u8, Rc<Document>)>>,
    directories: RefCell<Vec<(u8, PathBuf)>>,
}

impl Library {
    pub fn new() -> Self {
        Library::default()
    }

    /// Adds documents that win over those of directories added with a higher priority number.
    pub fn add_documents(&self, priority: u8, documents: impl IntoIterator<Item = Rc<Document>>) {
        self.documents
            .borrow_mut()
            .extend(documents.into_iter().map(|document| (priority, document)));
    }

    /// Adds a directory whose documents are read when first needed.
    pub fn add_directory(&self, priority: u8, directory: impl Into<PathBuf>) {
        self.directories
            .borrow_mut()
            .push((priority, directory.into()));
    }

    /// Reads a directory now, for callers that share one library among many runs.
    pub fn read_directories(&self) {
        let directories: Vec<(u8, PathBuf)> = self.directories.borrow_mut().drain(..).collect();
        for (priority, directory) in directories {
            let mut files = Vec::new();
            collect_files(&directory, &mut files);
            for file in files {
                if let Ok(documents) = load_file(&file) {
                    self.add_documents(priority, documents);
                }
            }
        }
    }

    /// The documents read so far.
    pub fn documents(&self) -> Vec<Rc<Document>> {
        self.documents
            .borrow()
            .iter()
            .map(|(_, document)| Rc::clone(document))
            .collect()
    }

    /// The document of this kind and name that is best for a reference from `near`: the one
    /// with the lowest priority number, then the one whose path shares most with `near`.
    pub fn find(&self, kind: Kind, name: &str, near: &Path) -> Option<Rc<Document>> {
        if let Some(found) = self.best(kind, name, near) {
            return Some(found);
        }
        if self.directories.borrow().is_empty() {
            return None;
        }
        self.read_directories();
        self.best(kind, name, near)
    }

    fn best(&self, kind: Kind, name: &str, near: &Path) -> Option<Rc<Document>> {
        let documents = self.documents.borrow();
        let mut best: Option<(u8, usize, &Rc<Document>)> = None;
        for (priority, document) in documents.iter() {
            if document.kind != kind || document.name != name {
                continue;
            }
            let shared = document
                .path
                .components()
                .zip(near.components())
                .take_while(|(a, b)| a == b)
                .count();
            let better = match best {
                None => true,
                Some((best_priority, best_shared, _)) => {
                    *priority < best_priority
                        || (*priority == best_priority && shared > best_shared)
                }
            };
            if better {
                best = Some((*priority, shared, document));
            }
        }
        best.map(|(_, _, document)| Rc::clone(document))
    }
}

/// The YAML and JSON files below a directory, in a stable order.
pub fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if path.is_dir() {
            if name != ".git" && name != "node_modules" {
                collect_files(&path, files);
            }
        } else if name.ends_with(".yaml") || name.ends_with(".yml") || name.ends_with(".json") {
            files.push(path);
        }
    }
}

/// The repository that holds a file: the nearest directory above it with a `.git`, or the
/// file's own directory.
pub fn repository_of(file: &Path) -> PathBuf {
    let file = std::path::absolute(file).unwrap_or_else(|_| file.to_owned());
    let directory = file.parent().unwrap_or(Path::new("/")).to_owned();
    directory
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_owned)
        .unwrap_or(directory)
}

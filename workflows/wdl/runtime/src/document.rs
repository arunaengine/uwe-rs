//! Loads and checks a WDL document with wdl-analysis, and finds what its calls name.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Arc;

use url::Url;
use wdl_analysis::types::v1::AstTypeConverter;
use wdl_analysis::types::{Type, TypeNameResolver};
use wdl_analysis::{Analyzer, Config, Document, IncrementalChange};
use wdl_ast::v1::{Ast, CallTarget, TaskDefinition, WorkflowDefinition};
use wdl_ast::{AstNode, AstToken, Diagnostic, Severity, Span, SupportedVersion, TreeNode};
use workflow_graph::DriverError;

/// A task or a workflow, with the document that holds it.
#[derive(Clone, Debug)]
pub struct Unit {
    pub document: Document,
    pub callable: Callable,
}

#[derive(Clone, Debug)]
pub enum Callable {
    Task(TaskDefinition),
    Workflow(WorkflowDefinition),
}

impl Unit {
    pub fn name(&self) -> String {
        match &self.callable {
            Callable::Task(task) => task.name().text().to_owned(),
            Callable::Workflow(workflow) => workflow.name().text().to_owned(),
        }
    }
}

/// Reads a document at a web address that a WDL document imports, and may refuse it.
#[derive(Clone)]
pub struct Imports(Arc<Read>);

type Read = dyn Fn(&str) -> Result<String, String> + Send + Sync;

impl Imports {
    pub fn new(read: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Imports {
        Imports(Arc::new(read))
    }
}

impl std::fmt::Debug for Imports {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Imports")
    }
}

/// The documents at web addresses that the document at `uri` imports, also through the
/// documents it imports, each read with `imports`. Addresses resolve as in the analysis.
fn remote_imports(uri: &Url, imports: &Imports) -> Result<Vec<(Url, String)>, DriverError> {
    let text = |uri: &Url| std::fs::read_to_string(uri.to_file_path().ok()?).ok();
    let root = text(uri).ok_or_else(|| DriverError::new(format!("cannot read {uri}")))?;
    let mut pending = vec![(uri.clone(), root)];
    let (mut seen, mut fetched) = (std::collections::HashSet::new(), Vec::new());
    while let Some((location, source)) = pending.pop() {
        let (document, _) = wdl_ast::Document::parse(&source, None);
        let wdl_ast::Ast::V1(ast) = document.ast() else {
            continue;
        };
        for import in ast.imports() {
            let wdl_ast::v1::ImportSource::Uri(given) = import.source() else {
                continue;
            };
            let Some(target) = given
                .text()
                .and_then(|given| location.join(given.text()).ok())
            else {
                continue;
            };
            if !seen.insert(target.clone()) {
                continue;
            }
            let source = match target.scheme() {
                "http" | "https" => {
                    let read = (imports.0)(target.as_str());
                    let source =
                        read.map_err(|why| DriverError::new(format!("{target}: {why}")))?;
                    fetched.push((target.clone(), source.clone()));
                    source
                }
                // A local import is read where it is; its own imports may be remote.
                _ => match text(&target) {
                    Some(source) => source,
                    None => continue,
                },
            };
            pending.push((target, source));
        }
    }
    Ok(fetched)
}

/// Loads a document and its imports, and fails on the errors that the analysis finds.
/// With `imports`, the analysis reads each document at a web address only through it.
pub fn load(path: &Path, imports: Option<&Imports>) -> Result<Document, DriverError> {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let uri = wdl_analysis::path_to_uri(&absolute)
        .ok_or_else(|| DriverError::new(format!("{} is not a file path", path.display())))?;
    let fetched = match imports {
        Some(imports) => remote_imports(&uri, imports)?,
        None => Vec::new(),
    };
    // The analyzer needs a Tokio runtime of its own; the engine may already run one.
    let analysis = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async {
                let analyzer = Analyzer::new(Config::default(), |_: (), _, _, _| async {});
                analyzer
                    .add_document(uri.clone())
                    .await
                    .map_err(|error| error.to_string())?;
                for (address, source) in fetched {
                    let change = IncrementalChange {
                        version: 1,
                        start: Some(source),
                        edits: Vec::new(),
                    };
                    let added = analyzer.add_document(address.clone()).await;
                    added
                        .and_then(|()| analyzer.notify_incremental_change(address, change))
                        .map_err(|error| error.to_string())?;
                }
                analyzer
                    .analyze(())
                    .await
                    .map_err(|error| error.to_string())
            })
        });
        worker
            .join()
            .unwrap_or_else(|_| Err("the analysis of the document failed".into()))
    });
    let results = analysis.map_err(DriverError::new)?;
    let mut errors = Vec::new();
    for result in &results {
        if let Some(error) = result.error() {
            errors.push(format!("{}: {error}", result.document().uri()));
        }
        for diagnostic in result.document().diagnostics() {
            if diagnostic.severity() == Severity::Error {
                errors.push(located(result.document(), diagnostic));
            }
        }
    }
    if !errors.is_empty() {
        return Err(DriverError::new(errors.join("\n")));
    }
    let document = (results.into_iter())
        .find(|result| *result.document().uri() == uri)
        .map(|result| result.document().clone())
        .ok_or_else(|| DriverError::new(format!("{} was not analyzed", path.display())))?;
    match document.version() {
        Some(SupportedVersion::V1(_)) => Ok(document),
        _ => Err(DriverError::unsupported(format!(
            "{}: only WDL 1.0, 1.1 and 1.2 documents are supported",
            path.display()
        ))),
    }
}

/// The message of a diagnostic with the file and line where it points.
fn located(document: &Document, diagnostic: &Diagnostic) -> String {
    let path = document.path();
    let line = (diagnostic.labels().next()).and_then(|label| {
        let text = std::fs::read_to_string(document.uri().to_file_path().ok()?).ok()?;
        let start = label.span().start().min(text.len());
        Some(text[..start].matches('\n').count() + 1)
    });
    match line {
        Some(line) => format!("{path}:{line}: {}", diagnostic.message()),
        None => format!("{path}: {}", diagnostic.message()),
    }
}

/// The syntax tree of a document.
pub fn tree(document: &Document) -> Ast {
    match document.root().ast() {
        wdl_ast::Ast::V1(ast) => ast,
        wdl_ast::Ast::Unsupported => {
            unreachable!("the version was checked when the document loaded")
        }
    }
}

/// The task or workflow that runs a document: the one named by `entry`, else its workflow,
/// else its only task.
pub fn entry(document: &Document, entry: Option<&str>) -> Result<Unit, DriverError> {
    let ast = tree(document);
    let unit = |callable| Unit {
        document: document.clone(),
        callable,
    };
    if let Some(name) = entry {
        let workflow = ast
            .workflows()
            .find(|workflow| workflow.name().text() == name);
        let task = ast.tasks().find(|task| task.name().text() == name);
        return match (workflow, task) {
            (Some(workflow), _) => Ok(unit(Callable::Workflow(workflow))),
            (None, Some(task)) => Ok(unit(Callable::Task(task))),
            _ => Err(DriverError::new(format!(
                "the document has no task or workflow `{name}`"
            ))),
        };
    }
    if let Some(workflow) = ast.workflows().next() {
        return Ok(unit(Callable::Workflow(workflow)));
    }
    let mut tasks = ast.tasks();
    match (tasks.next(), tasks.next()) {
        (Some(task), None) => Ok(unit(Callable::Task(task))),
        (None, _) => Err(DriverError::new("the document has no workflow and no task")),
        (Some(_), Some(_)) => Err(DriverError::new(
            "the document has no workflow and several tasks; name one with `-entry`",
        )),
    }
}

/// The task or workflow that a call names, like `align` or `lib.align`.
pub fn called(document: &Document, target: &CallTarget) -> Result<Unit, String> {
    let names: Vec<String> = target.names().map(|name| name.text().to_owned()).collect();
    let (holder, name) = match names.as_slice() {
        [name] => (document.clone(), name),
        [namespace, name] => {
            let namespace = (document.namespace(namespace))
                .ok_or_else(|| format!("unknown namespace `{namespace}`"))?;
            (namespace.document().clone(), name)
        }
        _ => return Err(format!("cannot call `{}`", names.join("."))),
    };
    let ast = tree(&holder);
    let unit = |callable| Unit {
        document: holder.clone(),
        callable,
    };
    if let Some(task) = ast.tasks().find(|task| task.name().text() == name.as_str()) {
        return Ok(unit(Callable::Task(task)));
    }
    if let Some(workflow) = ast
        .workflows()
        .find(|workflow| workflow.name().text() == name.as_str())
    {
        return Ok(unit(Callable::Workflow(workflow)));
    }
    Err(format!("no task or workflow `{}`", names.join(".")))
}

/// The type of a declaration, with the structs of its document.
pub fn declared<N: TreeNode>(
    document: &Document,
    ty: &wdl_ast::v1::Type<N>,
) -> Result<Type, String> {
    struct Structs<'a>(&'a Document);

    impl TypeNameResolver for Structs<'_> {
        fn resolve(&mut self, name: &str, span: Span) -> Result<Type, Diagnostic> {
            (self.0.struct_by_name(name))
                .and_then(|found| found.ty().cloned())
                .ok_or_else(|| wdl_analysis::diagnostics::unknown_type(name, span))
        }
    }

    (AstTypeConverter::new(Structs(document)).convert_type(ty))
        .map_err(|diagnostic| diagnostic.message().to_owned())
}

/// The span of a node in a document, as `path:line`, for messages.
pub fn place(document: &Document, node: &impl AstNode<wdl_ast::SyntaxNode>) -> String {
    let path = document.path();
    let line = (document.uri().to_file_path().ok())
        .and_then(|file| std::fs::read_to_string(file).ok())
        .map(|text| {
            text[..node.span().start().min(text.len())]
                .matches('\n')
                .count()
                + 1
        });
    match line {
        Some(line) => format!("{path}:{line}"),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The analysis takes the imports at web addresses from the reader, which resolves them as
    /// the analysis does, and fetches none itself: no host of `.invalid` exists.
    #[test]
    fn reads_guarded_imports() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("main.wdl");
        let main = "version 1.1\nimport \"HTTP://lib.invalid/sub/lib.wdl\" as lib\n\nworkflow main {\n  call lib.hello\n}\n";
        std::fs::write(&path, main).unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let imports = Imports::new(move |address| {
            seen.lock().unwrap().push(address.to_owned());
            Ok(match address.ends_with("lib.wdl") {
                true => "version 1.1\nimport \"more.wdl\" as more\ntask hello {\n  command <<< echo hi >>>\n}\n",
                false => "version 1.1\ntask other {\n  command <<< echo hi >>>\n}\n",
            }
            .to_owned())
        });
        let loaded = load(&path, Some(&imports));
        assert!(
            loaded.is_ok(),
            "{:?}",
            loaded.err().map(|error| error.message)
        );
        assert_eq!(
            *asked.lock().unwrap(),
            [
                "http://lib.invalid/sub/lib.wdl",
                "http://lib.invalid/sub/more.wdl"
            ]
        );
    }
}

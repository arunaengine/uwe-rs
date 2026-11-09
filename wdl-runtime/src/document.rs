//! Loads and checks a WDL document with wdl-analysis, and finds what its calls name.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use wdl_analysis::types::v1::AstTypeConverter;
use wdl_analysis::types::{Type, TypeNameResolver};
use wdl_analysis::{Analyzer, Config, Document};
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

/// Reads every document at a web address that `path` imports, also through the documents it
/// imports, with `read`, which may refuse one, before the analysis of [`load`] fetches it.
pub fn remote_imports(
    path: &Path,
    read: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<(), DriverError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| DriverError::new(format!("cannot read {}: {error}", path.display())))?;
    let start = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let mut pending = vec![(start.to_string_lossy().into_owned(), text)];
    let mut seen = std::collections::HashSet::new();
    while let Some((location, text)) = pending.pop() {
        let (document, _) = wdl_ast::Document::parse(&text, None);
        let wdl_ast::Ast::V1(ast) = document.ast() else {
            continue;
        };
        for import in ast.imports() {
            let wdl_ast::v1::ImportSource::Uri(uri) = import.source() else {
                continue;
            };
            let Some(given) = uri.text() else {
                continue;
            };
            let given = given.text().to_owned();
            let target = match (given.contains("://"), location.contains("://")) {
                (true, _) => given,
                (false, true) => match location.rsplit_once('/') {
                    Some((folder, _)) => format!("{folder}/{given}"),
                    None => given,
                },
                (false, false) => {
                    let folder = Path::new(&location).parent().unwrap_or(Path::new("/"));
                    folder.join(&given).to_string_lossy().into_owned()
                }
            };
            if !seen.insert(target.clone()) {
                continue;
            }
            let text = match target.starts_with("http://") || target.starts_with("https://") {
                true => {
                    read(&target).map_err(|why| DriverError::new(format!("{target}: {why}")))?
                }
                // A local import is read where it is; its own imports may be remote.
                false => match std::fs::read_to_string(target.trim_start_matches("file://")) {
                    Ok(text) => text,
                    Err(_) => continue,
                },
            };
            pending.push((target, text));
        }
    }
    Ok(())
}

/// Loads a document and its imports, and fails on the errors that the analysis finds.
pub fn load(path: &Path) -> Result<Document, DriverError> {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let uri = wdl_analysis::path_to_uri(&absolute)
        .ok_or_else(|| DriverError::new(format!("{} is not a file path", path.display())))?;
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

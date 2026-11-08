//! Groovy callbacks of Nextflow sources: the scope script loaded with its includes, run in an
//! interpreter of the run with its params, globals and plugins, and captured closures rebuilt.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Interpreter};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::Value;
use nextflow_runtime::driver::Settings;
use nextflow_runtime::{Adapter, Loader};
use serde_json::{Map, Value as Json};
use wir::{Document, Language, TaskBuilder};
use workflow_graph::Launch;

use crate::engine::Context;
use crate::groovy::ENTRY;

/// The run of a Nextflow scope: its main script, the script of the scope, the profiles,
/// config files and params of the run, and the scripts of the document by source id.
#[derive(Clone, Debug)]
pub struct Launching {
    main: PathBuf,
    path: PathBuf,
    profiles: Vec<String>,
    configs: Vec<PathBuf>,
    params: Json,
    scratch: PathBuf,
    simulated: bool,
    sources: HashMap<String, PathBuf>,
}

/// The run of the scope of a callback, when its source is a Nextflow script.
pub fn launching(context: &Context<'_>, source: &str) -> Option<Launching> {
    let document = context.document;
    let sources: HashMap<String, PathBuf> = (document.sources.iter())
        .filter(|source| source.language == Language::Nextflow)
        .filter(|source| !source.location.contains(':'))
        .map(|source| (source.id.clone(), context.base.join(&source.location)))
        .collect();
    let path = sources.get(source)?.clone();
    let main = sources.get("main").filter(|main| main.is_file())?.clone();
    let task = document
        .graphs
        .iter()
        .flat_map(|graph| &graph.ops)
        .find_map(|op| match &op.kind {
            wir::Kind::Task(task) => match &task.builder {
                Some(TaskBuilder::NextflowProcess(task)) => Some(task),
                _ => None,
            },
            _ => None,
        });
    let configs = (task.iter()).flat_map(|task| &task.configs);
    let configs = configs.filter_map(|id| sources.get(id).cloned()).collect();
    Some(Launching {
        main,
        path,
        profiles: task.map(|task| task.profiles.clone()).unwrap_or_default(),
        configs,
        params: crate::nextflow::params(&defaults(document)),
        scratch: context.files.to_owned(),
        simulated: context.simulated,
        sources,
    })
}

/// The defaults of the inputs of the entry graph, which are the params of the run.
fn defaults(document: &Document) -> Map<String, Json> {
    let entry = document
        .graphs
        .iter()
        .find(|graph| graph.name == document.entry);
    let inputs = entry.into_iter().flat_map(|graph| &graph.inputs);
    let given = inputs.filter_map(|port| Some((port.name.clone(), port.default.clone()?)));
    given.collect()
}

thread_local! {
    /// The interpreters of the runs, by run, which keep the loaded config and plugins.
    static ADAPTERS: RefCell<HashMap<String, Rc<RefCell<Adapter>>>> = RefCell::default();
    /// The scripts loaded with the code of a callback, by script and code.
    static MODULES: RefCell<HashMap<(PathBuf, String), Rc<Module>>> = RefCell::default();
}

/// A callback of a Nextflow scope, loaded once.
pub struct Scoped {
    module: Rc<Module>,
    names: Vec<String>,
    launching: Rc<Launching>,
}

impl Scoped {
    /// Loads `code` as the body of a function of `names` at the end of the scope script,
    /// after the `library` lines.
    pub fn load(
        launching: Launching,
        code: &str,
        library: &[String],
        names: Vec<String>,
    ) -> Result<Scoped, String> {
        let mut extra = library.join("\n");
        extra.push_str(&format!(
            "\ndef {ENTRY}({}) {{\n    return ({code})\n}}\n",
            names.join(", ")
        ));
        let module = module(&launching, &launching.path, &extra)?;
        Ok(Scoped {
            module,
            names,
            launching: Rc::new(launching),
        })
    }

    /// The value of the code, or of the closure it gives called with `arguments`.
    pub fn run(
        &self,
        values: &Map<String, Json>,
        arguments: Option<&[Json]>,
        base: &Path,
    ) -> Result<Json, String> {
        let adapter = adapter(&self.launching)?;
        let launching = &self.launching;
        let result = adapter.borrow_mut().evaluate(|interpreter| {
            let mut given = Vec::new();
            for name in &self.names {
                let value = values.get(name).unwrap_or(&Json::Null);
                given.push(rebuilt(value, base, launching, interpreter)?);
            }
            let made = interpreter.call_function(&self.module, ENTRY, given)?;
            let Some(arguments) = arguments else {
                return Ok(made);
            };
            let Value::Closure(closure) = made else {
                return Err(unsupported("the Groovy code gives no closure"));
            };
            let mut items = Vec::new();
            for argument in arguments {
                items.push(rebuilt(argument, base, launching, interpreter)?);
            }
            interpreter.call_closure(&closure, items)
        });
        crate::groovy::to_json(&result.map_err(|error| error.message)?)
    }
}

/// A wir value as Groovy sees it, with captured closures in the reserved `Closure` form made
/// again in their scripts.
fn rebuilt(
    json: &Json,
    base: &Path,
    launching: &Launching,
    interpreter: &mut Interpreter,
) -> Result<Value, Error> {
    match json {
        Json::Array(items) => {
            let mut made = Vec::new();
            for item in items {
                made.push(rebuilt(item, base, launching, interpreter)?);
            }
            Ok(Value::list(made))
        }
        Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("Closure") => {
            let source = object
                .get("source")
                .and_then(Json::as_str)
                .unwrap_or("main");
            let path = (launching.sources.get(source))
                .ok_or_else(|| unsupported(&format!("a closure of the unknown source {source}")))?;
            let code = object
                .get("code")
                .and_then(Json::as_str)
                .unwrap_or("{ -> }");
            let captures = object.get("captures").and_then(Json::as_object);
            let captures: Vec<(&String, &Json)> = captures.into_iter().flatten().collect();
            let names: Vec<&str> = captures.iter().map(|(name, _)| name.as_str()).collect();
            let extra = format!(
                "def {ENTRY}({}) {{\n    return ({code})\n}}\n",
                names.join(", ")
            );
            let module = module(launching, path, &extra).map_err(|why| unsupported(&why))?;
            let mut given = Vec::new();
            for (_, value) in captures {
                given.push(rebuilt(value, base, launching, interpreter)?);
            }
            interpreter.call_function(&module, ENTRY, given)
        }
        Json::Object(object) if object.get("class").is_none() => {
            let mut entries = Vec::new();
            for (key, item) in object {
                let item = rebuilt(item, base, launching, interpreter)?;
                entries.push((Value::string(key.as_str()), item));
            }
            Ok(Value::map(entries))
        }
        other => Ok(crate::groovy::to_value(other, base)),
    }
}

fn unsupported(message: &str) -> Error {
    Error::Unsupported {
        message: message.to_owned(),
        span: Default::default(),
    }
}

/// The script at `path` with `extra` code at its end, loaded once with its includes.
fn module(launching: &Launching, path: &Path, extra: &str) -> Result<Rc<Module>, String> {
    let key = (path.to_owned(), extra.to_owned());
    if let Some(module) = MODULES.with_borrow(|modules| modules.get(&key).cloned()) {
        return Ok(module);
    }
    let loaded = Loader::default().load_extended(&launching.main, path, extra);
    let module = loaded.map_err(|error| format!("the Groovy code does not load: {error}"))?;
    MODULES.with_borrow_mut(|modules| modules.insert(key, module.clone()));
    Ok(module)
}

/// The interpreter of a run, made once with the config and params of the run.
fn adapter(launching: &Launching) -> Result<Rc<RefCell<Adapter>>, String> {
    let key = format!(
        "{}|{:?}|{:?}|{}|{}",
        launching.main.display(),
        launching.profiles,
        launching.configs,
        launching.params,
        launching.scratch.display()
    );
    if let Some(adapter) = ADAPTERS.with_borrow(|adapters| adapters.get(&key).cloned()) {
        return Ok(adapter);
    }
    let launch = Launch {
        target: launching.main.clone(),
        profiles: launching.profiles.clone(),
        configs: launching.configs.clone(),
        scratch: launching.scratch.clone(),
        simulated: launching.simulated,
        ..Launch::default()
    };
    let settings = Settings {
        run_name: "uwe-callbacks".into(),
        ..Settings::default()
    };
    let made = Adapter::new(&launch, settings, launching.params.clone());
    let adapter = Rc::new(RefCell::new(made.map_err(|error| error.message)?));
    ADAPTERS.with_borrow_mut(|adapters| adapters.insert(key, adapter.clone()));
    Ok(adapter)
}

//! Groovy callbacks of Nextflow sources: the scope script loaded with its includes, run in an
//! interpreter of the run with its params, globals and plugins, and captured closures rebuilt.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Interpreter};
use nextflow_evaluator::lowered::{Copied, Module};
use nextflow_evaluator::value::Value;
use nextflow_runtime::{Adapter, Loader, RunEnd};
use serde_json::{Map, Value as Json};
use wir::{Document, Language, TaskBuilder};
use workflow_graph::{Delivery, DeliveryMode, Launch};

use crate::engine::Context;
use crate::groovy::ENTRY;
use crate::operator::Turn;

/// What the Groovy callbacks of one wir run share: how Nextflow drivers get their settings,
/// and the interpreters and loaded scripts of the run, which end with it.
#[derive(Clone, Default)]
pub struct ScopeRun(Rc<RunState>);

/// The cell of a variable of a script, which the closures of all callbacks capture.
type Variable = Rc<RefCell<Value>>;

#[derive(Default)]
struct RunState {
    options: crate::nextflow::Options,
    adapters: RefCell<HashMap<String, Rc<RefCell<Adapter>>>>,
    modules: RefCell<HashMap<(PathBuf, String), Rc<Module>>>,
    /// The variables of the scripts that callbacks read, by source and name, which all
    /// callbacks of the run share, as the code of a native run does.
    variables: RefCell<HashMap<(String, String), Variable>>,
    /// The published values of each output op, with their files at their new places.
    indexes: RefCell<HashMap<String, Vec<Json>>>,
    /// The scratch folder of the interpreters, which goes with the run.
    folder: RefCell<Option<tempfile::TempDir>>,
}

impl RunState {
    /// The scratch folder of the interpreters: below the file area of the run, or a temporary
    /// folder when the run has none; never the working folder.
    fn folder(&self, area: &Path) -> Result<PathBuf, String> {
        let mut folder = self.folder.borrow_mut();
        if folder.is_none() {
            let made = match area.as_os_str().is_empty() {
                true => tempfile::Builder::new().prefix("callbacks-").tempdir(),
                false => std::fs::create_dir_all(area).and_then(|()| {
                    tempfile::Builder::new()
                        .prefix("callbacks-")
                        .tempdir_in(area)
                }),
            };
            *folder = Some(made.map_err(|error| format!("no folder for callbacks: {error}"))?);
        }
        Ok(folder
            .as_ref()
            .map(|folder| folder.path().to_owned())
            .unwrap_or_default())
    }
}

impl ScopeRun {
    pub fn new(options: crate::nextflow::Options) -> ScopeRun {
        ScopeRun(Rc::new(RunState {
            options,
            ..RunState::default()
        }))
    }

    /// What the code of the scopes noticed since the last call, like reads of task outputs.
    pub fn notes(&self) -> Vec<workflow_graph::Note> {
        let adapters = self.0.adapters.borrow();
        let notes = adapters
            .values()
            .flat_map(|adapter| adapter.borrow_mut().notes());
        notes.collect()
    }
}

/// The run of a Nextflow scope: its main script, the script of the scope, the profiles,
/// config files and params of the run, and the scripts of the document by source id.
#[derive(Clone)]
pub struct Launching {
    run: ScopeRun,
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
        .map(|source| {
            let path = match nextflow_runtime::plugin_script(&source.location) {
                Some(_) => PathBuf::from(&source.location),
                None => context.base.join(&source.location),
            };
            (source.id.clone(), path)
        })
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
    let mut params = defaults(document);
    params.extend(context.inputs.clone());
    Some(Launching {
        run: context.nextflow.clone(),
        main,
        path,
        profiles: task.map(|task| task.profiles.clone()).unwrap_or_default(),
        configs,
        params: crate::nextflow::params(&params),
        scratch: context.files.to_owned(),
        simulated: context.simulated,
        sources,
    })
}

/// The text of a source that is the script of a Nextflow plugin, which has no file.
pub fn plugin_text(document: &Document, id: &str) -> Option<&'static str> {
    let source = document.sources.iter().find(|source| source.id == id)?;
    match source.language {
        Language::Nextflow => nextflow_runtime::plugin_script(&source.location),
        _ => None,
    }
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
        (code, closure): (&str, Range<u64>),
        library: &[String],
        names: Vec<String>,
    ) -> Result<Scoped, String> {
        let mut extra = library.join("\n");
        extra.push_str(&format!(
            "\ndef {ENTRY}({}) {{\n    return (",
            names.join(", ")
        ));
        let at = extra.len();
        extra.push_str(&format!("{code})\n}}\n"));
        let module = module(&launching, &launching.path, &extra)?;
        copied(&module, (&extra, at, code), closure);
        Ok(Scoped {
            module,
            names,
            launching: Rc::new(launching),
        })
    }

    /// The value of the code, or of the closure it gives called with `arguments`, printing
    /// through `printer` if given; a [`HOOK`] records the end of the run in `workflow` first.
    pub fn run(
        &self,
        values: &Map<String, Json>,
        arguments: Option<&[Json]>,
        base: &Path,
        printer: Option<&crate::driver::Printer>,
    ) -> Result<Json, String> {
        let adapter = adapter(&self.launching)?;
        let launching = &self.launching;
        let hook = values.get(HOOK).and_then(Json::as_str);
        let hooked;
        let arguments = match (hook, arguments) {
            (Some(kind), Some([run])) => {
                let error = (run["success"] != true).then(|| run["error"].as_str().unwrap_or(""));
                let end = RunEnd {
                    start: run["start"].as_i64(),
                    succeeded: run["succeeded"].as_i64().unwrap_or(0),
                    trace: trace(&run["failed"]),
                };
                adapter.borrow_mut().finish_run(error, &end);
                // The plugins act once the `onComplete` handlers ran, as in a native run.
                if kind == PLUGINS {
                    adapter.borrow_mut().finish_plugins();
                    return Ok(Json::Null);
                }
                hooked = match kind {
                    "error" => vec![end.trace.unwrap_or_default()],
                    _ => Vec::new(),
                };
                Some(&hooked[..])
            }
            _ => arguments,
        };
        // The captured channels of a handler, which it reads as value channels.
        let channels: Vec<&str> = (values.get(CHANNELS).and_then(Json::as_array))
            .map(|names| names.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        let bound: Vec<Json> = (channels.iter())
            .map(|name| values.get(*name).cloned().unwrap_or_default())
            .collect();
        let code = |interpreter: &mut Interpreter, made: Vec<Value>| {
            let made: HashMap<&str, Value> = channels.iter().copied().zip(made).collect();
            let mut given = Vec::new();
            for name in &self.names {
                if let Some(channel) = made.get(name.as_str()) {
                    given.push(channel.clone());
                    continue;
                }
                let value = values.get(name).unwrap_or(&Json::Null);
                given.push(rebuilt(value, base, launching, interpreter)?);
            }
            let made = interpreter.call_function(&self.module, ENTRY, given)?;
            let made = shared(made, Some(values), launching);
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
        };
        let result = match printer {
            Some(printer) if !bound.is_empty() => {
                let print = |text: &str| printer.print(text);
                adapter.borrow_mut().evaluate_ended(&print, &bound, code)
            }
            Some(printer) => adapter
                .borrow_mut()
                .evaluate_printed(&|text| printer.print(text), |at| code(at, Vec::new())),
            None => adapter.borrow_mut().evaluate(|at| code(at, Vec::new())),
        };
        crate::groovy::to_json(&result.map_err(|error| error.message)?)
    }
}

/// The capture that marks a run hook of the `workflow.onComplete` or `onError` handlers, as
/// `complete` or `error`, or the end of the plugins of the run, as [`PLUGINS`].
pub const HOOK: &str = "wirHook";

/// The run hook in which the plugins of the config act at the end of the run.
pub const PLUGINS: &str = "plugins";

/// The capture that names the captures of a handler that are value channels.
pub const CHANNELS: &str = "wirChannels";

/// The trace of the task that failed the run, with its id in the run, as `onError` gets it.
fn trace(failed: &Json) -> Option<Json> {
    let mut shown = failed.get("shown")?.clone();
    if let (Some(task), Json::Object(object)) = (failed.get("task"), &mut shown) {
        object.insert("task_id".into(), Json::String(task.to_string()));
    }
    Some(shown)
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
            let made = interpreter.call_function(&module, ENTRY, given)?;
            Ok(shared(
                made,
                object.get("captures").and_then(Json::as_object),
                launching,
            ))
        }
        Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("Script") => {
            let key = script_key(Some(json)).unwrap_or_default();
            if let Some(shared) = launching.run.0.variables.borrow().get(&key) {
                return Ok(shared.borrow().clone());
            }
            let value = object.get("value").unwrap_or(&Json::Null);
            let value = rebuilt(value, base, launching, interpreter)?;
            let cell = Rc::new(RefCell::new(value.clone()));
            launching.run.0.variables.borrow_mut().insert(key, cell);
            Ok(value)
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

/// The closure `made` with its captures of script variables, in `captures`, bound to the cells
/// of the run, so that what it assigns is there for later callbacks, as in native code.
fn shared(made: Value, captures: Option<&Map<String, Json>>, launching: &Launching) -> Value {
    let Value::Closure(closure) = &made else {
        return made;
    };
    let variables = launching.run.0.variables.borrow();
    let mut captured = closure.captured.clone();
    let names = closure.code.capture_names.iter().zip(&mut captured);
    for (name, cell) in names {
        let key = script_key(captures.and_then(|captures| captures.get(name.as_ref())));
        if let Some(shared) = key.and_then(|key| variables.get(&key)) {
            *cell = shared.clone();
        }
    }
    Value::Closure(Rc::new(nextflow_evaluator::value::Closure {
        code: closure.code.clone(),
        module: closure.module.clone(),
        captured,
    }))
}

/// The source and name of a variable of a script that a capture holds in its reserved
/// `Script` form, which the callbacks of a run share.
fn script_key(json: Option<&Json>) -> Option<(String, String)> {
    let object = json?.as_object()?;
    let text = |key| {
        object
            .get(key)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    (object.get("class").and_then(Json::as_str) == Some("Script"))
        .then(|| (text("source"), text("name")))
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
    let modules = &launching.run.0.modules;
    if let Some(module) = modules.borrow().get(&key).cloned() {
        return Ok(module);
    }
    let loaded = Loader::default().load_extended(&launching.main, path, extra);
    let module = loaded.map_err(|error| format!("the Groovy code does not load: {error}"))?;
    modules.borrow_mut().insert(key, module.clone());
    Ok(module)
}

/// Notes where the code appended to the script, `extra` with the callback `code` at `at`,
/// copies the closure of the script at `closure`, which a rewrite, like a `branch` case, holds.
fn copied(module: &Module, (extra, at, code): (&str, usize, &str), closure: Range<u64>) {
    let source = module.source.borrow();
    let Some(source) = source.as_deref() else {
        return;
    };
    let appended = source.len().saturating_sub(extra.len());
    let original = (usize::try_from(closure.start).ok())
        .zip(usize::try_from(closure.end).ok())
        .and_then(|(start, end)| source.get(start..end).filter(|_| end <= appended));
    let inside = original.and_then(|text| code.find(text));
    let offset = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
    module.copied.set(Some(Copied {
        appended: offset(appended),
        copy: offset(appended + at + inside.unwrap_or(0)),
        length: offset(inside.and(original).map_or(0, str::len)),
        original: offset(usize::try_from(closure.start).unwrap_or(0)),
    }));
}

/// The interpreter of a run, made once with the config and params of the run.
fn adapter(launching: &Launching) -> Result<Rc<RefCell<Adapter>>, String> {
    let key = format!(
        "{}|{:?}|{:?}|{}",
        launching.main.display(),
        launching.profiles,
        launching.configs,
        launching.params,
    );
    let adapters = &launching.run.0.adapters;
    if let Some(adapter) = adapters.borrow().get(&key).cloned() {
        return Ok(adapter);
    }
    // A scratch folder of its own, which the driver deletes when it ends, so that the run
    // keeps its name and session.
    let scratch = launching.run.0.folder(&launching.scratch)?;
    let inputs = crate::nextflow::params_file(&scratch, &launching.params);
    let launch = Launch {
        target: launching.main.clone(),
        profiles: launching.profiles.clone(),
        configs: launching.configs.clone(),
        inputs: Some(inputs.map_err(|error| error.message)?),
        scratch,
        simulated: launching.simulated,
        ..Launch::default()
    };
    let options = &launching.run.0.options;
    let folder = options.launch_folder().map_err(|error| error.message)?;
    let settings = (options.settings)(&launch, &folder);
    let made = Adapter::new(&launch, settings, launching.params.clone());
    let adapter = Rc::new(RefCell::new(made.map_err(|error| error.message)?));
    adapters.borrow_mut().insert(key, adapter.clone());
    Ok(adapter)
}

/// The callbacks of the `output { }` block as the compiler writes them, with the options of
/// the output as the capture `output`.
const PUBLISH: &str = "{ value -> publishOutput(output, value) }";
const INDEX: &str = "{ -> indexOutput(output) }";

/// Publishes the files of an item of a workflow output, or writes its index at the close, as
/// the native driver does, when `closure` is a callback of the `output { }` block.
pub fn output(
    turn: &Turn<'_>,
    context: &Context<'_>,
    closure: &wir::Closure,
    arguments: &[Json],
) -> Option<Result<(), String>> {
    let Some(wir::Binding::Value(output)) = closure.captures.get("output") else {
        return None;
    };
    match (closure.code.as_str(), arguments) {
        (PUBLISH, [item]) => Some(publish(turn, context, output, item)),
        (INDEX, []) => Some(index(turn, context, output)),
        _ => None,
    }
}

/// Where the files of one item go: a folder they keep their names in, or a place by name.
enum Target {
    Folder(PathBuf),
    Files(HashMap<String, PathBuf>),
    Nothing,
}

fn publish(
    turn: &Turn<'_>,
    context: &Context<'_>,
    output: &Json,
    item: &Json,
) -> Result<(), String> {
    let directory = PathBuf::from(output["directory"].as_str().unwrap_or("results"));
    let target = match &output["options"]["path"] {
        Json::Null => Target::Folder(directory.clone()),
        Json::String(path) => Target::Folder(directory.join(path)),
        form => targets(context, form, item, &directory)?,
    };
    let mode = serde_json::from_value(output["mode"].clone()).unwrap_or_default();
    let overwrite = output["overwrite"].as_bool().unwrap_or(true);
    let mut places = HashMap::new();
    let mut deliveries = Vec::new();
    // As in Nextflow, only the files of tasks go out, by their names in the task folders.
    let options = &context.nextflow.0.options;
    each_file(item, &mut |file| {
        let location = file["location"].as_str().unwrap_or_default().to_owned();
        let Some(name) = options.task_file(&location) else {
            return;
        };
        let name = name.as_str();
        let place = match &target {
            Target::Folder(folder) => folder.join(name),
            Target::Files(places) => match places.get(name) {
                Some(place) => place.clone(),
                None => return,
            },
            Target::Nothing => return,
        };
        let place = place.to_string_lossy().into_owned();
        places.insert(location.clone(), place.clone());
        deliveries.push(Delivery {
            location,
            target: place,
            mode,
            overwrite,
        });
    });
    context.deliveries.ask(&deliveries);
    if output["index"]["path"].is_string() {
        let record = placed(item, &places);
        let mut indexes = context.nextflow.0.indexes.borrow_mut();
        indexes.entry(turn.path.clone()).or_default().push(record);
    }
    Ok(())
}

/// The targets of an item from the `path` closure of an output, with its publish statements.
fn targets(
    context: &Context<'_>,
    form: &Json,
    item: &Json,
    directory: &Path,
) -> Result<Target, String> {
    let source = form["source"].as_str().unwrap_or("main");
    let launching = launching(context, source).ok_or("no Nextflow script for the output path")?;
    let adapter = adapter(&launching)?;
    let base = context.base;
    let called = adapter.borrow_mut().evaluate(|interpreter| {
        let closure = rebuilt(form, base, &launching, interpreter)?;
        let value = rebuilt(item, base, &launching, interpreter)?;
        interpreter.call_output_path(&closure, value)
    });
    let (folder, statements) = called.map_err(|error| error.message)?;
    let Some(statements) = statements else {
        return Ok(match folder {
            Value::Null => Target::Nothing,
            folder => Target::Folder(directory.join(folder.to_string())),
        });
    };
    let mut places = HashMap::new();
    for (source, target) in statements {
        let paths = match source {
            Value::Path(path) => vec![path],
            Value::List(list) => (list.borrow().items.iter())
                .filter_map(|item| match item {
                    Value::Path(path) => Some(path.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        for path in paths {
            let location = nextflow_runtime::bridge::location(&path);
            let Some(name) = context.nextflow.0.options.task_file(&location) else {
                continue;
            };
            let place = match target.ends_with('/') {
                true => format!("{target}{name}"),
                false => target.clone(),
            };
            places.insert(name, directory.join(place));
        }
    }
    Ok(Target::Files(places))
}

/// Writes the index file of an output at its close, which a simulated run does not, as the
/// native driver does not.
fn index(turn: &Turn<'_>, context: &Context<'_>, output: &Json) -> Result<(), String> {
    let records = context.nextflow.0.indexes.borrow_mut().remove(&turn.path);
    let records = records.unwrap_or_default();
    let Some(path) = output["index"]["path"].as_str() else {
        return Ok(());
    };
    if context.simulated {
        return Ok(());
    }
    let directory = PathBuf::from(output["directory"].as_str().unwrap_or("results"));
    let file = directory.join(path);
    let base = context.base;
    let values: Vec<Value> = records
        .iter()
        .map(|record| crate::groovy::to_value(record, base))
        .collect();
    let header =
        (output["index"].get("header")).map(|header| crate::groovy::to_value(header, base));
    let separator = output["index"]["sep"].as_str();
    let text = nextflow_runtime::index_text(&file, &values, header.as_ref(), separator);
    let text = text.map_err(|error| error.to_string())?;
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let written = context.files.join(&turn.path).join(&name);
    let made = written.parent().map(std::fs::create_dir_all).transpose();
    made.and_then(|_| std::fs::write(&written, text))
        .map_err(|error| error.to_string())?;
    context.deliveries.ask(&[Delivery {
        location: written.to_string_lossy().into_owned(),
        target: file.to_string_lossy().into_owned(),
        mode: DeliveryMode::Copy,
        overwrite: true,
    }]);
    Ok(())
}

fn each_file(value: &Json, found: &mut dyn FnMut(&Json)) {
    match value {
        Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("File") => {
            found(value)
        }
        Json::Array(items) => items.iter().for_each(|item| each_file(item, found)),
        Json::Object(object) => object.values().for_each(|item| each_file(item, found)),
        _ => {}
    }
}

/// A value with its published files at their new places.
fn placed(value: &Json, places: &HashMap<String, String>) -> Json {
    match value {
        Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("File") => {
            let mut file = object.clone();
            let location = file
                .get("location")
                .and_then(Json::as_str)
                .unwrap_or_default();
            if let Some(place) = places.get(location) {
                file.insert("location".into(), Json::from(place.as_str()));
            }
            Json::Object(file)
        }
        Json::Array(items) => Json::Array(items.iter().map(|item| placed(item, places)).collect()),
        Json::Object(object) => Json::Object(
            (object.iter())
                .map(|(key, item)| (key.clone(), placed(item, places)))
                .collect(),
        ),
        other => other.clone(),
    }
}

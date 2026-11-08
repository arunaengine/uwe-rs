//! Nextflow native blocks and `nextflow_process` tasks: the Nextflow driver of uwe as a child
//! in a launch folder of its own, and as the adapter that builds and judges single tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use nextflow_runtime::{Adapter, Completed, Prepared};
use serde_json::{Map, Value as Json, json};
use wir::{Binding, Port, Type, TypeDef};
use workflow_graph::{Delivery, DriverError, Launch, Outcome, Task, TaskId};

use crate::adapter::{Adapters, CallId, Changes, Ending, Opening, TaskAdapter, TaskCall};
use crate::child::Block;

/// Makes the settings of the driver of a block from its launch and its launch folder.
type Making = dyn Fn(&Launch, &Path) -> Settings + Send + Sync;

/// How a run makes the settings of the Nextflow driver of each block, from the launch of the
/// block and its launch folder. The host of the settings must start in that folder.
#[derive(Clone)]
pub struct Options {
    pub(crate) settings: Arc<Making>,
    /// The files that tasks made, by location, with their names in the work folder of their
    /// task, which workflow outputs publish by.
    pub(crate) names: Arc<std::sync::Mutex<HashMap<String, String>>>,
}

impl Options {
    pub fn new(settings: impl Fn(&Launch, &Path) -> Settings + Send + Sync + 'static) -> Options {
        Options {
            settings: Arc::new(settings),
            names: Arc::default(),
        }
    }

    /// The name of a file that a task made in its work folder, by its location.
    pub(crate) fn task_file(&self, location: &str) -> Option<String> {
        let names = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        names.get(location).cloned()
    }
}

impl Default for Options {
    /// A host that prints to standard output and reads local files.
    fn default() -> Self {
        Options::new(|_, folder| Settings {
            host: Box::new(Console(folder.to_string_lossy().into_owned())),
            ..Settings::default()
        })
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Options { .. }")
    }
}

/// A host in a launch folder that prints to standard output.
struct Console(String);

impl Host for Console {
    fn print(&mut self, text: &str) {
        print!("{text}");
    }

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

/// A Nextflow driver for the launch of one block, launched in `folder`.
pub(crate) fn block(
    launch: &Launch,
    folder: &Path,
    options: &Options,
) -> Result<Box<dyn Block>, DriverError> {
    let settings = (options.settings)(launch, folder);
    let started = PathBuf::from(settings.host.launch_dir());
    if started != folder {
        let message = format!(
            "the Nextflow host starts in {}, not in the launch folder {} of the block",
            started.display(),
            folder.display()
        );
        return Err(DriverError::new(message));
    }
    Ok(Box::new(Nextflow::new(launch, settings)?))
}

impl Block for Nextflow {
    /// What the entry workflow emitted, which a simulated run makes up too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        self.emitted()
    }
}

/// Runs `nextflow_process` tasks through adapters whose drivers get their settings from
/// `options`, launched in the working folder, as a Nextflow run alone is.
pub fn register(adapters: &mut Adapters, options: Options) {
    adapters.register("nextflow_process", move |opening| open(opening, &options));
}

fn open(opening: &Opening, options: &Options) -> Result<Box<dyn TaskAdapter>, DriverError> {
    let launch = Launch {
        target: opening.source.clone(),
        params: Vec::new(),
        inputs: None,
        profiles: opening.builder.task().profiles.clone(),
        configs: opening.configs.clone(),
        targets: Vec::new(),
        ..opening.launch.clone()
    };
    let folder = std::env::current_dir()
        .map_err(|error| DriverError::new(format!("no working folder: {error}")))?;
    let settings = (options.settings)(&launch, &folder);
    let mut adapter = Adapter::new(&launch, settings, params(opening.inputs))?;
    adapter.start(Vec::new());
    Ok(Box::new(Tasks {
        adapter,
        calls: HashMap::new(),
        counts: HashMap::new(),
        next: 0,
        accepted: HashSet::new(),
        deliveries: Vec::new(),
        names: options.names.clone(),
        folder: (opening.source.parent()).map_or_else(PathBuf::new, Path::to_owned),
        limits: Limits::default(),
    }))
}

/// The process tasks of one source context.
struct Tasks {
    adapter: Adapter,
    /// The op instance of each task and its output ports by output index.
    calls: HashMap<TaskId, (CallId, Vec<(usize, String)>)>,
    /// How many tasks each process call asked for, which numbers them as Nextflow does.
    counts: HashMap<String, u32>,
    next: u64,
    accepted: HashSet<(TaskId, u32)>,
    deliveries: Vec<Delivery>,
    /// The names of the files of tasks in their work folders, shared with workflow outputs.
    names: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// The folder of the main script, which the scripts of processes are named from.
    folder: PathBuf,
    limits: Limits,
}

/// The `maxForks` and `fair` rules of the process calls: the tasks held back while a call runs
/// as many as it may, and the endings held back until those of the earlier tasks went.
#[derive(Default)]
struct Limits {
    /// The call name and the task index of each op instance.
    rounds: HashMap<CallId, (String, u32)>,
    natives: HashMap<String, nextflow_runtime::Call>,
    running: HashMap<String, usize>,
    held: HashMap<String, VecDeque<(CallId, Task)>>,
    /// The endings of `fair` calls that wait for earlier ones, by call.
    waiting: HashMap<String, Waiting>,
}

/// The index of the next ending of a `fair` call to give, and the endings that wait for it.
type Waiting = (u32, BTreeMap<u32, (CallId, Ending)>);

impl Tasks {
    /// The `maxForks` and `fair` of the call of an op instance.
    fn rules(&self, call: CallId) -> (String, u32, Option<usize>, bool) {
        let (name, index) = self.limits.rounds.get(&call).cloned().unwrap_or_default();
        let (forks, fair) = (self.limits.natives.get(&name))
            .map_or((None, false), |native| self.adapter.limits(native));
        (name, index, forks, fair)
    }

    /// The task of an op instance, or none while its call runs as many tasks as it may.
    fn launch(&mut self, call: CallId, task: Task) -> Vec<(CallId, Task)> {
        let (name, _, forks, _) = self.rules(call);
        let running = self.limits.running.entry(name.clone()).or_default();
        if forks.is_some_and(|forks| *running >= forks) {
            self.limits
                .held
                .entry(name)
                .or_default()
                .push_back((call, task));
            return Vec::new();
        }
        *running += 1;
        vec![(call, task)]
    }

    /// The changes when an op instance ends: the held tasks that may run now, and the endings
    /// that may go, in task order for a `fair` call.
    fn finish(&mut self, call: CallId, ending: Ending, ran: bool) -> Changes {
        let (name, index, forks, fair) = self.rules(call);
        let mut tasks = Vec::new();
        if ran {
            let running = self.limits.running.entry(name.clone()).or_default();
            *running = running.saturating_sub(1);
            while forks.is_none_or(|forks| self.limits.running[&name] < forks) {
                let held = self.limits.held.get_mut(&name);
                let Some((next, task)) = held.and_then(VecDeque::pop_front) else {
                    break;
                };
                *self.limits.running.entry(name.clone()).or_default() += 1;
                tasks.push((next, task));
            }
        }
        self.limits.rounds.remove(&call);
        let ended = match fair {
            false => vec![(call, ending)],
            true => {
                let (next, waiting) = self
                    .limits
                    .waiting
                    .entry(name)
                    .or_insert((1, BTreeMap::new()));
                waiting.insert(index, (call, ending));
                let mut ended = Vec::new();
                while let Some(given) = waiting.remove(next) {
                    ended.push(given);
                    *next += 1;
                }
                ended
            }
        };
        Changes {
            tasks,
            ended,
            ready: Vec::new(),
        }
    }

    /// What an attempt that the native driver judged gives: a retry, or the ending.
    fn judged(&mut self, id: TaskId, attempt: u32, completed: Completed) -> Changes {
        let Some((call, ports)) = self.calls.get(&id).cloned() else {
            return Changes::default();
        };
        let ending = match completed {
            Completed::Retry(task) => {
                return Changes {
                    tasks: vec![(call, *task)],
                    ended: Vec::new(),
                    ready: Vec::new(),
                };
            }
            Completed::Accepted {
                outputs,
                deliveries,
                names,
            } => {
                self.accepted.insert((id, attempt));
                self.deliveries.extend(deliveries);
                let mut known = self.names.lock().unwrap_or_else(PoisonError::into_inner);
                known.extend(names);
                drop(known);
                // A missing optional output closes empty, as Nextflow emits nothing for it.
                let mut values = Map::new();
                let mut empty = Vec::new();
                for (index, port) in ports {
                    match outputs.get(index).cloned().flatten() {
                        Some(value) => drop(values.insert(port, value)),
                        None => empty.push(port),
                    }
                }
                Ending::Succeeded { values, empty }
            }
            Completed::Ignored(message) => Ending::Ignored(message),
            Completed::Failed(message) => Ending::Failed(message),
        };
        self.calls.remove(&id);
        // A task from its `storeDir` ran nothing.
        self.finish(call, ending, attempt > 0)
    }
}

impl TaskAdapter for Tasks {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let task = call.builder.task();
        let singleton = match task.captures.get("singleton") {
            Some(Binding::Value(value)) => value.as_bool().unwrap_or(false),
            _ => false,
        };
        // The script of the process, from the folder of the main script, else by its place.
        let script = match task.captures.get("script") {
            Some(Binding::Value(Json::String(script))) if script.starts_with("plugin/") => {
                Some(PathBuf::from(script))
            }
            Some(Binding::Value(Json::String(script))) => Some(self.folder.join(script)),
            _ => None,
        };
        let script = script
            .map(|script| script.to_string_lossy().into_owned())
            .or_else(|| {
                let scope = task.scope.as_ref()?;
                self.adapter.script_at(&task.entry, scope.start, scope.end)
            });
        let native = nextflow_runtime::Call {
            script: script.unwrap_or_default(),
            process: task.entry.clone(),
            name: task.call.clone(),
            singleton,
        };
        let index = |selector: &str, side: &str| {
            let number = selector.strip_prefix(side)?.strip_prefix('/')?;
            number.parse::<usize>().ok()
        };
        let mut inputs = vec![Json::Null; task.inputs.len()];
        for map in &task.inputs {
            if let Some(slot) = index(&map.native, "input").and_then(|at| inputs.get_mut(at)) {
                *slot = call.inputs.get(&map.port).cloned().unwrap_or_default();
            }
        }
        let ports = (task.outputs.iter())
            .filter_map(|map| Some((index(&map.native, "output")?, map.port.clone())))
            .collect();
        let count = self.counts.entry(task.call.clone()).or_default();
        *count += 1;
        self.next += 1;
        let id = TaskId(self.next);
        self.calls.insert(id, (call.call, ports));
        let index = *count;
        self.limits
            .rounds
            .insert(call.call, (task.call.clone(), index));
        let prepared = self.adapter.prepare(&native, &inputs, index, id)?;
        self.limits.natives.insert(task.call.clone(), native);
        Ok(match prepared {
            Prepared::Run(task) => Changes {
                tasks: self.launch(call.call, *task),
                ended: Vec::new(),
                ready: Vec::new(),
            },
            Prepared::Skipped => {
                self.calls.remove(&id);
                self.finish(call.call, Ending::Skipped(serde_json::Map::new()), false)
            }
            Prepared::Done(completed) => self.judged(id, 0, completed),
        })
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Changes, DriverError> {
        let (id, attempt) = (outcome.id, outcome.attempt);
        let completed = self.adapter.complete(outcome)?;
        Ok(self.judged(id, attempt, completed))
    }

    fn started(&mut self, task: TaskId, attempt: u32) {
        self.adapter.started(task, attempt);
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.accepted.remove(&(task, attempt))
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        self.adapter.delivered(delivery)
    }

    fn end(&mut self, failure: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        self.adapter.end(failure)
    }

    /// Tasks keep the names of the native run, like `NFCORE_DEMO:DEMO:FASTQC (sample1)`.
    fn prefixed(&self) -> bool {
        false
    }
}

/// The op inputs as Nextflow params: a File or a Directory becomes its path, and a null
/// input stays out, so that the default of the pipeline holds.
pub(crate) fn params(bound: &Map<String, Json>) -> Json {
    let given = bound.iter().filter(|(_, value)| !value.is_null());
    Json::Object(
        given
            .map(|(name, value)| (name.clone(), param(value)))
            .collect(),
    )
}

fn param(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(param).collect()),
        Json::Object(object) => match entry_path(value) {
            Some(path) => Json::String(path),
            None => Json::Object(
                (object.iter())
                    .map(|(k, v)| (k.clone(), param(v)))
                    .collect(),
            ),
        },
        other => other.clone(),
    }
}

/// The path of a File or Directory value: a local path, else its address.
fn entry_path(value: &Json) -> Option<String> {
    let class = value.get("class").and_then(Json::as_str);
    let location = value.get("location").and_then(Json::as_str);
    let (Some("File" | "Directory"), Some(location)) = (class, location) else {
        return None;
    };
    Some(match cwl_parser::iri::file_path(location) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => location.to_owned(),
    })
}

/// The op outputs of a finished block, by port: the emit of the entry workflow with the
/// port's name, else the files published below the folder that the op input of that name
/// names. docs/wir.md describes the mapping.
pub(crate) fn outputs(
    emitted: Json,
    targets: &[String],
    ports: &[Port],
    bound: &Map<String, Json>,
    folder: &Path,
    defs: &[TypeDef],
) -> Result<Map<String, Json>, String> {
    let emitted = match emitted {
        Json::Object(object) => object,
        _ => Map::new(),
    };
    let mut values = Map::new();
    for port in ports {
        let value = match emitted.get(&port.name) {
            Some(value) => shape(value.clone(), &port.r#type, defs),
            None => published(port, targets, bound.get(&port.name), folder, defs)?,
        };
        values.insert(port.name.clone(), value);
    }
    Ok(values)
}

/// A File of an emit becomes a Directory where the port type wants one: Nextflow paths do
/// not say which they are.
fn shape(value: Json, r#type: &Type, defs: &[TypeDef]) -> Json {
    let Some(r#type) = wir::resolve(r#type, defs) else {
        return value;
    };
    match (r#type, value) {
        (Type::Optional(inner), value) => shape(value, inner, defs),
        (Type::Directory, Json::Object(mut object))
            if object.get("class") == Some(&json!("File")) =>
        {
            object.insert("class".into(), json!("Directory"));
            Json::Object(object)
        }
        (Type::Array(array), Json::Array(items)) => Json::Array(
            (items.into_iter())
                .map(|item| shape(item, &array.items, defs))
                .collect(),
        ),
        (Type::Tuple(types), Json::Array(items)) => Json::Array(
            (items.into_iter().enumerate())
                .map(|(index, item)| match types.get(index) {
                    Some(r#type) => shape(item, r#type, defs),
                    None => item,
                })
                .collect(),
        ),
        (Type::Record(fields), Json::Object(object)) => Json::Object(
            (object.into_iter())
                .map(
                    |(name, item)| match fields.iter().find(|field| field.name == name) {
                        Some(field) => {
                            let item = shape(item, &field.r#type, defs);
                            (name, item)
                        }
                        None => (name, item),
                    },
                )
                .collect(),
        ),
        (_, value) => value,
    }
}

/// The files published below the folder of an input: the folder itself for a Directory, each
/// file for an array, the only one for a File.
fn published(
    port: &Port,
    targets: &[String],
    input: Option<&Json>,
    folder: &Path,
    defs: &[TypeDef],
) -> Result<Json, String> {
    let name = &port.name;
    let base = input.and_then(|value| match value {
        Json::String(text) => Some(text.clone()),
        other => entry_path(other),
    });
    let Some(base) = base else {
        return Err(format!(
            "output {name} is no emit of the entry workflow and no input names its folder"
        ));
    };
    let base = match base.contains("://") {
        true => PathBuf::from(base),
        false => folder.join(base),
    };
    let mut files: Vec<&String> = (targets.iter())
        .filter(|target| Path::new(target).starts_with(&base) && Path::new(target) != base)
        .collect();
    files.sort();
    files.dedup();
    let entry = |class: &str, location: &str| {
        let basename = location.rsplit('/').next().unwrap_or(location);
        json!({"class": class, "location": location, "basename": basename})
    };
    let shown = base.to_string_lossy();
    let mut r#type = wir::resolve(&port.r#type, defs);
    let optional = matches!(r#type, Some(Type::Optional(_)));
    if let Some(Type::Optional(inner)) = r#type {
        r#type = wir::resolve(inner, defs);
    }
    match r#type {
        _ if files.is_empty() && optional => Ok(Json::Null),
        Some(Type::Directory) if files.is_empty() => Err(format!(
            "output {name}: the block published nothing below {shown}"
        )),
        Some(Type::Directory) => Ok(entry("Directory", &shown)),
        Some(Type::Array(_) | Type::Dynamic) => Ok(Json::Array(
            files.iter().map(|file| entry("File", file)).collect(),
        )),
        Some(Type::File(_)) if files.len() == 1 => Ok(entry("File", files[0])),
        Some(Type::File(_)) => Err(format!(
            "output {name}: the block published {} files below {shown}, the port takes one",
            files.len()
        )),
        _ => Err(format!(
            "output {name} takes the files published below {shown}, so it needs a File, a \
             Directory or an array type"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &str, r#type: Json) -> Port {
        serde_json::from_value(json!({"name": name, "type": r#type})).unwrap()
    }

    #[test]
    fn writes_params_values() {
        let bound = json!({
            "input": {"class": "File", "location": "file:///data/a%20b.csv"},
            "samples": [{"class": "File", "location": "/data/s1.fq"}],
            "outdir": "results",
            "skip": null,
            "depth": 3
        });
        let params = params(bound.as_object().unwrap());
        let expected = json!({
            "input": "/data/a b.csv", "samples": ["/data/s1.fq"], "outdir": "results", "depth": 3
        });
        assert_eq!(params, expected);
    }

    #[test]
    fn maps_block_outputs() {
        let folder = Path::new("/out/block");
        let targets = [
            "/out/block/results/b.txt".to_owned(),
            "/out/block/results/a.txt".to_owned(),
            "/out/block/pipeline_info/trace.txt".to_owned(),
        ];
        let bound = json!({"outdir": "results"});
        let bound = bound.as_object().unwrap();
        let file = json!({"class": "File", "location": "/store/x", "basename": "x"});
        let emitted = json!({"folder": file, "count": 2});
        let ports = [
            port("folder", json!("directory")),
            port("count", json!("int")),
            port("outdir", json!({"array": {"items": {"file": {}}}})),
        ];
        let values = outputs(emitted.clone(), &targets, &ports, bound, folder, &[]).unwrap();
        assert_eq!(
            values["folder"]["class"], "Directory",
            "a path as a Directory"
        );
        assert_eq!(values["count"], 2);
        let names: Vec<&str> = (values["outdir"].as_array().unwrap().iter())
            .map(|file| file["basename"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["a.txt", "b.txt"],
            "published below the folder, sorted"
        );
        let ports = [port("outdir", json!("directory"))];
        let values = outputs(emitted.clone(), &targets, &ports, bound, folder, &[]).unwrap();
        assert_eq!(values["outdir"]["location"], "/out/block/results");
        let ports = [port("outdir", json!({"file": {}}))];
        let error = outputs(emitted.clone(), &targets, &ports, bound, folder, &[]).unwrap_err();
        assert!(error.contains("published 2 files"), "{error}");
        let ports = [port("missing", json!({"file": {}}))];
        let error = outputs(emitted, &targets, &ports, bound, folder, &[]).unwrap_err();
        assert!(error.contains("no emit of the entry workflow"), "{error}");
    }
}

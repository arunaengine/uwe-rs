//! The Galaxy driver: one tool as one task, or the tool steps of a workflow as tasks that start
//! once the steps they read from have ended, with the outputs delivered at the end.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value as Json, json};
use workflow_graph::{
    Delivery, DeliveryMode, Driver, DriverError, Inputs, Language, Launch, Outcome, Task, TaskId,
};

use crate::job::{self, Dataset};
use crate::schema;
use crate::task::{self, Loaded, Made};
use crate::workflow::{self, InputKind, Workflow};

/// The image of tools without a Docker container, as WDL and CWL tasks get one.
const DEFAULT_IMAGE: &str = "debian:stable-slim";

/// How Galaxy runs go.
#[derive(Clone, Debug)]
pub struct Options {
    /// The image of tools that name no Docker container.
    pub image: String,
    /// Folders with the tool files that workflow steps name, after the folder of the workflow.
    pub tools: Vec<PathBuf>,
    /// Whether a real run prints its outputs.
    pub print: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            image: DEFAULT_IMAGE.to_owned(),
            tools: Vec::new(),
            print: true,
        }
    }
}

/// A Galaxy driver for a tool XML file or a `.ga` workflow.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    driver_with(launch, Options::default())
}

pub fn driver_with(launch: &Launch, options: Options) -> Result<Box<dyn Driver>, DriverError> {
    Ok(Box::new(GalaxyDriver::new(launch, options)?))
}

/// What a launch runs: one tool, or a workflow with the tool of each step.
enum Plan {
    Tool(Box<Loaded>),
    Workflow {
        workflow: Workflow,
        tools: Vec<Loaded>,
    },
}

/// One tool run: the step it stands for, its parameters, and where its data inputs come from.
struct Node {
    tool: usize,
    label: String,
    parameters: Map<String, Json>,
    links: Vec<(String, Vec<Source>)>,
    state: State,
}

enum Source {
    Files(Vec<Dataset>),
    /// An output of the node at the index.
    Node(usize, String),
}

enum State {
    Waiting,
    Running,
    Done(BTreeMap<String, Vec<Dataset>>),
    /// It failed, or a node it reads from did.
    Failed,
}

/// Runs one Galaxy tool or workflow with one job.
pub struct GalaxyDriver {
    launch: Launch,
    options: Options,
    plan: Plan,
    /// The job values after the check of the inputs; else those of the launch are read.
    values: Option<Map<String, Json>>,
    base: PathBuf,
    nodes: Vec<Node>,
    running: HashMap<TaskId, (usize, Vec<Made>)>,
    last: u64,
    failures: Vec<String>,
    acceptance: HashSet<(TaskId, u32)>,
    deliveries: Vec<Delivery>,
    results: Option<Json>,
    exported: Option<Json>,
}

impl GalaxyDriver {
    /// Reads the tool or the workflow of a launch, and the tools of the steps of a workflow.
    pub fn new(launch: &Launch, options: Options) -> Result<Self, DriverError> {
        let shown = launch.target.display();
        let text = std::fs::read_to_string(&launch.target)
            .map_err(|error| DriverError::new(format!("cannot read {shown}: {error}")))?;
        let plan = match workflow::is_workflow(&text) {
            true => {
                let workflow = workflow::parse(&text)
                    .map_err(|error| DriverError::unsupported(format!("{shown}: {error}")))?;
                let folder = launch.target.parent().unwrap_or(Path::new(".")).to_owned();
                let mut folders = vec![folder];
                folders.extend(launch.configs.iter().filter(|path| path.is_dir()).cloned());
                folders.extend(options.tools.iter().cloned());
                let mut tools = Vec::new();
                for step in &workflow.steps {
                    let found =
                        workflow::find_tool(&step.tool_id, step.tool_version.as_deref(), &folders)
                            .map_err(|error| {
                                DriverError::new(format!("step {}: {error}", step.id))
                            })?;
                    tools.push(load(&found)?);
                }
                check(&workflow, &tools)
                    .map_err(|error| DriverError::new(format!("{shown}: {error}")))?;
                Plan::Workflow { workflow, tools }
            }
            false => Plan::Tool(Box::new(load(&launch.target)?)),
        };
        Ok(GalaxyDriver {
            launch: launch.clone(),
            options,
            plan,
            values: None,
            base: std::env::current_dir().unwrap_or_default(),
            nodes: Vec::new(),
            running: HashMap::new(),
            last: 0,
            failures: Vec::new(),
            acceptance: HashSet::new(),
            deliveries: Vec::new(),
            results: None,
            exported: None,
        })
    }

    /// The outputs of the finished run as wir values: Files by output name, at the paths
    /// they were delivered to, or made-up ones in a simulated run.
    pub fn exported(&self) -> Option<Json> {
        self.exported.clone()
    }

    fn tools(&self) -> &[Loaded] {
        match &self.plan {
            Plan::Tool(loaded) => std::slice::from_ref(loaded.as_ref()),
            Plan::Workflow { tools, .. } => tools,
        }
    }

    /// The job values: those that the check of the inputs gave, else the launch's.
    fn values(&mut self) -> Result<Map<String, Json>, DriverError> {
        if let Some(values) = &self.values {
            return Ok(values.clone());
        }
        let (values, base) = job::given(&self.launch)?;
        self.base = base;
        Ok(values)
    }

    /// The nodes of the run from the job values.
    fn plan_nodes(&self, values: &Map<String, Json>) -> Result<Vec<Node>, DriverError> {
        let base = self.base.clone();
        match &self.plan {
            Plan::Tool(loaded) => {
                let typed = job::typed(&loaded.tool.inputs, values);
                let (parameters, datasets) = job::split(&loaded.tool, &typed, &base);
                let links = (datasets.into_iter())
                    .map(|(path, files)| (path, vec![Source::Files(files)]))
                    .collect();
                Ok(vec![Node {
                    tool: 0,
                    label: loaded.tool.id.clone(),
                    parameters,
                    links,
                    state: State::Waiting,
                }])
            }
            Plan::Workflow { workflow, tools } => {
                let index: HashMap<u64, usize> = (workflow.steps.iter().enumerate())
                    .map(|(at, step)| (step.id, at))
                    .collect();
                let mut nodes = Vec::new();
                for (at, step) in workflow.steps.iter().enumerate() {
                    let tool = &tools[at].tool;
                    let mut state = step.state.clone();
                    let mut links = Vec::new();
                    for connection in &step.connections {
                        let mut sources = Vec::new();
                        for (id, output) in &connection.sources {
                            if let Some(&node) = index.get(id) {
                                sources.push(Source::Node(node, output.clone()));
                                continue;
                            }
                            let Some(input) = workflow.inputs.iter().find(|input| input.id == *id)
                            else {
                                let message = format!("step {}: no step {id}", step.id);
                                return Err(DriverError::new(message));
                            };
                            let given = values.get(&input.label).cloned();
                            let given = given.or_else(|| input.default.clone());
                            match (&input.kind, given) {
                                (InputKind::Parameter(_), Some(value)) => {
                                    set(&mut state, &connection.input, value);
                                }
                                (InputKind::Parameter(_), None) => {}
                                (_, value) => {
                                    let files = paths(value.unwrap_or_default(), &base);
                                    sources.push(Source::Files(files));
                                }
                            }
                        }
                        if !sources.is_empty() {
                            links.push((connection.input.clone(), sources));
                        }
                    }
                    nodes.push(Node {
                        tool: at,
                        label: step
                            .label
                            .clone()
                            .unwrap_or_else(|| workflow::short_id(&step.tool_id).to_owned()),
                        parameters: job::typed(&tool.inputs, &state),
                        links,
                        state: State::Waiting,
                    });
                }
                Ok(nodes)
            }
        }
    }

    /// The tasks of the nodes that can start now; nodes after a failed one fail too.
    fn ready(&mut self) -> Result<Vec<Task>, DriverError> {
        let mut tasks = Vec::new();
        if self.draining() {
            return Ok(tasks);
        }
        loop {
            let mut changed = false;
            for at in 0..self.nodes.len() {
                if !matches!(self.nodes[at].state, State::Waiting) {
                    continue;
                }
                let mut datasets: BTreeMap<String, Vec<Dataset>> = BTreeMap::new();
                let mut blocked = false;
                let mut broken = false;
                for (path, sources) in &self.nodes[at].links {
                    for source in sources {
                        match source {
                            Source::Files(files) => {
                                datasets
                                    .entry(path.clone())
                                    .or_default()
                                    .extend(files.clone());
                            }
                            Source::Node(from, output) => match &self.nodes[*from].state {
                                State::Done(outputs) => {
                                    let files = outputs.get(output).cloned().unwrap_or_default();
                                    datasets.entry(path.clone()).or_default().extend(files);
                                }
                                State::Failed => broken = true,
                                _ => blocked = true,
                            },
                        }
                    }
                }
                if broken {
                    self.nodes[at].state = State::Failed;
                    changed = true;
                    continue;
                }
                if blocked {
                    continue;
                }
                self.last += 1;
                let id = TaskId(self.last);
                let node = &self.nodes[at];
                let loaded = &self.tools()[node.tool];
                let image = (galaxy_parser::definition::image(&loaded.tool))
                    .map_or_else(|| self.options.image.clone(), |image| image.name);
                let label = node.label.clone();
                let planned = task::build(
                    id,
                    (&label, &label),
                    loaded,
                    (&node.parameters, &datasets),
                    &image,
                )?;
                self.nodes[at].state = State::Running;
                self.running.insert(id, (at, planned.made));
                tasks.push(planned.task);
                changed = true;
            }
            if !changed {
                break;
            }
        }
        Ok(tasks)
    }

    /// The outputs of the run by name, as files: the outputs of the tool, or the workflow
    /// outputs by label.
    fn produced(&self) -> Vec<(String, Vec<Dataset>)> {
        let done = |at: usize| match &self.nodes.get(at).map(|node| &node.state) {
            Some(State::Done(outputs)) => Some(outputs),
            _ => None,
        };
        match &self.plan {
            Plan::Tool(_) => (done(0).into_iter())
                .flat_map(|outputs| outputs.clone())
                .collect(),
            Plan::Workflow { workflow, .. } => (workflow.outputs.iter())
                .filter_map(|output| {
                    let at = workflow
                        .steps
                        .iter()
                        .position(|step| step.id == output.step)?;
                    let files = done(at)?.get(&output.output)?.clone();
                    Some((output.label.clone(), files))
                })
                .collect(),
        }
    }
}

impl Driver for GalaxyDriver {
    fn language(&self) -> Language {
        Language::Galaxy
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        let values = self.values()?;
        self.nodes = self.plan_nodes(&values)?;
        self.ready()
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let Some((at, made)) = self.running.remove(&outcome.id) else {
            return Err(DriverError::new(format!(
                "task {} is not a task of this run",
                outcome.id
            )));
        };
        let tool = &self.tools()[self.nodes[at].tool].tool;
        let collected = match task::refused(tool, &outcome) {
            Some(why) => Err(why),
            None => task::collected(tool, &made, &outcome),
        };
        match collected {
            Ok(outputs) => {
                self.acceptance.insert((outcome.id, outcome.attempt));
                self.nodes[at].state = State::Done(outputs);
            }
            Err(why) => {
                self.failures
                    .push(format!("{}: {why}", self.nodes[at].label));
                self.nodes[at].state = State::Failed;
            }
        }
        self.ready()
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.acceptance.remove(&(task, attempt))
    }

    fn draining(&self) -> bool {
        !self.failures.is_empty() && !self.launch.keep_going
    }

    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        let values = self.values()?;
        let schema = match &self.plan {
            Plan::Tool(loaded) => schema::tool_inputs(&loaded.tool),
            Plan::Workflow { workflow, .. } => schema::workflow_inputs(workflow),
        };
        let values = match &self.plan {
            Plan::Tool(loaded) => job::typed(&loaded.tool.inputs, &values),
            Plan::Workflow { .. } => values,
        };
        Ok(Some(Inputs {
            schema,
            values: Json::Object(values),
            base: self.base.clone(),
            read: None,
            assumes_values: false,
            advisory: false,
            ignored: Default::default(),
        }))
    }

    fn use_inputs(&mut self, values: Json) -> Result<(), DriverError> {
        let Json::Object(values) = values else {
            return Err(DriverError::new("the inputs of a Galaxy job are an object"));
        };
        if self.values.is_none() {
            self.base = job::given(&self.launch)?.1;
        }
        self.values = Some(job::nested(values));
        Ok(())
    }

    fn outputs(&mut self) -> Result<Option<Json>, DriverError> {
        Ok(Some(match &self.plan {
            Plan::Tool(loaded) => schema::tool_outputs(&loaded.tool),
            Plan::Workflow { workflow, .. } => schema::workflow_outputs(workflow),
        }))
    }

    fn results(&self) -> Option<Json> {
        self.results.clone()
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    /// A real run delivers each output to `<output folder>/<name>/`, numbered for lists, and
    /// prints the paths by output name.
    fn finish(&mut self) -> Result<(), DriverError> {
        if let Some(first) = self.failures.first() {
            let message = match self.failures.len() {
                1 => first.clone(),
                count => format!("{first}; and {} more steps failed", count - 1),
            };
            return Err(DriverError::new(message));
        }
        if let Some(node) = (self.nodes.iter()).find(|node| !matches!(node.state, State::Done(_))) {
            return Err(DriverError::new(format!(
                "the run ended before step {} ran",
                node.label
            )));
        }
        let folder = output_folder(self.launch.output_dir.as_deref());
        let mut results = Map::new();
        let mut exported = Map::new();
        for (name, files) in self.produced() {
            let mut paths = Vec::new();
            for (index, file) in files.iter().enumerate() {
                let basename = file.location.rsplit('/').next().unwrap_or("output");
                let index = index.to_string();
                let names = match files.len() {
                    1 => vec![name.as_str(), basename],
                    _ => vec![name.as_str(), &index, basename],
                };
                let target = target(&folder, &names)
                    .map_err(|error| DriverError::new(format!("output {name}: {error}")))?;
                let location = match self.launch.simulated {
                    true => file.location.clone(),
                    false => {
                        self.deliveries.push(Delivery {
                            location: file.location.clone(),
                            target: target.clone(),
                            mode: DeliveryMode::Copy,
                            overwrite: true,
                        });
                        target
                    }
                };
                paths.push(location);
            }
            let wir = |path: &String| {
                let basename = path.rsplit('/').next().unwrap_or_default();
                json!({"class": "File", "location": path, "basename": basename})
            };
            let (value, file) = match paths.as_slice() {
                [only] => (json!(only), wir(only)),
                _ => (json!(paths), Json::Array(paths.iter().map(wir).collect())),
            };
            results.insert(name.clone(), value);
            exported.insert(name, file);
        }
        let results = Json::Object(results);
        if self.options.print && !self.launch.simulated {
            println!(
                "{}",
                serde_json::to_string_pretty(&results).unwrap_or_default()
            );
        }
        self.results = Some(results);
        self.exported = Some(Json::Object(exported));
        Ok(())
    }
}

/// Refuses a workflow whose connections or outputs name a step or an output that does not
/// exist, or whose steps read from each other in a cycle, before any task starts.
fn check(workflow: &Workflow, tools: &[Loaded]) -> Result<(), String> {
    let outputs = |id: u64| -> Option<Vec<&str>> {
        let at = workflow.steps.iter().position(|step| step.id == id)?;
        Some(
            (tools[at].tool.outputs.iter())
                .map(|output| output.name.as_str())
                .collect(),
        )
    };
    let inputs: HashSet<u64> = workflow.inputs.iter().map(|input| input.id).collect();
    for step in &workflow.steps {
        let sources = step.connections.iter().flat_map(|link| &link.sources);
        for (id, output) in sources {
            match outputs(*id) {
                Some(names) if !names.contains(&output.as_str()) => {
                    return Err(format!(
                        "step {}: step {id} has no output {output}",
                        step.id
                    ));
                }
                None if !inputs.contains(id) => {
                    return Err(format!("step {}: no step {id}", step.id));
                }
                _ => {}
            }
        }
    }
    for output in &workflow.outputs {
        if !outputs(output.step).is_some_and(|names| names.contains(&output.output.as_str())) {
            return Err(format!(
                "the workflow output {} is not an output {} of a tool step {}",
                output.label, output.output, output.step
            ));
        }
    }
    // Steps whose sources all ran can run next; the steps left read from each other.
    let mut ran = inputs;
    let mut left: Vec<&workflow::Step> = workflow.steps.iter().collect();
    while !left.is_empty() {
        let before = left.len();
        left.retain(|step| {
            let mut sources = step.connections.iter().flat_map(|link| &link.sources);
            let ready = sources.all(|(id, _)| ran.contains(id));
            if ready {
                ran.insert(step.id);
            }
            !ready
        });
        if left.len() == before {
            let ids: Vec<u64> = left.iter().map(|step| step.id).collect();
            return Err(format!("the steps {ids:?} read from each other in a cycle"));
        }
    }
    Ok(())
}

/// Where an output file goes in `folder`, by names that must each be one plain path segment;
/// in a local folder, a link on the way must not lead outside it.
fn target(folder: &str, names: &[&str]) -> std::io::Result<String> {
    let place = workflow_graph::files::place(Path::new(folder), names)?;
    Ok(place.to_string_lossy().into_owned())
}

/// Reads a tool file with the macro files beside it.
fn load(path: &Path) -> Result<Loaded, DriverError> {
    let tool =
        galaxy_parser::tool::load(path).map_err(|error| DriverError::new(error.to_string()))?;
    if tool.command.is_none() {
        return Err(DriverError::unsupported(format!(
            "tool {}: a tool without a command is not supported yet",
            tool.id
        )));
    }
    Ok(Loaded {
        tool,
        path: path.to_owned(),
    })
}

/// The files of a workflow input value: a path or a list of paths.
fn paths(value: Json, base: &Path) -> Vec<Dataset> {
    let items = match job::plain(value) {
        Json::Array(items) => items,
        Json::Null => Vec::new(),
        other => vec![other],
    };
    (items.into_iter())
        .filter_map(|item| item.as_str().map(str::to_owned))
        .map(|location| {
            let location = job::local(&location).to_owned();
            let location = match location.contains("://") || Path::new(&location).is_absolute() {
                true => location,
                false => base.join(&location).to_string_lossy().into_owned(),
            };
            let name = location.rsplit('/').next().unwrap_or_default().to_owned();
            let ext = name.split_once('.').map_or("data", |(_, ending)| ending);
            Dataset {
                ext: ext.to_ascii_lowercase(),
                name,
                location,
            }
        })
        .collect()
}

/// Sets a value at a path like `adv|threshold` of nested values.
fn set(values: &mut Map<String, Json>, path: &str, value: Json) {
    let parts: Vec<&str> = path.split('|').collect();
    let (last, path) = parts.split_last().unwrap_or((&"", &[]));
    let mut object = values;
    for part in path {
        let entry = (object.entry((*part).to_owned())).or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        let Json::Object(inner) = entry else {
            return;
        };
        object = inner;
    }
    object.insert((*last).to_owned(), value);
}

/// Where output files go: the `-output-dir` folder or address, else the current folder.
fn output_folder(folder: Option<&Path>) -> String {
    let folder: PathBuf = match folder {
        Some(folder) if folder.to_string_lossy().contains("://") => {
            return folder.to_string_lossy().trim_end_matches('/').to_owned();
        }
        Some(folder) => std::path::absolute(folder).unwrap_or(folder.to_owned()),
        None => std::env::current_dir().unwrap_or_default(),
    };
    folder.to_string_lossy().trim_end_matches('/').to_owned()
}

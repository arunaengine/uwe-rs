//! The op instances of a run: the ops of the entry graph and of the graphs they call, with
//! where each input gets its value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use serde_json::{Map, Value as Json};
use wir::{Call, Document, Endpoint, Graph, Kind, Op, Scatter, Status};
use workflow_graph::DriverError;

use crate::adapter::Adapters;

/// One op instance. Inside a call its path starts with the path of the call, like
/// `align/sort` for the op `sort` of the graph that the op `align` calls.
pub struct Node {
    pub path: String,
    /// The path without the round numbers of maps, which tasks keep as their step.
    pub step: String,
    pub op: Op,
    /// Where each input port gets its value.
    pub inputs: Vec<(String, Source)>,
    /// The nodes this one runs after, with the statuses that satisfy each.
    pub after: Vec<(usize, Vec<Status>)>,
    /// The call node whose graph holds this one; it starts only once that call started.
    pub parent: Option<usize>,
    /// For a call node: where each of its outputs comes from inside the called graph.
    pub exports: Vec<(String, Source)>,
    /// For a map node that started: the number of items of each level of its outputs.
    pub shape: Vec<usize>,
    /// For a call, a branch or a round: the defaults of the inputs of the graph it runs.
    pub defaults: Map<String, Json>,
    /// The stream inputs whose edge reads `live`.
    pub live: Vec<String>,
    /// The tree of status tests that must hold too.
    pub depends: Option<ControlPlan>,
    /// The nodes whose failures this one handles once it succeeds, with those statuses.
    pub handles: Vec<(usize, Vec<Status>)>,
    /// For a status group: the nodes it waits for.
    pub members: Vec<usize>,
}

/// A tree of status tests with the ops resolved to nodes.
#[derive(Clone, Debug, PartialEq)]
pub enum ControlPlan {
    All(Vec<ControlPlan>),
    Any(Vec<ControlPlan>),
    Not(Box<ControlPlan>),
    Test(usize, Vec<wir::ResultKind>),
}

/// The tree of `control` with the op paths of a graph resolved by `index`.
fn resolve(control: &wir::Control, index: &HashMap<&str, usize>) -> ControlPlan {
    match control {
        wir::Control::All(items) => {
            ControlPlan::All(items.iter().map(|item| resolve(item, index)).collect())
        }
        wir::Control::Any(items) => {
            ControlPlan::Any(items.iter().map(|item| resolve(item, index)).collect())
        }
        wir::Control::Not(inner) => ControlPlan::Not(Box::new(resolve(inner, index))),
        wir::Control::Result(test) => ControlPlan::Test(index[test.op.as_str()], test.on.clone()),
    }
}

/// Where a value comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An input of the entry graph.
    Input(String),
    /// An output of a node.
    Output(usize, String),
    /// An input of a call node, read inside the called graph.
    Call(usize, String),
    /// No producer: an optional input, bound to null.
    Null,
    /// A value fixed when the node was made, like the item of a round of a map.
    Value(Json),
}

/// The nodes of a run of the entry graph, and where each graph output comes from.
pub struct Plan {
    pub nodes: Vec<Node>,
    pub outputs: Vec<(String, Source)>,
}

/// Plans a verified document. Reserved op kinds parse and verify, but no runner of this
/// version runs them; dialect tasks run when `adapters` has their builder.
pub fn plan(document: &Document, adapters: &Adapters) -> Result<Plan, DriverError> {
    refuse(document, adapters)?;
    let mut nodes = Vec::new();
    let entry = graph(document, &document.entry)?;
    let outputs = instantiate(document, entry, ("", ""), None, &mut nodes)?;
    Ok(Plan { nodes, outputs })
}

/// Refuses the settings that this runner cannot honour, in every graph that a run of the
/// entry graph can reach, so that none of them is dropped silently.
fn refuse(document: &Document, adapters: &Adapters) -> Result<(), DriverError> {
    let mut reached = vec![document.entry.as_str()];
    let mut next = 0;
    while let Some(name) = reached.get(next).copied() {
        next += 1;
        let graph = graph(document, name)?;
        for op in &graph.ops {
            if let Some(why) = refusal(op, adapters) {
                let message = format!("op {name}/{}: {why}", op.path);
                return Err(DriverError::unsupported(message));
            }
            let body = match &op.kind {
                Kind::Call(call) => Some(call.graph.as_str()),
                Kind::Branch(branch) => Some(branch.body.as_str()),
                Kind::Map(map) => Some(map.body.as_str()),
                Kind::Process(process) => Some(process.body.as_str()),
                Kind::Loop(looped) => Some(looped.body.as_str()),
                _ => None,
            };
            if let Some(body) = body.filter(|body| !reached.contains(body)) {
                reached.push(body);
            }
        }
    }
    Ok(())
}

/// Why this runner cannot run an op with its settings, if it cannot.
fn refusal(op: &Op, adapters: &Adapters) -> Option<&'static str> {
    let runs = matches!(op.kind, Kind::Task(_) | Kind::Native(_));
    if op.attributes.place.is_some() && !runs {
        return Some("only task and native ops run at a place");
    }
    let effectful = matches!(
        op.kind,
        Kind::View(_) | Kind::Observe(_) | Kind::CollectFile(_) | Kind::Split(_)
    );
    if !op.effects.is_empty() && !runs && !effectful {
        return Some("only task, native and effect ops have effects");
    }
    if let Kind::Map(map) = &op.kind
        && map.parallelism == Some(0)
    {
        return Some("a map runs at least one round at a time");
    }
    let sized = op.attributes.resources.is_some() || op.attributes.time.is_some();
    if matches!(op.kind, Kind::Native(_)) && (sized || op.attributes.retry.is_some()) {
        return Some("a native block sets the resources, time and retry of its tasks itself");
    }
    unrun(op, adapters)
}

/// Whether this runner runs a stream or control op kind.
fn supported(kind: &Kind) -> bool {
    matches!(
        kind,
        Kind::Emit(_)
            | Kind::Transform(_)
            | Kind::Filter(_)
            | Kind::FlatMap(_)
            | Kind::Flatten(_)
            | Kind::Mix(_)
            | Kind::Concat(_)
            | Kind::Merge(_)
            | Kind::Zip(_)
            | Kind::Route(_)
            | Kind::MultiMap(_)
            | Kind::Collect(_)
            | Kind::ToList(_)
            | Kind::Join(_)
            | Kind::Combine(_)
            | Kind::Group(_)
            | Kind::GroupTuple(_)
            | Kind::Cross(_)
            | Kind::Reduce(_)
            | Kind::Buffer(_)
            | Kind::Window(_)
            | Kind::Take(_)
            | Kind::First(_)
            | Kind::Last(_)
            | Kind::Unique(_)
            | Kind::Distinct(_)
            | Kind::Until(_)
            | Kind::IfEmpty(_)
            | Kind::Split(_)
            | Kind::View(_)
            | Kind::Observe(_)
            | Kind::CollectFile(_)
            | Kind::StatusGroup(_)
            | Kind::Process(_)
            | Kind::Loop(_)
    )
}

/// Why this runner cannot run a field of the format that it does not run yet, if it cannot.
fn unrun(op: &Op, adapters: &Adapters) -> Option<&'static str> {
    let streams = (op.inputs.iter().chain(&op.outputs)).any(|port| !port.flow.is_value());
    if streams && !matches!(op.kind, Kind::Call(_)) && !supported(&op.kind) {
        return Some("uwe does not run stream ports yet");
    }
    if !op.values.is_wir() {
        return Some("uwe compares values only by the wir rules yet");
    }
    match &op.kind {
        Kind::Expr(expr) if expr.language == wir::ExprLanguage::Python && expr.scope.is_some() => {
            Some("uwe does not load Snakefile scopes for Python code yet")
        }
        Kind::Expr(expr)
            if !matches!(
                expr.language,
                wir::ExprLanguage::Groovy | wir::ExprLanguage::Python
            ) && (expr.scope.is_some() || !expr.captures.is_empty()) =>
        {
            Some("uwe does not run expressions with a scope or captures yet")
        }
        Kind::Task(task)
            if task
                .builder
                .as_ref()
                .is_some_and(|built| !adapters.knows(built)) =>
        {
            Some("uwe does not run dialect tasks yet")
        }
        _ => None,
    }
}

/// Plans one graph of a document as if it were the entry, like the graph of a callback.
pub fn graph_plan(document: &Document, name: &str) -> Result<Plan, DriverError> {
    let mut nodes = Vec::new();
    let outputs = instantiate(document, graph(document, name)?, ("", ""), None, &mut nodes)?;
    Ok(Plan { nodes, outputs })
}

fn graph<'d>(document: &'d Document, name: &str) -> Result<&'d Graph, DriverError> {
    (document.graphs.iter().find(|graph| graph.name == name))
        .ok_or_else(|| DriverError::new(format!("the document has no graph {name}")))
}

/// Adds a round node for each item of a map node, as a call of its body graph whose inputs
/// are the values of the round, and the ops of the body below it.
pub fn rounds(
    document: &Document,
    nodes: &mut Vec<Node>,
    at: usize,
    body: &str,
    items: Vec<Map<String, Json>>,
) -> Result<(), DriverError> {
    for (index, item) in items.into_iter().enumerate() {
        round(document, nodes, (at, &index.to_string()), body, item)?;
    }
    Ok(())
}

/// Adds one round of the node `at`, at `<path of at>/<name>`, as a call of `body` with the
/// values of `item`, and the ops of the body below it; the node of the round.
pub fn round(
    document: &Document,
    nodes: &mut Vec<Node>,
    (at, name): (usize, &str),
    body: &str,
    item: Map<String, Json>,
) -> Result<usize, DriverError> {
    let graph = self::graph(document, body)?;
    let (path, step) = (format!("{}/{name}", nodes[at].path), nodes[at].step.clone());
    let mut op = nodes[at].op.clone();
    op.kind = Kind::Call(Call {
        graph: body.to_owned(),
    });
    op.after.clear();
    op.depends = None;
    op.handles.clear();
    // A round has the value ports of its body; the graph's defaults apply to its inputs.
    (op.inputs, op.outputs) = (graph.inputs.clone(), graph.outputs.clone());
    op.inputs.iter_mut().for_each(|port| port.default = None);
    let round = nodes.len();
    nodes.push(Node {
        path: path.clone(),
        step: step.clone(),
        op,
        inputs: (item.into_iter())
            .map(|(port, value)| (port, Source::Value(value)))
            .collect(),
        after: Vec::new(),
        parent: Some(at),
        exports: Vec::new(),
        shape: Vec::new(),
        defaults: defaults(graph),
        live: Vec::new(),
        depends: None,
        handles: Vec::new(),
        members: Vec::new(),
    });
    let (inner, steps) = (format!("{path}/"), format!("{step}/"));
    nodes[round].exports = instantiate(document, graph, (&inner, &steps), Some(round), nodes)?;
    Ok(round)
}

/// The rounds of a map that started.
pub struct Rounds {
    /// The input values of each round.
    pub items: Vec<Map<String, Json>>,
    /// The number of items of each level of the outputs.
    pub shape: Vec<usize>,
}

/// The rounds of a map from its input values, or why it cannot make them.
pub fn items(map: &wir::Map, bound: &Map<String, Json>) -> Result<Rounds, String> {
    let mut lists = Vec::new();
    for name in &map.over {
        match bound.get(name) {
            Some(Json::Array(items)) => lists.push(items.as_slice()),
            _ => return Err(format!("the input {name} of a map is not an array")),
        }
    }
    let lengths: Vec<usize> = lists.iter().map(|items| items.len()).collect();
    let (count, shape) = match map.mode {
        Scatter::Dot if lengths.windows(2).any(|pair| pair[0] != pair[1]) => {
            let shown: Vec<String> = lengths.iter().map(usize::to_string).collect();
            let message = format!("the inputs of a dot map have lengths {}", shown.join(", "));
            return Err(message);
        }
        Scatter::Dot => {
            let count = lengths.first().copied().unwrap_or(0);
            (count, vec![count])
        }
        Scatter::FlatCross => {
            let count = lengths.iter().product();
            (count, vec![count])
        }
        Scatter::NestedCross => (lengths.iter().product(), lengths.clone()),
    };
    let mut rounds = Vec::new();
    for round in 0..count {
        let mut item = bound.clone();
        // In a cross product the first input varies slowest.
        let mut rest = round;
        for (at, name) in map.over.iter().enumerate().rev() {
            let index = match map.mode {
                Scatter::Dot => round,
                _ => {
                    let index = rest % lengths[at];
                    rest /= lengths[at];
                    index
                }
            };
            item.insert(name.clone(), lists[at][index].clone());
        }
        rounds.push(item);
    }
    Ok(Rounds {
        items: rounds,
        shape,
    })
}

/// Adds the ops of `graph` to `nodes`, inside the call node `call` when there is one, and
/// gives where the outputs of the graph come from. `prefixes` start the paths and the steps.
fn instantiate(
    document: &Document,
    graph: &Graph,
    prefixes: (&str, &str),
    call: Option<usize>,
    nodes: &mut Vec<Node>,
) -> Result<Vec<(String, Source)>, DriverError> {
    let (prefix, steps) = prefixes;
    let mut index = HashMap::new();
    for op in &graph.ops {
        if op.kind.is_reserved() && !supported(&op.kind) {
            let message = format!("op {prefix}{}: uwe does not run this op kind yet", op.path);
            return Err(DriverError::unsupported(message));
        }
        index.insert(op.path.as_str(), nodes.len());
        nodes.push(Node {
            path: format!("{prefix}{}", op.path),
            step: format!("{steps}{}", op.path),
            op: op.clone(),
            inputs: Vec::new(),
            after: Vec::new(),
            parent: call,
            exports: Vec::new(),
            shape: Vec::new(),
            defaults: Map::new(),
            live: Vec::new(),
            depends: None,
            handles: Vec::new(),
            members: Vec::new(),
        });
    }
    let source = |from: &Endpoint| match &from.op {
        Some(op) => Source::Output(index[op.as_str()], from.port.clone()),
        None => match call {
            Some(call) => Source::Call(call, from.port.clone()),
            None => Source::Input(from.port.clone()),
        },
    };
    let mut outputs = Vec::new();
    for op in &graph.ops {
        let at = index[op.path.as_str()];
        let inputs = (op.inputs.iter())
            .map(|port| {
                let edge = (graph.edges.iter()).find(|edge| {
                    edge.to.op.as_deref() == Some(op.path.as_str()) && edge.to.port == port.name
                });
                let from = edge.map_or(Source::Null, |edge| source(&edge.from));
                (port.name.clone(), from)
            })
            .collect();
        let after = (op.after.iter())
            .map(|dependency| (index[dependency.op.as_str()], dependency.on.clone()))
            .collect();
        nodes[at].live = (graph.edges.iter())
            .filter(|edge| edge.to.op.as_deref() == Some(op.path.as_str()))
            .filter(|edge| edge.read == wir::Subscription::Live)
            .map(|edge| edge.to.port.clone())
            .collect();
        nodes[at].inputs = inputs;
        nodes[at].after = after;
        nodes[at].depends = op.depends.as_ref().map(|control| resolve(control, &index));
        nodes[at].handles = (op.handles.iter())
            .map(|handled| (index[handled.op.as_str()], handled.on.clone()))
            .collect();
        if let Kind::StatusGroup(group) = &op.kind {
            nodes[at].members = group
                .members
                .iter()
                .map(|member| index[member.as_str()])
                .collect();
        }
        let body = match &op.kind {
            Kind::Call(called) => Some(&called.graph),
            Kind::Branch(branch) => Some(&branch.body),
            _ => None,
        };
        if let Some(body) = body {
            let called = self::graph(document, body)?;
            nodes[at].defaults = defaults(called);
            let inner = (
                format!("{prefix}{}/", op.path),
                format!("{steps}{}/", op.path),
            );
            let inner = (inner.0.as_str(), inner.1.as_str());
            nodes[at].exports = instantiate(document, called, inner, Some(at), nodes)?;
        }
    }
    for port in &graph.outputs {
        let edge =
            (graph.edges.iter()).find(|edge| edge.to.op.is_none() && edge.to.port == port.name);
        outputs.push((
            port.name.clone(),
            edge.map_or(Source::Null, |edge| source(&edge.from)),
        ));
    }
    Ok(outputs)
}

/// The defaults of the inputs of a graph, by port name.
fn defaults(graph: &Graph) -> Map<String, Json> {
    (graph.inputs.iter())
        .filter_map(|port| Some((port.name.clone(), port.default.clone()?)))
        .collect()
}

//! The state of a run of a plan: which op instances wait, run or ended, the values they
//! bound, and the failures that fail the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value as Json};
use wir::{Kind, Status, Type, TypeDef, resolve};

use crate::plan::{Node, Plan, Source};
use crate::streams::Streams;

/// Where a node is in its run.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Waiting,
    /// Started, with its input values.
    Running(Map<String, Json>),
    /// Ended, with its output values when it succeeded.
    Ended(Status, Map<String, Json>),
}

/// What a source supplies.
#[derive(Clone, Debug, PartialEq)]
enum Supply {
    /// Not yet.
    Pending,
    Bound(Json),
    /// Closed empty: no value, which is not null.
    Empty,
    /// Never, since its producer did not succeed.
    Never,
}

/// Whether a waiting node can start.
enum Readiness {
    Wait,
    Omit,
    Ready(Map<String, Json>),
    /// A `dynamic` value does not fit the type of the input it reaches.
    Refuse(String),
}

/// The run of a plan.
pub struct Progress {
    pub plan: Plan,
    pub states: Vec<State>,
    /// The values of the inputs of the entry graph.
    pub inputs: Map<String, Json>,
    /// Why ops failed, for those failures that no dependency waits for.
    pub failures: Vec<String>,
    defs: Vec<TypeDef>,
    /// The node statuses that an `after` dependency waits for, so they do not fail the run.
    handled: Vec<(usize, Status)>,
    /// Why ops inside a call failed, by call: the call reports them when it fails.
    inner: Vec<(usize, String)>,
    /// The outputs of ended nodes that closed empty.
    empty: HashSet<(usize, String)>,
    /// The streams of the run.
    pub streams: Streams,
}

impl Progress {
    pub fn new(plan: Plan, defs: Vec<TypeDef>) -> Progress {
        let handled = (plan.nodes.iter())
            .flat_map(|node| &node.after)
            .flat_map(|(dependency, on)| on.iter().map(|status| (*dependency, *status)))
            .collect();
        Progress {
            states: vec![State::Waiting; plan.nodes.len()],
            plan,
            inputs: Map::new(),
            failures: Vec::new(),
            defs,
            handled,
            inner: Vec::new(),
            empty: HashSet::new(),
            streams: Streams::default(),
        }
    }

    pub fn node(&self, at: usize) -> &Node {
        &self.plan.nodes[at]
    }

    /// Takes in the nodes added to the plan from `from` on, like the rounds of a map.
    pub fn adopt(&mut self, from: usize) {
        self.states.resize(self.plan.nodes.len(), State::Waiting);
        let added = (self.plan.nodes[from..].iter()).flat_map(|node| &node.after);
        let handled =
            added.flat_map(|(dependency, on)| on.iter().map(|status| (*dependency, *status)));
        self.handled.extend(handled.collect::<Vec<_>>());
    }

    /// The next node that can start, with its input values. On the way it ends the nodes
    /// that can no longer run and the calls whose graph ended. While `draining`, nothing
    /// starts.
    pub fn next(&mut self, draining: bool) -> Option<(usize, Map<String, Json>)> {
        loop {
            let mut changed = false;
            let busy = self.busy();
            for at in 0..self.plan.nodes.len() {
                match &self.states[at] {
                    State::Waiting => {}
                    State::Running(_)
                        if matches!(self.node(at).op.kind, Kind::Call(_) | Kind::Branch(_)) =>
                    {
                        changed |= self.forward(at);
                        changed |= self.end_call(at);
                        continue;
                    }
                    State::Running(_) if matches!(self.node(at).op.kind, Kind::Map(_)) => {
                        changed |= self.end_map(at);
                        continue;
                    }
                    _ => continue,
                }
                if let Some((map, most)) = self.limit(at)
                    && busy.get(&map).is_some_and(|running| running >= &most)
                {
                    continue;
                }
                match self.readiness(at) {
                    Readiness::Wait => {}
                    Readiness::Omit => {
                        self.end(at, Status::Omitted, Map::new(), None);
                        changed = true;
                    }
                    Readiness::Ready(bound) if !draining => {
                        self.states[at] = State::Running(bound.clone());
                        return Some((at, bound));
                    }
                    Readiness::Ready(_) => {}
                    Readiness::Refuse(why) => {
                        self.end(at, Status::Failed, Map::new(), Some(why));
                        changed = true;
                    }
                }
            }
            if !changed {
                return None;
            }
        }
    }

    /// The map whose `parallelism` limits a node, when it is a round of one, with the limit.
    fn limit(&self, at: usize) -> Option<(usize, u32)> {
        let parent = self.node(at).parent?;
        match &self.node(parent).op.kind {
            Kind::Map(map) => Some((parent, map.parallelism?)),
            _ => None,
        }
    }

    /// The number of running rounds of each map that limits them.
    fn busy(&self) -> HashMap<usize, u32> {
        let mut counts = HashMap::new();
        for (at, state) in self.states.iter().enumerate() {
            if let (State::Running(_), Some((map, _))) = (state, self.limit(at)) {
                *counts.entry(map).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Ends a node whose outputs `empty` closed empty, with the values of its other outputs.
    pub fn end_with(
        &mut self,
        at: usize,
        status: Status,
        values: Map<String, Json>,
        empty: Vec<String>,
    ) {
        self.empty.extend(empty.into_iter().map(|port| (at, port)));
        self.end(at, status, values, None);
    }

    /// Ends a node. A failure that no dependency waits for fails the run; one inside a call
    /// fails the call instead.
    pub fn end(
        &mut self,
        at: usize,
        status: Status,
        values: Map<String, Json>,
        why: Option<String>,
    ) {
        self.states[at] = State::Ended(status, values);
        self.close_streams(at, status, why.as_deref());
        let failure = matches!(status, Status::Failed | Status::Errored);
        if failure && !self.handled.contains(&(at, status)) {
            let node = self.node(at);
            let why = why.unwrap_or_else(|| format!("op {} failed", node.path));
            match node.parent {
                Some(call) => self.inner.push((call, why)),
                None => self.failures.push(why),
            }
        }
    }

    /// The values of the outputs of the entry graph, without those that closed empty, or the
    /// first one that has none. A `dynamic` value must fit the type of its port in `ports`.
    pub fn outputs(&self, ports: &[wir::Port]) -> Result<Map<String, Json>, String> {
        let mut values = Map::new();
        for (port, source) in &self.plan.outputs {
            let typed = ports.iter().find(|item| item.name == *port);
            if typed.is_some_and(|item| !item.flow.is_value()) {
                values.insert(port.clone(), self.stream_output(port, source)?);
                continue;
            }
            match self.value(source) {
                Supply::Empty => {}
                Supply::Bound(value) => {
                    let typed = ports.iter().find(|item| item.name == *port);
                    if let Some(typed) = typed.filter(|_| self.unchecked(source))
                        && !wir::fits(&value, &typed.r#type, &self.defs)
                    {
                        return Err(format!("the output {port} does not fit its type"));
                    }
                    values.insert(port.clone(), value);
                }
                _ => return Err(format!("the output {port} has no value")),
            }
        }
        Ok(values)
    }

    /// Whether a source gives values that nothing checked: those of `dynamic` ports.
    fn unchecked(&self, source: &Source) -> bool {
        let Source::Output(node, port) = source else {
            return false;
        };
        let ports = &self.node(*node).op.outputs;
        let typed = ports.iter().find(|item| item.name == *port);
        typed.is_some_and(|item| holds_dynamic(&item.r#type, &self.defs))
    }

    fn readiness(&self, at: usize) -> Readiness {
        let node = self.node(at);
        if let Some(parent) = node.parent {
            match &self.states[parent] {
                State::Waiting => return Readiness::Wait,
                State::Ended(..) => return Readiness::Omit,
                State::Running(_) => {}
            }
        }
        for (dependency, on) in &node.after {
            match &self.states[*dependency] {
                State::Ended(status, _) if on.contains(status) => {}
                State::Ended(..) => return Readiness::Omit,
                _ => return Readiness::Wait,
            }
        }
        let mut bound = Map::new();
        for (port, source) in &node.inputs {
            let declared = node.op.inputs.iter().find(|item| item.name == *port);
            if declared.is_some_and(|item| !item.flow.is_value()) {
                match self.stream_ready(source) {
                    Ok(Some(())) => continue,
                    Ok(None) => return Readiness::Wait,
                    Err(()) => return Readiness::Omit,
                }
            }
            let value = match self.value(source) {
                Supply::Bound(value) => value,
                Supply::Pending => return Readiness::Wait,
                Supply::Empty | Supply::Never => return Readiness::Omit,
            };
            let value = match node.op.inputs.iter().find(|item| item.name == *port) {
                Some(item) => {
                    let value = match (value, &item.default) {
                        (Json::Null, Some(default)) => default.clone(),
                        (value, _) => value,
                    };
                    let value = widen(value, &item.r#type, &self.defs);
                    if self.unchecked(source) && !wir::fits(&value, &item.r#type, &self.defs) {
                        let path = &node.path;
                        return Readiness::Refuse(format!(
                            "op {path}: input {port} does not fit its type"
                        ));
                    }
                    value
                }
                None => value,
            };
            bound.insert(port.clone(), value);
        }
        for (port, default) in &node.defaults {
            if bound.get(port).is_none_or(Json::is_null) {
                bound.insert(port.clone(), default.clone());
            }
        }
        Readiness::Ready(bound)
    }

    /// What a source supplies. An absent input of the entry graph, or of a running call, is
    /// null; an output that an ended node neither bound nor closed empty is never bound.
    fn value(&self, source: &Source) -> Supply {
        match source {
            Source::Input(name) => {
                Supply::Bound(self.inputs.get(name).cloned().unwrap_or_default())
            }
            Source::Null => Supply::Bound(Json::Null),
            Source::Value(value) => Supply::Bound(value.clone()),
            Source::Call(call, port) => match &self.states[*call] {
                State::Running(bound) => {
                    Supply::Bound(bound.get(port).cloned().unwrap_or_default())
                }
                _ => Supply::Pending,
            },
            Source::Output(node, port) => match &self.states[*node] {
                // A skipped branch binds null to its outputs.
                State::Ended(Status::Succeeded | Status::Skipped, values) => match values.get(port)
                {
                    Some(value) => Supply::Bound(value.clone()),
                    None if self.empty.contains(&(*node, port.clone())) => Supply::Empty,
                    None => Supply::Never,
                },
                State::Ended(..) => Supply::Never,
                _ => Supply::Pending,
            },
        }
    }

    /// Ends a call once every op of its graph ended: it succeeds when none of them failed
    /// and every output of the graph is bound.
    fn end_call(&mut self, at: usize) -> bool {
        if !self.exports_ended(at) {
            return false;
        }
        self.forward(at);
        let mut failed = false;
        for member in (0..self.plan.nodes.len()).filter(|node| self.node(*node).parent == Some(at))
        {
            match &self.states[member] {
                State::Ended(Status::Failed | Status::Errored, _) => failed = true,
                State::Ended(..) => {}
                _ => return false,
            }
        }
        let mut values = Map::new();
        let mut unfit = None;
        let mut empty = Vec::new();
        for (port, source) in &self.node(at).exports {
            let ports = &self.node(at).op.outputs;
            if ports
                .iter()
                .any(|item| item.name == *port && !item.flow.is_value())
            {
                continue;
            }
            match self.value(source) {
                Supply::Empty => empty.push(port.clone()),
                Supply::Bound(value) => {
                    let ports = &self.node(at).op.outputs;
                    let typed = ports.iter().find(|item| item.name == *port);
                    if let Some(typed) = typed.filter(|_| self.unchecked(source))
                        && !wir::fits(&value, &typed.r#type, &self.defs)
                    {
                        let path = &self.node(at).path;
                        unfit = Some(format!("op {path}: output {port} does not fit its type"));
                    }
                    values.insert(port.clone(), value);
                }
                _ => failed = true,
            }
        }
        if let Some(why) = unfit {
            self.end(at, Status::Failed, Map::new(), Some(why));
            return true;
        }
        match failed {
            false => self.end_with(at, Status::Succeeded, values, empty),
            true => {
                let reasons: Vec<&str> = (self.inner.iter())
                    .filter(|(call, _)| *call == at)
                    .map(|(_, why)| why.as_str())
                    .collect();
                let message = match reasons.is_empty() {
                    true => format!(
                        "op {}: an output of its graph has no value",
                        self.node(at).path
                    ),
                    false => reasons.join("; "),
                };
                self.end(at, Status::Failed, Map::new(), Some(message));
            }
        }
        true
    }
}

impl Progress {
    /// Ends a map once every round ended: it succeeds when every round succeeded, or as many
    /// as `min_success`, with each output the array of the rounds' values in round order,
    /// nested by the map's shape. A round that did not succeed gives null.
    fn end_map(&mut self, at: usize) -> bool {
        let rounds: Vec<usize> = (0..self.plan.nodes.len())
            .filter(|node| self.node(*node).parent == Some(at))
            .collect();
        let mut values: Vec<Option<&Map<String, Json>>> = Vec::new();
        for round in &rounds {
            match &self.states[*round] {
                State::Ended(Status::Succeeded, given) => values.push(Some(given)),
                State::Ended(..) => values.push(None),
                _ => return false,
            }
        }
        let least = match &self.node(at).op.kind {
            Kind::Map(map) => map.min_success.map_or(rounds.len(), |least| least as usize),
            _ => rounds.len(),
        };
        let succeeded = values.iter().flatten().count();
        if succeeded < least {
            let reasons: Vec<&str> = (self.inner.iter())
                .filter(|(map, _)| *map == at)
                .map(|(_, why)| why.as_str())
                .collect();
            let message = match reasons.is_empty() {
                true => format!("op {}: a round failed", self.node(at).path),
                false => reasons.join("; "),
            };
            self.end(at, Status::Failed, Map::new(), Some(message));
            return true;
        }
        let node = self.node(at);
        for (_, why) in self.inner.iter().filter(|(map, _)| *map == at) {
            tracing::warn!(op = %node.path, "a round failed: {why}");
        }
        let mut outputs = Map::new();
        let mut unfit = None;
        for port in &node.op.outputs {
            let items: Vec<Json> = (values.iter())
                .map(|given| given.and_then(|given| given.get(&port.name).cloned()))
                .map(Option::unwrap_or_default)
                .collect();
            let value = nest(&items, &node.shape);
            if succeeded < rounds.len() && !wir::fits(&value, &port.r#type, &self.defs) {
                let (path, name) = (&node.path, &port.name);
                unfit = Some(format!(
                    "op {path}: a round failed, and output {name} takes no null"
                ));
                break;
            }
            outputs.insert(port.name.clone(), value);
        }
        match unfit {
            Some(why) => self.end(at, Status::Failed, Map::new(), Some(why)),
            None => self.end(at, Status::Succeeded, outputs, None),
        }
        true
    }
}

/// Values in round order as nested arrays of `shape`, one level per number.
fn nest(values: &[Json], shape: &[usize]) -> Json {
    match shape {
        [] | [_] => Json::Array(values.to_vec()),
        [count, rest @ ..] => {
            let size: usize = rest.iter().product();
            Json::Array(
                (0..*count)
                    .map(|index| nest(&values[index * size..(index + 1) * size], rest))
                    .collect(),
            )
        }
    }
}

/// Whether values of a type can hold parts of type `dynamic`.
fn holds_dynamic(r#type: &Type, defs: &[TypeDef]) -> bool {
    match resolve(r#type, defs) {
        Some(Type::Dynamic) => true,
        Some(Type::Optional(inner)) => holds_dynamic(inner, defs),
        Some(Type::Array(array)) => holds_dynamic(&array.items, defs),
        Some(Type::Map(map)) => holds_dynamic(&map.value, defs),
        Some(Type::Record(fields)) => {
            (fields.iter()).any(|field| holds_dynamic(&field.r#type, defs))
        }
        Some(Type::Union(items) | Type::Tuple(items)) => {
            items.iter().any(|item| holds_dynamic(item, defs))
        }
        _ => false,
    }
}

/// An `int` value where a `float` is wanted becomes a float, also inside other values.
pub fn widen(value: Json, r#type: &Type, defs: &[TypeDef]) -> Json {
    let Some(r#type) = resolve(r#type, defs) else {
        return value;
    };
    match (r#type, value) {
        (Type::Float, Json::Number(number)) if !number.is_f64() => {
            (number.as_f64()).map_or(Json::Number(number), Json::from)
        }
        (Type::Optional(inner), value) => widen(value, inner, defs),
        (Type::Array(array), Json::Array(items)) => Json::Array(
            items
                .into_iter()
                .map(|item| widen(item, &array.items, defs))
                .collect(),
        ),
        (Type::Map(map), Json::Object(object)) => Json::Object(
            (object.into_iter())
                .map(|(key, item)| (key, widen(item, &map.value, defs)))
                .collect(),
        ),
        (Type::Record(fields), Json::Object(object)) => Json::Object(
            (object.into_iter())
                .map(
                    |(key, item)| match fields.iter().find(|field| field.name == key) {
                        Some(field) => (key, widen(item, &field.r#type, defs)),
                        None => (key, item),
                    },
                )
                .collect(),
        ),
        // Extra items stay, so that the check of the value still sees its length.
        (Type::Tuple(types), Json::Array(items)) => Json::Array(
            (items.into_iter().enumerate())
                .map(|(index, item)| match types.get(index) {
                    Some(want) => widen(item, want, defs),
                    None => item,
                })
                .collect(),
        ),
        (_, value) => value,
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;

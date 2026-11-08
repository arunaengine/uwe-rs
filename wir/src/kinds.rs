//! Verifier checks of the fields of each op kind: task captures, call ports, native boundaries.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::callbacks::Arity;
use super::{Checker, Problem, show, walk};
use crate::document::Language;
use crate::document::Port;
use crate::flow::Flow;
use crate::op::{
    Argument, Branch, CaptureRule, Content, FileName, Kind, Map, Native, Op, Process, Scatter, Task,
};
use crate::reserved::Pick;
use crate::types::{ArrayType, Type, TypeDef, resolve};

impl<'d> Checker<'d> {
    pub(super) fn kind(&mut self, at: &str, op: &Op) {
        self.declared_effects(at, op);
        match &op.kind {
            Kind::Task(task) => self.task(at, op, task),
            Kind::Call(call) => self.call(at, op, &call.graph),
            Kind::Native(native) => self.native(at, op, native),
            Kind::Map(map) => self.map(at, op, map),
            Kind::Expr(expr) => {
                if op.outputs.len() != 1 {
                    self.report(at, Problem::Single("an expr".into()));
                }
                self.captures(at, op, &expr.captures);
                if let Some(scope) = &expr.scope {
                    self.scope(at, scope, expr.language);
                }
            }
            Kind::Emit(emit) => self.emit(at, op, emit),
            Kind::Transform(transform) => self.transform(at, op, transform),
            Kind::Filter(filter) => self.filter(at, op, &filter.body),
            Kind::FlatMap(flat) => self.flat_map(at, op, flat),
            Kind::Branch(branch) => self.branch(at, op, branch),
            Kind::Process(process) => self.process(at, op, process),
            Kind::Select(select) => self.select(at, op, select.rule),
            Kind::Merge(merge) if merge.takes_rounds(streams(op)) => {
                self.merge_rounds(at, op, merge)
            }
            Kind::Merge(_) => self.merge(at, op),
            Kind::Mix(_) => self.streams_of(at, op, "a mix"),
            Kind::Concat(_) => self.streams_of(at, op, "a concat"),
            Kind::Zip(zip) => self.zip(at, op, zip.remainder),
            Kind::Group(group) | Kind::GroupTuple(group) => self.group(at, op, group),
            Kind::Join(join) => self.join(at, op, join),
            Kind::Combine(combine) => self.combine(at, op, combine),
            Kind::Cross(cross) => self.cross(at, op, cross),
            Kind::Collect(collect) => self.collect(at, op, collect),
            Kind::ToList(_) => self.listed(at, op),
            Kind::Reduce(reduce) => self.reduce(at, op, reduce),
            Kind::First(first) => self.pick(at, op, "a first", &first.predicate, &first.empty),
            Kind::Last(last) => self.pick(at, op, "a last", &last.predicate, &last.empty),
            Kind::Buffer(buffer) => self.buffer(at, op, buffer),
            Kind::Window(window) => self.window(at, op, window),
            Kind::Take(_) => self.selects(at, op, "a take", None),
            Kind::Unique(unique) => self.selects(at, op, "a unique", unique.key.as_ref()),
            Kind::Distinct(distinct) => self.selects(at, op, "a distinct", distinct.key.as_ref()),
            Kind::Until(until) => self.until(at, op, &until.predicate),
            Kind::IfEmpty(fallback) => self.if_empty(at, op, fallback),
            Kind::Split(split) => self.split(at, op, split),
            Kind::CollectFile(collect) => self.collect_file(at, op, collect),
            Kind::View(view) => self.view(at, op, view),
            Kind::Observe(observe) => self.observe(at, op, observe),
            Kind::Route(route) => self.route(at, op, route),
            Kind::MultiMap(multi) => self.multi_map(at, op, multi),
            Kind::Flatten(flatten) if streams(op) => self.flatten_stream(at, op, flatten.depth),
            Kind::Flatten(_) => self.flatten(at, op),
            Kind::Dynamic(dynamic) => self.region(at, op, dynamic),
            Kind::Loop(looped) => self.looping(at, op, looped),
            _ => {}
        }
    }

    fn task(&mut self, at: &str, op: &Op, task: &Task) {
        if let Some(builder) = &task.builder {
            self.builder(at, op, task, builder);
            return;
        }
        if task.command.is_empty() {
            self.report(at, Problem::Empty("command".into()));
        }
        let mut used = Vec::new();
        for name in [&task.stdout, &task.stderr].into_iter().flatten() {
            match name {
                FileName::Text(text) if !inside(text) => {
                    self.report(at, Problem::Outside(text.clone()));
                }
                FileName::Text(_) => {}
                FileName::Input { input } => used.push(input),
            }
        }
        for capture in &task.captures {
            if let CaptureRule::Patterns(input) = &capture.from {
                used.push(input);
            }
        }
        let given = task.environment.values().chain(&task.stdin);
        for content in given {
            if let Content::Input { input } = content {
                used.push(input);
            }
        }
        for staged in &task.files {
            match (&staged.value, &staged.path) {
                (Content::Input { input }, _) => used.push(input),
                (Content::Text(_), None) => {
                    self.report(at, Problem::Empty("the path of staged text".into()));
                }
                (Content::Text(_), Some(_)) => {}
            }
            match &staged.path {
                Some(FileName::Text(text)) if !inside(text) => {
                    self.report(at, Problem::Outside(text.clone()));
                }
                Some(FileName::Input { input }) => used.push(input),
                _ => {}
            }
        }
        self.inputs(at, op, used);
        let folders = [&task.workdir, &task.staging].into_iter().flatten();
        for folder in folders.filter(|folder| !absolute(folder)) {
            self.report(at, Problem::Absolute(folder.clone()));
        }
        let used = task.command.iter().filter_map(|argument| match argument {
            Argument::Input { input, .. } => Some(input),
            Argument::Code { this, .. } => this.as_ref(),
            Argument::Text(_) | Argument::Raw { .. } => None,
        });
        self.inputs(at, op, used);
        for capture in &task.captures {
            if !op.outputs.iter().any(|port| port.name == capture.output) {
                self.report(at, Problem::NoPort(format!("output {}", capture.output)));
            }
        }
        for output in &op.outputs {
            let mut rules = task
                .captures
                .iter()
                .filter(|item| item.output == output.name);
            let (Some(capture), None) = (rules.next(), rules.next()) else {
                self.report(at, Problem::Capture(output.name.clone()));
                continue;
            };
            let loads = !capture.load_contents || holds_file(&output.r#type, self.defs);
            if !loads || !capture_fits(&capture.from, &output.r#type, self.defs) {
                self.report(at, Problem::CaptureType(output.name.clone()));
            }
            if let Some(arity) = &capture.arity {
                self.arity(at, output, arity);
            }
        }
        let retry = task
            .retry
            .iter()
            .flat_map(|retry| [&retry.retry_if, &retry.directives]);
        for closure in [&task.accept].into_iter().chain(retry).flatten() {
            self.closure(at, op, closure, Arity::Unary);
        }
    }

    /// The bounds are ordered, and an output that is not an array holds one path at most.
    fn arity(&mut self, at: &str, output: &Port, arity: &crate::op::Arity) {
        if arity.max.is_some_and(|max| max < arity.min) {
            let why = format!(
                "output {}: a least count no more than the most",
                output.name
            );
            self.report(at, Problem::Shape("a capture".into(), why));
        }
        let mut r#type = &output.r#type;
        while let Some(Type::Optional(inner)) = resolve(r#type, self.defs) {
            r#type = inner;
        }
        let single = !matches!(
            resolve(r#type, self.defs),
            Some(Type::Array(_) | Type::Dynamic)
        );
        if single && (arity.min > 1 || arity.max.is_some_and(|max| max > 1)) {
            let why = format!(
                "output {}: an array to hold more than one path",
                output.name
            );
            self.report(at, Problem::Shape("a capture".into(), why));
        }
    }

    /// The op's ports must be the called graph's ports, with types each side accepts.
    fn call(&mut self, at: &str, op: &Op, name: &str) {
        let Some(graph) = self.graph(name) else {
            return;
        };
        let sides = [
            ("input", &op.inputs, &graph.inputs),
            ("output", &op.outputs, &graph.outputs),
        ];
        for (side, ours, theirs) in sides {
            let names: BTreeSet<&str> =
                ours.iter().chain(theirs).map(|p| p.name.as_str()).collect();
            for port in names {
                let mine = ours.iter().find(|item| item.name == port);
                let other = theirs.iter().find(|item| item.name == port);
                let fits = match (side, mine, other) {
                    ("input", Some(mine), Some(other)) => {
                        other.takes().accepts(&mine.r#type, self.defs)
                    }
                    (_, Some(mine), Some(other)) => mine.r#type.accepts(&other.r#type, self.defs),
                    _ => false,
                };
                if !fits || mine.map(|item| item.flow) != other.map(|item| item.flow) {
                    let port = format!("{side} {port}");
                    self.report(
                        at,
                        Problem::CallPort {
                            port,
                            graph: name.to_owned(),
                        },
                    );
                }
            }
        }
    }

    /// The op's ports must be the body's ports: an input in `over` is an array of what the
    /// body takes, and each output an array, nested per input for `nested_cross`, of what it gives.
    fn map(&mut self, at: &str, op: &Op, map: &Map) {
        self.inputs(at, op, map.over.iter());
        if map.over.is_empty() {
            self.report(at, Problem::Empty("over".into()));
        }
        let Some(body) = self.graph(&map.body) else {
            return;
        };
        let depth = match map.mode {
            Scatter::NestedCross => map.over.len(),
            _ => 1,
        };
        let sides = [
            ("input", &op.inputs, &body.inputs),
            ("output", &op.outputs, &body.outputs),
        ];
        for (side, ours, theirs) in sides {
            let names: BTreeSet<&str> =
                ours.iter().chain(theirs).map(|p| p.name.as_str()).collect();
            for port in names {
                let mine = ours.iter().find(|item| item.name == port);
                let other = theirs.iter().find(|item| item.name == port);
                let fits = match (side, mine, other) {
                    ("input", Some(mine), Some(other)) if map.over.iter().any(|n| n == port) => {
                        match resolve(&mine.r#type, self.defs) {
                            Some(Type::Array(array)) => {
                                other.takes().accepts(&array.items, self.defs)
                            }
                            _ => false,
                        }
                    }
                    ("input", Some(mine), Some(other)) => {
                        other.takes().accepts(&mine.r#type, self.defs)
                    }
                    (_, Some(mine), Some(other)) => {
                        let mut made = other.r#type.clone();
                        for _ in 0..depth {
                            made = Type::Array(ArrayType {
                                items: Box::new(made),
                                nonempty: false,
                            });
                        }
                        mine.r#type.accepts(&made, self.defs)
                    }
                    _ => false,
                };
                let flows = [mine, other]
                    .iter()
                    .flatten()
                    .all(|item| item.flow.is_value());
                if !fits || !flows {
                    let port = format!("{side} {port}");
                    let graph = map.body.clone();
                    self.report(at, Problem::CallPort { port, graph });
                }
            }
        }
    }

    /// The body has the op's ports: the item type of a stream input, an element of an
    /// `each` input, and each output the stream or value of what the body gives.
    fn process(&mut self, at: &str, op: &Op, process: &Process) {
        self.inputs(at, op, process.each.iter());
        let mut seen = BTreeSet::new();
        for name in process.each.iter().filter(|name| !seen.insert(*name)) {
            self.report(at, Problem::Duplicate(format!("each input {name}")));
        }
        if process.parallelism == Some(0) {
            self.report(
                at,
                Problem::Shape("a process".into(), "a positive parallelism".into()),
            );
        }
        let streaming =
            !process.each.is_empty() || op.inputs.iter().any(|port| !port.flow.is_value());
        let Some(body) = self.graph(&process.body) else {
            return;
        };
        let names: BTreeSet<&str> = (op
            .inputs
            .iter()
            .chain(&op.outputs)
            .chain(&body.inputs)
            .chain(&body.outputs))
        .map(|port| port.name.as_str())
        .collect();
        for name in names {
            let mine = op.inputs.iter().find(|port| port.name == name);
            let theirs = body.inputs.iter().find(|port| port.name == name);
            let (made, side) = match (mine, theirs) {
                (Some(mine), Some(theirs)) => {
                    let each = process.each.iter().any(|item| item == name);
                    let item = match each {
                        true if !mine.flow.is_value() => None,
                        true => match resolve(&mine.r#type, self.defs) {
                            Some(Type::Array(array)) => Some((*array.items).clone()),
                            _ => Some(mine.r#type.clone()),
                        },
                        false => Some(mine.r#type.clone()),
                    };
                    let fits = item.is_some_and(|item| theirs.takes().accepts(&item, self.defs));
                    (fits && theirs.flow.is_value(), "input")
                }
                (None, None) => {
                    let mine = op.outputs.iter().find(|port| port.name == name);
                    let theirs = body.outputs.iter().find(|port| port.name == name);
                    let fits = match (mine, theirs) {
                        (Some(mine), Some(theirs)) => {
                            let flow = if streaming { Flow::Stream } else { Flow::Value };
                            mine.flow == flow
                                && theirs.flow.is_value()
                                && mine.r#type.accepts(&theirs.r#type, self.defs)
                        }
                        _ => false,
                    };
                    (fits, "output")
                }
                _ => (false, "input"),
            };
            if !made {
                let port = format!("{side} {name}");
                let graph = process.body.clone();
                self.report(at, Problem::CallPort { port, graph });
            }
        }
    }

    /// The condition is a bool input, the body has the other inputs, and each output takes
    /// the body's output or null.
    fn branch(&mut self, at: &str, op: &Op, branch: &Branch) {
        self.inputs(at, op, [&branch.condition]);
        let condition = op.inputs.iter().find(|port| port.name == branch.condition);
        if let Some(port) =
            condition.filter(|port| resolve(&port.r#type, self.defs) != Some(&Type::Bool))
        {
            let (found, want) = (show(&port.r#type), "\"bool\"".to_owned());
            let port = format!("input {}", port.name);
            self.report(at, Problem::Mismatch { port, found, want });
        }
        let Some(body) = self.graph(&branch.body) else {
            return;
        };
        let inputs = op
            .inputs
            .iter()
            .filter(|port| port.name != branch.condition);
        let sides = [
            (
                "input",
                inputs.collect::<Vec<_>>(),
                body.inputs.iter().collect::<Vec<_>>(),
            ),
            (
                "output",
                op.outputs.iter().collect(),
                body.outputs.iter().collect(),
            ),
        ];
        for (side, ours, theirs) in sides {
            let names: BTreeSet<&str> = ours
                .iter()
                .chain(&theirs)
                .map(|p| p.name.as_str())
                .collect();
            for port in names {
                let mine = ours.iter().find(|item| item.name == port);
                let other = theirs.iter().find(|item| item.name == port);
                let fits = match (side, mine, other) {
                    ("input", Some(mine), Some(other)) => {
                        other.takes().accepts(&mine.r#type, self.defs)
                    }
                    (_, Some(mine), Some(other)) => {
                        let made = Type::Optional(Box::new(other.r#type.clone()));
                        mine.r#type.accepts(&made, self.defs)
                    }
                    _ => false,
                };
                let flows = [mine, other]
                    .iter()
                    .flatten()
                    .all(|item| item.flow.is_value());
                if !fits || !flows {
                    let port = format!("{side} {port}");
                    let graph = branch.body.clone();
                    self.report(at, Problem::CallPort { port, graph });
                }
            }
        }
    }

    /// The output holds the items that are set, one for `first` and `only`, all for `all`.
    fn select(&mut self, at: &str, op: &Op, rule: Pick) {
        let Some((items, output)) = self.unary(at, op, "a select") else {
            return;
        };
        let item = match resolve(&items, self.defs) {
            Some(Type::Optional(inner)) => (**inner).clone(),
            _ => items,
        };
        let made = match rule {
            Pick::All => array_of(item),
            Pick::First | Pick::Only => item,
        };
        self.gives(at, output, &made);
    }

    /// The output is an array whose items accept every input.
    fn merge(&mut self, at: &str, op: &Op) {
        if op.inputs.is_empty() {
            self.report(at, Problem::Empty("the inputs of a merge".into()));
        }
        let [output] = op.outputs.as_slice() else {
            self.report(at, Problem::Single("a merge".into()));
            return;
        };
        for input in &op.inputs {
            self.gives(at, output, &array_of(input.r#type.clone()));
        }
    }

    /// The output is an array of the items of the input's array items.
    fn flatten(&mut self, at: &str, op: &Op) {
        let Some((items, output)) = self.unary(at, op, "a flatten") else {
            return;
        };
        let inner = match resolve(&items, self.defs) {
            Some(Type::Array(array)) => (*array.items).clone(),
            _ => items,
        };
        self.gives(at, output, &array_of(inner));
    }

    /// The item type of the one input of an op, an array, and its one output.
    fn unary<'o>(&mut self, at: &str, op: &'o Op, what: &str) -> Option<(Type, &'o Port)> {
        let ([input], [output]) = (op.inputs.as_slice(), op.outputs.as_slice()) else {
            self.report(at, Problem::Unary(what.into()));
            return None;
        };
        match resolve(&input.r#type, self.defs) {
            Some(Type::Array(array)) => Some(((*array.items).clone(), output)),
            Some(Type::Dynamic) => Some((Type::Dynamic, output)),
            _ => {
                let port = format!("input {}", input.name);
                let (found, want) = (show(&input.r#type), "an array".to_owned());
                self.report(at, Problem::Mismatch { port, found, want });
                None
            }
        }
    }

    /// The output port must accept what the op makes.
    pub(super) fn gives(&mut self, at: &str, output: &Port, made: &Type) {
        if !output.r#type.accepts(made, self.defs) {
            let port = format!("output {}", output.name);
            let (found, want) = (show(made), show(&output.r#type));
            self.report(at, Problem::Mismatch { port, found, want });
        }
    }

    fn inputs<'n>(&mut self, at: &str, op: &Op, names: impl IntoIterator<Item = &'n String>) {
        for name in names {
            if !op.inputs.iter().any(|port| port.name == *name) {
                self.report(at, Problem::NoPort(format!("input {name}")));
            }
        }
    }

    fn native(&mut self, at: &str, op: &Op, native: &Native) {
        if let Some(source) = self.source(at, &native.source)
            && source.language != native.language
        {
            let named = native.source.clone();
            let (found, want) = (show(&native.language), show(&source.language));
            self.report(at, Problem::Language { named, found, want });
        }
        for config in &native.configs {
            if let Some(source) = self.source(at, config)
                && source.language != native.language
            {
                let named = config.clone();
                let (found, want) = (show(&native.language), show(&source.language));
                self.report(at, Problem::Language { named, found, want });
            }
        }
        // Nextflow emits have no types, so a block may take what one gives as dynamic.
        let open = native.language == Language::Nextflow;
        let sides = [(&op.inputs, false), (&op.outputs, open)];
        for (ports, allowed) in sides {
            for port in ports {
                if !allowed && self.has_dynamic(&port.r#type) {
                    self.report(at, Problem::Untyped(format!("port {}", port.name)));
                }
            }
        }
    }

    fn has_dynamic(&self, r#type: &Type) -> bool {
        let mut found = false;
        walk(r#type, &mut |item| match item {
            Type::Dynamic => found = true,
            Type::Named(name) => found |= self.named(name).is_some_and(|t| self.has_dynamic(t)),
            _ => {}
        });
        found
    }
}

/// Whether an op has a stream port, which selects the stream form of `merge` and `flatten`.
pub(super) fn streams(op: &Op) -> bool {
    (op.inputs.iter().chain(&op.outputs)).any(|port| !port.flow.is_value())
}

pub(super) fn array_of(items: Type) -> Type {
    Type::Array(ArrayType {
        items: Box::new(items),
        nonempty: false,
    })
}

/// Whether a path is absolute and has no `..`.
fn absolute(path: &str) -> bool {
    path.starts_with('/') && !path.split('/').any(|part| part == "..")
}

/// Whether a path is relative and stays inside the working folder.
fn inside(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|part| part == "..")
}

/// Whether a type is a File, also inside an optional or an array, so contents can be loaded.
fn holds_file(r#type: &Type, defs: &[TypeDef]) -> bool {
    match resolve(r#type, defs) {
        Some(Type::File(_)) => true,
        Some(Type::Optional(inner)) => holds_file(inner, defs),
        Some(Type::Array(array)) => holds_file(&array.items, defs),
        _ => false,
    }
}

/// Glob captures fill File, Directory or arrays of them; stdout and stderr fill a File.
fn capture_fits(rule: &CaptureRule, r#type: &Type, defs: &[TypeDef]) -> bool {
    let Some(r#type) = resolve(r#type, defs) else {
        return false;
    };
    match (rule, r#type) {
        (_, Type::Optional(inner)) => capture_fits(rule, inner, defs),
        (_, Type::File(_) | Type::Dynamic) => true,
        (CaptureRule::Glob(_) | CaptureRule::Patterns(_), Type::Directory) => true,
        (CaptureRule::Glob(_) | CaptureRule::Patterns(_), Type::Array(array)) => {
            matches!(
                resolve(&array.items, defs),
                Some(Type::File(_) | Type::Directory)
            )
        }
        _ => false,
    }
}

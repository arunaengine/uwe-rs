//! Compiles one step or DAG task: a dialect task that the Argo runtime expands as its native run
//! does, or a call of the graph of a steps or DAG template, and a map for a loop.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::{
    Arguments, ContinueOn, DagTask, Sequence, Template, TemplateRef, WorkflowStep,
};
use argo_parser::{Node, decode};
use argo_runtime::adapter::Form;
use serde_json::{Value as Json, json};
use wir::reserved::{Carry, Flatten, Merge};
use wir::{
    Binding, Edge, Endpoint, ExprForm, Graph, Kind, Op, Port, PortMap, TaskBuilder, TaskRef, Type,
};

use crate::graphs::{Built, Compiler, Place, Scope, Target, kept};
use crate::tags::{Tag, has_tag, read_tags, single_tag};
use crate::texts::{VALUE, text_node, text_op};
use crate::{Kept, SOURCE, array, artifact, edge, end, op, origin, path_type, port, text};

/// A step or DAG task as the compiler reads it.
pub(crate) struct Site {
    /// `[1].name` for a step of group 1, `.name` for a DAG task.
    pub call: String,
    pub name: String,
    pub raw: Node,
    pub template: String,
    pub template_ref: Option<TemplateRef>,
    pub inline: bool,
    pub when: String,
    pub with_items: Vec<Node>,
    pub with_param: String,
    pub with_sequence: Option<Sequence>,
    pub arguments: Arguments,
    pub continue_on: ContinueOn,
    /// Whether it has lifecycle hooks or an exit handler.
    pub hooks: bool,
}

impl Site {
    pub fn of_step(group: usize, raw: &Node) -> Result<Site, String> {
        let (step, _) = decode::<WorkflowStep>(raw).map_err(|error| error.to_string())?;
        Ok(Site {
            call: format!("[{group}].{}", step.name),
            name: step.name,
            raw: raw.clone(),
            template: step.template,
            template_ref: step.template_ref,
            inline: step.inline.is_some(),
            when: step.when,
            with_items: step.with_items,
            with_param: step.with_param,
            with_sequence: step.with_sequence,
            arguments: step.arguments,
            continue_on: step.continue_on.unwrap_or_default(),
            hooks: !step.hooks.is_empty() || !step.on_exit.is_empty(),
        })
    }

    pub fn of_task(raw: &Node) -> Result<Site, String> {
        let (task, _) = decode::<DagTask>(raw).map_err(|error| error.to_string())?;
        Ok(Site {
            call: format!(".{}", task.name),
            name: task.name,
            raw: raw.clone(),
            template: task.template,
            template_ref: task.template_ref,
            inline: task.inline.is_some(),
            when: task.when,
            with_items: task.with_items,
            with_param: task.with_param,
            with_sequence: task.with_sequence,
            arguments: task.arguments,
            continue_on: task.continue_on.unwrap_or_default(),
            hooks: !task.hooks.is_empty() || !task.on_exit.is_empty(),
        })
    }
}

/// The items of a loop: known before the run, with the labels that they give the node names,
/// or the text that argo expr ops read them from.
enum Rounds {
    Known(Vec<Json>, Vec<Json>),
    Computed(String),
}

/// A value that the op of a site reads: its port, where it comes from in the graph of the
/// site, and its native selector when a dialect task reads it.
struct Need {
    port: Port,
    from: Endpoint,
    native: String,
}

/// The ops of a site before they go into the graph of the site or into the body of a map;
/// `None` endpoints name the needs, which are graph inputs in a body.
pub(crate) struct Piece {
    pub ops: Vec<Op>,
    pub edges: Vec<Edge>,
    pub main: String,
    pub outputs: Vec<Port>,
}

impl Compiler {
    /// Compiles a site of the template at `holding` into `graph`.
    pub fn site(
        &mut self,
        holding: &Place,
        graph: &mut Graph,
        scope: &Scope,
        site: &Site,
    ) -> Result<Built, Kept> {
        let at = format!("{}/{}", graph.name, site.name);
        let fail = |reason: &str| Err(kept(&at, reason));
        if site.inline {
            return fail("an inline template");
        }
        if site.hooks {
            return fail("lifecycle hooks and exit handlers of steps need their status as a value");
        }
        let place = match &site.template_ref {
            Some(reference) => Place {
                name: reference.template.clone(),
                reference: Some((reference.name.clone(), reference.cluster_scope)),
            },
            None => Place {
                name: site.template.clone(),
                reference: holding.reference.clone(),
            },
        };
        let target = self.target(&place, &at)?;
        let tags = read_tags(&site.raw).map_err(|reason| kept(&at, &reason))?;
        let rounds = self.rounds(site, &at)?;
        let mut needs = vec![Need {
            port: port("node.path", path_type()),
            from: end(None, "node.path"),
            native: "parameter/node.path".into(),
        }];
        for tag in &tags {
            if *tag == Tag::Item {
                continue;
            }
            let Some((from, r#type)) = source(scope, tag) else {
                continue;
            };
            let name = tag.name();
            let kind = match name.contains(".artifacts.") {
                true => "artifact",
                false => "parameter",
            };
            needs.push(Need {
                port: port(name.clone(), r#type),
                from,
                native: format!("{kind}/{name}"),
            });
        }
        let looping = rounds.is_some();
        let calls_graph = matches!(target, Target::Graph(..));
        let mut available: Vec<(String, Type)> = (needs.iter())
            .map(|need| (need.port.name.clone(), need.port.r#type.clone()))
            .collect();
        if looping {
            available.push(("item".into(), Type::Dynamic));
            available.push(("label".into(), text()));
        }
        let piece = match target {
            Target::Leaf(template) => dialect(site, holding, &template, &needs, looping),
            Target::Graph(called, takes, gives) => {
                let strings = rounds.as_ref().map(|rounds| match rounds {
                    Rounds::Known(items, _) => items.iter().all(Json::is_string),
                    Rounds::Computed(_) => false,
                });
                let carry = self.loops.get(&called).map(|(_, carry)| carry.clone());
                let piece = call(
                    site, &called, &takes, &gives, &available, scope, strings, carry,
                )
                .map_err(|reason| kept(&at, &reason))?;
                match site.when.is_empty() {
                    true => piece,
                    false => (self.guard(&graph.name, site, piece, &available))
                        .map_err(|reason| kept(&at, &reason))?,
                }
            }
            Target::Recursion if looping || site.continue_on != ContinueOn::default() => {
                return fail("a recursive call in a loop or with continueOn");
            }
            Target::Recursion => (self.recursion(graph, site, &available, scope))
                .map_err(|reason| kept(&at, &reason))?,
        };
        let Some(rounds) = rounds else {
            for need in &needs {
                for wire in &piece.edges {
                    if wire.from.op.is_none() && wire.from.port == need.port.name {
                        graph.edges.push(edge(need.from.clone(), wire.to.clone()));
                    }
                }
            }
            graph.edges.extend(
                piece
                    .edges
                    .into_iter()
                    .filter(|wire| wire.from.op.is_some()),
            );
            graph.ops.extend(piece.ops);
            return Ok(Built {
                op: piece.main,
                outputs: piece.outputs,
            });
        };
        let body = format!("{}.{}", graph.name, crate::safe(&site.name));
        let mut inputs: Vec<Port> = needs.iter().map(|need| need.port.clone()).collect();
        inputs.push(port("item", Type::Dynamic));
        if calls_graph {
            inputs.push(port("label", text()));
        }
        let mut edges: Vec<Edge> = piece.edges;
        let mut outputs = Vec::new();
        for output in &piece.outputs {
            outputs.push(output.clone());
            edges.push(edge(
                end(Some(&piece.main), &output.name),
                end(None, &output.name),
            ));
        }
        let mut map = op(
            site.name.clone(),
            Kind::Map(wir::Map {
                body: body.clone(),
                over: match calls_graph {
                    true => vec!["item".into(), "label".into()],
                    false => vec!["item".into()],
                },
                mode: wir::Scatter::default(),
                parallelism: None,
                min_success: None,
            }),
        );
        map.origin = origin(&site.raw);
        for need in &needs {
            map.inputs.push(need.port.clone());
            graph.edges.push(edge(
                need.from.clone(),
                end(Some(&site.name), &need.port.name),
            ));
        }
        let mut over = vec![("item", ExprForm::Items, array(Type::Dynamic))];
        if calls_graph {
            over.push(("label", ExprForm::Labels, array(text())));
        }
        for (name, form, r#type) in over {
            let given = port(name, r#type);
            let (items, labels, code) = match &rounds {
                Rounds::Known(items, labels) => (items, labels, None),
                Rounds::Computed(code) => (&Vec::new(), &Vec::new(), Some(code)),
            };
            let Some(code) = code else {
                let known = match form {
                    ExprForm::Items => items.clone(),
                    _ => labels.clone(),
                };
                map.inputs
                    .push(crate::with_default(given, Json::Array(known)));
                continue;
            };
            let path = format!("{}.{name}s", site.name);
            let found = |tag: &Tag| {
                (needs.iter())
                    .find(|need| need.port.name == tag.name())
                    .map(|need| (need.from.clone(), need.port.r#type.clone()))
            };
            let (made, wires) =
                text_op(&path, code, form, found).map_err(|reason| kept(&at, &reason))?;
            graph.ops.push(made);
            graph.edges.extend(wires);
            graph
                .edges
                .push(edge(end(Some(&path), VALUE), end(Some(&site.name), name)));
            map.inputs.push(given);
        }
        map.outputs = (outputs.iter())
            .map(|output| port(output.name.clone(), array(output.r#type.clone())))
            .collect();
        let built = Built {
            op: site.name.clone(),
            outputs: map.outputs.clone(),
        };
        self.graphs.push(Graph {
            name: body,
            inputs,
            outputs,
            ops: piece.ops,
            edges,
        });
        graph.ops.push(map);
        Ok(built)
    }

    /// The items of a loop, and their node name labels when the list is known before the run.
    fn rounds(&mut self, site: &Site, at: &str) -> Result<Option<Rounds>, Kept> {
        if !site.with_param.is_empty() {
            return Ok(Some(Rounds::Computed(site.with_param.clone())));
        }
        let text = if !site.with_items.is_empty() {
            Json::Array(site.with_items.iter().map(crate::json_of).collect()).to_string()
        } else if let Some(sequence) = &site.with_sequence {
            let mut object = serde_json::Map::new();
            for (key, bound) in [
                ("count", &sequence.count),
                ("start", &sequence.start),
                ("end", &sequence.end),
            ] {
                if let Some(bound) = bound {
                    object.insert(key.into(), bound.0.clone().into());
                }
            }
            object.insert("format".into(), sequence.format.clone().into());
            Json::Object(object).to_string()
        } else {
            return Ok(None);
        };
        if has_tag(&text) {
            return Ok(Some(Rounds::Computed(text)));
        }
        let empty = serde_json::Map::new();
        let mut evaluate = |form| {
            let value = (self.adapter.evaluate(&text, form, &site.call, &empty))
                .map_err(|reason| kept(at, &reason))?;
            match value {
                Json::Array(items) => Ok(items),
                _ => Err(kept(at, "the items of a loop are no list")),
            }
        };
        let items = evaluate(Form::Items)?;
        let labels = evaluate(Form::Labels)?;
        Ok(Some(Rounds::Known(items, labels)))
    }

    /// Runs a piece only when the `when` of its site holds: an argo expr op gives the condition
    /// of a branch, whose body graph holds the piece.
    fn guard(
        &mut self,
        holder: &str,
        site: &Site,
        piece: Piece,
        available: &[(String, Type)],
    ) -> Result<Piece, String> {
        let main = piece.main.clone();
        let body = format!("{holder}.{}.when", crate::safe(&site.name));
        let mut inputs: Vec<Port> = Vec::new();
        for wire in piece.edges.iter().filter(|wire| wire.from.op.is_none()) {
            if inputs.iter().any(|input| input.name == wire.from.port) {
                continue;
            }
            let (name, r#type) = (available.iter())
                .find(|(name, _)| *name == wire.from.port)
                .ok_or_else(|| format!("the call reads {}, which has no value", wire.from.port))?;
            inputs.push(port(name.clone(), r#type.clone()));
        }
        let mut edges = piece.edges;
        for output in &piece.outputs {
            let from = end(Some(&main), &output.name);
            edges.push(edge(from, end(None, &output.name)));
        }
        // Optional outputs of the body would nest in those of the branch; dynamic ones do not.
        let outputs = (piece.outputs.iter())
            .map(|output| port(output.name.clone(), Type::Dynamic))
            .collect();
        self.graphs.push(Graph {
            name: body.clone(),
            inputs: inputs.clone(),
            outputs,
            ops: piece.ops,
            edges,
        });
        let condition = format!("{main}.when");
        let (when, mut wires) = text_op(
            &condition,
            &site.when,
            ExprForm::Condition,
            from_available(available),
        )?;
        let mut branch = op(
            main.clone(),
            Kind::Branch(wir::Branch {
                body,
                condition: "condition".into(),
            }),
        );
        branch.origin = origin(&site.raw);
        for input in &inputs {
            wires.push(edge(end(None, &input.name), end(Some(&main), &input.name)));
        }
        branch.inputs = inputs;
        branch.inputs.push(port("condition", Type::Bool));
        wires.push(edge(
            end(Some(&condition), VALUE),
            end(Some(&main), "condition"),
        ));
        branch.outputs = (piece.outputs.iter())
            .map(|output| port(output.name.clone(), optional(output.r#type.clone())))
            .collect();
        Ok(Piece {
            outputs: branch.outputs.clone(),
            ops: vec![when, branch],
            edges: wires,
            main,
        })
    }
}

/// Where a tag comes from in a piece: the value of that name among `available`.
pub(crate) fn from_available(
    available: &[(String, Type)],
) -> impl Fn(&Tag) -> Option<(Endpoint, Type)> + '_ {
    |tag| {
        (available.iter())
            .find(|(name, _)| *name == tag.name())
            .map(|(name, r#type)| (end(None, name), r#type.clone()))
    }
}

fn optional(r#type: Type) -> Type {
    match r#type {
        Type::Optional(_) => r#type,
        other => Type::Optional(Box::new(other)),
    }
}

/// Where the value of a tag comes from in a graph, and its type.
pub(crate) fn source(scope: &Scope, tag: &Tag) -> Option<(Endpoint, Type)> {
    match tag {
        Tag::Input(name) | Tag::Status(name) => (scope.inputs.iter())
            .find(|input| &input.name == name)
            .map(|input| (end(None, name), input.r#type.clone())),
        Tag::Made(prefix, rest) => {
            let (op, outputs) = scope.made.get(prefix)?;
            let found = outputs.iter().find(|output| &output.name == rest)?;
            Some((end(Some(op), rest), found.r#type.clone()))
        }
        Tag::Item => None,
    }
}

/// The dialect task of a site whose template is a leaf.
fn dialect(site: &Site, holding: &Place, template: &Template, needs: &[Need], item: bool) -> Piece {
    let mut inputs: Vec<(Port, String)> = needs
        .iter()
        .map(|need| (need.port.clone(), need.native.clone()))
        .collect();
    if item {
        inputs.push((port("item", Type::Dynamic), "parameter/item".into()));
    }
    let mut task = task_op(&site.name, holding, &site.call, template, inputs);
    task.origin = origin(&site.raw);
    let edges = (task.inputs.iter())
        .map(|input| edge(end(None, &input.name), end(Some(&site.name), &input.name)))
        .collect();
    Piece {
        main: site.name.clone(),
        outputs: task.outputs.clone(),
        ops: vec![task],
        edges,
    }
}

/// The dialect task that calls the entrypoint, a leaf, for the workflow itself, and its outputs.
pub(crate) fn entry_task(
    graph: &mut Graph,
    path: &str,
    place: &Place,
    template: &Template,
) -> Vec<Port> {
    let path_port = crate::with_default(port("node.path", path_type()), json!([]));
    let inputs = vec![(path_port, "parameter/node.path".to_owned())];
    let task = task_op(path, place, "entrypoint", template, inputs);
    let outputs = task.outputs.clone();
    graph.ops.push(task);
    outputs
}

/// A dialect task of the Argo runtime: `entry` holds the step `call`, which runs `template`.
fn task_op(
    path: &str,
    holding: &Place,
    call: &str,
    template: &Template,
    inputs: Vec<(Port, String)>,
) -> Op {
    let maybe = |r#type: Type| Type::Optional(Box::new(r#type));
    let mut outputs: Vec<(Port, String)> = Vec::new();
    for parameter in &template.outputs.parameters {
        let name = format!("outputs.parameters.{}", parameter.name);
        outputs.push((
            port(name.clone(), maybe(text())),
            format!("parameter/{name}"),
        ));
    }
    for output in &template.outputs.artifacts {
        let name = format!("outputs.artifacts.{}", output.name);
        outputs.push((port(name.clone(), artifact()), format!("artifact/{name}")));
    }
    for name in ["outputs.result", "exitCode"] {
        outputs.push((port(name, maybe(text())), format!("parameter/{name}")));
    }
    let map = |ports: &[(Port, String)]| -> Vec<PortMap> {
        (ports.iter())
            .map(|(port, native)| PortMap {
                port: port.name.clone(),
                native: native.clone(),
            })
            .collect()
    };
    let mut captures = BTreeMap::new();
    if let Some((document, cluster_scope)) = &holding.reference {
        let reference = json!({"name": document, "cluster_scope": cluster_scope});
        captures.insert("template_ref".to_owned(), Binding::Value(reference));
    }
    let reference = TaskRef {
        source: SOURCE.into(),
        entry: holding.name.clone(),
        call: call.to_owned(),
        scope: None,
        inputs: map(&inputs),
        outputs: map(&outputs),
        profiles: Vec::new(),
        configs: Vec::new(),
        captures,
    };
    let kind = Kind::Task(wir::Task {
        builder: Some(TaskBuilder::ArgoTask(reference)),
        ..wir::Task::default()
    });
    let mut task = op(path, kind);
    task.inputs = inputs.into_iter().map(|(port, _)| port).collect();
    task.outputs = outputs.into_iter().map(|(port, _)| port).collect();
    task
}

/// The call of the graph of a steps or DAG template, with its node name made from the path
/// of the holding template: `merge` gives the parts and `flatten` joins them into one list.
#[allow(clippy::too_many_arguments, reason = "the parts of one call")]
pub(crate) fn call(
    site: &Site,
    called: &str,
    takes: &[Port],
    gives: &[Port],
    available: &[(String, Type)],
    scope: &Scope,
    strings: Option<bool>,
    carry: Option<Vec<Carry>>,
) -> Result<Piece, String> {
    let main = site.name.clone();
    let parts = format!("{main}.parts");
    let joined = format!("{main}.path");
    let mut merge = op(parts.clone(), Kind::Merge(Merge::default()));
    merge.inputs.push(port("path", path_type()));
    merge.inputs.push(crate::with_default(
        port("step", text()),
        site.call.clone().into(),
    ));
    if strings.is_some() {
        merge.inputs.push(port("label", text()));
    }
    merge.outputs.push(port("parts", array(Type::Dynamic)));
    let mut flatten = op(joined.clone(), Kind::Flatten(Flatten::default()));
    flatten.inputs.push(port("parts", array(Type::Dynamic)));
    flatten.outputs.push(port("path", path_type()));
    let mut edges = vec![
        edge(end(None, "node.path"), end(Some(&parts), "path")),
        edge(end(Some(&parts), "parts"), end(Some(&joined), "parts")),
        edge(end(Some(&joined), "path"), end(Some(&main), "node.path")),
    ];
    if strings.is_some() {
        edges.push(edge(end(None, "label"), end(Some(&parts), "label")));
    }
    let mut call = op(main.clone(), crate::recursion::call_kind(called, carry));
    call.origin = origin(&site.raw);
    let mut computed = Vec::new();
    for take in takes {
        let name = &take.name;
        let mut input = take.clone();
        input.default = None;
        if name == "node.path" {
            call.inputs.push(input);
            continue;
        }
        match name.strip_prefix("inputs.parameters.") {
            Some(parameter) => {
                let given = site
                    .arguments
                    .parameters
                    .iter()
                    .find(|given| given.name == parameter);
                let Some(given) = given else {
                    call.inputs.push(input);
                    continue;
                };
                if given.value_from.is_some() {
                    return Err(format!("argument {parameter} reads valueFrom"));
                }
                let Some(value) = &given.value else {
                    call.inputs.push(input);
                    continue;
                };
                if !has_tag(&value.0) {
                    call.inputs
                        .push(crate::with_default(input, value.0.clone().into()));
                    continue;
                }
                if let Some(from) = direct(&value.0, available, scope, strings) {
                    edges.push(edge(end(None, &from), end(Some(&main), name)));
                } else {
                    let path = format!("{main}.arguments.{}", crate::safe(parameter));
                    let (made, wires) =
                        text_op(&path, &value.0, ExprForm::Text, from_available(available))?;
                    computed.push(made);
                    edges.extend(wires);
                    edges.push(edge(end(Some(&path), VALUE), end(Some(&main), name)));
                }
                call.inputs.push(input);
                continue;
            }
            None => {
                let artifact = name.strip_prefix("inputs.artifacts.").unwrap_or(name);
                let given = site
                    .arguments
                    .artifacts
                    .iter()
                    .find(|given| given.name == artifact);
                let Some(given) = given else {
                    call.inputs.push(input);
                    continue;
                };
                let from = single_tag(&given.from)
                    .ok_or_else(|| format!("argument {artifact} is no File of one tag"))?;
                let tags = read_tags(&text_node(&format!("{{{{{from}}}}}")))?;
                let Some(tag) = tags.into_iter().next().filter(|_| from != "item") else {
                    return Err(format!(
                        "argument {name} reads {from}, which wir has no File for"
                    ));
                };
                let source = source(scope, &tag);
                if source
                    .as_ref()
                    .is_some_and(|(_, r#type)| matches!(r#type, Type::Array(_)))
                {
                    return Err(format!(
                        "argument {name} reads the Files of a loop, which wir does not list"
                    ));
                }
                if source.is_some() && available.iter().any(|(known, _)| *known == tag.name()) {
                    edges.push(edge(end(None, &tag.name()), end(Some(&main), name)));
                }
            }
        }
        call.inputs.push(input);
    }
    call.outputs = gives.to_vec();
    let mut ops = vec![merge, flatten];
    ops.append(&mut computed);
    ops.push(call);
    Ok(Piece {
        ops,
        edges,
        main,
        outputs: gives.to_vec(),
    })
}

/// The value that an argument of one plain tag takes as it is: a text item of a loop, or a
/// value of the piece that is no list.
fn direct(
    value: &str,
    available: &[(String, Type)],
    scope: &Scope,
    strings: Option<bool>,
) -> Option<String> {
    let from = single_tag(value)?;
    if from == "item" {
        return (strings == Some(true)).then(|| from.to_owned());
    }
    let tags = read_tags(&text_node(&format!("{{{{{from}}}}}"))).ok()?;
    let tag = tags.into_iter().next().filter(|tag| tag.name() == from)?;
    let (_, r#type) = source(scope, &tag)?;
    let listed = matches!(r#type, Type::Array(_));
    let known = available.iter().any(|(name, _)| *name == from);
    (known && !listed).then(|| from.to_owned())
}

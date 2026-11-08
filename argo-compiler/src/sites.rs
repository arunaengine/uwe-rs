//! Compiles one step or DAG task: a dialect task that the Argo runtime expands as its native run
//! does, or a call of the graph of a steps or DAG template, and a map for a loop.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::{ContinueOn, DagTask, Sequence, Template, TemplateRef, WorkflowStep};
use argo_parser::{Node, decode};
use argo_runtime::adapter::Form;
use serde_json::{Value as Json, json};
use wir::{Binding, Edge, Endpoint, Graph, Kind, Op, Port, PortMap, TaskBuilder, TaskRef, Type};

use crate::graphs::{Built, Compiler, Place, Scope, Target, kept};
use crate::tags::{Tag, has_tag, read_tags};
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
            continue_on: task.continue_on.unwrap_or_default(),
            hooks: !task.hooks.is_empty() || !task.on_exit.is_empty(),
        })
    }
}

/// The items of a loop and the labels that they give the node names.
type Rounds = (Vec<Json>, Vec<Json>);

/// A value that the op of a site reads: its port, where it comes from in the graph of the
/// site, and its native selector when a dialect task reads it.
struct Need {
    port: Port,
    from: Endpoint,
    native: String,
}

/// The ops of a site before they go into the graph of the site or into the body of a map;
/// `None` endpoints name the needs, which are graph inputs in a body.
struct Piece {
    ops: Vec<Op>,
    edges: Vec<Edge>,
    main: String,
    outputs: Vec<Port>,
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
        let piece = match target {
            Target::Leaf(template) => dialect(site, holding, &template, &needs, looping),
            Target::Graph(..) if !site.when.is_empty() => {
                return fail(
                    "a when of a call of a steps or DAG template needs an Argo expression op",
                );
            }
            Target::Graph(..) => {
                return fail("calls of steps and DAG templates do not compile yet");
            }
        };
        let Some((items, labels)) = rounds else {
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
        let calls_graph = piece.ops.iter().any(|op| matches!(op.kind, Kind::Call(_)));
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
        map.inputs.push(crate::with_default(
            port("item", array(Type::Dynamic)),
            Json::Array(items),
        ));
        if calls_graph {
            map.inputs.push(crate::with_default(
                port("label", array(text())),
                Json::Array(labels),
            ));
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

    /// The items and node name labels of a loop whose list is known before the run.
    fn rounds(&mut self, site: &Site, at: &str) -> Result<Option<Rounds>, Kept> {
        if !site.with_param.is_empty() {
            return Err(kept(
                at,
                "withParam needs an Argo expression op to read its list",
            ));
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
            return Err(kept(at, "loop items with tags need an Argo expression op"));
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
        Ok(Some((items, labels)))
    }
}

/// Where the value of a tag comes from in a graph, and its type.
fn source(scope: &Scope, tag: &Tag) -> Option<(Endpoint, Type)> {
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

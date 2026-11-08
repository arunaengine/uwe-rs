//! Compiles the entry graph of a workflow, which takes the workflow parameters and calls the
//! entrypoint, and the entry graph that runs the whole workflow as one native op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::LifecycleHook;
use wir::{Graph, Kind, Language, Native, Type};

use crate::graphs::{Compiler, Place, Scope, Target, kept};
use crate::sites::{Site, after_end};
use crate::texts::text_node;
use crate::{ENTRY, Kept, SOURCE, artifact, edge, end, op, port, text, typed};

impl Compiler {
    /// The graphs of the workflow, the entry graph first: it takes the workflow parameters,
    /// calls the entrypoint and gives its outputs by their names.
    pub fn workflow(&mut self) -> Result<Vec<Graph>, Kept> {
        let spec = self.adapter.spec().clone();
        let at = |reason: &str| Err(kept(ENTRY, reason));
        if spec.hooks.keys().any(|name| name != "exit") {
            return at("lifecycle hooks of the workflow hold on status expressions");
        }
        let exit = (spec.hooks.get("exit").cloned()).or_else(|| {
            let template = spec.on_exit.clone();
            (!template.is_empty()).then(|| LifecycleHook {
                template,
                ..LifecycleHook::default()
            })
        });
        if exit.is_some() && self.reads_status {
            return at("the exit handler reads workflow.status, which needs status as a value");
        }
        if exit
            .as_ref()
            .is_some_and(|hook| !hook.expression.is_empty())
        {
            return at("an exit handler with an expression");
        }
        if spec.retry_strategy.is_some() {
            return at("a retryStrategy of the workflow retries steps and DAG templates too");
        }
        if spec.synchronization.is_some() || spec.suspend.is_some() {
            return at("synchronization and suspend hold the whole workflow");
        }
        if spec.parallelism.is_some() {
            return at("the parallelism of the workflow limits all its pods together");
        }
        if !spec.arguments.artifacts.is_empty() {
            return at("artifacts of the workflow arguments");
        }
        let entry = Place {
            name: self.adapter.entrypoint().to_owned(),
            reference: None,
        };
        let mut inputs = Vec::new();
        for parameter in &spec.arguments.parameters {
            let mut given = port(parameter.name.clone(), text());
            given.default = parameter.value.as_ref().map(|value| value.0.clone().into());
            inputs.push(given);
        }
        let mut graph = Graph {
            name: ENTRY.into(),
            inputs,
            outputs: Vec::new(),
            ops: Vec::new(),
            edges: Vec::new(),
        };
        let main = "main";
        let ports = match self.target(&entry, ENTRY)? {
            Target::Leaf(template) => crate::sites::entry_task(&mut graph, main, &entry, &template),
            Target::Recursion => return at("the entrypoint calls itself"),
            Target::Graph(called, takes, gives) => {
                let carry = self.loops.get(&called).map(|(_, carry)| carry.clone());
                let mut call = op(main, crate::recursion::call_kind(&called, carry));
                for input in takes {
                    let name = input.name.clone();
                    if name == "node.path" {
                        call.inputs
                            .push(crate::with_default(input, serde_json::json!([])));
                        continue;
                    }
                    let given = name.strip_prefix("inputs.parameters.");
                    if let Some(given) = given.filter(|n| graph.inputs.iter().any(|p| p.name == *n))
                    {
                        graph
                            .edges
                            .push(edge(end(None, given), end(Some(main), &name)));
                    } else if input.default.is_none() && name.starts_with("inputs.artifacts.") {
                        return at("the entrypoint takes artifacts");
                    }
                    call.inputs.push(input);
                }
                call.outputs = gives.clone();
                graph.ops.push(call);
                gives
            }
        };
        if let Some(hook) = exit {
            self.exit_handler(&mut graph, &entry, main, hook)?;
        }
        for output in ports {
            let name = (output.name.strip_prefix("outputs.parameters."))
                .or_else(|| output.name.strip_prefix("outputs.artifacts."));
            let Some(name) = name else { continue };
            if graph.outputs.iter().any(|known| known.name == name) {
                continue;
            }
            graph.outputs.push(port(name, output.r#type.clone()));
            graph
                .edges
                .push(edge(end(Some(main), &output.name), end(None, name)));
        }
        let mut graphs = vec![graph];
        graphs.append(&mut self.graphs);
        Ok(graphs)
    }

    /// The exit handler of the workflow, which runs once `main` ended. It starts from an empty
    /// node path, as the entry graph has none.
    fn exit_handler(
        &mut self,
        graph: &mut Graph,
        entry: &Place,
        main: &str,
        hook: LifecycleHook,
    ) -> Result<(), Kept> {
        let site = Site::of_hook(".onExit".into(), "onExit".into(), hook, text_node(""));
        let scope = Scope {
            inputs: graph.inputs.clone(),
            made: BTreeMap::new(),
        };
        let built = self.site(entry, graph, &scope, &site)?;
        after_end(graph, &built.op, main);
        let (empty, kept): (Vec<_>, Vec<_>) = (std::mem::take(&mut graph.edges).into_iter())
            .partition(|wire| wire.from.op.is_none() && wire.from.port == "node.path");
        graph.edges = kept;
        for wire in empty {
            let op = (graph.ops.iter_mut()).find(|op| Some(&op.path) == wire.to.op.as_ref());
            let input = op.and_then(|op| op.inputs.iter_mut().find(|p| p.name == wire.to.port));
            if let Some(input) = input {
                input.default = Some(serde_json::json!([]));
            }
        }
        Ok(())
    }

    /// The entry graph that runs the whole workflow as one native Argo op.
    pub fn native(&mut self) -> Result<Graph, String> {
        let spec = self.adapter.spec().clone();
        let entrypoint = self.adapter.entrypoint().to_owned();
        let template = (self.adapter.template(&entrypoint, None)).and_then(|node| typed(&node))?;
        let mut whole = op(
            ENTRY,
            Kind::Native(Native {
                source: SOURCE.into(),
                language: Language::Argo,
                entry: Some(entrypoint.clone()).filter(|name| *name != spec.entrypoint),
                profiles: Vec::new(),
                configs: Vec::new(),
            }),
        );
        let mut graph = Graph {
            name: ENTRY.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            ops: Vec::new(),
            edges: Vec::new(),
        };
        for parameter in &spec.arguments.parameters {
            let mut given = port(parameter.name.clone(), text());
            given.default = parameter.value.as_ref().map(|value| value.0.clone().into());
            graph.inputs.push(given.clone());
            given.default = None;
            whole.inputs.push(given);
            let name = &parameter.name;
            graph
                .edges
                .push(edge(end(None, name), end(Some(ENTRY), name)));
        }
        let parameters = (template.outputs.parameters.iter()).map(|output| (&output.name, text()));
        let artifacts =
            (template.outputs.artifacts.iter()).map(|output| (&output.name, artifact()));
        for (name, r#type) in parameters.chain(artifacts) {
            if whole.outputs.iter().any(|known| &known.name == name) {
                continue;
            }
            let r#type = match r#type {
                Type::Optional(_) => r#type,
                other => Type::Optional(Box::new(other)),
            };
            whole.outputs.push(port(name.clone(), r#type.clone()));
            graph.outputs.push(port(name.clone(), r#type));
            graph
                .edges
                .push(edge(end(Some(ENTRY), name), end(None, name)));
        }
        graph.ops.push(whole);
        Ok(graph)
    }
}

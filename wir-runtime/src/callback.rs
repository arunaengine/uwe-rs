//! Calls the callbacks of ops: a graph body runs in the engine, and a closure runs in the
//! evaluator of its language with its captures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Map, Value as Json};
use wir::{Body, Closure, ExprLanguage};

use crate::engine::{self, Context};
use crate::groovy;
use crate::state::Progress;

/// The callbacks of one op, which keeps the Groovy code it lowered between calls.
#[derive(Default)]
pub struct Callbacks {
    lowered: HashMap<String, Rc<groovy::Compiled>>,
}

impl Callbacks {
    /// The result of `body` for the positional `arguments` named `names`, with the value
    /// inputs of its op, which a graph body takes by name.
    pub fn call(
        &mut self,
        context: &Context<'_>,
        body: &Body,
        (names, arguments): (&[&str], &[Json]),
        values: &Map<String, Json>,
    ) -> Result<Json, String> {
        match body {
            Body::Graph(name) => {
                let mut inputs = values.clone();
                for (name, argument) in names.iter().zip(arguments) {
                    inputs.insert((*name).to_owned(), argument.clone());
                }
                let mut outputs = run_graph(context, name, inputs)?;
                outputs
                    .remove("out")
                    .ok_or_else(|| format!("callback {name} gave no output out"))
            }
            Body::Closure(closure) => self.closure(context, closure, arguments, values),
        }
    }

    fn closure(
        &mut self,
        context: &Context<'_>,
        closure: &Closure,
        arguments: &[Json],
        values: &Map<String, Json>,
    ) -> Result<Json, String> {
        let captures = engine::captured(&closure.captures, values);
        match closure.language {
            ExprLanguage::Groovy => {
                let key = serde_json::to_string(closure).unwrap_or_default();
                let compiled = match self.lowered.get(&key) {
                    Some(compiled) => compiled.clone(),
                    None => {
                        let scope = groovy::scope(context, closure.scope.as_ref());
                        let names = captures.keys().cloned().collect();
                        let compiled =
                            groovy::compile(&closure.code, &closure.library, scope, names)?;
                        let compiled = Rc::new(compiled);
                        self.lowered.insert(key, compiled.clone());
                        compiled
                    }
                };
                compiled.call(&captures, arguments, context.base)
            }
            ExprLanguage::Python => {
                let code = crate::python::Code {
                    code: &closure.code,
                    library: &closure.library,
                };
                code.call(&captures, arguments, context.base)
            }
            ExprLanguage::Javascript => {
                let code = (closure.code.as_str(), closure.library.as_slice());
                crate::javascript::call(code, &captures, arguments, context.javascript)
            }
            ExprLanguage::Wdl => {
                crate::wdl_closure::call(&closure.code, &closure.parameters, &captures, arguments)
            }
            ExprLanguage::Js => Err("js is no closure language; use javascript".into()),
            ExprLanguage::Argo => Err("argo is no closure language".into()),
        }
    }
}

/// The outputs of a callback graph for its inputs, which run in the engine.
pub fn run_graph(
    context: &Context<'_>,
    name: &str,
    mut inputs: Map<String, Json>,
) -> Result<Map<String, Json>, String> {
    let document = context.document;
    let graph = (document.graphs.iter())
        .find(|graph| graph.name == name)
        .ok_or_else(|| format!("no graph {name}"))?;
    for port in &graph.inputs {
        if let Some(default) = &port.default
            && inputs.get(&port.name).is_none_or(Json::is_null)
        {
            inputs.insert(port.name.clone(), default.clone());
        }
    }
    let plan = crate::plan::graph_plan(document, name).map_err(|error| error.message)?;
    let mut progress = Progress::new(plan, document.types.clone());
    progress.inputs = inputs;
    progress.activate(None).map_err(|error| error.message)?;
    while let Some((at, bound)) = progress.next(false) {
        if !engine::begin(&mut progress, at, &bound, context).map_err(|error| error.message)? {
            return Err(format!(
                "op {} of callback {name} does not run in the engine",
                progress.node(at).path
            ));
        }
    }
    if let Some(first) = progress.failures.first() {
        return Err(first.clone());
    }
    progress.outputs(&graph.outputs)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    #[test]
    fn graph_callbacks() {
        let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
        let port = |name: &str| json!({"name": name, "type": "int"});
        let add = json!({"path": "add", "kind": {"expr": {"language": "groovy", "code": "inputs.item + inputs.step"}},
            "inputs": [port("item"), port("step")], "outputs": [port("out")]});
        let graph = json!({"name": "next", "inputs": [port("item"), {"name": "step", "type": "int", "default": 1}],
            "outputs": [port("out")], "ops": [add],
            "edges": [edge((None, "item"), (Some("add"), "item")), edge((None, "step"), (Some("add"), "step")),
                edge((Some("add"), "out"), (None, "out"))]});
        let document = json!({"wir_version": "0.1", "graphs": [graph], "entry": "next"});
        let document = wir::parse(&document.to_string()).unwrap();
        let settings = cwl_runtime::JavaScriptSettings::default();
        let scopes = HashMap::new();
        let context = Context {
            document: &document,
            base: Path::new("/"),
            javascript: &settings,
            scopes: &scopes,
            files: Path::new("/nonexistent"),
            printer: &crate::driver::Printer::default(),
            simulated: false,
            provider: None,
            deliveries: &Default::default(),
        };
        let mut callbacks = Callbacks::default();
        let body = Body::Graph("next".into());
        let values = Map::from_iter([("step".into(), json!(10))]);
        assert_eq!(
            callbacks
                .call(&context, &body, (&["item"], &[json!(1)]), &values)
                .unwrap(),
            json!(11),
            "a capture by name"
        );
        assert_eq!(
            callbacks
                .call(&context, &body, (&["item"], &[json!(1)]), &Map::new())
                .unwrap(),
            json!(2),
            "a default"
        );
        let closure: Body =
            serde_json::from_value(json!({"language": "groovy", "code": "{ it + step }",
            "captures": {"step": {"input": "step"}}}))
            .unwrap();
        assert_eq!(
            callbacks
                .call(&context, &closure, (&["item"], &[json!(1)]), &values)
                .unwrap(),
            json!(11)
        );
        let wrong = callbacks.call(&context, &body, (&["item"], &[json!("x")]), &values);
        assert!(
            wrong.is_err(),
            "a callback that fails is an error: {wrong:?}"
        );
    }
}

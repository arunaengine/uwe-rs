//! Lowers a WDL task into a graph: an expr op that builds its command, a task op that runs
//! the command, and an expr op per output.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wdl_ast::v1::{NameRefExpr, TaskDefinition};
use wdl_ast::{AstNode, AstToken};
use wdl_runtime::document::{Callable, Unit};
use wdl_runtime::expression::names;
use wir::{Argument, Expr as Code, Graph, Kind, Op, Type};

use crate::library::{Line, check, ordered, ports};
use crate::outputs::{Output, outputs};
use crate::types::{has_files, port};
use crate::{DEFAULT_IMAGE, Lowerer, blank, edge, end, origin, reads, text};

/// Runs the command as uwe's WDL driver does, with its streams in `.wdl/stdout` and
/// `.wdl/stderr`.
const RUNNER: &str = r#"mkdir -p .wdl
printf '%s' "$1" > .wdl/command.sh
bash .wdl/command.sh > .wdl/stdout 2> .wdl/stderr
status=$?
cat .wdl/stdout
cat .wdl/stderr >&2
exit $status
"#;

/// The input of the task op with the command.
const COMMAND: &str = "command";

impl Lowerer {
    /// The op at `path` that runs a call of a task: a call of its lowered graph, else a native
    /// op.
    pub(crate) fn task(&mut self, unit: &Unit, path: &str) -> Result<Op, String> {
        let graph = match self.known(unit) {
            Some(graph) => graph,
            None => {
                let graph = self.task_graph(unit);
                self.remember(unit, graph.clone());
                graph
            }
        };
        let source = self.source(&unit.document);
        match graph {
            Ok(name) => Ok(self.call(&name, path, origin(&source, &unit.name()))),
            Err(reason) => self.native(unit, path, reason),
        }
    }

    /// The graph of a task, or the first thing in it that a task op cannot express.
    fn task_graph(&mut self, unit: &Unit) -> Result<String, String> {
        let Callable::Task(definition) = &unit.callable else {
            return Err("not a task".into());
        };
        let source = self.source(&unit.document);
        let at = origin(&source, &unit.name());
        let (inputs, mut library) = ports(unit, definition)?;
        for decl in definition.declarations() {
            if decl.env().is_some() {
                return Err("an env declaration".into());
            }
            check(&decl.expr())?;
            library.push(Line {
                name: decl.name().text().to_owned(),
                uses: names(&decl.expr()),
                code: text(&decl),
            });
        }
        let library = ordered(library)?;
        let command = match definition.command() {
            Some(section) => {
                if let Some(function) = reads(&section) {
                    return Err(format!("{function} in the command"));
                }
                if section
                    .descendants::<NameRefExpr>()
                    .any(|n| n.name().text() == "task")
                {
                    return Err("the task variable".into());
                }
                text(&section)
            }
            None => "command <<<\n>>>".into(),
        };
        let mut task = runtime(definition)?;
        let outputs = outputs(unit, definition)?;
        let name = self.graph_name(&format!("task.{}", unit.name()));
        let path = unit.name();
        let mut graph = Graph {
            name: name.clone(),
            inputs: inputs.clone(),
            outputs: outputs
                .iter()
                .map(|item| port(&item.name, item.r#type.clone()))
                .collect(),
            ops: Vec::new(),
            edges: Vec::new(),
        };
        let expr = |path: &str, code: String, library: Vec<String>, output: Type| {
            let kind = Kind::Expr(Code {
                language: "wdl".into(),
                code,
                library,
            });
            let mut op = blank(path, kind, at.clone());
            op.inputs = inputs.clone();
            op.outputs = vec![port("value", output)];
            op
        };
        graph
            .ops
            .push(expr(COMMAND, command, library.clone(), Type::String));
        task.task.command.push(Argument::Input {
            input: COMMAND.into(),
        });
        let mut ports = vec![port(COMMAND, Type::String)];
        graph
            .edges
            .push(edge(end(Some(COMMAND), "value"), end(Some(&path), COMMAND)));
        for input in inputs.iter().filter(|input| has_files(&input.r#type)) {
            task.task.command.push(Argument::Input {
                input: input.name.clone(),
            });
            ports.push(port(&input.name, input.r#type.clone()));
        }
        let mut run = blank(&path, Kind::Task(task.task), at.clone());
        run.attributes.resources = task.resources;
        run.inputs = ports;
        for output in &outputs {
            let at = format!("output.{}", output.name);
            let mut op = expr(
                &at,
                output.code.clone(),
                library.clone(),
                output.r#type.clone(),
            );
            for used in &output.uses {
                let Some(other) = outputs.iter().find(|item| item.name == *used) else {
                    continue;
                };
                op.inputs.push(port(used, other.r#type.clone()));
                graph
                    .edges
                    .push(edge(produced(other), end(Some(&at), used)));
            }
            graph.ops.push(op);
        }
        for op in graph.ops.iter().chain([&run]) {
            for input in inputs
                .iter()
                .filter(|input| op.inputs.iter().any(|p| p.name == input.name))
            {
                graph.edges.push(edge(
                    end(None, &input.name),
                    end(Some(&op.path), &input.name),
                ));
            }
        }
        for output in &outputs {
            graph
                .edges
                .push(edge(produced(output), end(None, &output.name)));
        }
        graph.ops.push(run);
        self.graphs.push(graph);
        Ok(name)
    }
}

/// Where an output of the task graph comes from: its own expr op.
fn produced(output: &Output) -> wir::Endpoint {
    end(Some(&format!("output.{}", output.name)), "value")
}

/// The task op without its inputs, and the resources it asks for.
struct Built {
    task: wir::Task,
    resources: Option<wir::Resources>,
}

/// The task op without its inputs; a `runtime` or `requirements` section has no lowering yet.
fn runtime(definition: &TaskDefinition) -> Result<Built, String> {
    if definition.runtime().is_some() || definition.requirements().is_some() {
        return Err("a runtime section".into());
    }
    let task = wir::Task {
        image: Some(DEFAULT_IMAGE.to_owned()),
        command: vec!["bash".into(), "-c".into(), RUNNER.into(), "wdl".into()]
            .into_iter()
            .map(Argument::Text)
            .collect(),
        captures: Vec::new(),
        success_codes: Vec::new(),
        // WDL tasks may use the network, as uwe's WDL driver lets them.
        network: true,
        stdout: None,
        stderr: None,
    };
    Ok(Built {
        task,
        resources: None,
    })
}

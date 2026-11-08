//! Lowers a WDL task into a graph: expr ops that build its command and the paths of its
//! outputs, a task op that runs the command, and an expr op per output that is a value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wdl_ast::v1::{Expr, NameRefExpr, TaskDefinition};
use wdl_ast::{AstNode, AstToken};
use wdl_runtime::Value;
use wdl_runtime::document::{Callable, Unit};
use wdl_runtime::expression::names;
use wir::{Argument, Capture, CaptureRule, Expr as Code, Graph, Kind, Op, Type};

use crate::library::{Inputs, Line, check, ordered, ports};
use crate::outputs::{Output, Read, listing, outputs};
use crate::types::{has_files, port};
use crate::{DEFAULT_IMAGE, Lowerer, READS, blank, called, constant, edge, end, origin, text};

/// Runs the command as uwe's WDL driver does, with its streams in `.wdl/stdout` and
/// `.wdl/stderr`, then copies each `<output>\t<path>` line of `$2` below `.wdl/out/<output>/`.
pub(crate) const RUNNER: &str = r#"mkdir -p .wdl
printf '%s' "$1" > .wdl/command.sh
bash .wdl/command.sh > .wdl/stdout 2> .wdl/stderr
status=$?
cat .wdl/stdout
cat .wdl/stderr >&2
tab=$(printf '\t')
line=0
printf '%s\n' "$2" | while IFS="$tab" read -r name path; do
  line=$((line + 1))
  path=${path%/}
  if [ -n "$name" ] && [ -e "$path" ]; then
    folder=.wdl/out/$name/$(printf '%06d' "$line")
    mkdir -p "$folder" && cp -R "$path" "$folder/"
  fi
done
exit $status
"#;

/// The input of the task op with the command, and the one with the output paths.
const COMMAND: &str = "command";
const PATHS: &str = "command.outputs";

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
        let Inputs {
            ports: inputs,
            mut library,
            fallbacks,
        } = ports(unit, definition)?;
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
                if let Some(function) = called(&section, &READS) {
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
        let held = fallbacks
            .into_iter()
            .map(|(port, value)| (name.clone(), port, value));
        self.fallbacks.extend(held);
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
        let placed: Vec<&Output> = (outputs.iter())
            .filter(|item| matches!(item.read, Read::Placed))
            .collect();
        task.task.command.push(Argument::Input {
            input: COMMAND.into(),
        });
        let mut ports = vec![port(COMMAND, Type::String)];
        if placed.is_empty() {
            task.task.command.push(Argument::Text(String::new()));
        } else {
            let mut lines = library.clone();
            lines.extend(
                outputs
                    .iter()
                    .filter(|item| !matches!(item.read, Read::Glob(_)))
                    .map(|item| item.decl.clone()),
            );
            graph
                .ops
                .push(expr(PATHS, listing(&placed), lines, Type::String));
            task.task.command.push(Argument::Input {
                input: PATHS.into(),
            });
            ports.push(port(PATHS, Type::String));
            graph
                .edges
                .push(edge(end(Some(PATHS), "value"), end(Some(&path), PATHS)));
        }
        graph
            .edges
            .push(edge(end(Some(COMMAND), "value"), end(Some(&path), COMMAND)));
        for input in inputs.iter().filter(|input| has_files(&input.r#type)) {
            task.task.command.push(Argument::Input {
                input: input.name.clone(),
            });
            ports.push(port(&input.name, input.r#type.clone()));
        }
        let mut captured = Vec::new();
        for output in &outputs {
            let pattern = match &output.read {
                Read::Glob(pattern) => pattern.clone(),
                Read::Placed => format!(".wdl/out/{}/*/*", output.name),
                Read::Value => continue,
            };
            task.task.captures.push(Capture {
                output: output.name.clone(),
                from: CaptureRule::Glob(pattern),
            });
            captured.push(port(&output.name, output.r#type.clone()));
        }
        let mut run = blank(&path, Kind::Task(task.task), at.clone());
        run.attributes.resources = task.resources;
        (run.inputs, run.outputs) = (ports, captured);
        for output in outputs
            .iter()
            .filter(|item| matches!(item.read, Read::Value))
        {
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
                    .push(edge(produced(&path, other), end(Some(&at), used)));
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
                .push(edge(produced(&path, output), end(None, &output.name)));
        }
        graph.ops.push(run);
        self.graphs.push(graph);
        Ok(name)
    }
}

/// Where an output of the task graph comes from: the task op, or its own expr op.
fn produced(task: &str, output: &Output) -> wir::Endpoint {
    match output.read {
        Read::Value => end(Some(&format!("output.{}", output.name)), "value"),
        _ => end(Some(task), &output.name),
    }
}

/// The task op without its inputs, and the resources it asks for.
struct Built {
    task: wir::Task,
    resources: Option<wir::Resources>,
}

/// The task op of the `runtime` or `requirements` section, whose values must not depend on
/// inputs, without its inputs and captures.
fn runtime(definition: &TaskDefinition) -> Result<Built, String> {
    let items: Vec<(String, Expr)> = match (definition.requirements(), definition.runtime()) {
        (Some(section), _) => (section.items())
            .map(|item| (item.name().text().to_owned(), item.expr()))
            .collect(),
        (None, Some(section)) => (section.items())
            .map(|item| (item.name().text().to_owned(), item.expr()))
            .collect(),
        (None, None) => Vec::new(),
    };
    let mut image = DEFAULT_IMAGE.to_owned();
    let mut resources = wir::Resources::default();
    let mut codes = Vec::new();
    for (key, expr) in items {
        let used = [
            "container",
            "docker",
            "cpu",
            "memory",
            "gpu",
            "return_codes",
            "returnCodes",
        ];
        if !used.contains(&key.as_str()) {
            continue;
        }
        let value =
            constant(&expr).ok_or_else(|| format!("the runtime value {key} depends on inputs"))?;
        match (key.as_str(), value) {
            ("container" | "docker", Value::String(name)) if name != "*" => image = name,
            ("container" | "docker", Value::Array(names)) => {
                if let Some(name) = names
                    .first()
                    .and_then(Value::text)
                    .filter(|name| name != "*")
                {
                    image = name;
                }
            }
            ("cpu", Value::Int(count)) => resources.cpus = u32::try_from(count).ok(),
            ("cpu", Value::Float(count)) => resources.cpus = Some(count.ceil() as u32),
            ("memory", value) => resources.memory = wdl_runtime::library::memory_bytes(&value),
            ("gpu", Value::Boolean(true)) => resources.gpus = Some(1),
            ("return_codes" | "returnCodes", Value::Int(code)) => codes = vec![code],
            ("return_codes" | "returnCodes", Value::Array(items)) => {
                for item in items {
                    let Value::Int(code) = item else {
                        return Err("return codes that are not integers".into());
                    };
                    codes.push(code);
                }
            }
            ("return_codes" | "returnCodes", _) => return Err("return codes `*`".into()),
            _ => {}
        }
    }
    let success_codes = (codes.into_iter())
        .map(|code| i32::try_from(code).map_err(|_| format!("the return code {code}")))
        .collect::<Result<_, _>>()?;
    let task = wir::Task {
        image: Some(image),
        command: vec!["bash".into(), "-c".into(), RUNNER.into(), "wdl".into()]
            .into_iter()
            .map(Argument::Text)
            .collect(),
        captures: Vec::new(),
        success_codes,
        // WDL tasks may use the network, as uwe's WDL driver lets them.
        network: true,
        stdout: None,
        stderr: None,
    };
    let resources = (resources != wir::Resources::default()).then_some(resources);
    Ok(Built { task, resources })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::RUNNER;

    #[test]
    fn runner_places_outputs() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let work = folder.path();
        let command =
            "mkdir -p d && echo x > d/a.txt && echo y > 'b c.txt' && echo shown && exit 3";
        let paths = "first\td/a.txt\nlist\tb c.txt\nlist\tmissing.txt\nlist\td/\n";
        let status = Command::new("bash")
            .args(["-c", RUNNER, "wdl", command, paths])
            .current_dir(work)
            .status()
            .expect("bash runs");
        assert_eq!(status.code(), Some(3), "the exit code of the command");
        let read = |path: &str| std::fs::read_to_string(work.join(path)).ok();
        assert_eq!(read(".wdl/stdout").as_deref(), Some("shown\n"));
        assert_eq!(read(".wdl/out/first/000001/a.txt").as_deref(), Some("x\n"));
        assert_eq!(read(".wdl/out/list/000002/b c.txt").as_deref(), Some("y\n"));
        assert!(
            !work.join(".wdl/out/list/000003").exists(),
            "a missing path is left out"
        );
        assert!(
            work.join(".wdl/out/list/000004/d/a.txt").is_file(),
            "a folder is copied"
        );
    }
}

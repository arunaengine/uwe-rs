//! Lowers a WDL task into a graph: expr ops that build its command and the paths of its
//! outputs, a task op that runs the command, and an expr op per output that is a value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use wdl_ast::v1::{Decl, Expr, NameRefExpr, TaskDefinition};
use wdl_ast::{AstNode, AstToken};
use wdl_runtime::Value;
use wdl_runtime::document::{Callable, Unit, declared};
use wdl_runtime::expression::names;
use wir::{Argument, Capture, CaptureRule, Expr as Code, Graph, Kind, Op, Type};

use crate::library::{Inputs, Line, check, ordered, ports, staged};
use crate::outputs::{Output, Read, listing, outputs, patterns};
use crate::types::{convert, has_files, port};
use crate::{Lowerer, READS, blank, called, constant, edge, end, origin, text, unclaimed};

/// Runs the command as uwe's WDL driver does, then copies below `.wdl/out/<output>/` the files
/// that match each `<output>\t<regex>` line of `$4`, then each `<output>\t<path>` line of `$2`.
/// `$3` is a failure code, then the success codes; a missing path or a failed copy turns a
/// success into the failure code, unless the output starts with `?` as optional ones do.
pub(crate) const RUNNER: &str = r#"mkdir -p .wdl
printf '%s' "$1" > .wdl/command.sh
bash .wdl/command.sh > .wdl/stdout 2> .wdl/stderr
status=$?
cat .wdl/stdout
cat .wdl/stderr >&2
tab=$(printf '\t')
fail() { case " $1 " in *" $status "*) status=${1%% *};; esac; }
place() {
  folder=.wdl/out/$1/$(printf '%06d' "$2")
  mkdir -p "$folder" && cp -R "$3" "$folder/"
}
printf '%s\n' "$4" | while IFS="$tab" read -r name pattern; do
  [ -n "$name" ] || continue
  find . ! -type d ! -path './.wdl/out/*' | cut -c3- | LC_ALL=C sort | grep -E -x -e "$pattern" |
    { count=0; while IFS= read -r path; do
      count=$((count + 1))
      place "$name" "$count" "$path" || exit 1
    done; } || exit 1
done || fail "$3"
line=0
printf '%s\n' "$2" | while IFS="$tab" read -r name path; do
  line=$((line + 1))
  path=${path%/}
  required=true
  case $name in '?'*) name=${name#?}; required=false;; esac
  [ -n "$name" ] || continue
  if [ ! -e "$path" ]; then
    $required || continue
    echo "the task did not make $path" >&2
    exit 1
  fi
  place "$name" "$line" "$path" || exit 1
done || fail "$3"
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
        } = ports(unit, definition)?;
        let defaulted: BTreeSet<String> = library.iter().map(|line| line.name.clone()).collect();
        let mut types: BTreeMap<String, Type> = (inputs.iter())
            .map(|port| (port.name.clone(), port.r#type.clone()))
            .collect();
        for decl in definition.declarations() {
            let name = decl.name().text().to_owned();
            unclaimed(&name)?;
            if decl.env().is_some() {
                return Err("an env declaration".into());
            }
            check(&decl.expr())?;
            let ty = declared(&unit.document, &decl.ty())?;
            let r#type = convert(&ty).map_err(|why| format!("{name}: {why}"))?;
            types.insert(name.clone(), r#type);
            library.push(Line {
                name,
                uses: names(&decl.expr()),
                code: text(&decl),
            });
        }
        let given: Vec<Decl> = (definition.input())
            .map(|section| section.declarations().collect())
            .unwrap_or_default();
        let computed = given.into_iter().filter_map(|decl| match decl {
            Decl::Bound(bound) if defaulted.contains(bound.name().text()) => Some(bound),
            _ => None,
        });
        for decl in computed.chain(definition.declarations()) {
            staged(&decl, &types)?;
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
        let mut task = runtime(definition, &self.image)?;
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
                language: wir::ExprLanguage::Wdl,
                code,
                library,
                scope: None,
                captures: Default::default(),
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
        task.task.command.push(Argument::input(COMMAND));
        let mut ports = vec![port(COMMAND, Type::String)];
        if placed.is_empty() {
            task.task.command.push(Argument::Text(String::new()));
        } else {
            task.task.command.push(Argument::input(PATHS));
            let mut lines = library.clone();
            lines.extend(
                outputs
                    .iter()
                    .filter(|item| !matches!(item.read, Read::Glob(_) | Read::Matched(_)))
                    .map(|item| item.decl.clone()),
            );
            graph
                .ops
                .push(expr(PATHS, listing(&placed), lines, Type::String));
            ports.push(port(PATHS, Type::String));
            graph
                .edges
                .push(edge(end(Some(PATHS), "value"), end(Some(&path), PATHS)));
        }
        let code = failure(&task.task);
        task.task.command.push(Argument::Text(code));
        task.task.command.push(Argument::Text(patterns(&outputs)));
        graph
            .edges
            .push(edge(end(Some(COMMAND), "value"), end(Some(&path), COMMAND)));
        for input in inputs.iter().filter(|input| has_files(&input.r#type)) {
            task.task.command.push(Argument::input(&input.name));
            ports.push(port(&input.name, input.r#type.clone()));
        }
        let mut captured = Vec::new();
        for output in &outputs {
            let pattern = match &output.read {
                Read::Glob(pattern) => pattern.clone(),
                Read::Placed | Read::Matched(_) => format!(".wdl/out/{}/*/*", output.name),
                Read::Value => continue,
            };
            task.task.captures.push(Capture {
                output: output.name.clone(),
                from: CaptureRule::Glob(pattern),
                load_contents: false,
                optional: false,
                arity: None,
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

/// The exit code of a run whose required outputs are missing, one that is not a success,
/// then the success codes.
fn failure(task: &wir::Task) -> String {
    let codes = match task.success_codes.is_empty() {
        true => vec![0],
        false => task.success_codes.clone(),
    };
    let code = (1..).find(|code| !codes.contains(code)).unwrap_or(1);
    let codes = codes.iter().map(|code| format!(" {code}"));
    format!("{code}{}", codes.collect::<String>())
}

/// The task op of the `runtime` or `requirements` section, whose values must not depend on
/// inputs, without its inputs and captures.
fn runtime(definition: &TaskDefinition, image: &str) -> Result<Built, String> {
    let items: Vec<(String, Expr)> = match (definition.requirements(), definition.runtime()) {
        (Some(section), _) => (section.items())
            .map(|item| (item.name().text().to_owned(), item.expr()))
            .collect(),
        (None, Some(section)) => (section.items())
            .map(|item| (item.name().text().to_owned(), item.expr()))
            .collect(),
        (None, None) => Vec::new(),
    };
    let mut image = image.to_owned();
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
        ..wir::Task::default()
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
        let paths = "first\td/a.txt\nlist\tb c.txt\n?list\tmissing.txt\nlist\td/\n";
        let status = Command::new("bash")
            .args(["-c", RUNNER, "wdl", command, paths, "1 0", ""])
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
            "a missing optional path is left out"
        );
        assert!(
            work.join(".wdl/out/list/000004/d/a.txt").is_file(),
            "a folder is copied"
        );
    }

    #[test]
    fn missing_files_fail() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let run = |command: &str| {
            let paths = "files\ta.txt\nfiles\tb.txt\n";
            let status = Command::new("bash")
                .args(["-c", RUNNER, "wdl", command, paths, "7 0 3", ""])
                .current_dir(folder.path())
                .status()
                .expect("bash runs");
            status.code()
        };
        assert_eq!(run("touch a.txt"), Some(7), "a required file is missing");
        assert_eq!(
            run("touch a.txt; exit 3"),
            Some(7),
            "after an accepted code"
        );
        assert_eq!(run("exit 5"), Some(5), "the code of a failed command");
        assert_eq!(run("touch a.txt b.txt; exit 3"), Some(3));
        assert_eq!(run("touch a.txt b.txt"), Some(0));
    }

    #[test]
    fn globs_match_files() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let work = folder.path();
        let command = "mkdir -p out/sub && touch out/a.txt out/sub/b.txt";
        let globs = "files\tout/[^/]*\nall\t.*\\.txt\n";
        let status = Command::new("bash")
            .args([
                "-c",
                RUNNER,
                "wdl",
                command,
                "copy\tout/a.txt\n",
                "1 0",
                globs,
            ])
            .current_dir(work)
            .status()
            .expect("bash runs");
        assert_eq!(status.code(), Some(0));
        let found = |output: &str| {
            let mut paths = Vec::new();
            for entry in std::fs::read_dir(work.join(".wdl/out").join(output))
                .into_iter()
                .flatten()
            {
                let folder = entry.expect("an entry").path();
                for file in std::fs::read_dir(&folder).expect("a folder") {
                    let name = file.expect("a file").file_name();
                    paths.push(name.to_string_lossy().into_owned());
                }
            }
            paths.sort();
            paths
        };
        assert_eq!(found("files"), ["a.txt"], "a glob matches no folders");
        assert_eq!(
            found("all"),
            ["a.txt", "b.txt"],
            "nor the copies of outputs"
        );
        assert_eq!(found("copy"), ["a.txt"]);
    }
}

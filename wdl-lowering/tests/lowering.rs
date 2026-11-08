//! Lowers small WDL documents, checks the ops they become, and runs each natively and lowered
//! on the simulator with the same tasks and outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wdl_lowering::Lowering;
use wir::Kind;
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// A lowered document and both simulated runs: the number of tasks and the outputs, with
/// Files as their basenames.
struct Runs {
    lowering: Lowering,
    native: (usize, Json),
    lowered: (usize, Json),
}

fn runs(text: &str, inputs: Json) -> Runs {
    documents(&[("main.wdl", text)], inputs)
}

/// The runs of the first of several files, which may import or read the others.
fn documents(files: &[(&str, &str)], inputs: Json) -> Runs {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    for (name, text) in files {
        std::fs::write(root.join(name), text).expect("writes a document");
    }
    let target = root.join(files[0].0);
    let lowering = wdl_lowering::lower(&target, None, root).expect("the document lowers");
    let wir = root.join("main.wir.json");
    let document = serde_json::to_string(&lowering.document).expect("serializes");
    std::fs::write(&wir, document).expect("writes the wir document");
    let job = root.join("inputs.json");
    std::fs::write(&job, inputs.to_string()).expect("writes the inputs");
    let converted = lowering.inputs(&inputs, root).expect("the inputs convert");
    let values = root.join("values.json");
    std::fs::write(&values, converted.to_string()).expect("writes the wir inputs");
    let launch = |target: PathBuf, inputs: &Path| Launch {
        target,
        inputs: Some(inputs.to_owned()),
        scratch: root.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let options = wdl_runtime::Options {
        print: false,
        ..Default::default()
    };
    let mut native =
        wdl_runtime::WdlDriver::new(&launch(target, &job), options).expect("a WDL driver");
    let count = run(&mut native, "native", root);
    let native = (count, shown(native.exported().unwrap_or_default()));
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut wired = WirDriver::new(&launch(wir, &values), settings).expect("a wir driver");
    let count = run(&mut wired, "lowered", root);
    let lowered = (count, shown(wired.outputs().unwrap_or_default()));
    Runs {
        lowering,
        native,
        lowered,
    }
}

/// The number of tasks of a simulated run.
fn run(driver: &mut dyn Driver, name: &str, root: &Path) -> usize {
    let mut simulator = Simulator::new(root.join(format!("simulator-{name}")));
    let execution = execute(name, driver, &mut simulator);
    let graph = execution.into_result().expect("the run succeeds");
    graph.nodes.len()
}

/// Outputs with each File and Directory as its basename, as the folders differ.
fn shown(value: Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.into_iter().map(shown).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("File" | "Directory") => object.get("basename").cloned().unwrap_or_default(),
            _ => Json::Object(object.into_iter().map(|(k, v)| (k, shown(v))).collect()),
        },
        other => other,
    }
}

/// The kinds of the ops of a graph, by path.
fn kinds(lowering: &Lowering, graph: &str) -> Vec<(String, &'static str)> {
    let graph = (lowering.document.graphs.iter()).find(|item| item.name == graph);
    let ops = graph.expect("the graph exists").ops.iter();
    ops.map(|op| {
        let kind = match &op.kind {
            Kind::Task(_) => "task",
            Kind::Call(_) => "call",
            Kind::Native(_) => "native",
            Kind::Expr(_) => "expr",
            Kind::Map(_) => "map",
            Kind::Branch(_) => "branch",
            Kind::Select(_) => "select",
            Kind::Merge(_) => "merge",
            _ => "other",
        };
        (op.path.clone(), kind)
    })
    .collect()
}

fn named(items: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    (items.iter())
        .map(|(path, kind)| (path.to_string(), *kind))
        .collect()
}

#[test]
fn task_outputs_captured() {
    let text = r#"version 1.1
task greet {
  input {
    String name
    Int count = 2
    File? greeting
    Array[File] extra = []
  }
  String file = "~{name}.txt"
  command <<<
    cat ~{greeting} ~{sep=" " extra} > /dev/null
    echo hello ~{name} > ~{file}
    touch a.log b.log
    echo done > done.txt
  >>>
  output {
    File out = stdout()
    File done = "done.txt"
    File named = file
    Array[File] logs = glob("*.log")
    Int twice = count * 2
    String base = basename(named)
  }
}
"#;
    let inputs = json!({"greet.name": "ada", "greet.greeting": "hello.txt", "extra": ["a.txt"]});
    let files = [("main.wdl", text), ("hello.txt", "hi\n"), ("a.txt", "a\n")];
    let runs = documents(&files, inputs);
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    let ops = [
        ("command", "expr"),
        ("command.outputs", "expr"),
        ("output.twice", "expr"),
        ("output.base", "expr"),
        ("greet", "task"),
    ];
    assert_eq!(kinds(&runs.lowering, "task.greet"), named(&ops));
    assert_eq!(runs.native.0, runs.lowered.0, "as many tasks");
    let (native, lowered) = (&runs.native.1, &runs.lowered.1);
    for output in ["out", "done", "twice"] {
        assert_eq!(native[output], lowered[output], "output {output}");
    }
    assert_eq!(lowered["twice"], json!(4));
    assert_eq!(native["named"], json!("ada.txt"));
    // The simulator reads the files a native command makes from its script; a task op shows
    // it no script, so it makes up the files that a glob or a copied path matches.
    assert_eq!(native["logs"], json!(["a.log", "b.log"]));
    assert_eq!(lowered["logs"], json!(["simulated.log"]));
    assert_eq!(lowered["named"], json!("simulated"));
}

#[test]
fn runtime_sets_task() {
    let text = r#"version 1.1
task count {
  input {
    String text
  }
  command <<<
    echo ~{text} | wc -c
  >>>
  runtime {
    docker: "ubuntu:24.04"
    cpu: 2
    memory: "1 GiB"
    returnCodes: [0, 1]
  }
  output {
    File size = stdout()
  }
}
"#;
    let runs = runs(text, json!({"text": "four"}));
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    let graph = (runs.lowering.document.graphs.iter()).find(|item| item.name == "task.count");
    let op = graph.and_then(|graph| graph.ops.iter().find(|op| op.path == "count"));
    let op = op.expect("the task op of count");
    let Kind::Task(task) = &op.kind else {
        panic!("count is a task op");
    };
    assert_eq!(task.image.as_deref(), Some("ubuntu:24.04"));
    assert_eq!(task.success_codes, [0, 1]);
    let resources = op.attributes.resources.clone().unwrap_or_default();
    assert_eq!((resources.cpus, resources.memory), (Some(2), Some(1 << 30)));
    assert_eq!(runs.native, runs.lowered);
    let picked = text.replace("docker: \"ubuntu:24.04\"", "docker: text");
    let runs = self::runs(&picked, json!({"text": "four"}));
    let kept: Vec<(&str, &str)> = (runs.lowering.kept.iter())
        .map(|kept| (kept.path.as_str(), kept.reason.as_str()))
        .collect();
    assert_eq!(
        kept,
        [("count", "the runtime value docker depends on inputs")]
    );
    assert_eq!(runs.native, runs.lowered);
}

const SAY: &str = r#"
task say {
  input {
    String name
  }
  command <<<
    echo hi ~{name}
  >>>
  output {
    String text = "hi ~{name}"
    File out = stdout()
  }
}
"#;

#[test]
fn scatter_becomes_map() {
    let workflow = r#"version 1.1
workflow scattered {
  input {
    Array[String] names
  }
  scatter (name in names) {
    call say { input: name = name }
    String loud = name + "!"
  }
  output {
    Array[String] texts = say.text
    Array[String] louds = loud
    Array[File] outs = say.out
  }
}
"#;
    let runs = runs(
        &format!("{workflow}{SAY}"),
        json!({"names": ["a", "b", "c"]}),
    );
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    assert_eq!(
        kinds(&runs.lowering, "main"),
        named(&[("scatter.say", "map")])
    );
    let body = [("say", "call"), ("loud", "expr")];
    assert_eq!(kinds(&runs.lowering, "scatter.say"), named(&body));
    assert_eq!(runs.native, runs.lowered);
    assert_eq!(runs.lowered.0, 3);
    assert_eq!(runs.lowered.1["louds"], json!(["a!", "b!", "c!"]));
}

#[test]
fn conditional_becomes_branch() {
    let workflow = r#"version 1.1
workflow choose {
  input {
    Boolean loud
    String name
  }
  if (loud) {
    call say as shout { input: name = name + "!" }
  }
  if (!loud) {
    call say as whisper { input: name }
  }
  String chosen = select_first([shout.text, whisper.text])
  output {
    String said = chosen
    File? shouted = shout.out
  }
}
"#;
    let text = format!("{workflow}{SAY}");
    let runs = self::runs(&text, json!({"loud": true, "name": "ada"}));
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    let ops = [
        ("if.shout", "branch"),
        ("if.whisper.condition", "expr"),
        ("if.whisper", "branch"),
        ("chosen.items", "merge"),
        ("chosen", "select"),
    ];
    assert_eq!(kinds(&runs.lowering, "main"), named(&ops));
    assert_eq!(runs.native, runs.lowered);
    assert_eq!(runs.lowered.1["said"], json!("hi ada!"));
    let quiet = self::runs(&text, json!({"loud": false, "name": "ada"}));
    assert_eq!(quiet.native, quiet.lowered);
    assert_eq!(quiet.lowered.1, json!({"said": "hi ada", "shouted": null}));
}

#[test]
fn nested_scatter_maps() {
    let text = r#"version 1.1
workflow grid {
  input {
    Array[Int] rows
    Array[Int] columns
  }
  scatter (row in rows) {
    scatter (column in columns) {
      call times { input: a = row, b = column }
    }
  }
  output {
    Array[Array[Int]] products = times.product
  }
}

task times {
  input {
    Int a
    Int b
  }
  command <<<
    echo ~{a} ~{b}
  >>>
  output {
    Int product = a * b
  }
}
"#;
    let runs = runs(text, json!({"rows": [1, 2], "columns": [3, 4, 5]}));
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    assert_eq!(
        kinds(&runs.lowering, "main"),
        named(&[("scatter.times", "map")])
    );
    let graph = &runs.lowering.document.graphs[0];
    let Kind::Map(outer) = &graph.ops[0].kind else {
        panic!("the scatter is a map op");
    };
    let inner = [("scatter.times", "map")];
    assert_eq!(kinds(&runs.lowering, &outer.body), named(&inner));
    assert_eq!(runs.native, runs.lowered);
    assert_eq!(runs.lowered.1["products"], json!([[3, 4, 5], [6, 8, 10]]));
}

#[test]
fn subworkflow_becomes_call() {
    let main = r#"version 1.1
import "inner.wdl" as inner

workflow outer {
  input {
    String name
  }
  call inner.twice { input: name = name }
  call inner.twice as again { input: name = twice.first }
  output {
    Array[String] texts = again.texts
  }
}
"#;
    let inner = format!(
        r#"version 1.1
workflow twice {{
  input {{
    String name
  }}
  call say as one {{ input: name = name }}
  call say as two {{ input: name = one.text }}
  output {{
    String first = one.text
    Array[String] texts = [one.text, two.text]
  }}
}}
{SAY}"#
    );
    let runs = documents(
        &[("main.wdl", main), ("inner.wdl", &inner)],
        json!({"name": "ada"}),
    );
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    let ops = [("twice", "call"), ("again", "call")];
    assert_eq!(kinds(&runs.lowering, "main"), named(&ops));
    let inner = [("one", "call"), ("two", "call"), ("output.texts", "expr")];
    assert_eq!(kinds(&runs.lowering, "twice"), named(&inner));
    assert_eq!(runs.native, runs.lowered);
    assert_eq!(runs.lowered.0, 4);
    assert_eq!(
        runs.lowered.1["texts"],
        json!(["hi hi ada", "hi hi hi ada"])
    );
}

#[test]
fn given_none_stays() {
    let text = r#"version 1.1
workflow greet {
  input {
    String name
  }
  call hello as plain { input: name = name, salutation = None }
  call hello as polite { input: name = name }
  output {
    Array[String] greetings = [plain.greeting, polite.greeting]
  }
}

task hello {
  input {
    String name
    String? salutation = "hello"
  }
  command <<< >>>
  output {
    String greeting = if defined(salutation) then "~{salutation} ~{name}" else name
  }
}
"#;
    let runs = runs(text, json!({"name": "ada"}));
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    assert_eq!(runs.native, runs.lowered);
    assert_eq!(runs.lowered.1["greetings"], json!(["ada", "hello ada"]));
}

#[test]
fn nested_inputs_given() {
    let workflow = r#"version 1.2
workflow nested {
  hints {
    allow_nested_inputs: true
  }
  call say
  call say as again { input: name = "bob" }
  output {
    Array[String] texts = [say.text, again.text]
  }
}
"#;
    let text = format!("{workflow}{SAY}");
    let runs = runs(&text, json!({"nested.say.name": "ada"}));
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    let inputs = &runs.lowering.document.graphs[0].inputs;
    let names: Vec<&str> = inputs.iter().map(|port| port.name.as_str()).collect();
    assert_eq!(names, ["say.name"]);
    assert_eq!(runs.native, runs.lowered);
    assert_eq!(runs.lowered.1["texts"], json!(["hi ada", "hi bob"]));
}

#[test]
fn nested_conditional_lowers() {
    let workflow = r#"version 1.1
workflow nested {
  input {
    Boolean morning
    Boolean friendly
  }
  if (morning) {
    if (friendly) {
      call say { input: name = "ada" }
    }
  }
  output {
    String? text = say.text
    String chosen = select_first([say.text, "hi"])
  }
}
"#;
    let text = format!("{workflow}{SAY}");
    for (morning, friendly, chosen) in [(true, true, "hi ada"), (true, false, "hi")] {
        let runs = self::runs(&text, json!({"morning": morning, "friendly": friendly}));
        assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
        assert_eq!(runs.native, runs.lowered);
        assert_eq!(runs.lowered.1["chosen"], json!(chosen));
    }
}

#[test]
fn optional_files_stay() {
    let text = r#"version 1.1
task maybe {
  command <<<
    touch one.txt
  >>>
  output {
    Array[File?] files = ["one.txt", "two.txt"]
  }
}
"#;
    let runs = runs(text, json!({}));
    let kept: Vec<&str> = runs
        .lowering
        .kept
        .iter()
        .map(|kept| kept.reason.as_str())
        .collect();
    assert_eq!(kept, ["output files: files inside this type"]);
    assert_eq!(runs.native, runs.lowered);
}

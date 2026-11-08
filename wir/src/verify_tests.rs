//! Tests of the verifier: one valid document and one change per problem.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};

use super::*;

fn task(path: &str) -> Json {
    json!({
        "path": path,
        "kind": {"task": {
            "command": ["cat", {"input": "input"}],
            "captures": [{"output": "out", "from": "stdout"}]
        }},
        "inputs": [{"name": "input", "type": {"file": {}}}],
        "outputs": [{"name": "out", "type": {"file": {}}}]
    })
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// Two chained tasks: `text` -> sort -> head -> `top`.
fn base() -> Json {
    json!({
        "wir_version": VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "text", "type": {"file": {}}}],
            "outputs": [{"name": "top", "type": {"file": {}}}],
            "ops": [task("sort"), task("head")],
            "edges": [
                edge((None, "text"), (Some("sort"), "input")),
                edge((Some("sort"), "out"), (Some("head"), "input")),
                edge((Some("head"), "out"), (None, "top"))
            ]
        }],
        "entry": "main"
    })
}

fn problems(document: Json) -> Vec<VerifyError> {
    let document: Document = serde_json::from_value(document).expect("a parsable document");
    verify(&document).err().unwrap_or_default()
}

fn only(document: Json) -> (String, Problem) {
    let found = problems(document);
    assert_eq!(found.len(), 1, "one problem expected: {found:?}");
    (found[0].at.clone(), found[0].problem.clone())
}

#[test]
fn valid_document_passes() {
    assert_eq!(problems(base()), Vec::new());
}

#[test]
fn rejects_unknown_version() {
    let mut document = base();
    document["wir_version"] = json!("0.0");
    assert_eq!(
        only(document),
        ("document".into(), Problem::Version("0.0".into()))
    );
}

#[test]
fn paths_are_unique() {
    let mut document = base();
    document["graphs"][0]["ops"][1]["path"] = json!("sort");
    let found = problems(document);
    assert!(found.contains(&VerifyError {
        at: "main".into(),
        problem: Problem::Duplicate("sort".into())
    }));
    let mut document = base();
    document["graphs"][0]["ops"][1]["path"] = json!("steps/head two");
    assert!(
        problems(document)
            .iter()
            .any(|e| e.problem == Problem::BadName("steps/head two".into()))
    );
}

#[test]
fn edge_types_match() {
    let mut document = base();
    document["graphs"][0]["ops"][1]["inputs"][0]["type"] = json!("string");
    let (at, problem) = only(document);
    assert_eq!(at, "main/head");
    assert!(matches!(problem, Problem::Mismatch { port, .. } if port == "input input"));
    let mut document = base();
    document["graphs"][0]["ops"][1]["inputs"][0]["flow"] = json!("stream");
    assert_eq!(
        only(document),
        (
            "main/head".into(),
            Problem::FlowMismatch("input input".into())
        )
    );
}

#[test]
fn one_producer_each() {
    let mut document = base();
    let extra = edge((None, "text"), (Some("head"), "input"));
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .push(extra);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::Producers("input input".into()))
    );
}

#[test]
fn required_inputs_bound() {
    let mut document = base();
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .remove(1);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::Unbound("input input".into()))
    );
    let mut document = base();
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .remove(1);
    document["graphs"][0]["ops"][1]["inputs"][0]["type"] = json!({"optional": {"file": {}}});
    assert_eq!(problems(document), Vec::new());
}

#[test]
fn defaults_checked() {
    let file = json!({"class": "File", "location": "a.txt"});
    let mut document = base();
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .remove(0);
    document["graphs"][0]["ops"][0]["inputs"][0]["default"] = file.clone();
    assert_eq!(
        problems(document),
        Vec::new(),
        "a default needs no producer"
    );
    let mut document = base();
    document["graphs"][0]["inputs"][0]["type"] = json!({"optional": {"file": {}}});
    let mismatch = problems(document.clone());
    assert!(matches!(
        mismatch[..],
        [VerifyError {
            problem: Problem::Mismatch { .. },
            ..
        }]
    ));
    document["graphs"][0]["ops"][0]["inputs"][0]["default"] = file.clone();
    assert_eq!(
        problems(document.clone()),
        Vec::new(),
        "a default takes null"
    );
    document["graphs"][0]["ops"][0]["inputs"][0]["default"] = json!(3);
    let unfit = Problem::Unfit("input input".into());
    assert_eq!(only(document), ("main/sort".into(), unfit));
    let mut document = base();
    document["graphs"][0]["outputs"][0]["default"] = file.clone();
    let output = Problem::Default("graph output top".into());
    assert_eq!(only(document), ("main".into(), output));
    let mut document = base();
    document["graphs"][0]["inputs"][0]["default"] = file;
    assert!(problems(document).is_empty());
}

#[test]
fn rejects_op_cycles() {
    let mut document = base();
    let back = json!({"op": "head", "on": ["succeeded"]});
    document["graphs"][0]["ops"][0]["after"] = json!([back]);
    let (at, problem) = only(document);
    assert!(at.starts_with("main/"));
    assert!(matches!(problem, Problem::Cycle(_)));
}

#[test]
fn rejects_graph_recursion() {
    let mut document = base();
    let call = json!({"path": "again", "kind": {"call": {"graph": "main"}},
        "inputs": [{"name": "text", "type": {"file": {}}}],
        "outputs": [{"name": "top", "type": {"file": {}}}]});
    document["graphs"][0]["ops"]
        .as_array_mut()
        .expect("ops")
        .push(call);
    let edge = edge((None, "text"), (Some("again"), "text"));
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .push(edge);
    assert_eq!(
        only(document),
        ("main".into(), Problem::Recursion("main".into()))
    );
}

#[test]
fn call_ports_match() {
    let mut document = base();
    let inner = document["graphs"][0].clone();
    document["graphs"][0]["name"] = json!("inner");
    let call = json!({"path": "run", "kind": {"call": {"graph": "inner"}},
        "inputs": [{"name": "text", "type": "string"}],
        "outputs": [{"name": "top", "type": {"file": {}}}]});
    let outer = json!({"name": "main", "inputs": inner["inputs"], "outputs": inner["outputs"],
    "ops": [call], "edges": [
        edge((None, "text"), (Some("run"), "text")),
        edge((Some("run"), "top"), (None, "top"))
    ]});
    document["graphs"]
        .as_array_mut()
        .expect("graphs")
        .push(outer);
    let found = problems(document);
    let wanted = Problem::CallPort {
        port: "input text".into(),
        graph: "inner".into(),
    };
    assert!(
        found
            .iter()
            .any(|e| e.at == "main/run" && e.problem == wanted),
        "{found:?}"
    );
}

#[test]
fn native_ports_typed() {
    let mut document = base();
    document["sources"] = json!([{"id": "flow", "language": "nextflow", "location": "main.nf"}]);
    document["graphs"][0]["ops"][1]["kind"] =
        json!({"native": {"source": "flow", "language": "cwl"}});
    document["graphs"][0]["ops"][1]["outputs"][0]["type"] = json!("dynamic");
    let found: Vec<Problem> = problems(document).into_iter().map(|e| e.problem).collect();
    assert!(
        found.contains(&Problem::Untyped("port out".into())),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|p| matches!(p, Problem::Language { named, .. } if named == "flow"))
    );
}

#[test]
fn nextflow_outputs_dynamic() {
    let mut document = base();
    document["sources"] = json!([
        {"id": "flow", "language": "nextflow", "location": "main.nf"},
        {"id": "cluster", "language": "nextflow", "location": "cluster.config"},
        {"id": "tool", "language": "cwl", "location": "tool.cwl"}
    ]);
    let native = json!({"source": "flow", "language": "nextflow", "profiles": ["test"]});
    document["graphs"][0]["ops"][1]["kind"] = json!({"native": native});
    document["graphs"][0]["ops"][1]["outputs"][0]["type"] = json!("dynamic");
    let mut configured = document.clone();
    configured["graphs"][0]["ops"][1]["kind"]["native"]["configs"] = json!(["cluster"]);
    assert_eq!(
        problems(configured.clone()),
        [],
        "a Nextflow output may be dynamic"
    );
    configured["graphs"][0]["ops"][1]["kind"]["native"]["configs"] = json!(["tool", "missing"]);
    let found: Vec<Problem> = problems(configured)
        .into_iter()
        .map(|e| e.problem)
        .collect();
    assert!(
        found
            .iter()
            .any(|p| matches!(p, Problem::Language { named, .. } if named == "tool")),
        "{found:?}"
    );
    assert!(
        found.contains(&Problem::NoSource("missing".into())),
        "{found:?}"
    );
    let mut input = document.clone();
    input["graphs"][0]["ops"][1]["inputs"][0]["type"] = json!("dynamic");
    input["graphs"][0]["ops"][0]["outputs"][0]["type"] = json!("dynamic");
    let found: Vec<Problem> = problems(input).into_iter().map(|e| e.problem).collect();
    assert!(
        found.contains(&Problem::Untyped("port input".into())),
        "{found:?}"
    );
}

#[test]
fn stream_files_checked() {
    let mut document = base();
    document["graphs"][0]["ops"][1]["kind"]["task"]["stdout"] = json!("out.txt");
    assert!(problems(document.clone()).is_empty());
    document["graphs"][0]["ops"][1]["kind"]["task"]["stderr"] = json!("../err.txt");
    let outside = Problem::Outside("../err.txt".into());
    assert_eq!(only(document.clone()), ("main/head".into(), outside));
    document["graphs"][0]["ops"][1]["kind"]["task"]["stderr"] = json!("err.txt");
    assert!(problems(document.clone()).is_empty());
    document["graphs"][0]["ops"][1]["kind"]["task"]["stdout"] = json!({"input": "name"});
    let missing = Problem::NoPort("input name".into());
    assert_eq!(only(document.clone()), ("main/head".into(), missing));
    document["graphs"][0]["ops"][1]["kind"]["task"]["stdout"] = json!({"input": "input"});
    let files = json!([{"value": "x=1"}, {"value": {"input": "input"}, "path": "/etc/x"}]);
    document["graphs"][0]["ops"][1]["kind"]["task"]["files"] = files;
    let found: Vec<Problem> = (problems(document.clone()).into_iter())
        .map(|error| error.problem)
        .collect();
    let text = Problem::Empty("the path of staged text".into());
    assert_eq!(found, [text, Problem::Outside("/etc/x".into())]);
    document["graphs"][0]["ops"][1]["kind"]["task"]["files"] = json!([]);
    document["graphs"][0]["ops"][1]["kind"]["task"]["workdir"] = json!("work");
    let relative = Problem::Absolute("work".into());
    assert_eq!(only(document), ("main/head".into(), relative));
}

/// A map `each` over the files `texts`, whose body `round` runs one task.
fn mapped() -> Json {
    let files = json!({"array": {"items": {"file": {}}}});
    let round = json!({
        "name": "round",
        "inputs": [{"name": "input", "type": {"file": {}}}],
        "outputs": [{"name": "out", "type": {"file": {}}}],
        "ops": [task("cat")],
        "edges": [
            edge((None, "input"), (Some("cat"), "input")),
            edge((Some("cat"), "out"), (None, "out"))
        ]
    });
    json!({
        "wir_version": VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "texts", "type": files}],
            "outputs": [{"name": "outs", "type": files}],
            "ops": [{
                "path": "each",
                "kind": {"map": {"body": "round", "over": ["input"]}},
                "inputs": [{"name": "input", "type": files}],
                "outputs": [{"name": "out", "type": files}]
            }],
            "edges": [
                edge((None, "texts"), (Some("each"), "input")),
                edge((Some("each"), "out"), (None, "outs"))
            ]
        }, round],
        "entry": "main"
    })
}

#[test]
fn single_expr_output() {
    let mut document = base();
    let expr = json!({
        "path": "pick",
        "kind": {"expr": {"language": "js", "code": "$(inputs.text)", "library": ["var a = 1;"]}},
        "inputs": [{"name": "text", "type": {"file": {}}}],
        "outputs": [{"name": "out", "type": {"file": {}}}, {"name": "more", "type": "int"}]
    });
    document["graphs"][0]["ops"]
        .as_array_mut()
        .unwrap()
        .push(expr);
    let edge = edge((None, "text"), (Some("pick"), "text"));
    document["graphs"][0]["edges"]
        .as_array_mut()
        .unwrap()
        .push(edge);
    let single = Problem::Single("an expr".into());
    assert_eq!(only(document.clone()), ("main/pick".into(), single));
    document["graphs"][0]["ops"][2]["outputs"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(problems(document).is_empty());
}

/// A branch `maybe` that runs the task of its body `inner` when `go` holds.
fn branched() -> Json {
    let optional = json!({"optional": {"file": {}}});
    json!({
        "wir_version": VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "text", "type": {"file": {}}}, {"name": "go", "type": "bool"}],
            "outputs": [{"name": "top", "type": optional}],
            "ops": [{
                "path": "maybe",
                "kind": {"branch": {"body": "inner", "condition": "go"}},
                "inputs": [{"name": "input", "type": {"file": {}}}, {"name": "go", "type": "bool"}],
                "outputs": [{"name": "out", "type": optional}]
            }],
            "edges": [
                edge((None, "text"), (Some("maybe"), "input")),
                edge((None, "go"), (Some("maybe"), "go")),
                edge((Some("maybe"), "out"), (None, "top"))
            ]
        }, {
            "name": "inner",
            "inputs": [{"name": "input", "type": {"file": {}}}],
            "outputs": [{"name": "out", "type": {"file": {}}}],
            "ops": [task("cat")],
            "edges": [
                edge((None, "input"), (Some("cat"), "input")),
                edge((Some("cat"), "out"), (None, "out"))
            ]
        }],
        "entry": "main"
    })
}

#[test]
fn branch_ports_checked() {
    assert!(problems(branched()).is_empty());
    let mut document = branched();
    document["graphs"][0]["ops"][0]["outputs"][0]["type"] = json!({"file": {}});
    let port = Problem::CallPort {
        port: "output out".into(),
        graph: "inner".into(),
    };
    assert_eq!(
        only(document),
        ("main/maybe".into(), port),
        "an output may be null"
    );
}

#[test]
fn map_ports_checked() {
    assert!(problems(mapped()).is_empty());
    let mut document = mapped();
    document["graphs"][0]["ops"][0]["outputs"][0]["type"] = json!({"file": {}});
    let port = Problem::CallPort {
        port: "output out".into(),
        graph: "round".into(),
    };
    let found = problems(document);
    assert!(
        found
            .iter()
            .any(|error| error.at == "main/each" && error.problem == port),
        "{found:?}"
    );
    let mut document = mapped();
    document["graphs"][0]["ops"][0]["kind"]["map"]["mode"] = json!("nested_cross");
    assert!(
        problems(document.clone()).is_empty(),
        "one input nests once"
    );
}

#[test]
fn task_outputs_captured() {
    let mut document = base();
    document["graphs"][0]["ops"][1]["kind"]["task"]["captures"] = json!([]);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::Capture("out".into()))
    );
    let mut document = base();
    document["graphs"][0]["ops"][1]["outputs"][0]["type"] = json!("int");
    document["graphs"][0]["outputs"][0]["type"] = json!("int");
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::CaptureType("out".into()))
    );
    let mut document = base();
    document["graphs"][0]["ops"][1]["kind"]["task"]["captures"][0]["load_contents"] = json!(true);
    assert!(
        problems(document.clone()).is_empty(),
        "a File loads contents"
    );
    document["graphs"][0]["ops"][1]["kind"]["task"]["captures"][0]["from"] = json!({"glob": "d"});
    document["graphs"][0]["ops"][1]["outputs"][0]["type"] = json!("directory");
    document["graphs"][0]["outputs"][0]["type"] = json!("directory");
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::CaptureType("out".into()))
    );
    let mut document = base();
    let patterns = json!({"patterns": "globs"});
    document["graphs"][0]["ops"][1]["kind"]["task"]["captures"][0]["from"] = patterns;
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::NoPort("input globs".into()))
    );
    let mut document = base();
    document["graphs"][0]["ops"][1]["kind"]["task"]["command"] = json!([{"input": "other"}]);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::NoPort("input other".into()))
    );
}

#[test]
fn rejects_recursive_types() {
    let mut document = base();
    document["types"] = json!([{"name": "Tree", "type": {"array": {"items": {"named": "Tree"}}}}]);
    assert_eq!(
        only(document),
        ("types/Tree".into(), Problem::RecursiveType("Tree".into()))
    );
    let mut document = base();
    document["graphs"][0]["inputs"][0]["type"] = json!({"named": "Missing"});
    let found = problems(document);
    assert!(
        found
            .iter()
            .any(|e| e.problem == Problem::NoType("Missing".into())),
        "{found:?}"
    );
}

/// A merge of the optional files `a` and `b` and a select of the first one that is set.
fn picked() -> Json {
    let optional = json!({"optional": {"file": {}}});
    let list = json!({"array": {"items": optional}});
    json!({
        "wir_version": VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "a", "type": optional}, {"name": "b", "type": optional}],
            "outputs": [{"name": "first", "type": {"file": {}}}],
            "ops": [{
                "path": "both",
                "kind": {"merge": {}},
                "inputs": [{"name": "0", "type": optional}, {"name": "1", "type": optional}],
                "outputs": [{"name": "out", "type": list}]
            }, {
                "path": "pick",
                "kind": {"select": {"rule": "first"}},
                "inputs": [{"name": "in", "type": list}],
                "outputs": [{"name": "out", "type": {"file": {}}}]
            }],
            "edges": [
                edge((None, "a"), (Some("both"), "0")),
                edge((None, "b"), (Some("both"), "1")),
                edge((Some("both"), "out"), (Some("pick"), "in")),
                edge((Some("pick"), "out"), (None, "first"))
            ]
        }],
        "entry": "main"
    })
}

#[test]
fn select_merge_checked() {
    assert_eq!(problems(picked()), Vec::new());
    let mut document = picked();
    document["graphs"][0]["ops"][1]["kind"]["select"]["rule"] = json!("all");
    let (at, problem) = only(document);
    assert_eq!(at, "main/pick");
    assert!(matches!(problem, Problem::Mismatch { port, .. } if port == "output out"));
    let mut document = picked();
    let extra = json!({"name": "more", "type": {"optional": "int"}});
    document["graphs"][0]["ops"][1]["inputs"]
        .as_array_mut()
        .expect("inputs")
        .push(extra);
    let unary = Problem::Unary("a select".into());
    assert_eq!(only(document), ("main/pick".into(), unary));
    let mut document = picked();
    document["graphs"][0]["ops"][0]["inputs"][1]["type"] = json!({"optional": "string"});
    document["graphs"][0]["inputs"][1]["type"] = json!({"optional": "string"});
    let (at, problem) = only(document);
    assert_eq!(at, "main/both");
    assert!(matches!(problem, Problem::Mismatch { port, .. } if port == "output out"));
}

#[test]
fn flatten_checked() {
    let list = |items: Json| json!({"array": {"items": items}});
    let mut document = picked();
    document["graphs"][0]["ops"][1] = json!({
        "path": "pick",
        "kind": {"flatten": {}},
        "inputs": [{"name": "in", "type": list(list(json!("int"))), "default": [[1]]}],
        "outputs": [{"name": "out", "type": list(json!("int"))}]
    });
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .truncate(2);
    document["graphs"][0]["outputs"] = json!([]);
    assert_eq!(problems(document.clone()), Vec::new());
    document["graphs"][0]["ops"][1]["outputs"][0]["type"] = list(json!("string"));
    let (at, problem) = only(document.clone());
    assert_eq!(at, "main/pick");
    assert!(matches!(problem, Problem::Mismatch { port, .. } if port == "output out"));
    document["graphs"][0]["ops"][1]["inputs"][0] =
        json!({"name": "in", "type": "int", "default": 1});
    let (_, problem) = only(document);
    assert!(matches!(problem, Problem::Mismatch { port, .. } if port == "input in"));
}

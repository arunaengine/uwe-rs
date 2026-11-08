//! Checks the JSON form of the stream format: defaults left out, fields kept, and the schema
//! and the parser accepting and rejecting the same documents.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jsonschema::{Draft, Validator};
use serde_json::{Value as Json, json};

fn validator() -> Validator {
    let schema: Json = serde_json::from_str(wir::SCHEMA).expect("the schema is JSON");
    (jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema))
    .expect("a validator")
}

/// A document of one graph `g` with these ops and edges; empty lists are left out.
fn document(ops: Json, edges: Json) -> Json {
    let mut graph = json!({"name": "g"});
    for (key, list) in [("ops", ops), ("edges", edges)] {
        if list.as_array().is_some_and(|items| !items.is_empty()) {
            graph[key] = list;
        }
    }
    json!({"wir_version": wir::VERSION, "graphs": [graph], "entry": "g"})
}

/// Whether the schema and the parser both accept the document; panics when they disagree.
fn accepted(document: &Json) -> bool {
    let schema = validator().is_valid(document);
    let parsed = wir::parse(&document.to_string());
    assert_eq!(
        schema,
        parsed.is_ok(),
        "schema and parser differ on {document}: {parsed:?}"
    );
    schema
}

/// The document after parsing and writing it again.
fn written(document: &Json) -> Json {
    let parsed = wir::parse(&document.to_string()).expect("a document");
    serde_json::to_value(parsed).expect("serializes")
}

#[test]
fn subscription_forms() {
    let edge = |read: Json| {
        let mut edge = json!({"from": {"op": "a", "port": "out"}, "to": {"op": "b", "port": "in"}});
        if !read.is_null() {
            edge["read"] = read;
        }
        document(json!([]), json!([edge]))
    };
    let all = edge(json!("all"));
    assert!(accepted(&all));
    let stored = &written(&all)["graphs"][0]["edges"][0];
    assert!(stored.get("read").is_none(), "all is the default: {stored}");
    let live = edge(json!("live"));
    assert!(accepted(&live));
    assert_eq!(written(&live)["graphs"][0]["edges"][0]["read"], "live");
    assert!(!accepted(&edge(json!("late"))));
}

#[test]
fn body_forms() {
    let graph: wir::Body = serde_json::from_value(json!("square")).expect("a graph body");
    assert_eq!(graph.graph(), Some("square"));
    let closure = json!({"language": "groovy", "code": "{ x -> x * 2 }",
        "scope": {"source": "main", "start": 10, "end": 42},
        "captures": {"factor": {"input": "factor"}, "base": {"value": [1, 2]}}});
    let body: wir::Body = serde_json::from_value(closure.clone()).expect("a closure body");
    assert_eq!(body.graph(), None);
    assert_eq!(serde_json::to_value(&body).expect("serializes"), closure);
    let unknown = json!({"language": "groovy", "code": "{ it }", "frame": 1});
    assert!(serde_json::from_value::<wir::Body>(unknown).is_err());
    let binding =
        json!({"captures": {"x": {"input": "a", "value": 1}}, "language": "wdl", "code": "x"});
    assert!(serde_json::from_value::<wir::Body>(binding).is_err());
}

#[test]
fn expr_closure_fields() {
    let expr = |language: &str| {
        let op = json!({"path": "e", "kind": {"expr": {"language": language, "code": "1",
            "captures": {"n": {"value": 3}}}}, "values": "groovy",
            "outputs": [{"name": "out", "type": "int"}]});
        document(json!([op]), json!([]))
    };
    for language in ["js", "javascript", "groovy", "python", "wdl"] {
        let document = expr(language);
        assert!(accepted(&document), "{language}");
        assert_eq!(written(&document), document);
    }
    assert!(!accepted(&expr("perl")));
    let mut plain = expr("js");
    plain["graphs"][0]["ops"][0]["values"] = json!("wir");
    let stored = &written(&plain)["graphs"][0]["ops"][0];
    assert!(
        stored.get("values").is_none(),
        "wir is the default: {stored}"
    );
}

/// Each kind parses, fits the schema, and is written back as it was; each wrong form fails.
fn kinds_round_trip(good: &[Json], bad: &[Json]) {
    for kind in good {
        let document = document(json!([{"path": "a", "kind": kind}]), json!([]));
        assert!(accepted(&document), "{kind}");
        assert_eq!(written(&document), document, "{kind}");
    }
    for kind in bad {
        let document = document(json!([{"path": "a", "kind": kind}]), json!([]));
        assert!(!accepted(&document), "{kind}");
    }
}

#[test]
fn transform_forms() {
    let good = [
        json!({"emit": {}}),
        json!({"emit": {"each": true}}),
        json!({"transform": {"body": "double"}}),
        json!({"transform": {"body": {"language": "groovy", "code": "{ it }"}, "nulls": "keep"}}),
        json!({"filter": {"body": "small"}}),
        json!({"flat_map": {}}),
        json!({"flat_map": {"body": "split"}}),
        json!({"flatten": {}}),
        json!({"flatten": {"depth": "all", "drop_null": true}}),
    ];
    let bad = [
        json!({"emit": {"each": "yes"}}),
        json!({"transform": {}}),
        json!({"transform": {"body": "double", "nulls": "skip"}}),
        json!({"flatten": {"depth": 2}}),
    ];
    kinds_round_trip(&good, &bad);
    let defaults = json!({"transform": {"body": "double", "nulls": "drop"}});
    let document = document(json!([{"path": "a", "kind": defaults}]), json!([]));
    let stored = &written(&document)["graphs"][0]["ops"][0]["kind"]["transform"];
    assert_eq!(*stored, json!({"body": "double"}));
}

#[test]
fn combination_forms() {
    let good = [
        json!({"mix": {}}),
        json!({"concat": {}}),
        json!({"merge": {}}),
        json!({"merge": {"rounds": true, "body": "pair"}}),
        json!({"zip": {"remainder": true}}),
    ];
    let bad = [
        json!({"mix": {"order": "fair"}}),
        json!({"merge": {"rounds": 1}}),
    ];
    kinds_round_trip(&good, &bad);
}

#[test]
fn group_spellings() {
    let good = [
        json!({"group": {"size": 2}}),
        json!({"group_tuple": {"by": [1, 0], "remainder": true, "sort": "hash"}}),
        json!({"group_tuple": {"size_body": "sizes", "sort": {"key": "name"}}}),
        json!({"combine": {"by": [0]}}),
        json!({"combine": {}}),
        json!({"cross": {"key": "id", "left": "error"}}),
    ];
    kinds_round_trip(
        &good,
        &[
            json!({"group_tuple": {"sort": "random"}}),
            json!({"cross": {"left": "all"}}),
        ],
    );
    let first = document(
        json!([{"path": "a", "kind": {"group": {"by": [0]}}}]),
        json!([]),
    );
    let stored = &written(&first)["graphs"][0]["ops"][0]["kind"];
    assert_eq!(
        *stored,
        json!({"group": {}}),
        "by [0] is the default and the spelling stays"
    );
}

#[test]
fn join_field_names() {
    let good = [
        json!({"join": {"remainder": true, "duplicate_error": true, "body": "pad"}}),
        json!({"join": {"by": [0, 2], "mismatch_error": true}}),
    ];
    let old = [
        json!({"join": {"by": [0], "fail_on_mismatch": true}}),
        json!({"join": {"by": [0], "fail_on_duplicate": true}}),
    ];
    kinds_round_trip(&good, &old);
}

#[test]
fn routing_forms() {
    let good = [
        json!({"route": {"cases": [{"output": "small", "condition": "is_small"},
            {"output": "big", "condition": {"language": "groovy", "code": "{ it > 9 }"}, "body": "half"}]}}),
        json!({"multi_map": {"outputs": [{"output": "a", "body": "f"}, {"output": "b", "body": "g", "nulls": "drop"}]}}),
    ];
    let bad = [
        json!({"route": {"cases": [{"output": "a"}]}}),
        json!({"multi_map": {}}),
    ];
    kinds_round_trip(&good, &bad);
    let kept = json!({"multi_map": {"outputs": [{"output": "a", "body": "f", "nulls": "keep"}]}});
    let document = document(json!([{"path": "a", "kind": kept}]), json!([]));
    let stored = &written(&document)["graphs"][0]["ops"][0]["kind"]["multi_map"]["outputs"][0];
    assert_eq!(
        *stored,
        json!({"output": "a", "body": "f"}),
        "keep is the default here"
    );
}

#[test]
fn null_seed() {
    let good = [
        json!({"collect": {}}),
        json!({"collect": {"body": "f", "flat": false, "sort": {"compare": "cmp"}}}),
        json!({"to_list": {}}),
        json!({"reduce": {"rule": "fold", "body": "add", "seed": {"value": null}, "nulls": "keep_state"}}),
        json!({"reduce": {"rule": "sum", "empty": {"value": 0}}}),
        json!({"reduce": {"rule": "max", "empty": "error"}}),
        json!({"first": {"predicate": "big", "empty": "null"}}),
        json!({"last": {}}),
    ];
    let bad = [
        json!({"reduce": {"rule": "mean"}}),
        json!({"reduce": {"rule": "fold", "seed": null}}),
        json!({"first": {"empty": "nothing"}}),
    ];
    kinds_round_trip(&good, &bad);
    let seeded: wir::reserved::Reduce =
        serde_json::from_value(json!({"rule": "fold", "seed": {"value": null}})).expect("a reduce");
    let unseeded: wir::reserved::Reduce =
        serde_json::from_value(json!({"rule": "fold"})).expect("a reduce");
    assert_eq!(
        seeded.seed,
        Some(wir::reserved::Literal { value: Json::Null })
    );
    assert_eq!(unseeded.seed, None, "no seed differs from a null seed");
    let flat = document(
        json!([{"path": "a", "kind": {"collect": {"flat": true}}}]),
        json!([]),
    );
    let stored = &written(&flat)["graphs"][0]["ops"][0]["kind"];
    assert_eq!(*stored, json!({"collect": {}}), "flat is true by default");
}

#[test]
fn selection_forms() {
    let good = [
        json!({"buffer": {"rule": {"size": {"size": 3, "skip": 1}}, "remainder": true}}),
        json!({"buffer": {"rule": {"match": {"open": "start", "close": "stop"}}}}),
        json!({"window": {"size": 3, "step": 1}}),
        json!({"window": {"size": 2, "remainder": false}}),
        json!({"take": {"count": -1}}),
        json!({"unique": {"key": "id"}}),
        json!({"distinct": {"initial": {"value": null}}}),
        json!({"until": {"predicate": "done", "inclusive": true}}),
        json!({"if_empty": {"value": {"value": []}}}),
    ];
    let bad = [
        json!({"take": {}}),
        json!({"buffer": {"rule": {"every": 2}}}),
        json!({"distinct": {"initial": null}}),
    ];
    kinds_round_trip(&good, &bad);
    let window = document(
        json!([{"path": "a", "kind": {"window": {"size": 2, "remainder": true}}}]),
        json!([]),
    );
    let stored = &written(&window)["graphs"][0]["ops"][0]["kind"];
    assert_eq!(
        *stored,
        json!({"window": {"size": 2}}),
        "remainder is true by default"
    );
}

#[test]
fn splitter_forms() {
    let split = |fields: Json| json!({"split": fields});
    let good = [
        split(json!({"format": {"text": {"keep_header": true}}, "size": 10, "limit": 100})),
        split(
            json!({"format": {"csv": {"separator": ";", "quote": null, "header": "read", "skip": 1, "strip": true}}}),
        ),
        split(json!({"format": {"csv": {"header": {"names": ["id", "path"]}}}, "elem": 1})),
        split(
            json!({"format": {"fasta": {"fields": [{"name": "seqString", "field": "seq_string"}]}}}),
        ),
        split(
            json!({"format": {"fastq": {}}, "pair": {"at": [1, 2]}, "decompress": "always",
            "files": {"name": "reads", "compress": true}}),
        ),
        split(json!({"format": {"fastq": {}}, "pair": "auto", "charset": "latin-1"})),
        split(json!({"format": {"json": {"path": "samples[0].files"}}, "body": "f"})),
    ];
    let bad = [
        split(json!({"format": {"tsv": {}}})),
        split(json!({"format": {"text": {}, "csv": {}}})),
        split(json!({"format": {"csv": {"separator": ";;"}}})),
        split(json!({"format": {"fastq": {}}, "pair": {"at": [1]}})),
        split(json!({"format": {"fasta": {"fields": [{"name": "x", "field": "quality"}]}}})),
    ];
    kinds_round_trip(&good, &bad);
    let defaults = split(
        json!({"format": {"csv": {"separator": ",", "quote": "\"", "header": "none"}},
        "size": 1, "decompress": "auto", "charset": "utf-8"}),
    );
    let document = document(json!([{"path": "a", "kind": defaults}]), json!([]));
    let stored = &written(&document)["graphs"][0]["ops"][0]["kind"];
    assert_eq!(
        *stored,
        split(json!({"format": {"csv": {}}})),
        "every default is left out"
    );
}

#[test]
fn effect_forms() {
    let good = [
        json!({"collect_file": {"name": "all.txt", "sort": "index", "new_line": true, "skip": 1,
            "keep_header": true, "seed": "id\n"}}),
        json!({"view": {"body": "show", "new_line": false}}),
        json!({"observe": {"item": {"language": "groovy", "code": "{ println it }"},
            "close": {"language": "groovy", "code": "{ -> println 'done' }"}}}),
    ];
    let bad = [
        json!({"observe": {"item": "graph"}}),
        json!({"view": {"newline": true}}),
    ];
    kinds_round_trip(&good, &bad);
    let sorted = document(
        json!([{"path": "a", "kind": {"collect_file": {"sort": "hash"}},
        "effects": ["write_files", "log", "read_files"]}]),
        json!([]),
    );
    assert!(accepted(&sorted));
    let stored = &written(&sorted)["graphs"][0]["ops"][0];
    assert_eq!(
        stored["kind"],
        json!({"collect_file": {}}),
        "hash is the default sort here"
    );
    assert_eq!(
        stored["effects"],
        json!(["write_files", "log", "read_files"])
    );
}

#[test]
fn process_forms() {
    let good = [
        json!({"process": {"body": "align"}}),
        json!({"process": {"body": "align", "each": ["mode"], "order": "fair", "parallelism": 4}}),
    ];
    let bad = [
        json!({"process": {"body": "align", "order": "random"}}),
        json!({"process": {}}),
    ];
    kinds_round_trip(&good, &bad);
}

#[test]
fn control_forms() {
    let depends = json!({"any": [{"result": {"op": "a", "on": ["succeeded", "skipped", "daemoned"]}},
        {"all": [{"not": {"result": {"op": "g", "on": ["all_failed"]}}}, {"all": []}]}]});
    let op = json!({"path": "b", "kind": {"call": {"graph": "g"}}, "depends": depends,
        "handles": [{"op": "a", "on": ["failed"]}], "after": [{"op": "a", "on": ["cancelled"]}]});
    let group = json!({"path": "g", "kind": {"status_group": {"members": ["a", "b"], "allow": ["failed"]}}});
    let document = document(json!([op, group]), json!([]));
    assert!(accepted(&document));
    assert_eq!(written(&document), document);
    let parsed: wir::Control = serde_json::from_value(depends).expect("a control tree");
    assert_eq!(parsed.tests().len(), 2);
    let unknown = json!({"path": "b", "kind": {"call": {"graph": "g"}}, "depends": {"xor": []}});
    assert!(!accepted(&document_of(unknown)));
    let extra = json!({"path": "b", "kind": {"call": {"graph": "g"}},
        "depends": {"result": {"op": "a", "on": ["failed"], "why": 1}}});
    assert!(!accepted(&document_of(extra)));
}

fn document_of(op: Json) -> Json {
    document(json!([op]), json!([]))
}

#[test]
fn task_acceptance() {
    let groovy = |code: &str| json!({"language": "groovy", "code": code});
    let task = json!({"task": {"command": ["run"],
        "captures": [{"output": "out", "from": {"glob": "*.txt"}, "optional": true, "arity": {"min": 0, "max": 4}}],
        "accept": groovy("{ it.out.size() > 0 }"),
        "retry": {"limit": 2, "retry_if": groovy("{ it.attempt < 3 }"),
            "directives": groovy("{ [memory: it.attempt * 1024] }"), "failure": "ignore"}}});
    kinds_round_trip(
        &[task],
        &[
            json!({"task": {"command": ["run"], "retry": {"limit": -1}}}),
            json!({"task": {"command": ["run"], "accept": "graph"}}),
        ],
    );
    let patch: wir::TaskPatch =
        serde_json::from_value(json!({"image": "debian:12", "time": 60})).expect("a patch");
    assert_eq!(patch.time, Some(60));
    assert!(serde_json::from_value::<wir::TaskPatch>(json!({"memory": 1})).is_err());
    let plain = json!({"task": {"command": ["run"], "retry": {"limit": 0, "failure": "fail"}}});
    let stored =
        &written(&document_of(json!({"path": "a", "kind": plain})))["graphs"][0]["ops"][0]["kind"];
    assert_eq!(*stored, json!({"task": {"command": ["run"], "retry": {}}}));
}

#[test]
fn builder_forms() {
    let reference = json!({"source": "s", "entry": "main", "call": "MAIN:ALIGN",
        "scope": {"source": "s", "start": 0, "end": 10},
        "inputs": [{"port": "reads", "native": "input/0"}], "outputs": [{"port": "bam", "native": "output/0"}],
        "profiles": ["docker"], "configs": ["c"], "captures": {"genome": {"input": "genome"}}});
    let builder = |builder: Json| json!({"task": {"builder": builder}});
    let good = [
        builder(json!({"nextflow_process": reference})),
        builder(
            json!({"snakemake_job": {"task": {"source": "s", "entry": "Snakefile", "call": "bwa_map"},
            "wildcards": {"sample": "A"}}}),
        ),
        builder(json!({"argo_task": {"source": "s", "entry": "main", "call": "steps.align"}})),
        builder(json!({"wdl_task": {"source": "s", "entry": "main", "call": "align"}})),
    ];
    let bad = [
        builder(json!({"cwl_tool": reference})),
        builder(json!({"wdl_task": {"source": "s", "entry": "main"}})),
        builder(json!({"nextflow_process": reference, "argo_task": reference})),
    ];
    kinds_round_trip(&good, &bad);
}

#[test]
fn dynamic_forms() {
    let good = [
        json!({"loop": {"body": "step", "condition": "again", "carry": [{"output": "n", "input": "n"}], "limit": 10}}),
        json!({"dynamic": {"plan": "plans", "order": "fair"}}),
        json!({"dynamic": {"planner": {"snakemake": {"source": "smk", "targets": ["all"], "configs": ["c"]}}}}),
    ];
    let bad = [
        json!({"dynamic": {"plan": "p", "planner": {"nextflow": {"source": "s"}}}}),
        json!({"loop": {"body": "step", "condition": "again", "limit": -1}}),
    ];
    kinds_round_trip(&good, &bad);
    let update = json!({"revision": 0, "graph": {"name": "region"},
        "keys": [{"op": "a", "identity": {"rule": {"source": "smk", "rule": "bwa_map", "wildcards": {"sample": "A"}}}},
            {"op": "b", "identity": {"named": "b"}}], "final_plan": true});
    let parsed: wir::reserved::PlanUpdate =
        serde_json::from_value(update.clone()).expect("an update");
    assert_eq!(serde_json::to_value(&parsed).expect("serializes"), update);
    let unrevised = json!({"graph": {"name": "region"}});
    assert!(
        serde_json::from_value::<wir::reserved::PlanUpdate>(unrevised).is_err(),
        "revision is required"
    );
    let unknown = json!({"revision": 1, "graph": {"name": "region"}, "keys": [{"op": "a", "identity": {"named": "a"}, "x": 1}]});
    assert!(serde_json::from_value::<wir::reserved::PlanUpdate>(unknown).is_err());
}

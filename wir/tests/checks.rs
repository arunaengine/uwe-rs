//! Checks the verifier rules of the stream format, one document per rule.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};

/// The problems the verifier finds in a document, as text.
fn problems(document: &Json) -> Vec<String> {
    let parsed = wir::parse(&document.to_string()).expect("a parsable document");
    match wir::verify(&parsed) {
        Ok(()) => Vec::new(),
        Err(found) => found.iter().map(ToString::to_string).collect(),
    }
}

/// Whether the verifier reports exactly `expected`.
fn reports(document: &Json, expected: &str) {
    assert_eq!(problems(document), [expected], "{document}");
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

fn stream(name: &str) -> Json {
    json!({"name": name, "type": "int", "flow": "stream"})
}

/// A graph `main` that passes its stream input `xs` through a call of the graph `pass`.
fn passing() -> Json {
    let pass = json!({"name": "pass", "inputs": [stream("x")], "outputs": [stream("y")],
        "edges": [edge((None, "x"), (None, "y"))]});
    let call = json!({"path": "p", "kind": {"call": {"graph": "pass"}},
        "inputs": [stream("x")], "outputs": [stream("y")]});
    let main = json!({"name": "main", "inputs": [stream("xs")], "outputs": [stream("ys")],
        "ops": [call],
        "edges": [edge((None, "xs"), (Some("p"), "x")), edge((Some("p"), "y"), (None, "ys"))]});
    json!({"wir_version": wir::VERSION, "graphs": [main, pass], "entry": "main"})
}

#[test]
fn stream_flow_rules() {
    assert_eq!(problems(&passing()), Vec::<String>::new());
    let mut defaulted = passing();
    defaulted["graphs"][0]["ops"][0]["inputs"][0]["default"] = json!(1);
    reports(
        &defaulted,
        "main/p: input x is a stream and takes no default",
    );
    let mut unfed = passing();
    unfed["graphs"][0]["edges"][0] = edge((None, "xs"), (None, "ys"));
    unfed["graphs"][0]["edges"].as_array_mut().unwrap().pop();
    reports(
        &unfed,
        "main/p: input x is a stream input without a producer",
    );
    let mut live = passing();
    live["graphs"][1]["inputs"][0]["flow"] = json!("value");
    live["graphs"][1]["outputs"][0]["flow"] = json!("value");
    live["graphs"][1]["edges"][0]["read"] = json!("live");
    let found = problems(&live);
    assert!(
        found.contains(&"pass: graph output y reads a value, which has no live reader".into()),
        "{found:?}"
    );
}

fn port(name: &str, r#type: &str) -> Json {
    json!({"name": name, "type": r#type})
}

/// A graph `main` whose op `keep` filters its input `n` with `body`, capturing `limit`.
fn filtering(body: Json, callback: Json) -> Json {
    let keep = json!({"path": "keep", "kind": {"filter": {"body": body}},
        "inputs": [port("source", "int"), port("limit", "int")],
        "outputs": [port("out", "int")]});
    let main = json!({"name": "main", "inputs": [port("n", "int"), port("limit", "int")],
        "outputs": [port("kept", "int")], "ops": [keep],
        "edges": [edge((None, "n"), (Some("keep"), "source")),
            edge((None, "limit"), (Some("keep"), "limit")),
            edge((Some("keep"), "out"), (None, "kept"))]});
    let sources = json!([{"id": "nf", "language": "nextflow", "location": "main.nf"},
        {"id": "cwl", "language": "cwl", "location": "main.cwl"}]);
    let mut graphs = vec![main];
    graphs.extend(callback.as_array().cloned().unwrap_or_default());
    json!({"wir_version": wir::VERSION, "sources": sources, "graphs": graphs, "entry": "main"})
}

/// A callback graph `small` that compares `item` with the capture `limit`.
fn small(outputs: Json, ops: Json) -> Json {
    let compare = json!({"path": "lt", "kind": {"expr": {"language": "js",
        "code": "$(inputs.item < inputs.limit)"}},
        "inputs": [port("item", "int"), port("limit", "int")], "outputs": [port("out", "bool")]});
    let mut all = vec![compare];
    all.extend(ops.as_array().cloned().unwrap_or_default());
    json!({"name": "small", "inputs": [port("item", "int"), port("limit", "int")],
        "outputs": outputs, "ops": all,
        "edges": [edge((None, "item"), (Some("lt"), "item")),
            edge((None, "limit"), (Some("lt"), "limit")), edge((Some("lt"), "out"), (None, "out"))]})
}

#[test]
fn callback_contract() {
    let good = filtering(
        json!("small"),
        json!([small(json!([port("out", "bool")]), json!([]))]),
    );
    assert_eq!(problems(&good), Vec::<String>::new());
    let named = filtering(
        json!("small"),
        json!([small(json!([port("ok", "bool")]), json!([]))]),
    );
    let found = problems(&named);
    assert!(
        found.contains(&"main/keep: callback small: a callback graph has one output, out".into()),
        "{found:?}"
    );
    let typed = filtering(
        json!("small"),
        json!([small(json!([port("out", "int")]), json!([]))]),
    );
    let found = problems(&typed);
    assert!(
        found.iter().any(|problem| problem.contains("is wanted")),
        "{found:?}"
    );
    let task = json!({"path": "t", "kind": {"task": {"command": ["true"]}}});
    let effects = filtering(
        json!("small"),
        json!([small(json!([port("out", "bool")]), json!([task]))]),
    );
    reports(
        &effects,
        "main/keep: callback small: it runs small/t, which is not pure",
    );
    let mut stranger = filtering(
        json!("small"),
        json!([small(json!([port("out", "bool")]), json!([]))]),
    );
    stranger["graphs"][1]["inputs"][1]["name"] = json!("other");
    stranger["graphs"][1]["edges"][1]["from"]["port"] = json!("other");
    reports(
        &stranger,
        "main/keep: callback small: capture other names no value input of the op",
    );
    let closure = |extra: Json| {
        let mut body = json!({"language": "groovy", "code": "{ it < limit }",
            "captures": {"limit": {"input": "limit"}}});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap());
        filtering(body, json!([]))
    };
    assert_eq!(problems(&closure(json!({}))), Vec::<String>::new());
    let missing = closure(json!({"captures": {"limit": {"input": "cap"}}}));
    reports(
        &missing,
        "main/keep: closure: capture limit names no value input cap",
    );
    reports(
        &closure(json!({"language": "js"})),
        "main/keep: closure: js is no closure language; use javascript",
    );
    let wdl = closure(json!({"language": "wdl", "code": "x < limit"}));
    reports(&wdl, "main/keep: closure: a wdl closure names 1 parameters");
    let clash = closure(json!({"language": "wdl", "parameters": ["limit"]}));
    reports(
        &clash,
        "main/keep: closure: capture limit is also a parameter",
    );
}

#[test]
fn scope_bounds() {
    let scoped = |scope: Json| {
        let body = json!({"language": "groovy", "code": "{ it > 1 }", "scope": scope});
        filtering(body, json!([]))
    };
    let good = scoped(json!({"source": "nf", "start": 4, "end": 9}));
    assert_eq!(problems(&good), Vec::<String>::new());
    let reversed = scoped(json!({"source": "nf", "start": 9, "end": 4}));
    reports(
        &reversed,
        "main/keep: closure: the scope 9..4 ends before it starts",
    );
    let other = scoped(json!({"source": "cwl", "start": 0, "end": 1}));
    reports(
        &other,
        "main/keep: language \"groovy\" differs from that of source cwl, \"cwl\"",
    );
    let unknown = scoped(json!({"source": "nope", "start": 0, "end": 1}));
    reports(&unknown, "main/keep: no source named nope");
}

/// A graph `main` with one op `op` of `kind` between a graph input and a graph output, and
/// the callback graph `double`, which gives `item` times two.
fn operator(kind: Json, source: Json, out: Json) -> Json {
    let op =
        json!({"path": "op", "kind": kind, "inputs": [source.clone()], "outputs": [out.clone()]});
    let mut input = source.clone();
    input["name"] = json!("in");
    let mut output = out.clone();
    output["name"] = json!("result");
    let main = json!({"name": "main", "inputs": [input], "outputs": [output], "ops": [op],
        "edges": [edge((None, "in"), (Some("op"), source["name"].as_str().unwrap())),
            edge((Some("op"), out["name"].as_str().unwrap()), (None, "result"))]});
    let twice = json!({"path": "twice", "kind": {"expr": {"language": "js", "code": "$(inputs.item * 2)"}},
        "inputs": [port("item", "int")], "outputs": [port("out", "int")]});
    let double = json!({"name": "double", "inputs": [port("item", "int")],
        "outputs": [port("out", "int")], "ops": [twice],
        "edges": [edge((None, "item"), (Some("twice"), "item")), edge((Some("twice"), "out"), (None, "out"))]});
    json!({"wir_version": wir::VERSION, "graphs": [main, double], "entry": "main"})
}

fn typed(name: &str, r#type: Json, flow: &str) -> Json {
    json!({"name": name, "type": r#type, "flow": flow})
}

#[test]
fn transform_shapes() {
    let ints = json!({"array": {"items": "int"}});
    let each = operator(
        json!({"emit": {"each": true}}),
        typed("source", ints.clone(), "value"),
        typed("out", json!("int"), "stream"),
    );
    assert_eq!(problems(&each), Vec::<String>::new());
    let flows = operator(
        json!({"emit": {}}),
        typed("source", json!("int"), "stream"),
        typed("out", json!("int"), "stream"),
    );
    reports(&flows, "main/op: an emit needs a value input source");
    let named = operator(
        json!({"emit": {}}),
        typed("value", json!("int"), "value"),
        typed("out", json!("int"), "stream"),
    );
    reports(&named, "main/op: an emit needs an input named source");
    let mapped = operator(
        json!({"transform": {"body": "double"}}),
        typed("source", json!("int"), "stream"),
        typed("out", json!("int"), "stream"),
    );
    assert_eq!(problems(&mapped), Vec::<String>::new());
    let crossed = operator(
        json!({"transform": {"body": "double"}}),
        typed("source", json!("int"), "stream"),
        typed("out", json!("int"), "value"),
    );
    reports(&crossed, "main/op: a transform needs a stream output");
    let texts = operator(
        json!({"transform": {"body": "double"}}),
        typed("source", json!("string"), "stream"),
        typed("out", json!("int"), "stream"),
    );
    reports(
        &texts,
        "main/op: callback double: input item does not take \"string\"",
    );
    let narrow = operator(
        json!({"transform": {"body": "double"}}),
        typed("source", json!("int"), "stream"),
        typed("out", json!("string"), "stream"),
    );
    reports(
        &narrow,
        "main/op: output out gets \"int\" but accepts \"string\"",
    );
    let predicate = operator(
        json!({"filter": {"body": "double"}}),
        typed("source", json!("int"), "value"),
        typed("out", json!("int"), "value"),
    );
    reports(
        &predicate,
        "main/op: callback double: output out gives \"int\" but \"bool\" is wanted",
    );
    let flat = operator(
        json!({"flatten": {}}),
        typed("source", ints.clone(), "stream"),
        typed("out", json!("int"), "stream"),
    );
    assert_eq!(problems(&flat), Vec::<String>::new());
    let flat_value = operator(
        json!({"flat_map": {}}),
        typed("source", ints, "value"),
        typed("out", json!("int"), "value"),
    );
    let found = problems(&flat_value);
    assert!(
        found.contains(&"main/op: a flat_map needs a stream input source".into()),
        "{found:?}"
    );
}

/// A graph `main` with one op `op` of `kind`, each input fed by a graph input of its name.
fn many(kind: Json, inputs: Json, out: Json) -> Json {
    let names: Vec<String> = (inputs.as_array().unwrap().iter())
        .map(|port| port["name"].as_str().unwrap().to_owned())
        .collect();
    let mut edges: Vec<Json> = (names.iter())
        .map(|name| edge((None, name), (Some("op"), name)))
        .collect();
    edges.push(edge((Some("op"), "out"), (None, "result")));
    let mut output = out.clone();
    output["name"] = json!("result");
    let op = json!({"path": "op", "kind": kind, "inputs": inputs, "outputs": [out]});
    let main =
        json!({"name": "main", "inputs": inputs, "outputs": [output], "ops": [op], "edges": edges});
    json!({"wir_version": wir::VERSION, "graphs": [main], "entry": "main"})
}

#[test]
fn combination_shapes() {
    let two = json!([
        typed("a", json!("int"), "stream"),
        typed("b", json!("int"), "stream")
    ]);
    let out = typed("out", json!("int"), "stream");
    assert_eq!(
        problems(&many(json!({"mix": {}}), two.clone(), out.clone())),
        Vec::<String>::new()
    );
    let one = json!([typed("a", json!("int"), "stream")]);
    reports(
        &many(json!({"concat": {}}), one, out.clone()),
        "main/op: a concat needs two or more stream inputs",
    );
    let mixed = json!([
        typed("a", json!("int"), "stream"),
        typed("b", json!("string"), "stream")
    ]);
    reports(
        &many(json!({"mix": {}}), mixed, out.clone()),
        "main/op: output out gets \"string\" but accepts \"int\"",
    );
    let pairs = typed("out", json!({"tuple": ["int", "int"]}), "stream");
    assert_eq!(
        problems(&many(json!({"zip": {}}), two.clone(), pairs.clone())),
        Vec::<String>::new()
    );
    let found = problems(&many(
        json!({"zip": {"remainder": true}}),
        two.clone(),
        pairs,
    ));
    assert!(
        found.iter().any(|problem| problem.contains("accepts")),
        "padding needs optional items: {found:?}"
    );
    let padded = typed(
        "out",
        json!({"tuple": [{"optional": "int"}, {"optional": "int"}]}),
        "stream",
    );
    assert_eq!(
        problems(&many(
            json!({"zip": {"remainder": true}}),
            two.clone(),
            padded
        )),
        Vec::<String>::new()
    );
    let rounds = typed("out", json!("dynamic"), "stream");
    assert_eq!(
        problems(&many(json!({"merge": {}}), two.clone(), rounds)),
        Vec::<String>::new()
    );
    let first_value = json!([
        typed("a", json!("int"), "value"),
        typed("b", json!("int"), "stream")
    ]);
    let value_out = typed("out", json!("dynamic"), "stream");
    reports(
        &many(json!({"merge": {}}), first_value, value_out),
        "main/op: a merge needs a value output, as its first input",
    );
}

/// Two tuple streams `left` and `right` of `width` ints.
fn tuples(width: usize) -> Json {
    let tuple = json!({"tuple": vec![json!("int"); width]});
    json!([
        typed("left", tuple.clone(), "stream"),
        typed("right", tuple, "stream")
    ])
}

#[test]
fn key_positions() {
    let out = typed("out", json!("dynamic"), "stream");
    let joined =
        |by: Json, width: usize| many(json!({"join": {"by": by}}), tuples(width), out.clone());
    assert_eq!(problems(&joined(json!([0, 1]), 2)), Vec::<String>::new());
    reports(&joined(json!([]), 2), "main/op: by must not be empty");
    let found = problems(&joined(json!([1, 1]), 2));
    assert!(
        found.contains(&"main/op: key position 1 is defined twice".into()),
        "{found:?}"
    );
    let found = problems(&joined(json!([2]), 2));
    assert!(
        found.contains(&"main/op: a keyed op needs key position 2 inside a tuple of 2".into()),
        "{found:?}"
    );
    let scalars = json!([
        typed("left", json!("int"), "stream"),
        typed("right", json!("int"), "stream")
    ]);
    assert_eq!(
        problems(&many(
            json!({"combine": {"by": [0]}}),
            scalars.clone(),
            out.clone()
        )),
        Vec::<String>::new()
    );
    let found = problems(&many(json!({"combine": {"by": [1]}}), scalars, out.clone()));
    assert!(
        found.iter().any(|problem| problem.contains("tuple items")),
        "{found:?}"
    );
    let source = json!([typed(
        "source",
        json!({"tuple": ["string", "int"]}),
        "stream"
    )]);
    let grouped = |kind: Json| many(kind, source.clone(), out.clone());
    assert_eq!(
        problems(&grouped(json!({"group_tuple": {"size": 2}}))),
        Vec::<String>::new()
    );
    reports(
        &grouped(json!({"group_tuple": {"size": 0}})),
        "main/op: a group needs a positive size",
    );
    let found = problems(&grouped(json!({"group": {"size": 2, "size_body": "s"}})));
    let both = "main/op: a group needs size or size_body, not both";
    assert!(found.contains(&both.into()), "{found:?}");
}

#[test]
fn join_options() {
    let out = typed("out", json!("dynamic"), "stream");
    let options = |kind: Json, inputs: Json| problems(&many(kind, inputs, out.clone()));
    let both = json!({"join": {"remainder": true, "mismatch_error": true}});
    assert_eq!(
        options(both, tuples(2)),
        ["main/op: a join needs remainder or mismatch_error, not both"]
    );
    assert_eq!(
        options(json!({"join": {"remainder": true}}), tuples(2)),
        Vec::<String>::new()
    );
    let open = json!([
        typed("left", json!("dynamic"), "stream"),
        typed("right", json!("dynamic"), "stream")
    ]);
    let padded = options(json!({"join": {"remainder": true}}), open.clone());
    assert_eq!(
        padded,
        ["main/op: a join needs a body that pads the remainder of open tuples"]
    );
    let found = options(json!({"join": {"remainder": true, "body": "pad"}}), open);
    assert_eq!(
        found,
        ["main/op: no graph named pad"],
        "a body is enough, once it exists"
    );
}

/// A graph `main` with an op `op` of `kind` that fans its stream `source` to `outputs`, and
/// the callback graph `positive`.
fn fanning(kind: Json, outputs: &[&str]) -> Json {
    let ports: Vec<Json> = outputs
        .iter()
        .map(|name| typed(name, json!("int"), "stream"))
        .collect();
    let op = json!({"path": "op", "kind": kind, "inputs": [typed("source", json!("int"), "stream")], "outputs": ports});
    let mut edges = vec![edge((None, "in"), (Some("op"), "source"))];
    edges.extend(
        outputs
            .iter()
            .map(|name| edge((Some("op"), name), (None, name))),
    );
    let main = json!({"name": "main", "inputs": [typed("in", json!("int"), "stream")],
        "outputs": ports, "ops": [op], "edges": edges});
    let test = json!({"path": "test", "kind": {"expr": {"language": "js", "code": "$(inputs.item > 0)"}},
        "inputs": [port("item", "int")], "outputs": [port("out", "bool")]});
    let positive = json!({"name": "positive", "inputs": [port("item", "int")], "outputs": [port("out", "bool")],
        "ops": [test], "edges": [edge((None, "item"), (Some("test"), "item")), edge((Some("test"), "out"), (None, "out"))]});
    json!({"wir_version": wir::VERSION, "graphs": [main, positive], "entry": "main"})
}

#[test]
fn routing_outputs() {
    let route =
        |output: &str| json!({"route": {"cases": [{"output": output, "condition": "positive"}]}});
    assert_eq!(
        problems(&fanning(route("up"), &["up", "rest"])),
        Vec::<String>::new()
    );
    reports(
        &fanning(route("down"), &["up"]),
        "main/op: no output down of a route",
    );
    reports(
        &fanning(json!({"route": {"cases": []}}), &["up"]),
        "main/op: the cases of a route must not be empty",
    );
    let closure = json!({"language": "groovy", "code": "{ it }"});
    let multi = |outputs: Json| json!({"multi_map": {"outputs": outputs}});
    let both = multi(json!([{"output": "a", "body": closure}, {"output": "b", "body": closure}]));
    assert_eq!(problems(&fanning(both, &["a", "b"])), Vec::<String>::new());
    let twice = multi(json!([{"output": "a", "body": closure}, {"output": "a", "body": closure}]));
    let found = problems(&fanning(twice, &["a", "b"]));
    assert!(
        found.contains(&"main/op: a multi_map needs one body for output a".into()),
        "{found:?}"
    );
    assert!(
        found.contains(&"main/op: a multi_map needs one body for output b".into()),
        "{found:?}"
    );
}

#[test]
fn reduction_options() {
    let source = json!([typed("source", json!("int"), "stream")]);
    let reduced =
        |kind: Json, out: Json| problems(&many(kind, source.clone(), typed("out", out, "value")));
    assert_eq!(
        reduced(json!({"reduce": {"rule": "count"}}), json!("int")),
        Vec::<String>::new()
    );
    assert_eq!(
        reduced(json!({"reduce": {"rule": "fold"}}), json!("int")),
        ["main/op: a reduce needs a fold needs a body"]
    );
    let seeded = reduced(
        json!({"reduce": {"rule": "sum", "seed": {"value": 0}}}),
        json!("int"),
    );
    assert_eq!(seeded, ["main/op: a reduce needs a seed only for a fold"]);
    let nulls = reduced(
        json!({"reduce": {"rule": "min", "nulls": "keep_state"}}),
        json!("int"),
    );
    assert_eq!(
        nulls,
        ["main/op: a reduce needs a null rule only for a fold"]
    );
    let compared = reduced(
        json!({"reduce": {"rule": "sum", "compare": {"language": "groovy", "code": "{ a, b -> a <=> b }"}}}),
        json!("int"),
    );
    assert_eq!(
        compared,
        ["main/op: a reduce needs a comparison only for min and max"]
    );
    let both = json!({"reduce": {"rule": "max", "body": {"language": "groovy", "code": "{ it }"},
        "compare": {"language": "groovy", "code": "{ a, b -> a <=> b }"}}});
    assert_eq!(
        reduced(both, json!("int")),
        ["main/op: a reduce needs a key body or a comparison, not both"]
    );
    let counted = reduced(json!({"reduce": {"rule": "count"}}), json!("string"));
    assert_eq!(
        counted,
        ["main/op: output out gets \"int\" but accepts \"string\""]
    );
    let empty = reduced(json!({"first": {"empty": {"value": "none"}}}), json!("int"));
    assert_eq!(
        empty,
        ["main/op: the default of empty value of output out does not fit its type"]
    );
    let listed = reduced(json!({"to_list": {}}), json!({"array": {"items": "int"}}));
    assert_eq!(listed, Vec::<String>::new());
    let streamed = problems(&many(
        json!({"to_list": {}}),
        source.clone(),
        typed("out", json!("dynamic"), "stream"),
    ));
    assert_eq!(streamed, ["main/op: a to_list needs a value output"]);
}

#[test]
fn selection_shapes() {
    let source = json!([typed("source", json!("int"), "stream")]);
    let shaped = |kind: Json, out: Json| problems(&many(kind, source.clone(), out));
    let lists = typed("out", json!({"array": {"items": "int"}}), "stream");
    let ints = typed("out", json!("int"), "stream");
    assert_eq!(
        shaped(
            json!({"buffer": {"rule": {"size": {"size": 2}}}}),
            lists.clone()
        ),
        Vec::<String>::new()
    );
    let empty = shaped(
        json!({"buffer": {"rule": {"size": {"size": 0}}}}),
        lists.clone(),
    );
    assert_eq!(empty, ["main/op: a buffer needs a positive size"]);
    let stepped = shaped(json!({"window": {"size": 2, "step": 0}}), lists);
    assert_eq!(
        stepped,
        ["main/op: a window needs a positive size and step"]
    );
    assert_eq!(
        shaped(json!({"take": {"count": 2}}), ints.clone()),
        Vec::<String>::new()
    );
    let neither = shaped(json!({"if_empty": {}}), ints.clone());
    assert_eq!(
        neither,
        ["main/op: an if_empty needs a value or a body, exactly one"]
    );
    let unfit = shaped(
        json!({"if_empty": {"value": {"value": "none"}}}),
        ints.clone(),
    );
    assert_eq!(
        unfit,
        ["main/op: the default of fallback value of output out does not fit its type"]
    );
    let fallback = json!({"if_empty": {"body": {"language": "groovy", "code": "{ -> 0 }"}}});
    assert_eq!(shaped(fallback, ints), Vec::<String>::new());
}

#[test]
fn split_options() {
    let source = json!([typed("source", json!("string"), "stream")]);
    let split = |fields: Json| {
        let out = typed("out", json!("dynamic"), "stream");
        let mut document = many(json!({"split": fields}), source.clone(), out);
        document["graphs"][0]["ops"][0]["effects"] = json!(["write_files"]);
        problems(&document)
    };
    let needs = |why: &str| vec![format!("main/op: a split needs {why}")];
    assert_eq!(
        split(json!({"format": {"fastq": {}}, "pair": "auto", "size": 4})),
        Vec::<String>::new()
    );
    assert_eq!(
        split(json!({"format": {"text": {}}, "size": 0})),
        needs("a positive size")
    );
    assert_eq!(
        split(json!({"format": {"fasta": {}}, "pair": "auto"})),
        needs("a pair only for fastq")
    );
    assert_eq!(
        split(json!({"format": {"fastq": {}}, "pair": "auto", "elem": 0})),
        needs("a pair or an elem, not both")
    );
    assert_eq!(
        split(json!({"format": {"fastq": {}}, "pair": {"at": [1, 1]}})),
        needs("two different pair elements")
    );
    let fasta =
        json!({"format": {"fasta": {"fields": [{"name": "q", "field": "quality_string"}]}}});
    assert_eq!(
        split(fasta),
        needs("fields of its format, not QualityString")
    );
    let twice = json!({"format": {"fastq": {"fields": [{"name": "r", "field": "read_string"}, {"name": "r", "field": "text"}]}}});
    assert_eq!(split(twice), needs("one field named r"));
    let csv_files = json!({"format": {"csv": {}}, "files": {}});
    assert_eq!(split(csv_files), needs("text chunks to write files"));
    for path in ["", "a", "a.b", "samples[0].files", "[2]", "a[1][2]"] {
        assert_eq!(
            split(json!({"format": {"json": {"path": path}}})),
            Vec::<String>::new(),
            "{path}"
        );
    }
    for path in ["a..b", "a[", "a[x]", "a[]", ".a", "a]"] {
        let found = split(json!({"format": {"json": {"path": path}}}));
        assert_eq!(
            found,
            needs(&format!("a JSON path of names and indices, not {path:?}")),
            "{path}"
        );
    }
    assert_eq!(
        split(json!({"format": {"text": {}}, "charset": "ebcdic"})),
        needs("a known charset, not ebcdic")
    );
    let outside = json!({"format": {"text": {}}, "files": {"directory": "/data/chunks"}});
    assert_eq!(
        split(outside.clone()),
        needs("a publish effect to write to /data/chunks")
    );
    let mut published = many(
        json!({"split": outside}),
        source.clone(),
        typed("out", json!("dynamic"), "stream"),
    );
    let targets = [
        ("/data/chunks", true),
        ("/data/", true),
        ("chunks", false),
        ("/data/chunk", false),
        ("/data/chunks/inner", false),
        ("", false),
    ];
    for (target, names) in targets {
        published["graphs"][0]["ops"][0]["effects"] = json!(["write_files", {"publish": target}]);
        let wanted = match names {
            true => Vec::new(),
            false => needs("a publish effect to write to /data/chunks"),
        };
        assert_eq!(
            problems(&published),
            wanted,
            "the publish target {target:?}"
        );
    }
    let escaping = json!({"format": {"text": {}}, "files": {"directory": "../up", "name": "a/b"}});
    let found = split(escaping);
    assert!(
        found.contains(&needs("a folder inside the run, not ../up")[0]),
        "{found:?}"
    );
    assert!(
        found.contains(&needs("a file name, not \"a/b\"")[0]),
        "{found:?}"
    );
}

#[test]
fn effect_declarations() {
    let ints = json!([typed("source", json!("int"), "stream")]);
    let files = json!([typed("source", json!({"file": {}}), "stream")]);
    let checked = |kind: Json, inputs: &Json, out: Json, effects: Json| {
        let mut document = many(kind, inputs.clone(), out);
        document["graphs"][0]["ops"][0]["effects"] = effects;
        problems(&document)
    };
    let int_out = typed("out", json!("int"), "stream");
    let file_out = typed("out", json!({"file": {}}), "stream");
    assert_eq!(
        checked(json!({"view": {}}), &ints, int_out.clone(), json!(["log"])),
        Vec::<String>::new()
    );
    let silent = checked(json!({"view": {}}), &ints, int_out.clone(), json!([]));
    assert_eq!(silent, ["main/op: op kind view needs the effect \"log\""]);
    let collected = checked(
        json!({"collect_file": {}}),
        &ints,
        file_out.clone(),
        json!([]),
    );
    assert_eq!(
        collected,
        ["main/op: op kind collect_file needs the effect \"write_files\""]
    );
    let bucket = checked(
        json!({"collect_file": {"directory": "s3://bucket/out"}}),
        &ints,
        file_out.clone(),
        json!(["write_files", {"publish": "s3://bucket"}]),
    );
    assert_eq!(bucket, Vec::<String>::new(), "a folder that holds it");
    let outside = checked(
        json!({"collect_file": {"directory": "s3://bucket/out"}}),
        &ints,
        file_out,
        json!(["write_files", {"publish": "s3://other"}]),
    );
    assert_eq!(
        outside,
        ["main/op: a collect_file needs a publish effect to write to s3://bucket/out"]
    );
    let text = typed("out", json!("string"), "stream");
    let read = checked(
        json!({"split": {"format": {"text": {}}}}),
        &files,
        text.clone(),
        json!([]),
    );
    assert_eq!(
        read,
        ["main/op: op kind split needs the effect \"read_files\""]
    );
    let both = checked(
        json!({"split": {"format": {"text": {}}, "files": {}}}),
        &files,
        text,
        json!(["read_files", "write_files"]),
    );
    assert_eq!(both, Vec::<String>::new());
    let observed = json!({"observe": {"item": {"language": "js", "code": "x"}}});
    let found = checked(observed, &ints, int_out, json!([]));
    assert_eq!(
        found,
        ["main/op: closure: js is no closure language; use javascript"]
    );
}

/// A graph `main` whose op `op` runs the process body `body` over `inputs`, giving `outs`;
/// the body adds its inputs `n` and `k` through an expr.
fn processing(process: Json, inputs: Json, outs: Json) -> Json {
    let mut document = many(json!({"process": process}), inputs, outs);
    let add = json!({"path": "add", "kind": {"expr": {"language": "js", "code": "$(inputs.n + inputs.k)"}},
        "inputs": [port("n", "int"), port("k", "int")], "outputs": [port("out", "int")]});
    let body = json!({"name": "body", "inputs": [port("n", "int"), port("k", "int")], "outputs": [port("out", "int")],
        "ops": [add], "edges": [edge((None, "n"), (Some("add"), "n")), edge((None, "k"), (Some("add"), "k")),
            edge((Some("add"), "out"), (None, "out"))]});
    document["graphs"].as_array_mut().unwrap().push(body);
    document
}

#[test]
fn process_ports() {
    let ints = json!({"array": {"items": "int"}});
    let streamed = json!([
        typed("n", json!("int"), "stream"),
        typed("k", json!("int"), "value")
    ]);
    let out = typed("out", json!("int"), "stream");
    let good = processing(json!({"body": "body"}), streamed.clone(), out.clone());
    assert_eq!(problems(&good), Vec::<String>::new());
    let single = json!([
        typed("n", json!("int"), "value"),
        typed("k", json!("int"), "value")
    ]);
    let once = processing(
        json!({"body": "body"}),
        single.clone(),
        typed("out", json!("int"), "value"),
    );
    assert_eq!(
        problems(&once),
        Vec::<String>::new(),
        "only values give values"
    );
    let wrong = processing(json!({"body": "body"}), single.clone(), out.clone());
    reports(&wrong, "main/op: output out does not match graph body");
    let each = json!([typed("n", json!("int"), "value"), typed("k", ints, "value")]);
    let choices = processing(json!({"body": "body", "each": ["k"]}), each, out.clone());
    assert_eq!(
        problems(&choices),
        Vec::<String>::new(),
        "each takes the elements of an array"
    );
    let missing = processing(
        json!({"body": "body", "each": ["m"]}),
        streamed.clone(),
        out.clone(),
    );
    reports(&missing, "main/op: no input m");
    let zero = processing(json!({"body": "body", "parallelism": 0}), streamed, out);
    reports(&zero, "main/op: a process needs a positive parallelism");
}

/// A graph `main` of the ops `a`, `b` and `g`, which call the empty graph `leaf`, with the
/// fields of `changes` set on them.
fn controlled(changes: Json) -> Json {
    let call = |path: &str| json!({"path": path, "kind": {"call": {"graph": "leaf"}}});
    let mut ops = vec![call("a"), call("b"), call("g")];
    for op in &mut ops {
        if let Some(fields) = changes.get(op["path"].as_str().unwrap()) {
            op.as_object_mut()
                .unwrap()
                .extend(fields.as_object().cloned().unwrap());
        }
    }
    json!({"wir_version": wir::VERSION, "graphs": [{"name": "main", "ops": ops}, {"name": "leaf"}], "entry": "main"})
}

#[test]
fn control_cycles() {
    let test = |op: &str, on: Json| json!({"result": {"op": op, "on": on}});
    let good = controlled(
        json!({"b": {"depends": {"any": [test("a", json!(["failed"])), {"not": test("g", json!(["succeeded"]))}]}}}),
    );
    assert_eq!(problems(&good), Vec::<String>::new());
    let cycle = controlled(
        json!({"a": {"depends": test("b", json!(["succeeded"]))}, "b": {"depends": {"not": test("a", json!(["failed"]))}}}),
    );
    let found = problems(&cycle);
    assert!(
        found.iter().any(|problem| problem.contains("form a cycle")),
        "{found:?}"
    );
    let grouped = controlled(
        json!({"g": {"kind": {"status_group": {"members": ["a", "b"]}}},
        "a": {"depends": test("g", json!(["succeeded"]))}}),
    );
    let found = problems(&grouped);
    assert!(
        found.iter().any(|problem| problem.contains("form a cycle")),
        "members feed the group: {found:?}"
    );
    let handler = controlled(
        json!({"a": {"handles": [{"op": "b", "on": ["failed"]}]}, "b": {"after": [{"op": "a", "on": ["succeeded"]}]}}),
    );
    let found = problems(&handler);
    assert!(
        found.iter().any(|problem| problem.contains("form a cycle")),
        "{found:?}"
    );
    let missing = controlled(json!({"a": {"depends": test("z", json!(["succeeded"]))}}));
    reports(&missing, "main/a: no op z in this graph");
    let empty = controlled(json!({"a": {"depends": test("b", json!([]))}}));
    reports(&empty, "main/a: the results of b must not be empty");
}

#[test]
fn control_results() {
    let test = |op: &str, on: Json| json!({"result": {"op": op, "on": on}});
    let group = controlled(json!({"g": {"kind": {"status_group": {"members": ["a"]}}},
        "b": {"depends": test("g", json!(["any_succeeded", "all_failed"]))}}));
    assert_eq!(problems(&group), Vec::<String>::new());
    let plain = controlled(json!({"b": {"depends": test("a", json!(["all_failed"]))}}));
    reports(
        &plain,
        "main/b: a control test needs a to have the result AllFailed",
    );
    let daemon = controlled(json!({"b": {"depends": test("a", json!(["daemoned"]))}}));
    reports(
        &daemon,
        "main/b: a control test needs a to have the result Daemoned",
    );
    let own = controlled(json!({"a": {"handles": [{"op": "a", "on": ["failed"]}]}}));
    let found = problems(&own);
    assert!(
        found.contains(&"main/a: a handler needs another op to handle".into()),
        "{found:?}"
    );
    let skipped = controlled(json!({"a": {"handles": [{"op": "b", "on": ["skipped"]}]}}));
    reports(
        &skipped,
        "main/a: a handler needs failed or errored, not Skipped",
    );
    let itself = controlled(json!({"g": {"kind": {"status_group": {"members": ["g", "a", "a"]}}}}));
    let found = problems(&itself);
    assert!(
        found.contains(&"main/g: a status group needs members besides itself".into()),
        "{found:?}"
    );
    assert!(
        found.contains(&"main/g: member a is defined twice".into()),
        "{found:?}"
    );
}

#[test]
fn task_forms() {
    let task = |capture: Json, out: Json, extra: Json| {
        let mut kind = json!({"command": ["run"], "captures": [capture]});
        kind.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap());
        let op =
            json!({"path": "t", "kind": {"task": kind}, "outputs": [{"name": "out", "type": out}]});
        problems(
            &json!({"wir_version": wir::VERSION, "graphs": [{"name": "main", "ops": [op]}], "entry": "main"}),
        )
    };
    let files = json!({"array": {"items": {"file": {}}}});
    let glob = |arity: Json| json!({"output": "out", "from": {"glob": "*"}, "arity": arity});
    assert_eq!(
        task(glob(json!({"min": 1, "max": 3})), files.clone(), json!({})),
        Vec::<String>::new()
    );
    let reversed = task(glob(json!({"min": 3, "max": 1})), files, json!({}));
    assert_eq!(
        reversed,
        ["main/t: a capture needs output out: a least count no more than the most"]
    );
    let scalar = task(
        glob(json!({"min": 0, "max": 2})),
        json!({"optional": {"file": {}}}),
        json!({}),
    );
    assert_eq!(
        scalar,
        ["main/t: a capture needs output out: an array to hold more than one path"]
    );
    let accept = json!({"accept": {"language": "wdl", "code": "true"}});
    let found = task(glob(json!({"min": 1})), json!({"file": {}}), accept);
    assert_eq!(found, ["main/t: closure: a wdl closure names 1 parameters"]);
}

/// A graph `main` with a dialect task `t` of `builder` in a source of `language`, with the
/// input `reads` and the output `bam`.
fn dialect(language: &str, builder: Json, extra: Json) -> Vec<String> {
    let mut task = json!({"builder": builder});
    task.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap());
    let op = json!({"path": "t", "kind": {"task": task},
        "inputs": [port("reads", "string")], "outputs": [port("bam", "string")]});
    let main = json!({"name": "main", "inputs": [port("reads", "string")], "outputs": [port("bam", "string")],
        "ops": [op], "edges": [edge((None, "reads"), (Some("t"), "reads")), edge((Some("t"), "bam"), (None, "bam"))]});
    let sources = json!([{"id": "s", "language": language, "location": "main", "digest": "sha256:ab"},
        {"id": "loose", "language": language, "location": "other"}]);
    problems(
        &json!({"wir_version": wir::VERSION, "sources": sources, "graphs": [main], "entry": "main"}),
    )
}

#[test]
fn dialect_tasks() {
    let reference = |source: &str, input: &str, output: &str| {
        json!({"source": source, "entry": "main", "call": "ALIGN",
        "inputs": [{"port": "reads", "native": input}], "outputs": [{"port": "bam", "native": output}]})
    };
    let nextflow = json!({"nextflow_process": reference("s", "input/0", "output/1")});
    assert_eq!(
        dialect("nextflow", nextflow.clone(), json!({})),
        Vec::<String>::new()
    );
    let mixed = dialect("nextflow", nextflow.clone(), json!({"command": ["run"]}));
    assert_eq!(
        mixed,
        ["main/t: a task needs no command, capture, acceptance or retry fields beside a builder"]
    );
    let wrong = dialect("snakemake", nextflow, json!({}));
    assert_eq!(
        wrong,
        ["main/t: language \"nextflow\" differs from that of source s, \"snakemake\""]
    );
    let selector = dialect(
        "nextflow",
        json!({"nextflow_process": reference("s", "params/x", "output/1")}),
        json!({}),
    );
    assert_eq!(
        selector,
        ["main/t: a port map needs a \"nextflow\" selector, not \"params/x\""]
    );
    let rule = json!({"snakemake_job": {"task": reference("s", "params/genome", "output/0")}});
    assert_eq!(dialect("snakemake", rule, json!({})), Vec::<String>::new());
    let argo = json!({"argo_task": reference("s", "artifact/reads", "parameter/bam")});
    assert_eq!(dialect("argo", argo, json!({})), Vec::<String>::new());
    let unpinned = dialect(
        "wdl",
        json!({"wdl_task": reference("loose", "reads", "bam")}),
        json!({}),
    );
    assert_eq!(
        unpinned,
        ["main/t: a dialect task needs a digest that pins source loose"]
    );
    let mut unmapped = reference("s", "reads", "bam");
    unmapped["outputs"] = json!([]);
    let found = dialect("wdl", json!({"wdl_task": unmapped}), json!({}));
    assert_eq!(found, ["main/t: a dialect task needs a map of output bam"]);
}

#[test]
fn snakemake_plans() {
    let job = |extra: Json| {
        let mut rule = json!({"task": {"source": "s", "entry": "all", "call": "map",
            "inputs": [{"port": "reads", "native": "input/0"}],
            "outputs": [{"port": "bam", "native": "output/0"}]}, "wildcards": {"n": "1"}});
        rule.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap());
        dialect("snakemake", json!({"snakemake_job": rule}), json!({}))
    };
    let good = job(
        json!({"targets": ["all", "b.txt"], "config_values": "s", "forcerun": ["map"],
        "group": [{"source": "s", "rule": "sort", "wildcards": {"n": "1"}}]}),
    );
    assert_eq!(good, Vec::<String>::new());
    let found = job(json!({"targets": ["b.txt", ""], "config_values": "loose",
        "group": [{"source": "t", "rule": "map", "wildcards": {"n": "1"}}]}));
    assert_eq!(
        found,
        [
            "main/t: a dialect task needs a digest that pins source loose",
            "main/t: a Snakemake job needs targets that start with its entry",
            "main/t: a Snakemake job needs targets that are not empty",
            "main/t: a Snakemake job needs a rule of source s in its group",
            "main/t: a Snakemake job needs job map once in its group",
        ]
    );
}

/// A graph `main` with a loop `op` over the body `count`, which adds one to `n` and says
/// `again` while it is below three.
fn looping(looped: Json, inputs: Json, outputs: Json) -> Json {
    let mut document = many(json!({"loop": looped}), inputs, outputs);
    let add = json!({"path": "add", "kind": {"expr": {"language": "js", "code": "$(inputs.n + 1)"}},
        "inputs": [port("n", "int")], "outputs": [port("out", "int")]});
    let test = json!({"path": "test", "kind": {"expr": {"language": "js", "code": "$(inputs.n < 2)"}},
        "inputs": [port("n", "int")], "outputs": [port("out", "bool")]});
    let body = json!({"name": "count", "inputs": [port("n", "int")], "outputs": [port("n", "int"), port("again", "bool")],
        "ops": [add, test], "edges": [edge((None, "n"), (Some("add"), "n")), edge((None, "n"), (Some("test"), "n")),
            edge((Some("add"), "out"), (None, "n")), edge((Some("test"), "out"), (None, "again"))]});
    document["graphs"].as_array_mut().unwrap().push(body);
    document["graphs"][0]["edges"][1]["from"]["port"] = outputs_name(&document);
    document
}

fn outputs_name(document: &Json) -> Json {
    document["graphs"][0]["ops"][0]["outputs"][0]["name"].clone()
}

#[test]
fn loop_bodies() {
    let carry = json!([{"output": "n", "input": "n"}]);
    let input = json!([port("n", "int")]);
    let good = looping(
        json!({"body": "count", "condition": "again", "carry": carry, "limit": 5}),
        input.clone(),
        port("n", "int"),
    );
    assert_eq!(problems(&good), Vec::<String>::new());
    let per_round = looping(
        json!({"body": "count", "condition": "again", "carry": carry}),
        input.clone(),
        typed("n", json!("int"), "stream"),
    );
    assert_eq!(
        problems(&per_round),
        Vec::<String>::new(),
        "a stream output gives one item per run"
    );
    let zero = looping(
        json!({"body": "count", "condition": "again", "limit": 0}),
        input.clone(),
        port("n", "int"),
    );
    reports(&zero, "main/op: a loop needs a positive limit");
    let unconditioned = looping(
        json!({"body": "count", "condition": "n"}),
        input.clone(),
        port("n", "int"),
    );
    let found = problems(&unconditioned);
    assert!(
        found.contains(&"main/op: a loop needs a bool body output n".into()),
        "{found:?}"
    );
    let twice = looping(
        json!({"body": "count", "condition": "again", "carry": [{"output": "n", "input": "n"},
        {"output": "n", "input": "n"}]}),
        input,
        port("n", "int"),
    );
    reports(&twice, "main/op: a loop needs one carry into n");
    let unfed = looping(
        json!({"body": "count", "condition": "again"}),
        json!([port("m", "int")]),
        port("n", "int"),
    );
    let found = problems(&unfed);
    assert!(
        found.contains(&"main/op: a loop needs an input n for the first run".into()),
        "{found:?}"
    );
}

#[test]
fn dynamic_boundary() {
    let plans = json!([typed("plans", json!("dynamic"), "stream")]);
    let out = typed("out", json!("dynamic"), "stream");
    let region = |dynamic: Json, inputs: &Json| {
        problems(&many(
            json!({"dynamic": dynamic}),
            inputs.clone(),
            out.clone(),
        ))
    };
    assert_eq!(
        region(json!({"plan": "plans"}), &plans),
        Vec::<String>::new()
    );
    let typed_plan = json!([typed("plans", json!("string"), "stream")]);
    assert_eq!(
        region(json!({"plan": "plans"}), &typed_plan),
        ["main/op: a dynamic region needs a dynamic plan input plans"]
    );
    assert_eq!(
        region(json!({}), &plans),
        ["main/op: a dynamic region needs a plan input or a planner, exactly one"]
    );
    let planner = json!({"planner": {"snakemake": {"source": "smk"}}});
    assert_eq!(region(planner, &plans), ["main/op: no source named smk"]);
}

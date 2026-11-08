//! Checks mix, concat, round merge and zip on streams whose batches the tests commit.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value as Json, json};
use wir::{Status, Subscription};

use crate::engine::Context;
use crate::flows::output_stream;
use crate::operator::{Operator, Step, Turn, make};
use crate::plan::Node;
use crate::streams::{Failure, ReaderId, StreamBatch, StreamEnd, StreamId, Streams};

/// A node `op` of `kind` with the stream inputs `streams`, value inputs `values` and the
/// output `out` of that flow.
pub fn node(kind: Json, streams: &[&str], values: &[&str], out: &str) -> Node {
    let port = |name: &&str, flow: &str| json!({"name": name, "type": "dynamic", "flow": flow});
    let mut inputs: Vec<Json> = streams.iter().map(|name| port(name, "stream")).collect();
    inputs.extend(values.iter().map(|name| port(name, "value")));
    let op = json!({"path": "op", "kind": kind, "inputs": inputs, "outputs": [port(&"out", out)]});
    Node {
        path: "op".into(),
        step: "op".into(),
        op: serde_json::from_value(op).unwrap(),
        inputs: Vec::new(),
        after: Vec::new(),
        parent: None,
        exports: Vec::new(),
        shape: Vec::new(),
        defaults: Map::new(),
        live: Vec::new(),
        depends: None,
        handles: Vec::new(),
        members: Vec::new(),
    }
}

/// The streams of the inputs of `node`, each with its reader.
pub fn wired(node: &Node) -> Streams {
    let mut streams = Streams::default();
    for port in &node.op.outputs {
        streams.open(output_stream("op", &port.name), "op");
    }
    for port in node.op.inputs.iter().filter(|port| !port.flow.is_value()) {
        let stream = StreamId(port.name.clone());
        streams.open(stream.clone(), "up");
        let reader = ReaderId {
            owner: "op".into(),
            input: port.name.clone(),
        };
        streams
            .subscribe(&stream, reader, Subscription::All)
            .unwrap();
    }
    streams
}

/// Commits items of streams, and the ends of `closed`, in one batch.
pub fn commit(streams: &mut Streams, items: &[(&str, Json)], closed: &[&str]) {
    let batch = StreamBatch {
        items: items
            .iter()
            .map(|(name, value)| (StreamId((*name).into()), value.clone()))
            .collect(),
        ends: closed
            .iter()
            .map(|name| (StreamId((*name).into()), StreamEnd::Close))
            .collect(),
        ..StreamBatch::default()
    };
    streams.commit(batch).unwrap();
}

/// Steps an op until it waits or ends: what it emitted, bound, and how its last step went.
pub fn steps(
    operator: &mut dyn Operator,
    node: &Node,
    streams: &mut Streams,
    values: &Map<String, Json>,
) -> (Vec<Json>, Option<Json>, Result<Step, Failure>) {
    steps_with(
        operator,
        node,
        streams,
        values,
        &crate::driver::Printer::default(),
    )
}

/// What the ops of a test read besides their inputs: a file area and a files provider.
pub struct Fixture {
    pub document: wir::Document,
    pub javascript: cwl_runtime::JavaScriptSettings,
    pub scopes: HashMap<String, String>,
    pub printer: crate::driver::Printer,
    pub files: PathBuf,
    pub provider: Option<Arc<dyn workflow_graph::Files>>,
    pub deliveries: crate::publishing::Deliveries,
    pub inputs: Map<String, Json>,
    pub nextflow: crate::nextflow_scope::ScopeRun,
}

impl Default for Fixture {
    fn default() -> Fixture {
        let document =
            json!({"wir_version": "0.1", "graphs": [{"name": "g"}], "entry": "g"}).to_string();
        Fixture {
            document: wir::parse(&document).unwrap(),
            javascript: Default::default(),
            scopes: HashMap::new(),
            printer: Default::default(),
            files: PathBuf::from("/nonexistent"),
            provider: None,
            deliveries: Default::default(),
            inputs: Map::new(),
            nextflow: Default::default(),
        }
    }
}

impl Fixture {
    pub fn context(&self) -> Context<'_> {
        Context {
            document: &self.document,
            base: Path::new("/"),
            javascript: &self.javascript,
            scopes: &self.scopes,
            files: &self.files,
            printer: &self.printer,
            simulated: false,
            provider: self.provider.as_deref(),
            deliveries: &self.deliveries,
            inputs: &self.inputs,
            nextflow: &self.nextflow,
        }
    }
}

/// A files provider whose locations `store://<name>` are files in its folder.
pub struct Store(pub PathBuf);

impl workflow_graph::Files for Store {
    fn local(&self, location: &str) -> std::io::Result<PathBuf> {
        let name = location.strip_prefix("store://");
        name.map(|name| self.0.join(name))
            .ok_or_else(|| std::io::Error::other(format!("no store location: {location}")))
    }

    fn deliver(&self, _: &workflow_graph::Delivery) -> std::io::Result<()> {
        Err(std::io::Error::other("the store delivers nothing"))
    }
}

/// The steps of [`steps`] with a printer for `view`.
pub fn steps_with(
    operator: &mut dyn Operator,
    node: &Node,
    streams: &mut Streams,
    values: &Map<String, Json>,
    printer: &crate::driver::Printer,
) -> (Vec<Json>, Option<Json>, Result<Step, Failure>) {
    let fixture = Fixture::default();
    let context = Context {
        printer,
        ..fixture.context()
    };
    steps_in(operator, node, streams, values, &context)
}

/// The steps of [`steps`] in a context.
pub fn steps_in(
    operator: &mut dyn Operator,
    node: &Node,
    streams: &mut Streams,
    values: &Map<String, Json>,
    context: &Context<'_>,
) -> (Vec<Json>, Option<Json>, Result<Step, Failure>) {
    let (mut emitted, mut bound) = (Vec::new(), None);
    loop {
        let mut turn = Turn::new(node, values.clone(), streams);
        let step = operator.step(&mut turn, context);
        let Turn {
            batch,
            bound: values_out,
            ..
        } = turn;
        emitted.extend(batch.items.iter().map(|(_, value)| value.clone()));
        bound = bound.or_else(|| values_out.get("out").cloned());
        streams.commit(batch).unwrap();
        match step {
            Ok(Step::Moved) => continue,
            other => return (emitted, bound, other),
        }
    }
}

#[test]
fn mix_arrivals() {
    let node = node(json!({"mix": {}}), &["a", "b"], &[], "stream");
    let mut streams = wired(&node);
    let mut operator = make(&node).unwrap();
    commit(&mut streams, &[("a", json!(1))], &[]);
    commit(&mut streams, &[("b", json!(10)), ("b", json!(11))], &[]);
    commit(&mut streams, &[("a", json!(2))], &["a"]);
    let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
    assert_eq!(
        emitted,
        [json!(1), json!(10), json!(11), json!(2)],
        "arrival order, each source in order"
    );
    assert_eq!(step, Ok(Step::Idle), "b is still open");
    commit(&mut streams, &[], &["b"]);
    assert_eq!(
        steps(operator.as_mut(), &node, &mut streams, &Map::new()).2,
        Ok(Step::Done)
    );
    let mut streams = wired(&node);
    let mut operator = make(&node).unwrap();
    let failure = Failure {
        status: Status::Failed,
        message: "a broke".into(),
        source: "up".into(),
    };
    streams
        .commit(StreamBatch {
            items: vec![(StreamId("a".into()), json!(1))],
            ends: vec![(StreamId("a".into()), StreamEnd::Error(failure))],
            ..StreamBatch::default()
        })
        .unwrap();
    let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
    assert_eq!(
        (emitted, step.unwrap_err().message),
        (vec![json!(1)], "a broke".to_owned()),
        "the items before an error first"
    );
}

#[test]
fn concat_sources() {
    let node = node(json!({"concat": {}}), &["a", "b"], &[], "stream");
    let mut streams = wired(&node);
    let mut operator = make(&node).unwrap();
    commit(&mut streams, &[("b", json!(3)), ("a", json!(1))], &["b"]);
    let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
    assert_eq!(
        (emitted, step),
        (vec![json!(1)], Ok(Step::Idle)),
        "b waits for a"
    );
    commit(&mut streams, &[("a", json!(2))], &["a"]);
    let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
    assert_eq!((emitted, step), (vec![json!(2), json!(3)], Ok(Step::Done)));
    let mut streams = wired(&node);
    let mut operator = make(&node).unwrap();
    let failure = Failure {
        status: Status::Failed,
        message: "b broke".into(),
        source: "up".into(),
    };
    let ends = vec![(StreamId("b".into()), StreamEnd::Error(failure))];
    streams
        .commit(StreamBatch {
            ends,
            ..StreamBatch::default()
        })
        .unwrap();
    let (_, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
    assert_eq!(
        step.unwrap_err().message,
        "b broke",
        "a later input that fails fails the op"
    );
}

#[test]
fn merge_rounds() {
    let node = node(json!({"merge": {}}), &["a", "b"], &["c"], "stream");
    let mut streams = wired(&node);
    let mut operator = make(&node).unwrap();
    let values = Map::from_iter([("c".into(), json!(5))]);
    commit(
        &mut streams,
        &[
            ("a", json!([1])),
            ("b", json!(10)),
            ("b", json!(20)),
            ("b", json!(30)),
        ],
        &["b"],
    );
    let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &values);
    assert_eq!(
        (emitted, step),
        (vec![json!([1, 10, 5])], Ok(Step::Idle)),
        "a round waits for every stream"
    );
    commit(&mut streams, &[("a", json!([2]))], &["a"]);
    let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &values);
    assert_eq!(
        (emitted, step),
        (vec![json!([2, 20, 5])], Ok(Step::Done)),
        "the first closed stream ends it"
    );
    let mut single = self::node(json!({"merge": {"rounds": true}}), &["b"], &["c"], "value");
    single.op.inputs.reverse();
    let mut streams = wired(&single);
    let mut operator = make(&single).unwrap();
    commit(&mut streams, &[("b", json!(10)), ("b", json!(20))], &[]);
    let (emitted, bound, step) = steps(operator.as_mut(), &single, &mut streams, &values);
    assert_eq!(
        (emitted, bound, step),
        (vec![], Some(json!([5, 10])), Ok(Step::Done)),
        "a value first takes one round"
    );
}

#[test]
fn zip_remainder() {
    for (remainder, expected) in [
        (false, json!([[1, 10]])),
        (true, json!([[1, 10], [2, null]])),
    ] {
        let node = node(
            json!({"zip": {"remainder": remainder}}),
            &["a", "b"],
            &[],
            "stream",
        );
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        commit(
            &mut streams,
            &[("a", json!(1)), ("a", json!(2)), ("b", json!(10))],
            &["a", "b"],
        );
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        assert_eq!((Json::Array(emitted), step), (expected, Ok(Step::Done)));
    }
}

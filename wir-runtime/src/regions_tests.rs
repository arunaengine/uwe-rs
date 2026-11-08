//! Checks dynamic regions: plan revisions, instances that stay, are retired or are added,
//! and the final plan that settles.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;
use wir::Kind;

use super::*;

/// A plan update of `revision` whose graph has the native ops `ops`, with `fields` set on
/// the op of each name.
fn update(revision: u64, ops: &[&str], fields: Json, final_plan: bool) -> Json {
    let ops: Vec<Json> = (ops.iter())
        .map(|path| {
            let mut op =
                json!({"path": path, "kind": {"native": {"source": "s", "language": "cwl"}}});
            for (name, value) in fields
                .get(path)
                .and_then(Json::as_object)
                .into_iter()
                .flatten()
            {
                op[name] = value.clone();
            }
            op
        })
        .collect();
    json!({"revision": revision, "graph": {"name": "p", "ops": ops}, "final_plan": final_plan})
}

/// The document of one region `r` whose plan is its value input, its progress with `r`
/// running, and the region.
fn region() -> (Document, Progress, Region) {
    let port = json!({"name": "plan", "type": "dynamic"});
    let op = json!({"path": "r", "kind": {"dynamic": {"plan": "plan"}}, "inputs": [port]});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "s", "language": "cwl", "location": "s.cwl"}],
        "graphs": [{"name": "main", "ops": [op]}], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    let mut progress = Progress::new(
        plan::plan(&document, &Adapters::default()).unwrap(),
        Vec::new(),
    );
    progress.activate(None).unwrap();
    let (at, bound) = progress.next(false).unwrap();
    let Kind::Dynamic(spec) = &progress.node(at).op.kind else {
        panic!("no region");
    };
    let region = Region::new(spec.clone(), bound);
    (document, progress, region)
}

fn node(progress: &Progress, path: &str) -> usize {
    (0..progress.plan.nodes.len())
        .rev()
        .find(|at| progress.node(*at).path == path)
        .unwrap()
}

#[test]
fn reconciles_instances() {
    let (document, mut progress, mut region) = region();
    let checks = (&document, &Adapters::default());
    region
        .adopt(
            &mut progress,
            checks,
            0,
            update(0, &["a", "b"], json!({}), false),
        )
        .unwrap();
    let (a, _) = progress.next(false).unwrap();
    assert_eq!(progress.node(a).path, "r/0/a");
    let b = node(&progress, "r/0/b");
    region
        .adopt(
            &mut progress,
            checks,
            0,
            update(1, &["a", "c"], json!({}), false),
        )
        .unwrap();
    assert_eq!(node(&progress, "r/0/a"), a, "a started instance stays");
    assert!(
        matches!(progress.states[b], State::Ended(Status::Cancelled, _)),
        "b is retired"
    );
    assert!(progress.failures.is_empty(), "retiring fails nothing");
    let again = update(1, &["a", "c"], json!({}), false);
    assert!(
        region.adopt(&mut progress, checks, 0, again).is_ok(),
        "a repeat is no change"
    );
    let changed = region.adopt(
        &mut progress,
        checks,
        0,
        update(1, &["a"], json!({}), false),
    );
    assert_eq!(changed.unwrap_err(), "revision 1 came twice, changed");
    let skipped = region.adopt(
        &mut progress,
        checks,
        0,
        update(3, &["a"], json!({}), false),
    );
    assert_eq!(skipped.unwrap_err(), "revision 3 came after 1");
    let after = json!({"a": {"after": [{"op": "c", "on": ["succeeded"]}]}});
    let moved = region.adopt(
        &mut progress,
        checks,
        0,
        update(2, &["a", "c"], after, true),
    );
    assert!(
        moved.unwrap_err().starts_with("the started instance"),
        "a started one keeps its definition"
    );
}

#[test]
fn final_plan() {
    let (document, mut progress, mut region) = region();
    // The value plan input is read once; this test adopts its plans itself.
    region.given(&mut progress, 0).unwrap();
    let checks = (&document, &Adapters::default());
    let mut regions = Regions::new();
    region
        .adopt(&mut progress, checks, 0, update(0, &["a"], json!({}), true))
        .unwrap();
    let late = region.adopt(&mut progress, checks, 0, update(1, &["a"], json!({}), true));
    assert_eq!(late.unwrap_err(), "revision 1 came after the final plan");
    regions.insert(0, region);
    assert!(
        !settle(&mut progress, &mut regions),
        "a waits for its instances"
    );
    let (a, _) = progress.next(false).unwrap();
    progress.end(a, Status::Succeeded, Map::new(), None);
    assert!(settle(&mut progress, &mut regions));
    assert!(matches!(
        progress.states[0],
        State::Ended(Status::Succeeded, _)
    ));
}

#[test]
fn plan_stream() {
    let stream = json!({"name": "out", "type": "dynamic", "flow": "stream"});
    let src = json!({"path": "src", "kind": {"emit": {}}, "inputs": [{"name": "source", "type": "dynamic"}],
        "outputs": [stream]});
    let region = json!({"path": "r", "kind": {"dynamic": {"plan": "plans"}},
        "inputs": [{"name": "plans", "type": "dynamic", "flow": "stream"}]});
    let edges = json!([{"from": {"op": "src", "port": "out"}, "to": {"op": "r", "port": "plans"}}]);
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "s", "language": "cwl", "location": "s.cwl"}],
        "graphs": [{"name": "main", "ops": [src, region], "edges": edges}], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    let mut progress = Progress::new(
        plan::plan(&document, &Adapters::default()).unwrap(),
        Vec::new(),
    );
    progress.activate(None).unwrap();
    progress.next(false).unwrap();
    let (at, bound) = progress.next(false).unwrap();
    let Kind::Dynamic(spec) = &progress.node(at).op.kind else {
        panic!("no region");
    };
    let mut regions = Regions::from([(at, Region::new(spec.clone(), bound))]);
    let update = update(0, &["a"], json!({}), true);
    crate::mixing::tests::commit(&mut progress.streams, &[("src#out", update)], &[]);
    let checks = (&document, &Adapters::default());
    let region = regions.get_mut(&at).unwrap();
    for given in region.given(&mut progress, at).unwrap() {
        region.adopt(&mut progress, checks, at, given).unwrap();
    }
    let (a, _) = progress.next(false).unwrap();
    progress.end(a, Status::Succeeded, Map::new(), None);
    assert!(
        !settle(&mut progress, &mut regions),
        "the plan input is open still"
    );
    crate::mixing::tests::commit(&mut progress.streams, &[], &["src#out"]);
    let region = regions.get_mut(&at).unwrap();
    assert!(region.given(&mut progress, at).unwrap().is_empty());
    assert!(
        settle(&mut progress, &mut regions),
        "the region settles once its plans ended"
    );
}

#[test]
fn forwarded_prefix() {
    let ints = json!({"array": {"items": "int"}});
    let ys = json!({"name": "ys", "type": "int", "flow": "stream"});
    let op = json!({"path": "r", "kind": {"dynamic": {"plan": "plan"}},
        "inputs": [{"name": "plan", "type": "dynamic"}], "outputs": [ys]});
    let document =
        json!({"wir_version": "0.1", "graphs": [{"name": "main", "ops": [op]}], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    let mut progress = Progress::new(
        plan::plan(&document, &Adapters::default()).unwrap(),
        Vec::new(),
    );
    progress.activate(None).unwrap();
    let (at, bound) = progress.next(false).unwrap();
    let Kind::Dynamic(spec) = &progress.node(at).op.kind else {
        panic!("no region");
    };
    let mut region = Region::new(spec.clone(), bound);
    let emit = json!({"path": "e", "kind": {"emit": {"each": true}},
        "inputs": [{"name": "source", "type": ints, "default": [1]}], "outputs": [{"name": "out", "type": "int", "flow": "stream"}]});
    let graph = json!({"name": "p", "outputs": [ys], "ops": [emit],
        "edges": [{"from": {"op": "e", "port": "out"}, "to": {"port": "ys"}}]});
    let update = json!({"revision": 0, "graph": graph, "final_plan": true});
    region
        .adopt(&mut progress, (&document, &Adapters::default()), at, update)
        .unwrap();
    let watcher = ReaderId {
        owner: "test".into(),
        input: "ys".into(),
    };
    let output = crate::flows::output_stream("r", "ys");
    progress
        .streams
        .subscribe(&output, watcher.clone(), wir::Subscription::All)
        .unwrap();
    let produced = crate::flows::output_stream("r/0/e", "out");
    let failure = crate::streams::Failure {
        status: Status::Failed,
        message: "broke".into(),
        source: "r/0/e".into(),
    };
    let batch = crate::streams::StreamBatch {
        event: 0,
        items: vec![(produced.clone(), json!(1)), (produced.clone(), json!(2))],
        ends: vec![(produced, StreamEnd::Error(failure))],
    };
    progress.streams.commit(batch).unwrap();
    assert_eq!(region.forward(&mut progress, at).unwrap_err(), "broke");
    let mut seen = Vec::new();
    while let Ok(Read::Item(_)) = progress.streams.peek(&watcher) {
        seen.extend(
            progress
                .streams
                .consume(std::slice::from_ref(&watcher))
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(
        seen,
        [json!(1), json!(2)],
        "the prefix leaves before the error"
    );
}

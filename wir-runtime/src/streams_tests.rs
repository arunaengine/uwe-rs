//! Checks the streams of a run: readers, late readers, ends, null items, errors after a
//! prefix, detaching, cancelling and batches that the log refuses.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;

use super::*;

fn stream() -> StreamId {
    StreamId("a#out".into())
}

fn reader(owner: &str) -> ReaderId {
    ReaderId {
        owner: owner.into(),
        input: "source".into(),
    }
}

fn items(values: &[Json]) -> StreamBatch {
    StreamBatch {
        items: values
            .iter()
            .map(|value| (stream(), value.clone()))
            .collect(),
        ..StreamBatch::default()
    }
}

fn close() -> StreamBatch {
    StreamBatch {
        ends: vec![(stream(), StreamEnd::Close)],
        ..StreamBatch::default()
    }
}

/// Reads everything a reader can see now: its items, and its end if it has one.
fn drain(streams: &mut Streams, reader: &ReaderId) -> (Vec<Json>, Option<StreamEnd>) {
    let mut seen = Vec::new();
    loop {
        match streams.peek(reader).unwrap() {
            Read::Item(_) => seen.extend(
                streams
                    .consume(std::slice::from_ref(reader))
                    .unwrap()
                    .unwrap(),
            ),
            Read::End(terminal) => return (seen, Some(terminal.end)),
            Read::Pending => return (seen, None),
        }
    }
}

fn opened() -> Streams {
    let mut streams = Streams::default();
    streams.open(stream(), "a");
    streams
}

#[test]
fn reader_broadcast() {
    let mut streams = opened();
    for name in ["b", "c"] {
        streams
            .subscribe(&stream(), reader(name), Subscription::All)
            .unwrap();
    }
    streams.commit(items(&[json!(1), json!(2)])).unwrap();
    assert_eq!(
        drain(&mut streams, &reader("b")),
        (vec![json!(1), json!(2)], None)
    );
    streams.commit(items(&[json!(3)])).unwrap();
    streams.commit(close()).unwrap();
    assert_eq!(
        drain(&mut streams, &reader("b")),
        (vec![json!(3)], Some(StreamEnd::Close))
    );
    let all = drain(&mut streams, &reader("c"));
    assert_eq!(
        all,
        (vec![json!(1), json!(2), json!(3)], Some(StreamEnd::Close)),
        "each reader sees every item once"
    );
}

#[test]
fn reader_late_all() {
    let mut streams = opened();
    streams.commit(items(&[json!("x"), json!("y")])).unwrap();
    streams.commit(close()).unwrap();
    streams
        .subscribe(&stream(), reader("late"), Subscription::All)
        .unwrap();
    let seen = drain(&mut streams, &reader("late"));
    assert_eq!(seen, (vec![json!("x"), json!("y")], Some(StreamEnd::Close)));
}

#[test]
fn reader_late_live() {
    let mut streams = opened();
    streams.commit(items(&[json!(1), json!(2)])).unwrap();
    streams
        .subscribe(&stream(), reader("live"), Subscription::Live)
        .unwrap();
    streams.commit(items(&[json!(3)])).unwrap();
    // Installing it again keeps the cursor of its first installation.
    streams
        .subscribe(&stream(), reader("live"), Subscription::Live)
        .unwrap();
    streams.commit(close()).unwrap();
    assert_eq!(
        drain(&mut streams, &reader("live")),
        (vec![json!(3)], Some(StreamEnd::Close))
    );
    streams
        .subscribe(&stream(), reader("after"), Subscription::Live)
        .unwrap();
    assert_eq!(
        drain(&mut streams, &reader("after")),
        (vec![], Some(StreamEnd::Close)),
        "a late live reader sees the end"
    );
}

#[test]
fn stream_terminal() {
    let failure = Failure {
        status: Status::Failed,
        message: "broke".into(),
        source: "a".into(),
    };
    for end in [
        StreamEnd::Close,
        StreamEnd::Error(failure),
        StreamEnd::Cancelled,
    ] {
        let mut streams = opened();
        streams
            .subscribe(&stream(), reader("b"), Subscription::All)
            .unwrap();
        let batch = StreamBatch {
            ends: vec![(stream(), end.clone())],
            ..StreamBatch::default()
        };
        streams.commit(batch).unwrap();
        assert_eq!(drain(&mut streams, &reader("b")), (vec![], Some(end)));
        assert!(streams.commit(close()).is_err(), "a stream ends once");
        assert!(
            streams.commit(items(&[json!(1)])).is_err(),
            "an ended stream takes no item"
        );
    }
}

#[test]
fn null_is_item() {
    let mut streams = opened();
    streams
        .subscribe(&stream(), reader("b"), Subscription::All)
        .unwrap();
    streams.commit(items(&[Json::Null])).unwrap();
    streams.commit(close()).unwrap();
    assert_eq!(
        drain(&mut streams, &reader("b")),
        (vec![Json::Null], Some(StreamEnd::Close))
    );
}

#[test]
fn stream_prefix_error() {
    let mut streams = opened();
    streams
        .subscribe(&stream(), reader("b"), Subscription::All)
        .unwrap();
    let failure = Failure {
        status: Status::Errored,
        message: "lost".into(),
        source: "a".into(),
    };
    let batch = StreamBatch {
        items: vec![(stream(), json!(1))],
        ends: vec![(stream(), StreamEnd::Error(failure.clone()))],
        ..StreamBatch::default()
    };
    streams.commit(batch).unwrap();
    let seen = drain(&mut streams, &reader("b"));
    assert_eq!(
        seen,
        (vec![json!(1)], Some(StreamEnd::Error(failure))),
        "the prefix comes before the error"
    );
}

#[test]
fn reader_detach() {
    let mut streams = opened();
    for name in ["take", "rest"] {
        streams
            .subscribe(&stream(), reader(name), Subscription::All)
            .unwrap();
    }
    streams.commit(items(&[json!(1)])).unwrap();
    streams.detach(&reader("take"));
    assert!(streams.peek(&reader("take")).is_err());
    streams.commit(items(&[json!(2)])).unwrap();
    assert_eq!(
        drain(&mut streams, &reader("rest")),
        (vec![json!(1), json!(2)], None),
        "the others go on"
    );
}

#[test]
fn stream_cancel() {
    let mut streams = opened();
    streams.open(StreamId("a/inner#out".into()), "a/inner");
    streams.open(StreamId("other#out".into()), "other");
    streams
        .subscribe(&stream(), reader("a/inner"), Subscription::All)
        .unwrap();
    streams.cancel("a").unwrap();
    let ended = |id: &str| {
        streams.channels[&StreamId(id.into())]
            .terminal
            .clone()
            .map(|terminal| terminal.end)
    };
    assert_eq!(ended("a#out"), Some(StreamEnd::Cancelled));
    assert_eq!(ended("a/inner#out"), Some(StreamEnd::Cancelled));
    assert_eq!(ended("other#out"), None, "only the cancelled region ends");
    assert!(
        streams.peek(&reader("a/inner")).is_err(),
        "its readers stop"
    );
}

/// A log that refuses every append.
struct Broken;

impl StreamLog for Broken {
    fn append(&mut self, _batch: &[(StreamId, StreamItem)]) -> Result<(), DriverError> {
        Err(DriverError::new("the disk is full"))
    }

    fn item(&self, _stream: &StreamId, _sequence: u64) -> Result<Option<StreamItem>, DriverError> {
        Ok(None)
    }
}

#[test]
fn stream_batch_atomic() {
    let mut streams = Streams::new(Box::new(Broken));
    streams.open(stream(), "a");
    streams
        .subscribe(&stream(), reader("b"), Subscription::All)
        .unwrap();
    let batch = StreamBatch {
        items: vec![(stream(), json!(1))],
        ends: vec![(stream(), StreamEnd::Close)],
        ..StreamBatch::default()
    };
    let error = streams.commit(batch).unwrap_err();
    assert_eq!(error.message, "the disk is full");
    let channel = &streams.channels[&stream()];
    assert_eq!(
        (channel.next, channel.terminal.is_none(), streams.event),
        (0, true, 0)
    );
    assert_eq!(
        streams.peek(&reader("b")).unwrap(),
        Read::Pending,
        "no cursor moved"
    );
}

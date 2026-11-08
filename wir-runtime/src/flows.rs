//! The streams of a plan: activations open the streams and readers of a graph, and calls
//! forward stream outputs and hold their close until the call succeeds.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json};
use wir::{Status, Subscription};
use workflow_graph::DriverError;

use crate::plan::Source;
use crate::state::{Progress, State};
use crate::streams::{Failure, Read, ReaderId, StreamBatch, StreamEnd, StreamId};

/// The stream of an output port of an op instance.
pub fn output_stream(path: &str, port: &str) -> StreamId {
    StreamId(format!("{path}#{port}"))
}

/// The stream of a stream input of the entry graph.
pub fn input_stream(port: &str) -> StreamId {
    StreamId(format!("#{port}"))
}

/// The reader a call uses to forward the stream of its output `port`.
fn export_reader(path: &str, port: &str) -> ReaderId {
    ReaderId {
        owner: path.to_owned(),
        input: format!("export {port}"),
    }
}

impl Progress {
    /// Opens the stream outputs of the nodes of one activation, the entry graph or the graph
    /// of a call, then installs their readers, all before any of them starts.
    pub fn activate(&mut self, parent: Option<usize>) -> Result<(), DriverError> {
        let members = self.children(parent).to_vec();
        for at in &members {
            let node = self.node(*at);
            let ports = node.op.outputs.iter().filter(|port| !port.flow.is_value());
            let opened: Vec<StreamId> = ports
                .map(|port| output_stream(&node.path, &port.name))
                .collect();
            let path = node.path.clone();
            for stream in opened {
                self.streams.open(stream, &path);
            }
        }
        for at in &members {
            let node = self.node(*at);
            let mut readers = Vec::new();
            for (port, source) in &node.inputs {
                let stream = node
                    .op
                    .inputs
                    .iter()
                    .any(|item| item.name == *port && !item.flow.is_value());
                if !stream {
                    continue;
                }
                let mode = match node.live.contains(port) {
                    true => Subscription::Live,
                    false => Subscription::All,
                };
                let reader = ReaderId {
                    owner: node.path.clone(),
                    input: port.clone(),
                };
                readers.push((self.stream_of(source)?, reader, mode));
            }
            for (stream, reader, mode) in readers {
                self.streams.subscribe(&stream, reader, mode)?;
            }
        }
        if let Some(call) = parent {
            let path = self.node(call).path.clone();
            let mut readers = Vec::new();
            for (port, source) in self.streamed_exports(call) {
                readers.push((self.stream_of(&source)?, export_reader(&path, &port)));
            }
            for (stream, reader) in readers {
                self.streams.subscribe(&stream, reader, Subscription::All)?;
            }
        }
        Ok(())
    }

    /// The stream a source reads: an output stream of a node, or a stream input of the entry
    /// graph, through the inputs of calls.
    pub fn stream_of(&self, source: &Source) -> Result<StreamId, DriverError> {
        match source {
            Source::Input(name) => Ok(input_stream(name)),
            Source::Output(node, port) => Ok(output_stream(&self.node(*node).path, port)),
            Source::Call(call, port) => {
                let inner = (self.node(*call).inputs.iter()).find(|(name, _)| name == port);
                match inner {
                    Some((_, source)) => self.stream_of(source),
                    None => Err(DriverError::new(format!("no stream feeds input {port}"))),
                }
            }
            Source::Null | Source::Value(_) => {
                Err(DriverError::new("a stream input needs a producer"))
            }
        }
    }

    /// Whether the producer of a stream input runs or ran, and so writes its stream: `None`
    /// while it waits, an error when it never will.
    pub fn stream_ready(&self, source: &Source) -> Result<Option<()>, ()> {
        match source {
            Source::Input(_) => Ok(Some(())),
            Source::Output(node, _) => match &self.states[*node] {
                State::Waiting => Ok(None),
                State::Running(_) | State::Ended(Status::Succeeded, _) => Ok(Some(())),
                State::Ended(..) => Err(()),
            },
            Source::Call(call, port) => {
                let inner = (self.node(*call).inputs.iter()).find(|(name, _)| name == port);
                inner.map_or(Err(()), |(_, source)| self.stream_ready(source))
            }
            Source::Null | Source::Value(_) => Err(()),
        }
    }

    /// The stream outputs of a call and where they come from inside it.
    fn streamed_exports(&self, call: usize) -> Vec<(String, Source)> {
        let node = self.node(call);
        let streamed = |port: &str| {
            node.op
                .outputs
                .iter()
                .any(|item| item.name == port && !item.flow.is_value())
        };
        (node.exports.iter())
            .filter(|(port, _)| streamed(port))
            .cloned()
            .collect()
    }

    /// Forwards the items that the stream outputs of a running call have inside it; the
    /// close waits for the call. Whether it forwarded any.
    pub fn forward(&mut self, call: usize) -> bool {
        let path = self.node(call).path.clone();
        let mut batch = StreamBatch::default();
        for (port, _) in self.streamed_exports(call) {
            let reader = export_reader(&path, &port);
            while let Ok(Read::Item(_)) = self.streams.peek(&reader) {
                match self.streams.consume(std::slice::from_ref(&reader)) {
                    Ok(Some(values)) => batch.items.extend(
                        values
                            .into_iter()
                            .map(|value| (output_stream(&path, &port), value)),
                    ),
                    _ => break,
                }
            }
        }
        let moved = !batch.items.is_empty();
        if moved && let Err(error) = self.streams.commit(batch) {
            self.failures.push(format!("op {path}: {}", error.message));
        }
        moved
    }

    /// Whether every stream that a call forwards has ended inside it.
    pub fn exports_ended(&self, call: usize) -> bool {
        let path = &self.node(call).path;
        (self.streamed_exports(call).iter()).all(|(port, _)| {
            matches!(
                self.streams.peek(&export_reader(path, port)),
                Ok(Read::End(_)) | Err(_)
            )
        })
    }

    /// Ends the open stream outputs of a node that ended: a close when it succeeded, else an
    /// error or a cancellation.
    pub fn close_streams(&mut self, at: usize, status: Status, why: Option<&str>) {
        let node = self.node(at);
        let path = node.path.clone();
        let open: Vec<StreamId> = (node.op.outputs.iter())
            .filter(|port| !port.flow.is_value())
            .map(|port| output_stream(&path, &port.name))
            .filter(|stream| {
                self.streams
                    .channels
                    .get(stream)
                    .is_some_and(|channel| channel.terminal.is_none())
            })
            .collect();
        if open.is_empty() {
            return;
        }
        let end = match status {
            Status::Succeeded => StreamEnd::Close,
            Status::Failed | Status::Errored => StreamEnd::Error(Failure {
                status,
                message: why.map_or_else(|| format!("op {path} failed"), str::to_owned),
                source: path.clone(),
            }),
            Status::Skipped | Status::Omitted | Status::Cancelled => StreamEnd::Cancelled,
        };
        let ends = open
            .into_iter()
            .map(|stream| (stream, end.clone()))
            .collect();
        if let Err(error) = self.streams.commit(StreamBatch {
            ends,
            ..StreamBatch::default()
        }) {
            self.failures.push(format!("op {path}: {}", error.message));
        }
    }

    /// The items of a stream output of the entry graph, once it closed.
    pub fn stream_output(&self, port: &str, source: &Source) -> Result<Json, String> {
        let stream = self.stream_of(source).map_err(|error| error.message)?;
        match self
            .streams
            .history(&stream)
            .map_err(|error| error.message)?
        {
            (items, Some(StreamEnd::Close)) => Ok(Json::Array(items)),
            (_, Some(StreamEnd::Error(failure))) => {
                Err(format!("the output {port} failed: {}", failure.message))
            }
            (_, Some(StreamEnd::Cancelled)) => Err(format!("the output {port} was cancelled")),
            (_, None) => Err(format!("the output {port} did not close")),
        }
    }

    /// Commits the items of the stream inputs of the entry graph, and closes them.
    pub fn feed(
        &mut self,
        values: &Map<String, Json>,
        streams: &[String],
    ) -> Result<(), DriverError> {
        let mut batch = StreamBatch::default();
        for name in streams {
            let stream = input_stream(name);
            self.streams.open(stream.clone(), "");
            let items = values
                .get(name)
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            batch
                .items
                .extend(items.into_iter().map(|item| (stream.clone(), item)));
            batch.ends.push((stream, StreamEnd::Close));
        }
        self.streams.commit(batch)
    }
}

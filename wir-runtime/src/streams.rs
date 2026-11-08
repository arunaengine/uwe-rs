//! The streams of a run: channels of items that end once, readers with a cursor each, and
//! the log that keeps every item from the first, so that a late reader sees them all.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use serde_json::Value as Json;
use wir::{Status, Subscription};
use workflow_graph::DriverError;

/// A stream of the run, named after its producer and port, like `align#bam`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(pub String);

/// A reader: the op instance that reads and the input it reads into.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReaderId {
    pub owner: String,
    pub input: String,
}

/// Where a stream is: how many items it committed, its end, and its readers.
#[derive(Debug)]
pub struct StreamState {
    pub id: StreamId,
    /// The op instance that writes it.
    pub owner: String,
    pub next: u64,
    pub terminal: Option<Terminal>,
    pub readers: BTreeMap<ReaderId, ReaderState>,
}

/// The cursor of one reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderState {
    pub start: u64,
    pub next: u64,
    pub detached: bool,
}

/// One committed item, with the event of the batch that committed it.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamItem {
    pub sequence: u64,
    pub event: u64,
    pub value: Json,
}

/// How and when a stream ended; its sequence is the number of its items.
#[derive(Clone, Debug, PartialEq)]
pub struct Terminal {
    pub sequence: u64,
    pub event: u64,
    pub end: StreamEnd,
}

/// The one end of a stream.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEnd {
    Close,
    Error(Failure),
    Cancelled,
}

/// Why a stream or a value failed, and which op instance failed.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub status: Status,
    pub message: String,
    pub source: String,
}

/// What a reader sees next.
#[derive(Clone, Debug, PartialEq)]
pub enum Read {
    Pending,
    Item(StreamItem),
    End(Terminal),
}

/// Items and ends committed together, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamBatch {
    pub event: u64,
    pub items: Vec<(StreamId, Json)>,
    pub ends: Vec<(StreamId, StreamEnd)>,
}

/// Where the items of the streams are kept. An append writes a whole batch or nothing.
pub trait StreamLog {
    fn append(&mut self, batch: &[(StreamId, StreamItem)]) -> Result<(), DriverError>;

    fn item(&self, stream: &StreamId, sequence: u64) -> Result<Option<StreamItem>, DriverError>;
}

/// A log in memory.
#[derive(Debug, Default)]
pub struct MemoryLog {
    items: HashMap<StreamId, Vec<StreamItem>>,
}

impl StreamLog for MemoryLog {
    fn append(&mut self, batch: &[(StreamId, StreamItem)]) -> Result<(), DriverError> {
        for (stream, item) in batch {
            self.items
                .entry(stream.clone())
                .or_default()
                .push(item.clone());
        }
        Ok(())
    }

    fn item(&self, stream: &StreamId, sequence: u64) -> Result<Option<StreamItem>, DriverError> {
        let items = self.items.get(stream);
        Ok(items
            .and_then(|items| items.get(sequence as usize))
            .cloned())
    }
}

/// The streams of a run. One owner changes them; a commit changes the log first, and the
/// cursors and tails only once the log took the batch.
pub struct Streams {
    pub channels: BTreeMap<StreamId, StreamState>,
    /// The number of committed batches.
    pub event: u64,
    log: Box<dyn StreamLog>,
    /// The stream of each reader.
    reading: HashMap<ReaderId, StreamId>,
}

impl Default for Streams {
    fn default() -> Self {
        Streams::new(Box::new(MemoryLog::default()))
    }
}

impl Streams {
    pub fn new(log: Box<dyn StreamLog>) -> Streams {
        Streams {
            channels: BTreeMap::new(),
            event: 0,
            log,
            reading: HashMap::new(),
        }
    }

    /// Makes a stream that `owner` writes, once.
    pub fn open(&mut self, id: StreamId, owner: &str) {
        self.channels
            .entry(id.clone())
            .or_insert_with(|| StreamState {
                id,
                owner: owner.to_owned(),
                next: 0,
                terminal: None,
                readers: BTreeMap::new(),
            });
    }

    /// Installs a reader of a stream: at item zero, or for `live` after its last committed
    /// item. A reader installed again keeps its cursor.
    pub fn subscribe(
        &mut self,
        stream: &StreamId,
        reader: ReaderId,
        mode: Subscription,
    ) -> Result<(), DriverError> {
        let channel = self.channel_mut(stream)?;
        let start = match mode {
            Subscription::All => 0,
            Subscription::Live => channel.next,
        };
        let state = ReaderState {
            start,
            next: start,
            detached: false,
        };
        channel.readers.entry(reader.clone()).or_insert(state);
        self.reading.insert(reader, stream.clone());
        Ok(())
    }

    /// What a reader sees next, without moving it.
    pub fn peek(&self, reader: &ReaderId) -> Result<Read, DriverError> {
        let (stream, channel) = self.read_by(reader)?;
        let state = &channel.readers[reader];
        if state.detached {
            let message = format!(
                "{} read {} after it stopped reading",
                reader.owner, stream.0
            );
            return Err(DriverError::new(message));
        }
        if state.next < channel.next {
            let item = self.log.item(stream, state.next)?;
            let missing =
                || DriverError::new(format!("the log lost item {} of {}", state.next, stream.0));
            return item.map(Read::Item).ok_or_else(missing);
        }
        Ok(match &channel.terminal {
            Some(terminal) => Read::End(terminal.clone()),
            None => Read::Pending,
        })
    }

    /// The next item of every reader, moving them all, or none when one of them has no
    /// item yet.
    pub fn consume(&mut self, readers: &[ReaderId]) -> Result<Option<Vec<Json>>, DriverError> {
        let mut values = Vec::new();
        for reader in readers {
            match self.peek(reader)? {
                Read::Item(item) => values.push(item.value),
                Read::Pending | Read::End(_) => return Ok(None),
            }
        }
        for reader in readers {
            let stream = self.reading[reader].clone();
            let channel = self.channel_mut(&stream)?;
            if let Some(state) = channel.readers.get_mut(reader) {
                state.next += 1;
            }
        }
        Ok(Some(values))
    }

    /// Commits a batch: every item and end is checked first, then the log takes it, then
    /// the tails move. Items come before the ends of the same batch.
    pub fn commit(&mut self, batch: StreamBatch) -> Result<(), DriverError> {
        let mut ending = Vec::new();
        for (stream, _) in &batch.items {
            if self.channel(stream)?.terminal.is_some() {
                return Err(DriverError::new(format!(
                    "{} has ended and takes no item",
                    stream.0
                )));
            }
        }
        for (stream, _) in &batch.ends {
            if self.channel(stream)?.terminal.is_some() || ending.contains(&stream) {
                return Err(DriverError::new(format!("{} ends twice", stream.0)));
            }
            ending.push(stream);
        }
        let event = self.event + 1;
        let mut tails: HashMap<&StreamId, u64> = HashMap::new();
        let mut items = Vec::new();
        for (stream, value) in &batch.items {
            let next = tails.entry(stream).or_insert(self.channels[stream].next);
            items.push((
                stream.clone(),
                StreamItem {
                    sequence: *next,
                    event,
                    value: value.clone(),
                },
            ));
            *next += 1;
        }
        self.log.append(&items)?;
        for (stream, next) in tails {
            if let Some(channel) = self.channels.get_mut(stream) {
                channel.next = next;
            }
        }
        for (stream, end) in batch.ends {
            if let Some(channel) = self.channels.get_mut(&stream) {
                channel.terminal = Some(Terminal {
                    sequence: channel.next,
                    event,
                    end,
                });
            }
        }
        self.event = event;
        Ok(())
    }

    /// How the stream of a reader ended, if it did, wherever the reader is.
    pub fn terminal_of(&self, reader: &ReaderId) -> Option<&Terminal> {
        let stream = self.reading.get(reader)?;
        self.channels.get(stream)?.terminal.as_ref()
    }

    /// Stops a reader; its stream and the other readers go on.
    pub fn detach(&mut self, reader: &ReaderId) {
        let Some(stream) = self.reading.get(reader) else {
            return;
        };
        let state =
            (self.channels.get_mut(stream)).and_then(|channel| channel.readers.get_mut(reader));
        if let Some(state) = state {
            state.detached = true;
        }
    }

    /// Ends the open streams of an op instance and those below it as cancelled, and stops
    /// their readers.
    pub fn cancel(&mut self, owner: &str) -> Result<(), DriverError> {
        let inside = |name: &str| name == owner || name.starts_with(&format!("{owner}/"));
        let open: Vec<StreamId> = (self.channels.values())
            .filter(|channel| channel.terminal.is_none() && inside(&channel.owner))
            .map(|channel| channel.id.clone())
            .collect();
        let readers: Vec<ReaderId> = (self.reading.keys())
            .filter(|reader| inside(&reader.owner))
            .cloned()
            .collect();
        for reader in &readers {
            self.detach(reader);
        }
        let ends = open
            .into_iter()
            .map(|stream| (stream, StreamEnd::Cancelled))
            .collect();
        self.commit(StreamBatch {
            ends,
            ..StreamBatch::default()
        })
    }

    /// The items of a stream from the first, and its end, for the results of a run.
    pub fn history(
        &self,
        stream: &StreamId,
    ) -> Result<(Vec<Json>, Option<StreamEnd>), DriverError> {
        let channel = self.channel(stream)?;
        let mut items = Vec::new();
        for sequence in 0..channel.next {
            let item = self.log.item(stream, sequence)?;
            items.push(item.map(|item| item.value).unwrap_or_default());
        }
        Ok((
            items,
            channel
                .terminal
                .as_ref()
                .map(|terminal| terminal.end.clone()),
        ))
    }

    fn channel(&self, stream: &StreamId) -> Result<&StreamState, DriverError> {
        (self.channels.get(stream))
            .ok_or_else(|| DriverError::new(format!("no stream {}", stream.0)))
    }

    fn channel_mut(&mut self, stream: &StreamId) -> Result<&mut StreamState, DriverError> {
        let missing = || DriverError::new(format!("no stream {}", stream.0));
        self.channels.get_mut(stream).ok_or_else(missing)
    }

    fn read_by(&self, reader: &ReaderId) -> Result<(&StreamId, &StreamState), DriverError> {
        let missing = || {
            DriverError::new(format!(
                "{} reads no stream into {}",
                reader.owner, reader.input
            ))
        };
        let stream = self.reading.get(reader).ok_or_else(missing)?;
        Ok((stream, self.channel(stream)?))
    }
}

#[cfg(test)]
#[path = "streams_tests.rs"]
mod tests;

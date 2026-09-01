//! Channels: queue channels, which every reader reads in full, and value channels, bound once.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;

use nextflow_evaluator::value::Value;

/// A message of a channel: a value, or the stop signal that closes the channel.
#[derive(Clone, Debug)]
pub enum Message {
    Value(Value),
    Stop,
}

/// A channel of the network. A queue channel is multicast: each reader has its own cursor
/// and sees every value written after it subscribed. A value channel holds one value, or
/// the stop signal for an empty value channel.
#[derive(Debug)]
pub enum Channel {
    Queue {
        /// The messages that some reader has not read yet; those that every reader has read
        /// are dropped, so that a long channel does not stay in memory.
        messages: VecDeque<Message>,
        /// The number of messages dropped from the front.
        dropped: usize,
        /// Whether the stop signal was written.
        stopped: bool,
        /// The position of the first message each reader has not read yet, counted from the
        /// first message of the channel.
        cursors: Vec<Option<usize>>,
    },
    Value(Option<Message>),
}

/// A reader's handle on a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor(pub usize);

impl Channel {
    pub fn queue() -> Self {
        Channel::Queue {
            messages: VecDeque::new(),
            dropped: 0,
            stopped: false,
            cursors: Vec::new(),
        }
    }

    pub fn value() -> Self {
        Channel::Value(None)
    }

    pub fn is_value(&self) -> bool {
        matches!(self, Channel::Value(_))
    }

    /// A new reader. On a queue it sees what is written from now on (GPars' "from now on").
    pub fn subscribe(&mut self) -> Cursor {
        match self {
            Channel::Queue {
                messages,
                dropped,
                cursors,
                ..
            } => {
                cursors.push(Some(*dropped + messages.len()));
                Cursor(cursors.len() - 1)
            }
            Channel::Value(_) => Cursor(0),
        }
    }

    /// Writes a message. On a queue, nothing is written after the stop signal, and what no
    /// reader can read is not kept. A value channel keeps its first binding; binding it again
    /// is ignored here.
    pub fn write(&mut self, message: Message) {
        match self {
            Channel::Queue {
                messages,
                dropped,
                stopped,
                cursors,
            } => {
                if *stopped {
                    return;
                }
                *stopped = matches!(message, Message::Stop);
                if cursors.iter().all(Option::is_none) {
                    *dropped += 1;
                } else {
                    messages.push_back(message);
                }
            }
            Channel::Value(bound) => {
                if bound.is_none() {
                    *bound = Some(message);
                }
            }
        }
    }

    /// Closes the channel: a stop signal on a queue, or an empty value if still unbound.
    pub fn close(&mut self) {
        self.write(Message::Stop);
    }

    /// The next message for a reader of a queue, which it then has read. A value channel gives
    /// its value on every read, and nothing before it is bound.
    pub fn read(&mut self, cursor: Cursor) -> Option<Message> {
        match self {
            Channel::Queue {
                messages,
                dropped,
                cursors,
                ..
            } => {
                let position = cursors.get(cursor.0).copied().flatten()?;
                let message = messages.get(position - *dropped).cloned()?;
                cursors[cursor.0] = Some(position + 1);
                // Drop the messages that every reader has read.
                let first = cursors
                    .iter()
                    .filter_map(|cursor| *cursor)
                    .min()
                    .unwrap_or(position + 1);
                while *dropped < first && messages.pop_front().is_some() {
                    *dropped += 1;
                }
                Some(message)
            }
            Channel::Value(bound) => bound.clone(),
        }
    }

    /// The next message for a reader without reading it; a value channel's binding.
    pub fn peek(&self, cursor: Cursor) -> Option<&Message> {
        match self {
            Channel::Queue {
                messages,
                dropped,
                cursors,
                ..
            } => messages.get(cursors.get(cursor.0).copied().flatten()? - dropped),
            Channel::Value(bound) => bound.as_ref(),
        }
    }

    /// Whether a reader has a message waiting.
    pub fn ready(&self, cursor: Cursor) -> bool {
        match self {
            Channel::Queue {
                messages,
                dropped,
                cursors,
                ..
            } => cursors
                .get(cursor.0)
                .copied()
                .flatten()
                .is_some_and(|position| position < dropped + messages.len()),
            Channel::Value(bound) => bound.is_some(),
        }
    }

    /// Releases a reader and the messages that no remaining reader needs.
    pub fn unsubscribe(&mut self, cursor: Cursor) {
        if let Channel::Queue {
            messages,
            dropped,
            cursors,
            ..
        } = self
        {
            if let Some(position) = cursors.get_mut(cursor.0) {
                *position = None;
            }
            let first = cursors
                .iter()
                .filter_map(|cursor| *cursor)
                .min()
                .unwrap_or(*dropped + messages.len());
            while *dropped < first && messages.pop_front().is_some() {
                *dropped += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_reader_does_not_retain_messages() {
        let mut channel = Channel::queue();
        let stopped = channel.subscribe();
        let active = channel.subscribe();
        channel.write(Message::Value(Value::Null));
        channel.read(active).unwrap();
        channel.unsubscribe(stopped);
        for _ in 0..1000 {
            channel.write(Message::Value(Value::Null));
            channel.read(active).unwrap();
        }
        let Channel::Queue { messages, .. } = &channel else {
            panic!("queue")
        };
        assert!(messages.is_empty());
        assert!(channel.read(stopped).is_none());
        assert!(!channel.ready(stopped));
        channel.unsubscribe(active);
        channel.write(Message::Stop);
        let Channel::Queue { messages, .. } = channel else {
            panic!("queue")
        };
        assert!(messages.is_empty());
    }
}

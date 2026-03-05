//! Channels: queue channels, which every reader reads in full, and value channels, bound once.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

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
        messages: Vec<Message>,
        /// The index of the first message each reader has not read yet.
        cursors: Vec<usize>,
    },
    Value(Option<Message>),
}

/// A reader's handle on a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor(pub usize);

impl Channel {
    pub fn queue() -> Self {
        Channel::Queue {
            messages: Vec::new(),
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
            Channel::Queue { messages, cursors } => {
                cursors.push(messages.len());
                Cursor(cursors.len() - 1)
            }
            Channel::Value(_) => Cursor(0),
        }
    }

    /// Writes a message. On a queue, nothing is written after the stop signal. A value channel
    /// keeps its first binding; binding it again is ignored here.
    pub fn write(&mut self, message: Message) {
        match self {
            Channel::Queue { messages, .. } => {
                if !matches!(messages.last(), Some(Message::Stop)) {
                    messages.push(message);
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
            Channel::Queue { messages, cursors } => {
                let position = cursors[cursor.0];
                let message = messages.get(position).cloned()?;
                cursors[cursor.0] += 1;
                Some(message)
            }
            Channel::Value(bound) => bound.clone(),
        }
    }

    /// Whether a reader has a message waiting.
    pub fn ready(&self, cursor: Cursor) -> bool {
        match self {
            Channel::Queue { messages, cursors } => cursors[cursor.0] < messages.len(),
            Channel::Value(bound) => bound.is_some(),
        }
    }
}

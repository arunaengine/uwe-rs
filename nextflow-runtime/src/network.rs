//! The dataflow network: channels and nodes, driven by one queue of nodes that have messages
//! waiting. It is single-threaded and deterministic for a given order of task completions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Interpreter};
use nextflow_evaluator::value::{ChannelId, Value};

use crate::channel::{Channel, Cursor, Message};

/// What a node does with the messages of its inputs.
pub(crate) trait Behaviour {
    /// Starts the node when the network is ignited; factories emit here.
    fn ignite(&mut self, _context: &mut Context) -> Result<State, Error> {
        Ok(State::Running)
    }

    /// Acts on the messages that are waiting on the inputs.
    fn run(&mut self, context: &mut Context) -> Result<State, Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Running,
    Stopped,
}

struct Input {
    channel: ChannelId,
    cursor: Cursor,
    /// For a value channel read like a queue: whether its value was read already.
    consumed: bool,
}

struct Node {
    name: Rc<str>,
    inputs: Vec<Input>,
    behaviour: Option<Box<dyn Behaviour>>,
    state: State,
}

/// The channels and nodes of one run.
#[derive(Default)]
pub struct Network {
    channels: Vec<Channel>,
    nodes: Vec<Node>,
    /// The nodes that read each channel.
    readers: Vec<Vec<usize>>,
    /// Nodes with messages waiting, in the order they got them.
    waiting: VecDeque<usize>,
    queued: Vec<bool>,
}

/// What a running node can see: its inputs, the channels it writes, and the interpreter for
/// closures.
pub(crate) struct Context<'n, 'i, 'h> {
    channels: &'n mut [Channel],
    inputs: &'n mut [Input],
    written: Vec<ChannelId>,
    pub interpreter: &'i mut Interpreter<'h>,
}

impl Context<'_, '_, '_> {
    /// The next message of an input, as an operator reads it: a value channel gives its value
    /// once and then the stop signal.
    pub fn read(&mut self, input: usize) -> Option<Message> {
        let input = &mut self.inputs[input];
        let channel = &mut self.channels[input.channel.0 as usize];
        if !channel.is_value() {
            return channel.read(input.cursor);
        }
        match (input.consumed, channel.read(input.cursor)?) {
            (false, message) => {
                input.consumed = true;
                Some(message)
            }
            (true, _) => Some(Message::Stop),
        }
    }

    pub fn input_count(&self) -> usize {
        self.inputs.len()
    }

    pub fn write(&mut self, channel: ChannelId, message: Message) {
        self.channels[channel.0 as usize].write(message);
        self.written.push(channel);
    }

    pub fn emit(&mut self, channel: ChannelId, value: Value) {
        self.write(channel, Message::Value(value));
    }

    pub fn close(&mut self, channel: ChannelId) {
        self.write(channel, Message::Stop);
    }
}

impl Network {
    /// A new channel: a value channel, or a queue channel.
    pub fn channel(&mut self, value: bool) -> ChannelId {
        self.channels.push(if value {
            Channel::value()
        } else {
            Channel::queue()
        });
        self.readers.push(Vec::new());
        ChannelId(self.channels.len() as u32 - 1)
    }

    pub fn is_value(&self, channel: ChannelId) -> bool {
        self.channels[channel.0 as usize].is_value()
    }

    /// Adds a node that reads the given channels from now on.
    pub(crate) fn add(&mut self, name: &str, inputs: &[ChannelId], behaviour: Box<dyn Behaviour>) {
        let index = self.nodes.len();
        let inputs = inputs
            .iter()
            .map(|channel| {
                self.readers[channel.0 as usize].push(index);
                Input {
                    channel: *channel,
                    cursor: self.channels[channel.0 as usize].subscribe(),
                    consumed: false,
                }
            })
            .collect();
        self.nodes.push(Node {
            name: name.into(),
            inputs,
            behaviour: Some(behaviour),
            state: State::Running,
        });
        self.queued.push(false);
    }

    /// Starts every node in the order it was created, then runs until nothing is waiting.
    pub fn ignite(&mut self, interpreter: &mut Interpreter) -> Result<(), Error> {
        for index in 0..self.nodes.len() {
            self.step(index, interpreter, true)?;
            self.enqueue(index);
        }
        self.run(interpreter)
    }

    /// Runs the nodes that have messages waiting until there are none.
    pub fn run(&mut self, interpreter: &mut Interpreter) -> Result<(), Error> {
        while let Some(index) = self.waiting.pop_front() {
            self.queued[index] = false;
            self.step(index, interpreter, false)?;
        }
        Ok(())
    }

    /// The nodes that have not stopped, by name; after a run, they wait for input that will
    /// never come.
    pub fn running(&self) -> Vec<Rc<str>> {
        self.nodes
            .iter()
            .filter(|node| node.state == State::Running)
            .map(|node| node.name.clone())
            .collect()
    }

    fn step(
        &mut self,
        index: usize,
        interpreter: &mut Interpreter,
        ignite: bool,
    ) -> Result<(), Error> {
        let node = &mut self.nodes[index];
        if node.state == State::Stopped {
            return Ok(());
        }
        let mut behaviour = node
            .behaviour
            .take()
            .expect("a node does not run inside itself");
        let mut context = Context {
            channels: &mut self.channels,
            inputs: &mut node.inputs,
            written: Vec::new(),
            interpreter,
        };
        let result = if ignite {
            behaviour.ignite(&mut context)
        } else {
            behaviour.run(&mut context)
        };
        let written = std::mem::take(&mut context.written);
        node.behaviour = Some(behaviour);
        node.state = result?;
        for channel in written {
            for reader in self.readers[channel.0 as usize].clone() {
                self.enqueue(reader);
            }
        }
        Ok(())
    }

    fn enqueue(&mut self, index: usize) {
        if !self.queued[index] && self.nodes[index].state == State::Running {
            self.queued[index] = true;
            self.waiting.push_back(index);
        }
    }
}

//! The dataflow network: channels and nodes, driven by one queue of nodes that have messages
//! waiting. It is single-threaded and deterministic for a given order of task completions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Interpreter};
use nextflow_evaluator::value::{ChannelId, Value};

use crate::channel::{Channel, Cursor, Message};
use crate::process::{TaskId, TaskOutcome, TaskRequest};

/// Why a node runs.
enum Step {
    Ignite,
    Run,
    Complete(TaskOutcome),
}

/// What a node does with the messages of its inputs.
pub(crate) trait Behaviour {
    /// Starts the node when the network is ignited; factories emit here.
    fn ignite(&mut self, _context: &mut Context) -> Result<State, Error> {
        Ok(State::Running)
    }

    /// Acts on the messages that are waiting on the inputs.
    fn run(&mut self, context: &mut Context) -> Result<State, Error>;

    /// Takes the outcome of a task that this node requested.
    fn complete(&mut self, _context: &mut Context, _outcome: TaskOutcome) -> Result<State, Error> {
        Ok(State::Running)
    }
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
    /// Tasks that nodes requested and that were not handed out yet.
    requests: Vec<TaskRequest>,
    /// The node that requested each task in flight.
    owners: HashMap<TaskId, usize>,
    next_task: u64,
    /// Topics by name: the channel of `channel.topic(name)`, if a script reads it, and the
    /// process outputs that write to the topic.
    topics: indexmap::IndexMap<Rc<str>, (Option<ChannelId>, Vec<ChannelId>)>,
}

/// What a running node can see: its inputs, the channels it writes, and the interpreter for
/// closures.
pub(crate) struct Context<'n, 'i, 'h> {
    channels: &'n mut [Channel],
    inputs: &'n mut [Input],
    written: Vec<ChannelId>,
    requests: Vec<TaskRequest>,
    next_task: &'n mut u64,
    pub interpreter: &'i mut Interpreter<'h>,
    /// Whether the node has more to do without new input, and runs again after the nodes
    /// that read what it wrote.
    again: bool,
}

impl Context<'_, '_, '_> {
    /// Runs the node again later, after the nodes that read what it wrote, as a node that
    /// writes many values does, so that they need not all wait in its channel.
    pub fn again(&mut self) {
        self.again = true;
    }

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

    /// Whether an input is a value channel, which a process reads again in every round.
    pub fn is_value(&self, input: usize) -> bool {
        self.channels[self.inputs[input].channel.0 as usize].is_value()
    }

    /// The next message of an input without reading it; a value channel's binding.
    pub fn peek(&self, input: usize) -> Option<Message> {
        let input = &self.inputs[input];
        self.channels[input.channel.0 as usize]
            .peek(input.cursor)
            .cloned()
    }

    /// Reads the next message of a queue input.
    pub fn consume(&mut self, input: usize) -> Option<Message> {
        let input = &self.inputs[input];
        self.channels[input.channel.0 as usize].read(input.cursor)
    }

    /// A new task number.
    pub fn task_id(&mut self) -> TaskId {
        *self.next_task += 1;
        TaskId(*self.next_task)
    }

    /// Asks for a task to run.
    pub fn request(&mut self, request: TaskRequest) {
        self.requests.push(request);
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

    /// The channel of a topic, as `channel.topic(name)` gives it.
    pub(crate) fn topic(&mut self, name: &str) -> ChannelId {
        if let Some((Some(channel), _)) = self.topics.get(name) {
            return *channel;
        }
        let channel = self.channel(false);
        self.topics.entry(name.into()).or_default().0 = Some(channel);
        channel
    }

    /// Records a process output that writes to a topic.
    pub(crate) fn add_topic_source(&mut self, name: &str, source: ChannelId) {
        self.topics.entry(name.into()).or_default().1.push(source);
    }

    /// Binds a value channel now, as a plain process argument is at call time.
    pub(crate) fn bind(&mut self, channel: ChannelId, value: Value) {
        self.channels[channel.0 as usize].write(Message::Value(value));
    }

    pub fn is_value(&self, channel: ChannelId) -> bool {
        self.channels[channel.0 as usize].is_value()
    }

    /// The value a value channel is bound to; `None` for a queue or an empty value channel.
    pub fn bound(&self, channel: ChannelId) -> Option<Value> {
        match &self.channels[channel.0 as usize] {
            Channel::Value(Some(Message::Value(value))) => Some(value.clone()),
            _ => None,
        }
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
        // Each topic that a script reads mixes all its sources, or is empty without any.
        for (name, (channel, sources)) in std::mem::take(&mut self.topics) {
            let Some(output) = channel else {
                continue;
            };
            if sources.is_empty() {
                self.channels[output.0 as usize].write(Message::Stop);
                continue;
            }
            let closed = vec![false; sources.len()];
            let mix = crate::operator::Mix { output, closed };
            self.add(&format!("topic {name}"), &sources, Box::new(mix));
        }
        for index in 0..self.nodes.len() {
            self.step(index, interpreter, Step::Ignite)?;
            self.enqueue(index);
        }
        self.run(interpreter)
    }

    /// Runs the nodes that have messages waiting until there are none.
    pub fn run(&mut self, interpreter: &mut Interpreter) -> Result<(), Error> {
        while let Some(index) = self.waiting.pop_front() {
            self.queued[index] = false;
            self.step(index, interpreter, Step::Run)?;
        }
        Ok(())
    }

    /// The tasks that nodes asked for since the last call.
    pub fn take_requests(&mut self) -> Vec<TaskRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Whether tasks are in flight.
    pub fn has_tasks(&self) -> bool {
        !self.owners.is_empty()
    }

    /// Delivers the outcome of a task to the node that asked for it, then runs the network.
    pub fn complete(
        &mut self,
        outcome: TaskOutcome,
        interpreter: &mut Interpreter,
    ) -> Result<(), Error> {
        let Some(index) = self.owners.remove(&outcome.id) else {
            return Ok(());
        };
        self.step(index, interpreter, Step::Complete(outcome))?;
        self.run(interpreter)
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
        step: Step,
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
            requests: Vec::new(),
            next_task: &mut self.next_task,
            interpreter,
            again: false,
        };
        let result = match step {
            Step::Ignite => behaviour.ignite(&mut context),
            Step::Run => behaviour.run(&mut context),
            Step::Complete(outcome) => behaviour.complete(&mut context, outcome),
        };
        let written = std::mem::take(&mut context.written);
        let requests = std::mem::take(&mut context.requests);
        let again = context.again;
        for request in &requests {
            self.owners.insert(request.id, index);
        }
        self.requests.extend(requests);
        node.behaviour = Some(behaviour);
        node.state = result?;
        for channel in written {
            for reader in self.readers[channel.0 as usize].clone() {
                self.enqueue(reader);
            }
        }
        if again {
            self.enqueue(index);
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

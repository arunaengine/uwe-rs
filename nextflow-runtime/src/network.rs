//! The dataflow network: channels and nodes, driven by one queue of nodes that have messages
//! waiting. It is single-threaded and deterministic for a given order of task completions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Interpreter};
use nextflow_evaluator::value::{ChannelId, Value};

use crate::channel::{Channel, Cursor, Message};
use crate::cleanup::Cleanup;
use workflow_graph::Delivery;

use crate::process::{TaskId, TaskOutcome, TaskRequest};

/// Why a node runs.
enum Step {
    Ignite,
    Run,
    Complete(TaskOutcome),
}

/// Bound requests retained by the runtime until the executor returns their outcomes.
const MAX_OUTSTANDING: usize = 128;

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

    /// What the node does, as the compiler into wir reads it; an error names what has none.
    fn export(&self) -> Result<crate::export::Kind, String> {
        Err("a node that does not compile into wir".into())
    }

    /// The process that the node runs, for a process node.
    fn definition(&self) -> Option<Definition> {
        None
    }

    /// The workflow output that the node publishes, with its files so far.
    fn output(&self) -> Option<nextflow_plugins::observer::Output> {
        None
    }
}

/// A process definition: its name and the script file that defines it.
pub(crate) struct Definition {
    pub name: Rc<str>,
    pub script: Option<Rc<str>>,
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
    /// The channels made for the node, which it writes.
    outputs: Vec<ChannelId>,
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
    /// The channels made since the last node: the builder makes a node's outputs before it.
    fresh: Vec<ChannelId>,
    /// Nodes with messages waiting, in the order they got them.
    waiting: VecDeque<usize>,
    queued: Vec<bool>,
    /// Tasks that nodes requested and that were not handed out yet.
    requests: Vec<TaskRequest>,
    /// Files that nodes asked to deliver, like those of a `publishDir`.
    deliveries: Vec<Delivery>,
    /// The failure of a task whose error strategy is `finish`: no more tasks start, and the
    /// run ends with it once the running tasks have ended.
    failure: Option<Error>,
    /// Whether tasks go on starting after such a failure (`-keep-going`).
    keep_going: bool,
    /// The node that requested each task in flight.
    owners: HashMap<TaskId, usize>,
    /// Native output-check verdicts, consumed by the driver after each completion.
    acceptance: HashMap<TaskId, bool>,
    next_task: u64,
    pub(crate) succeeded: i64,
    pub(crate) failed_task: Option<Value>,
    /// The name of each file that a finished task made, relative to its work directory, by
    /// its location, for the workflow outputs.
    task_files: HashMap<String, String>,
    /// Topics by name: the channel of `channel.topic(name)`, if a script reads it, and the
    /// process outputs that write to the topic.
    topics: indexmap::IndexMap<Rc<str>, (Option<ChannelId>, Vec<ChannelId>)>,
    /// Frees the files of the run's folder that no node can pass on any more, when it is on.
    cleanup: Option<Cleanup>,
    /// The nodes that ever read each channel and the channels that each node ever wrote, which
    /// stay when nodes stop, for the cleanup.
    consumers: Vec<Vec<usize>>,
    wrote: Vec<Vec<ChannelId>>,
    /// The workflow call that each node was made in, the calls, and the current one.
    node_scopes: Vec<usize>,
    scopes: Vec<crate::export::Scope>,
    scope: usize,
}

/// What a running node can see: its inputs, the channels it writes, and the interpreter for
/// closures.
pub(crate) struct Context<'n, 'i, 'h> {
    channels: &'n mut [Channel],
    inputs: &'n mut [Input],
    written: Vec<ChannelId>,
    requests: Vec<TaskRequest>,
    deliveries: Vec<Delivery>,
    failure: Option<Error>,
    /// Whether the run goes on after a task failed that would end it.
    pub keep_going: bool,
    next_task: &'n mut u64,
    pub succeeded: &'n mut i64,
    pub failed_task: &'n mut Option<Value>,
    acceptance: &'n mut HashMap<TaskId, bool>,
    task_files: &'n mut HashMap<String, String>,
    pub interpreter: &'i mut Interpreter<'h>,
    /// Whether the node has more to do without new input, and runs again after the nodes
    /// that read what it wrote.
    again: bool,
    again_first: bool,
    /// The values that the node sent, kept only for the cleanup.
    sent: Option<Vec<Value>>,
}

impl<'h> Context<'_, '_, 'h> {
    pub fn accept(&mut self, task: TaskId, accepted: bool) {
        self.acceptance.insert(task, accepted);
    }

    /// Runs the node again later, after the nodes that read what it wrote, as a node that
    /// writes many values does, so that they need not all wait in its channel.
    pub fn again(&mut self) {
        self.again = true;
    }

    /// Finishes this process's ready round before starting downstream nodes.
    pub fn again_first(&mut self) {
        self.again_first = true;
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

    /// The interpreter and the task counter, which binding a task needs together.
    pub fn binder(&mut self) -> (&mut Interpreter<'h>, &mut u64) {
        (&mut *self.interpreter, &mut *self.next_task)
    }

    /// Asks for a task to run.
    pub fn request(&mut self, request: TaskRequest) {
        self.requests.push(request);
    }

    /// Ends the run with a failure once the running tasks have ended, and starts no more.
    pub fn fail(&mut self, error: Error) {
        self.failure.get_or_insert(error);
    }

    /// Notes the files that a finished task made, by their names in its work directory.
    pub fn remember_task_files(
        &mut self,
        files: &[(String, nextflow_evaluator::value::PathValue)],
    ) {
        for (name, location) in files {
            self.task_files.insert(location.path.clone(), name.clone());
        }
    }

    /// The name of a file in the work directory of the task that made it, when a task made it.
    pub fn task_file(&self, location: &str) -> Option<String> {
        self.task_files.get(location).cloned()
    }

    /// Asks to deliver files that a task made.
    pub fn deliver(&mut self, deliveries: Vec<Delivery>) {
        self.deliveries.extend(deliveries);
    }

    pub fn write(&mut self, channel: ChannelId, message: Message) {
        if let (Some(sent), Message::Value(value)) = (&mut self.sent, &message) {
            sent.push(value.clone());
        }
        self.channels[channel.0 as usize].write(message);
        self.written.push(channel);
    }

    pub fn emit(&mut self, channel: ChannelId, value: Value) {
        self.write(channel, Message::Value(value));
    }

    pub fn source(&mut self, channel: ChannelId, values: Vec<Value>, close: bool) {
        if let Some(sent) = &mut self.sent {
            sent.extend(values.iter().cloned());
        }
        self.channels[channel.0 as usize].source(values, close);
        self.written.push(channel);
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
        self.consumers.push(Vec::new());
        let channel = ChannelId(self.channels.len() as u32 - 1);
        self.fresh.push(channel);
        channel
    }

    /// The channel of a topic, as `channel.topic(name)` gives it.
    pub(crate) fn topic(&mut self, name: &str) -> ChannelId {
        if let Some((Some(channel), _)) = self.topics.get(name) {
            return *channel;
        }
        let channel = self.channel(false);
        // The mix of the topic's sources writes it, when the network ignites.
        self.fresh.retain(|fresh| *fresh != channel);
        self.topics.entry(name.into()).or_default().0 = Some(channel);
        channel
    }

    /// Records a process output that writes to a topic.
    pub(crate) fn add_topic_source(&mut self, name: &str, source: ChannelId) {
        self.topics.entry(name.into()).or_default().1.push(source);
    }

    /// Binds a value channel now, as a plain process argument is at call time.
    pub(crate) fn bind(&mut self, channel: ChannelId, value: Value) {
        self.fresh.retain(|fresh| *fresh != channel);
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
                self.consumers[channel.0 as usize].push(index);
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
            outputs: std::mem::take(&mut self.fresh),
            behaviour: Some(behaviour),
            state: State::Running,
        });
        self.queued.push(false);
        self.wrote.push(Vec::new());
        self.node_scopes.push(self.scope);
    }

    /// Starts a workflow call, whose nodes the export keeps together.
    pub(crate) fn enter(&mut self, name: &str, takes: Vec<usize>) {
        if self.scopes.is_empty() {
            self.scopes.push(crate::export::Scope {
                name: String::new(),
                parent: None,
                takes: Vec::new(),
                emits: Vec::new(),
            });
        }
        self.scopes.push(crate::export::Scope {
            name: name.into(),
            parent: Some(self.scope),
            takes,
            emits: Vec::new(),
        });
        self.scope = self.scopes.len() - 1;
    }

    /// Ends the current workflow call with the channels it emits.
    pub(crate) fn leave(&mut self, emits: Vec<(Option<String>, usize)>) {
        let Some(scope) = self.scopes.get_mut(self.scope) else {
            return;
        };
        scope.emits = emits;
        self.scope = scope.parent.unwrap_or(0);
    }

    /// The network as plain data, before it starts; `emits` are the entry workflow's.
    pub(crate) fn export(
        &mut self,
        emits: Vec<(String, usize)>,
        params: &Value,
    ) -> Result<crate::export::Network, String> {
        let params =
            crate::bridge::to_json(params).map_err(|error| format!("the params: {error}"))?;
        self.wire_topics();
        let written: HashSet<ChannelId> = (self.nodes.iter())
            .flat_map(|node| node.outputs.iter().copied())
            .collect();
        let channels = (0..self.channels.len())
            .map(|index| {
                let channel = ChannelId(index as u32);
                let bound = match written.contains(&channel) {
                    true => None,
                    false => self
                        .bound(channel)
                        .map(|value| crate::bridge::to_json(&value)),
                };
                Ok(crate::export::Channel {
                    value: self.is_value(channel),
                    bound: bound.transpose()?,
                })
            })
            .collect::<Result<_, String>>()?;
        let mut nodes = Vec::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let behaviour = node.behaviour.as_ref();
            let kind = behaviour.map_or(Err("a running node".into()), |node| node.export());
            let kind = kind.unwrap_or_else(crate::export::Kind::Opaque);
            nodes.push(crate::export::Node {
                name: node.name.to_string(),
                scope: self.node_scopes[index],
                inputs: node
                    .inputs
                    .iter()
                    .map(|input| input.channel.0 as usize)
                    .collect(),
                outputs: node
                    .outputs
                    .iter()
                    .map(|output| output.0 as usize)
                    .collect(),
                kind,
            });
        }
        let mut scopes = std::mem::take(&mut self.scopes);
        if scopes.is_empty() {
            scopes.push(crate::export::Scope {
                name: String::new(),
                parent: None,
                takes: Vec::new(),
                emits: Vec::new(),
            });
        }
        Ok(crate::export::Network {
            channels,
            nodes,
            scopes,
            emits,
            params,
            script: String::new(),
            configs: Vec::new(),
            config: String::new(),
            schema: None,
            launch: String::new(),
            processes: self.processes(),
            reads: Vec::new(),
            scripts: Vec::new(),
            handlers: Vec::new(),
        })
    }

    /// Frees the files of the run's folder once no node can pass them on any more.
    pub(crate) fn clean(&mut self, folder: std::path::PathBuf) {
        self.cleanup = Some(Cleanup::new(folder));
    }

    /// Deletes the files of the run's folder that no node can pass on any more: every node that
    /// sent one and every node that reads what they write have stopped, and none publishes.
    fn sweep(&mut self) {
        let Some(mut cleanup) = self.cleanup.take() else {
            return;
        };
        cleanup.sweep(|node| self.finished(node));
        self.cleanup = Some(cleanup);
    }

    /// Whether a node and every node that reads what it writes have stopped, and none is a
    /// workflow output. A reader that passes a file on sends it, and counts as its sender.
    fn finished(&self, node: usize) -> bool {
        let channels = self.nodes[node].outputs.iter().chain(&self.wrote[node]);
        let readers = channels.flat_map(|channel| &self.consumers[channel.0 as usize]);
        std::iter::once(&node).chain(readers).all(|index| {
            let node = &self.nodes[*index];
            let output = node
                .behaviour
                .as_ref()
                .is_some_and(|node| node.output().is_some());
            node.state == State::Stopped && !output
        })
    }

    /// The processes of the network in the order they were called, numbered from 1, each with
    /// the processes that its outputs reach through other nodes.
    pub(crate) fn processes(&self) -> Vec<nextflow_plugins::observer::Process> {
        let mut ids = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            if let Some(behaviour) = &node.behaviour
                && behaviour.definition().is_some()
            {
                ids.insert(index, ids.len() as u32 + 1);
            }
        }
        let mut processes = Vec::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let (Some(id), Some(definition)) = (
                ids.get(&index),
                node.behaviour.as_ref().and_then(|node| node.definition()),
            ) else {
                continue;
            };
            let mut feeds = BTreeSet::new();
            let mut seen: HashSet<ChannelId> = node.outputs.iter().copied().collect();
            let mut channels = node.outputs.clone();
            while let Some(channel) = channels.pop() {
                for reader in &self.readers[channel.0 as usize] {
                    match ids.get(reader) {
                        Some(id) => {
                            feeds.insert(*id);
                        }
                        None => {
                            let outputs = &self.nodes[*reader].outputs;
                            channels.extend(outputs.iter().filter(|output| seen.insert(**output)));
                        }
                    }
                }
            }
            processes.push(nextflow_plugins::observer::Process {
                id: *id,
                name: node.name.to_string(),
                definition: definition.name.to_string(),
                script: definition.script.map(|script| script.to_string()),
                feeds: feeds.into_iter().collect(),
            });
        }
        processes
    }

    /// The workflow outputs of the network, with the files that they published.
    pub(crate) fn outputs(&self) -> Vec<nextflow_plugins::observer::Output> {
        let nodes = self.nodes.iter().filter_map(|node| node.behaviour.as_ref());
        nodes.filter_map(|node| node.output()).collect()
    }

    /// Starts every node in the order it was created, then runs until nothing is waiting.
    pub fn ignite(&mut self, interpreter: &mut Interpreter) -> Result<(), Error> {
        self.wire_topics();
        for index in 0..self.nodes.len() {
            self.step(index, interpreter, Step::Ignite)?;
            self.enqueue(index);
        }
        self.run(interpreter)
    }

    /// Each topic that a script reads mixes all its sources, or is empty without any.
    fn wire_topics(&mut self) {
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
            self.fresh = vec![output];
            self.add(&format!("topic {name}"), &sources, Box::new(mix));
        }
    }

    /// Runs the nodes that have messages waiting until there are none.
    pub fn run(&mut self, interpreter: &mut Interpreter) -> Result<(), Error> {
        while self.owners.len() < MAX_OUTSTANDING {
            let Some(index) = self.waiting.pop_front() else {
                break;
            };
            self.queued[index] = false;
            self.step(index, interpreter, Step::Run)?;
        }
        Ok(())
    }

    /// The tasks that nodes asked for since the last call.
    pub fn take_requests(&mut self) -> Vec<TaskRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Lets tasks go on starting after a task failed, which fails the run at its end.
    pub fn keep_going(&mut self, keep_going: bool) {
        self.keep_going = keep_going;
    }

    /// Whether the run ends once the running tasks have ended, after a task with the error
    /// strategy `finish` failed: no other task starts, as Nextflow submits none.
    pub fn draining(&self) -> bool {
        self.failure.is_some() && !self.keep_going
    }

    /// The failure that ends the run once the running tasks have ended, if there is one.
    pub fn take_failure(&mut self) -> Option<Error> {
        self.failure.take()
    }

    /// The files that nodes asked to deliver since the last call.
    pub fn take_deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    pub fn take_acceptance(&mut self, task: TaskId) -> bool {
        self.acceptance.remove(&task).unwrap_or(false)
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
        self.enqueue(index);
        self.run(interpreter)?;
        self.sweep();
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
            deliveries: Vec::new(),
            failure: None,
            keep_going: self.keep_going,
            next_task: &mut self.next_task,
            succeeded: &mut self.succeeded,
            failed_task: &mut self.failed_task,
            acceptance: &mut self.acceptance,
            task_files: &mut self.task_files,
            interpreter,
            again: false,
            again_first: false,
            sent: self.cleanup.is_some().then(Vec::new),
        };
        let result = match step {
            Step::Ignite => behaviour.ignite(&mut context),
            Step::Run => behaviour.run(&mut context),
            Step::Complete(outcome) => behaviour.complete(&mut context, outcome),
        };
        let written = std::mem::take(&mut context.written);
        if let (Some(cleanup), Some(sent)) = (&mut self.cleanup, context.sent.take()) {
            sent.iter().for_each(|value| cleanup.sent(index, value));
            for delivery in &context.deliveries {
                cleanup.delivered(&delivery.location);
            }
        }
        let mut requests = std::mem::take(&mut context.requests);
        if let Some(failure) = context.failure.take() {
            self.failure.get_or_insert(failure);
        }
        // After a failure that ends the run once the running tasks end, no task starts, unless
        // the run keeps going.
        if self.failure.is_some() && !self.keep_going {
            requests.clear();
        }
        let again = context.again;
        let again_first = context.again_first;
        for request in &requests {
            self.owners.insert(request.id, index);
        }
        self.requests.extend(requests);
        self.deliveries.append(&mut context.deliveries);
        node.behaviour = Some(behaviour);
        for channel in &written {
            if !node.outputs.contains(channel) && !self.wrote[index].contains(channel) {
                self.wrote[index].push(*channel);
            }
        }
        node.state = result?;
        if node.state == State::Stopped {
            for input in &node.inputs {
                self.channels[input.channel.0 as usize].unsubscribe(input.cursor);
                self.readers[input.channel.0 as usize].retain(|reader| *reader != index);
            }
        }
        for channel in written {
            for reader in self.readers[channel.0 as usize].clone() {
                self.enqueue(reader);
            }
        }
        if again {
            self.enqueue(index);
        }
        if again_first && !self.queued[index] && self.nodes[index].state == State::Running {
            self.queued[index] = true;
            self.waiting.push_front(index);
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

#[cfg(test)]
mod tests {
    use super::*;
    use nextflow_evaluator::interpreter::Host;

    struct Silent;
    impl Host for Silent {
        fn print(&mut self, _: &str) {}
    }

    struct Drain;
    impl Behaviour for Drain {
        fn run(&mut self, context: &mut Context) -> Result<State, Error> {
            while context.read(0).is_some() {}
            Ok(State::Running)
        }
    }

    #[test]
    fn releases_stopped_subscriptions() {
        let mut network = Network::default();
        let channel = network.channel(false);
        let output = network.channel(false);
        network.add(
            "take",
            &[channel],
            Box::new(crate::operator::Single {
                kind: crate::operator::Each::Take(1),
                output,
            }),
        );
        network.add("drain", &[channel], Box::new(Drain));
        let mut host = Silent;
        let mut interpreter = Interpreter::new(&mut host);
        network.ignite(&mut interpreter).unwrap();
        for _ in 0..100 {
            for _ in 0..100 {
                network.channels[channel.0 as usize].write(Message::Value(Value::Null));
            }
            network.enqueue(0);
            network.enqueue(1);
            network.run(&mut interpreter).unwrap();
            let Channel::Queue { messages, .. } = &network.channels[channel.0 as usize] else {
                panic!("queue")
            };
            assert!(messages.is_empty());
        }
        assert_eq!(network.readers[channel.0 as usize], [1]);
    }

    #[test]
    fn yields_factory_values() {
        struct Count {
            count: Rc<std::cell::Cell<usize>>,
        }
        impl Behaviour for Count {
            fn run(&mut self, context: &mut Context) -> Result<State, Error> {
                let Channel::Queue { messages, .. } = &context.channels[0] else {
                    panic!("queue")
                };
                assert!(messages.len() <= 1, "factory emitted ahead of its reader");
                while let Some(message) = context.read(0) {
                    let Channel::Queue { messages, .. } = &context.channels[0] else {
                        panic!("queue")
                    };
                    assert!(messages.len() <= 1, "factory retained unread messages");
                    if matches!(message, Message::Stop) {
                        return Ok(State::Stopped);
                    }
                    self.count.set(self.count.get() + 1);
                }
                Ok(State::Running)
            }
        }
        let mut network = Network::default();
        let output = network.channel(false);
        network.add(
            "factory",
            &[],
            Box::new(crate::operator::Factory {
                values: vec![Value::Null; 10_000],
                output,
                close: true,
                failure: None,
                call: None,
            }),
        );
        let count = Rc::new(std::cell::Cell::new(0));
        network.add(
            "count",
            &[output],
            Box::new(Count {
                count: count.clone(),
            }),
        );
        let mut host = Silent;
        let mut interpreter = Interpreter::new(&mut host);
        network.ignite(&mut interpreter).unwrap();
        assert_eq!(count.get(), 10_000);
        assert!(network.running().is_empty());
    }
}

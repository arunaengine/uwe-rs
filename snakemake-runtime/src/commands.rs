//! Commands that workflow code runs while the workflow is planned, like `shell()` or
//! `subprocess.run`. They never run on the machine of iunctus (decision 11): each becomes a
//! container task that the engine runs like the task of a job. The image is the workflow's
//! global `container:`, else a small default image. The task runs in the working directory of
//! the workflow, which it changes where it is, as Snakemake runs the command there: the files
//! that the command writes stay for the workflow code and the jobs after it. So such a task
//! never comes from the cache.
//!
//! The session answers each request of the engine once. A command hands its task to the engine
//! as the answer to the request that the session is handling, and waits for the outcome of that
//! task. Outcomes of other tasks that come meanwhile get an empty answer and wait until the
//! session has handled the request that ran the command.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};

use python_evaluator::{Command, CommandOutcome, Error};
use workflow_graph::{DriverError, Input, Inputs, Outcome, Task, TaskId, Writable};

use crate::workflow::State;

/// The image of commands of a workflow without a global `container:`, the default image of
/// the CWL runtime too.
pub const DEFAULT_IMAGE: &str = "debian:stable-slim";

/// What the engine asks of the session.
pub enum Request {
    /// The inputs of the workflow, before the run starts.
    Inputs,
    /// The outputs of the workflow, before the run starts.
    Outputs,
    Start,
    Complete(Box<Outcome>),
    Finish,
}

/// The answer to a request: the tasks that can run now.
pub type Reply = Result<Vec<Task>, DriverError>;

/// The answer to a request for the inputs, which comes back on a channel of its own.
pub type InputsReply = Result<Option<Inputs>, DriverError>;

/// The answer to a request for the outputs, the schema of the results of a run, which comes
/// back on a channel of its own.
pub type OutputsReply = Result<Option<serde_json::Value>, DriverError>;

/// The session's end of its connection to the engine.
pub struct Channel {
    pub requests: Receiver<Request>,
    pub replies: Sender<Reply>,
    /// Outcomes of tasks of jobs that came while a command waited for its own.
    pub parked: RefCell<VecDeque<Outcome>>,
    /// Whether the session loads the workflow only to describe its inputs, which runs no task.
    pub describing: Cell<bool>,
    /// Whether a command of workflow code had to be refused while describing.
    pub refused: Cell<bool>,
}

impl Channel {
    pub fn new(requests: Receiver<Request>, replies: Sender<Reply>) -> Channel {
        Channel {
            requests,
            replies,
            parked: RefCell::new(VecDeque::new()),
            describing: Cell::new(false),
            refused: Cell::new(false),
        }
    }
}

/// Runs the commands of workflow code as tasks.
pub struct CommandRunner {
    pub channel: Rc<Channel>,
    /// The number of the next task, shared with the session.
    pub next_task: Rc<Cell<u64>>,
    pub state: Rc<RefCell<State>>,
}

fn ended() -> Error {
    Error::runtime_error("the engine ended while a command ran")
}

impl CommandRunner {
    /// The image of the workflow's global `container:`, else the default image.
    fn image(&self) -> String {
        let state = self.state.borrow();
        state
            .modifiers
            .first()
            .and_then(|modifier| modifier.container.as_ref())
            .and_then(|container| container.as_str().map(crate::task::image_name))
            .unwrap_or_else(|| DEFAULT_IMAGE.to_owned())
    }

    fn task(&self, id: TaskId, command: &Command) -> Task {
        let script = match command.argv.as_slice() {
            [shell, flag, text] if shell.ends_with("sh") && flag == "-c" => Some(text.clone()),
            _ => None,
        };
        let shown = script.clone().unwrap_or_else(|| command.argv.join(" "));
        let shown: String = shown.chars().take(60).collect();
        let workdir: PathBuf = self.state.borrow().workdir.clone();
        Task {
            id,
            name: format!("command {shown}"),
            step: "command".into(),
            image: Some(self.image()),
            command: command.argv.clone(),
            script,
            workdir: None,
            environment: Default::default(),
            inputs: vec![Input {
                path: ".".into(),
                source: workdir.to_string_lossy().into_owned(),
                directory: true,
                content: None,
                writable: Writable::InPlace,
            }],
            outputs: Vec::new(),
            stdin: None,
            resources: Default::default(),
            network: workflow_graph::Network::Unstated,
            after: Vec::new(),
            containers: Vec::new(),
            replaces_entrypoint: false,
        }
    }

    /// Runs a command as a task and waits for its outcome.
    pub fn run(&self, command: &Command) -> Result<CommandOutcome, Error> {
        if self.channel.describing.get() {
            self.channel.refused.set(true);
            return Err(Error::runtime_error(
                "the workflow runs a command while it is loaded, which only a run can do",
            ));
        }
        let id = TaskId(self.next_task.get());
        self.next_task.set(id.0 + 1);
        let task = self.task(id, command);
        self.channel
            .replies
            .send(Ok(vec![task]))
            .map_err(|_| ended())?;
        loop {
            match self.channel.requests.recv().map_err(|_| ended())? {
                Request::Complete(outcome) if outcome.id == id => {
                    return Ok(CommandOutcome {
                        exit_code: outcome.exit_code,
                        stdout: outcome.stdout.into_bytes(),
                        stderr: outcome.stderr.into_bytes(),
                    });
                }
                Request::Complete(outcome) => {
                    self.channel.parked.borrow_mut().push_back(*outcome);
                    self.channel
                        .replies
                        .send(Ok(Vec::new()))
                        .map_err(|_| ended())?;
                }
                Request::Inputs | Request::Outputs | Request::Start | Request::Finish => {
                    return Err(Error::runtime_error(
                        "the engine asked the session to start or finish while a command ran",
                    ));
                }
            }
        }
    }
}

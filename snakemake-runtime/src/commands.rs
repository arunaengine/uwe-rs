//! Runs planning commands as uncached container tasks and synchronizes their engine outcomes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};

use python_evaluator::{Command, CommandOutcome, Error};
use workflow_graph::{Delivery, DriverError, Input, Inputs, Outcome, Task, TaskId, Writable};

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
    Delivered(Delivery),
    Finish,
    Failed(String),
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
    pub parked_deliveries: RefCell<VecDeque<Delivery>>,
    /// Whether the session loads the workflow only to describe its inputs, which runs no task.
    pub describing: Cell<bool>,
    /// Whether a command of workflow code had to be refused while describing.
    pub refused: Cell<bool>,
    pub failed: RefCell<Option<String>>,
}

impl Channel {
    pub fn new(requests: Receiver<Request>, replies: Sender<Reply>) -> Channel {
        Channel {
            requests,
            replies,
            parked: RefCell::new(VecDeque::new()),
            parked_deliveries: RefCell::new(VecDeque::new()),
            describing: Cell::new(false),
            refused: Cell::new(false),
            failed: RefCell::new(None),
        }
    }
}

/// Runs the commands of workflow code as tasks.
pub struct CommandRunner {
    pub channel: Rc<Channel>,
    /// The number of the next task, shared with the session.
    pub next_task: Rc<Cell<u64>>,
    pub state: Rc<RefCell<State>>,
    pub settings: crate::driver::RunSettings,
    pub engine: String,
    pub scratch: PathBuf,
    pub simulated: bool,
    pub cancel: executor::CancellationToken,
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
            .unwrap_or_else(|| {
                self.settings
                    .image
                    .clone()
                    .unwrap_or_else(|| DEFAULT_IMAGE.to_owned())
            })
    }

    fn task(&self, id: TaskId, command: &Command) -> Task {
        let script = match command.argv.as_slice() {
            [shell, flag, text] if shell.ends_with("sh") && flag == "-c" => Some(text.clone()),
            _ => None,
        };
        let shown = script.clone().unwrap_or_else(|| command.argv.join(" "));
        let shown: String = shown.chars().take(60).collect();
        let workdir: PathBuf = command.cwd.clone();
        Task {
            output_manifest: None,
            run_as: None,
            id,
            name: format!("command {shown}"),
            step: "command".into(),
            image: Some(self.image()),
            command: command.argv.clone(),
            script,
            workdir: Some(workdir.to_string_lossy().into_owned()),
            environment: Default::default(),
            inputs: std::iter::once(Input {
                executable: false,
                path: ".".into(),
                source: workdir.to_string_lossy().into_owned(),
                directory: true,
                content: None,
                writable: Writable::InPlace,
            })
            .chain(
                self.settings
                    .shared_workdir
                    .then(crate::shared::local_time)
                    .flatten(),
            )
            .collect(),
            outputs: Vec::new(),
            stdin: None,
            defer_logs: false,
            resources: Default::default(),
            network: workflow_graph::Network::Unstated,
            after: Vec::new(),
            containers: Vec::new(),
            replaces_entrypoint: false,
            reuse: workflow_graph::Reuse::Allowed,
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
                Request::Delivered(delivery) => {
                    self.channel
                        .parked_deliveries
                        .borrow_mut()
                        .push_back(delivery);
                    self.channel
                        .replies
                        .send(Ok(Vec::new()))
                        .map_err(|_| ended())?;
                }
                Request::Failed(error) => {
                    *self.channel.failed.borrow_mut() = Some(error);
                    return Err(ended());
                }
                Request::Inputs | Request::Outputs | Request::Start | Request::Finish => {
                    return Err(Error::runtime_error(
                        "the engine asked the session to start or finish while a command ran",
                    ));
                }
            }
        }
    }

    pub fn run_hook(&self, command: &Command, hook: &str) -> Result<CommandOutcome, Error> {
        if self.simulated {
            return Ok(CommandOutcome::default());
        }
        let id = TaskId(self.next_task.get());
        self.next_task.set(id.0 + 1);
        let task = self.task(id, command);
        let cancel = hook_cancel(hook, &self.cancel);
        let outcome = crate::hooks::run(
            &task,
            &self.engine,
            &self.scratch,
            self.settings.offline,
            &cancel,
        )?;
        if outcome.exit_code != 0 {
            eprintln!(
                "{hook} hook command in {} exited with {}:\n{}",
                task.image.as_deref().unwrap_or(DEFAULT_IMAGE),
                outcome.exit_code,
                String::from_utf8_lossy(&outcome.stderr)
            );
        }
        Ok(outcome)
    }
}

fn hook_cancel(hook: &str, cancel: &executor::CancellationToken) -> executor::CancellationToken {
    if hook == "onerror" {
        executor::CancellationToken::new()
    } else {
        cancel.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolates_error_cleanup() {
        let work = executor::CancellationToken::new();
        let start = hook_cancel("onstart", &work);
        let success = hook_cancel("onsuccess", &work);
        let error = hook_cancel("onerror", &work);
        work.cancel();
        assert!(start.is_cancelled());
        assert!(success.is_cancelled());
        assert!(!error.is_cancelled());
        assert!(!hook_cancel("onerror", &work).is_cancelled());
    }
}

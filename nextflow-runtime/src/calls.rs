//! The model calls of agent tasks, which the driver makes on threads of their own while the
//! run goes on, with the answers kept between runs for `-resume`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_plugins::nf_agent::{self, Endpoint, Options, Question, Refused};
use serde_json::{Value as Json, json};

use crate::agent::Call;
use crate::{TaskId, TaskOutcome, TaskRequest};

/// How often the driver looks for answers while model calls run.
const POLL: Duration = Duration::from_millis(20);

/// The model calls of a run.
#[derive(Default)]
pub(crate) struct Calls {
    options: Option<Result<Options, String>>,
    /// Where the answers of earlier runs are, one file per call by its key.
    folder: PathBuf,
    resume: bool,
    simulated: bool,
    running: Vec<Running>,
    /// When the driver looks for answers next, while calls run.
    next_look: Option<Instant>,
    /// The answer of each task attempt, or why it has none.
    answers: HashMap<(TaskId, u32), Result<String, String>>,
    warned: HashSet<String>,
}

/// A call in flight, with the key that its answer is kept under.
struct Running {
    request: TaskRequest,
    key: Option<String>,
    answer: Receiver<Result<String, String>>,
}

/// What a call needs once it leaves the driver thread.
struct Prepared {
    endpoint: Endpoint,
    question: Question,
    timeout: Duration,
    key: Option<String>,
}

impl Calls {
    /// The calls of a run with this config; answers are kept in `folder`, and a resumed run
    /// reads them. A simulated run makes its answers up.
    pub(crate) fn new(config: &Config, folder: PathBuf, resume: bool, simulated: bool) -> Calls {
        Calls {
            options: Some(Options::of(config)),
            folder,
            resume,
            simulated,
            ..Calls::default()
        }
    }

    /// Starts the model call of an agent task, unless its answer is known: then it gives the
    /// request back, as for any other task. A missing credential stops the run.
    pub(crate) fn ask(
        &mut self,
        request: TaskRequest,
        host: &mut dyn Host,
    ) -> Result<Option<TaskRequest>, String> {
        let identity = (request.id, request.attempt);
        let Some(call) = request.agent.as_deref() else {
            return Ok(Some(request));
        };
        if self.answers.contains_key(&identity) {
            return Ok(Some(request));
        }
        if self.simulated {
            let answer =
                (call.schema.as_ref()).map_or(String::new(), |s| placeholder(s).to_string());
            self.answers.insert(identity, Ok(answer));
            return Ok(Some(request));
        }
        let prepared = match self.prepare(&request, call, host) {
            Ok(prepared) => prepared,
            Err(Refused::Task(message)) => {
                self.answers.insert(identity, Err(message));
                return Ok(Some(request));
            }
            Err(Refused::Run(message)) => return Err(message),
        };
        let stored = (prepared.key.as_deref())
            .filter(|_| self.resume && request.attempt == 1)
            .and_then(|key| self.stored(key));
        if let Some(answer) = stored {
            self.answers.insert(identity, Ok(answer));
            return Ok(Some(request));
        }
        let (sender, answer) = mpsc::channel();
        let Prepared {
            endpoint,
            question,
            timeout,
            key,
        } = prepared;
        std::thread::spawn(move || {
            let _ = sender.send(nf_agent::ask(&endpoint, &question, timeout));
        });
        self.running.push(Running {
            request,
            key,
            answer,
        });
        self.next_look.get_or_insert_with(|| Instant::now() + POLL);
        Ok(None)
    }

    /// When the driver should look for answers again.
    pub(crate) fn wake_at(&self) -> Option<Instant> {
        self.next_look
    }

    /// The requests whose calls ended since the last look; their answers are kept, also for
    /// later runs.
    pub(crate) fn answered(&mut self) -> Vec<TaskRequest> {
        let mut done = Vec::new();
        self.next_look = None;
        let mut index = 0;
        while index < self.running.len() {
            let result = match self.running[index].answer.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => {
                    Err("the model call ended without an answer".into())
                }
            };
            let running = self.running.remove(index);
            if let (Ok(answer), Some(key)) = (&result, &running.key) {
                self.store(key, answer);
            }
            let identity = (running.request.id, running.request.attempt);
            self.answers.insert(identity, result);
            done.push(running.request);
        }
        if !self.running.is_empty() {
            self.next_look = Some(Instant::now() + POLL);
        }
        done
    }

    /// The outcome of an agent task: the model's answer as its standard output, or a failure.
    pub(crate) fn outcome(&mut self, request: &TaskRequest) -> TaskOutcome {
        let identity = (request.id, request.attempt);
        let answer = (self.answers.remove(&identity))
            .unwrap_or_else(|| Err("the agent task has no answer".into()));
        match answer {
            Ok(stdout) => TaskOutcome {
                id: request.id,
                stdout,
                ..TaskOutcome::default()
            },
            Err(message) => TaskOutcome {
                id: request.id,
                failure: Some(Error::Thrown {
                    exception: Rc::new(Exception::new("ProcessFailedException", message)),
                    span: Default::default(),
                }),
                ..TaskOutcome::default()
            },
        }
    }

    /// The endpoint, the messages and the cache key of a call, on the driver thread, which
    /// reads the environment and prints the warnings of the credential lookup once.
    fn prepare(
        &mut self,
        request: &TaskRequest,
        call: &Call,
        host: &mut dyn Host,
    ) -> Result<Prepared, Refused> {
        let options = match &self.options {
            Some(Ok(options)) => options,
            Some(Err(message)) => return Err(Refused::Run(message.clone())),
            None => return Err(Refused::Task("the agent options were not read".into())),
        };
        let endpoint = nf_agent::endpoint(options, &call.model, &|name| host.environment(name))?;
        for warning in &endpoint.warnings {
            if self.warned.insert(warning.clone()) {
                host.print(&format!("WARN: {warning}\n"));
            }
        }
        let key = cacheable(request).then(|| {
            nf_agent::cache_key(&json!({
                "runner": "langchain4j",
                "version": env!("CARGO_PKG_VERSION"),
                "task": &*request.process,
                "model": call.model,
                "endpoint": endpoint.url,
                "system": call.system,
                "user": call.user,
                "schema": call.schema,
                "maxIterations": call.max_iterations,
                "files": files(request),
            }))
        });
        let model = call.model.split_once('/').map_or("", |(_, name)| name);
        Ok(Prepared {
            question: Question {
                agent: call.agent.clone(),
                model: model.to_owned(),
                system: call.system.clone(),
                user: call.user.clone(),
                schema: call.schema.clone(),
            },
            endpoint,
            timeout: options.timeout,
            key,
        })
    }

    fn stored(&self, key: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.folder.join(format!("{key}.json"))).ok()?;
        let entry: Json = serde_json::from_str(&text).ok()?;
        entry["answer"].as_str().map(str::to_owned)
    }

    /// Keeps an answer for later runs; a run that cannot write it only loses the reuse.
    fn store(&self, key: &str, answer: &str) {
        let path = self.folder.join(format!("{key}.json"));
        let partial = self
            .folder
            .join(format!("{key}.{}.partial", std::process::id()));
        let written = std::fs::create_dir_all(&self.folder)
            .and_then(|()| std::fs::write(&partial, json!({"answer": answer}).to_string()))
            .and_then(|()| std::fs::rename(&partial, &path));
        if let Err(error) = written {
            tracing::warn!(path = %path.display(), "cannot keep the answer of an agent task: {error}");
        }
    }
}

/// Whether the task may read and write kept answers: not with `cache false`.
fn cacheable(request: &TaskRequest) -> bool {
    let cache = (request.directives.iter().rev())
        .find(|directive| matches!(&*directive.name, "cache" | "cacheable"))
        .and_then(|directive| directive.arguments.first());
    !matches!(cache, Some(Value::Boolean(false)))
}

/// The input files of a task with their size and change time, as Nextflow's standard cache
/// mode keys them.
fn files(request: &TaskRequest) -> Json {
    let files = request.staged.iter().map(|(file, name)| {
        let metadata = std::fs::metadata(&file.path).ok();
        let size = metadata.as_ref().map(std::fs::Metadata::len);
        let changed = (metadata.and_then(|metadata| metadata.modified().ok()))
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_millis() as u64);
        json!([name, file.uri(), size, changed])
    });
    Json::Array(files.collect())
}

/// A made-up answer that fits a schema, for simulated runs.
fn placeholder(schema: &Json) -> Json {
    let kind = match &schema["type"] {
        Json::Array(kinds) => kinds.first().cloned().unwrap_or_default(),
        kind => kind.clone(),
    };
    match kind.as_str() {
        Some("string") => json!(""),
        Some("integer") => json!(0),
        Some("number") => json!(0.0),
        Some("boolean") => json!(false),
        Some("array") => json!([]),
        Some("object") => Json::Object(
            (schema["properties"].as_object().into_iter().flatten())
                .map(|(name, property)| (name.clone(), placeholder(property)))
                .collect(),
        ),
        _ => Json::Null,
    }
}

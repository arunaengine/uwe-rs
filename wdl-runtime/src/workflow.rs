//! Runs the statements of a WDL workflow once the names they use have values: declarations,
//! calls, scatters and conditionals, each scatter item and taken branch in a scope of its own.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use indexmap::IndexMap;
use wdl_analysis::Document;
use wdl_analysis::types::Optional;
use wdl_ast::AstToken;
use wdl_ast::v1::{BoundDecl, Decl, OutputSection, WorkflowStatement};
use workflow_graph::{DriverError, Outcome, Task, TaskId};

use crate::document::{Callable, Unit, declared};
use crate::expression::{self, Environment, Error};
use crate::task::{self, Settings};
use crate::value::Value;

#[path = "statements.rs"]
mod statements;

use statements::{failed, item};

/// The state of one run of a workflow or a task.
pub struct Run {
    frames: Vec<Frame>,
    blocks: Vec<Block>,
    gathers: Vec<Gather>,
    calls: HashMap<TaskId, Call>,
    next: u64,
    settings: Settings,
    /// Where files that the workflow writes itself go.
    scratch: PathBuf,
    written: usize,
    /// Inputs of the calls of the main workflow from the input file, by call name.
    nested: BTreeMap<String, BTreeMap<String, Value>>,
    keep_going: bool,
    /// Why calls failed, in the order they ended.
    pub failures: Vec<String>,
    /// The outputs of the main workflow or task once it finished.
    pub outputs: Option<IndexMap<String, Value>>,
}

/// The names of a scope; scatter items and branches see the names of their parent.
struct Frame {
    parent: Option<usize>,
    names: BTreeMap<String, Value>,
}

/// Statements that run in one scope.
struct Block {
    frame: usize,
    document: Document,
    steps: Vec<Item>,
    /// Steps that started and have not finished: calls and inner blocks.
    active: usize,
    parent: Parent,
    /// The scatter positions of the block, for task names.
    shard: Vec<usize>,
    /// The workflow the statements belong to, for step names.
    path: String,
    done: bool,
}

/// What a block reports to when it is done.
enum Parent {
    Main(Unit),
    Scatter {
        gather: usize,
        index: usize,
    },
    Branch {
        block: usize,
        names: Vec<String>,
    },
    Workflow {
        block: usize,
        alias: String,
        unit: Unit,
    },
}

/// The names that the items of a scatter give, as arrays once every item is done.
struct Gather {
    block: usize,
    names: Vec<String>,
    results: Vec<BTreeMap<String, Value>>,
    remaining: usize,
}

/// A call of a task that runs.
struct Call {
    block: usize,
    alias: String,
    name: String,
    state: task::State,
}

/// A statement with the names it needs from outside.
#[derive(Clone)]
struct Item {
    step: Step,
    free: BTreeSet<String>,
}

#[derive(Clone)]
enum Step {
    Decl(BoundDecl),
    Statement(WorkflowStatement),
}

impl Run {
    pub fn new(settings: Settings, scratch: PathBuf, keep_going: bool) -> Self {
        Run {
            frames: Vec::new(),
            blocks: Vec::new(),
            gathers: Vec::new(),
            calls: HashMap::new(),
            next: 1,
            settings,
            scratch,
            written: 0,
            nested: BTreeMap::new(),
            keep_going,
            failures: Vec::new(),
            outputs: None,
        }
    }

    /// Starts the main workflow or task with the values of its inputs, and the inputs of the
    /// calls of the workflow that the input file gives, by call name.
    pub fn start(
        &mut self,
        unit: Unit,
        given: BTreeMap<String, Value>,
        nested: BTreeMap<String, BTreeMap<String, Value>>,
    ) -> Result<Vec<Task>, DriverError> {
        self.nested = nested;
        let frame = self.frame(None);
        let name = unit.name();
        let block = self.block(
            frame,
            &unit.document,
            Parent::Main(unit.clone()),
            Vec::new(),
            name.clone(),
        );
        match &unit.callable {
            Callable::Workflow(_) => {
                let steps = self.inputs(&unit, frame, given)?;
                self.blocks[block].steps = steps;
                self.advance(block)
            }
            Callable::Task(definition) => {
                let names = (name.clone(), name.clone(), name);
                let task = self.plan(block, &unit, definition, given, names)?;
                Ok(vec![task])
            }
        }
    }

    /// The files that the tasks make, where the executor keeps them.
    pub fn use_files(&mut self, files: std::sync::Arc<dyn workflow_graph::Files>) {
        self.settings.files = files;
    }

    /// Takes the outcome of a task: its call gets its outputs, and what waited for them runs.
    pub fn complete(&mut self, outcome: &Outcome) -> Result<Vec<Task>, DriverError> {
        let Some(call) = self.calls.remove(&outcome.id) else {
            return Ok(Vec::new());
        };
        match task::finish(&call.state, outcome, &self.settings) {
            Ok(outputs) => {
                let frame = self.blocks[call.block].frame;
                self.frames[frame].names.insert(call.alias, outputs);
                self.blocks[call.block].active -= 1;
                self.advance(call.block)
            }
            Err(message) => {
                self.failures.push(format!("{}: {message}", call.name));
                Ok(Vec::new())
            }
        }
    }

    /// The calls that wait for something that never came.
    pub fn waiting(&self) -> Vec<String> {
        let mut waiting = Vec::new();
        for block in self.blocks.iter().filter(|block| !block.done) {
            for item in &block.steps {
                let missing: Vec<&str> = (item.free.iter())
                    .filter(|name| self.lookup(block.frame, name).is_none())
                    .map(String::as_str)
                    .collect();
                waiting.push(format!("{} needs {}", block.path, missing.join(", ")));
            }
        }
        waiting
    }

    fn stopped(&self) -> bool {
        !self.failures.is_empty() && !self.keep_going
    }

    fn frame(&mut self, parent: Option<usize>) -> usize {
        self.frames.push(Frame {
            parent,
            names: BTreeMap::new(),
        });
        self.frames.len() - 1
    }

    fn block(
        &mut self,
        frame: usize,
        document: &Document,
        parent: Parent,
        shard: Vec<usize>,
        path: String,
    ) -> usize {
        self.blocks.push(Block {
            frame,
            document: document.clone(),
            steps: Vec::new(),
            active: 0,
            parent,
            shard,
            path,
            done: false,
        });
        self.blocks.len() - 1
    }

    fn lookup(&self, frame: usize, name: &str) -> Option<&Value> {
        let mut current = Some(frame);
        while let Some(index) = current {
            if let Some(value) = self.frames[index].names.get(name) {
                return Some(value);
            }
            current = self.frames[index].parent;
        }
        None
    }

    /// The input values of a workflow in its frame, and the defaults still to evaluate.
    fn inputs(
        &mut self,
        unit: &Unit,
        frame: usize,
        given: BTreeMap<String, Value>,
    ) -> Result<Vec<Item>, DriverError> {
        let Callable::Workflow(workflow) = &unit.callable else {
            return Ok(Vec::new());
        };
        let mut steps = Vec::new();
        let decls: Vec<Decl> = (workflow.input())
            .map(|section| section.declarations().collect())
            .unwrap_or_default();
        for decl in decls {
            let name = decl.name().text().to_owned();
            let ty = declared(&unit.document, &decl.ty()).map_err(DriverError::new)?;
            match (given.get(&name), decl) {
                (Some(value), _) => {
                    let value = value
                        .clone()
                        .coerce(&ty)
                        .map_err(|error| DriverError::new(format!("input `{name}`: {error}")))?;
                    self.frames[frame].names.insert(name, value);
                }
                (None, Decl::Bound(bound)) => steps.push(item(Step::Decl(bound))),
                (None, Decl::Unbound(_)) if ty.is_optional() => {
                    self.frames[frame].names.insert(name, Value::None);
                }
                (None, Decl::Unbound(_)) => {
                    return Err(DriverError::new(format!(
                        "the input `{name}` of workflow {} has no value",
                        unit.name()
                    )));
                }
            }
        }
        steps.extend(
            workflow
                .statements()
                .map(|statement| item(Step::Statement(statement))),
        );
        Ok(steps)
    }

    /// Starts every step of a block whose names are known; a block without steps left reports.
    fn advance(&mut self, block: usize) -> Result<Vec<Task>, DriverError> {
        let mut tasks = Vec::new();
        while !self.stopped() {
            let frame = self.blocks[block].frame;
            let ready = (self.blocks[block].steps.iter()).position(|item| {
                item.free
                    .iter()
                    .all(|name| self.lookup(frame, name).is_some())
            });
            let Some(ready) = ready else {
                break;
            };
            let item = self.blocks[block].steps.remove(ready);
            tasks.extend(self.run_step(block, item.step)?);
        }
        let current = &self.blocks[block];
        if current.steps.is_empty() && current.active == 0 && !current.done && !self.stopped() {
            self.blocks[block].done = true;
            tasks.extend(self.close(block)?);
        }
        Ok(tasks)
    }

    /// Reports a block that is done to its parent.
    fn close(&mut self, block: usize) -> Result<Vec<Task>, DriverError> {
        let frame = self.blocks[block].frame;
        match std::mem::replace(
            &mut self.blocks[block].parent,
            Parent::Scatter {
                gather: usize::MAX,
                index: 0,
            },
        ) {
            Parent::Main(unit) => {
                let outputs = match &unit.callable {
                    Callable::Workflow(workflow) => {
                        self.outputs_of(&unit.document, workflow.output(), frame)?
                    }
                    Callable::Task(_) => match self.frames[frame].names.get(&unit.name()) {
                        Some(Value::Object(fields)) => fields.clone(),
                        _ => IndexMap::new(),
                    },
                };
                self.blocks[block].parent = Parent::Main(unit);
                self.outputs = Some(outputs);
                Ok(Vec::new())
            }
            Parent::Scatter { gather, index } => {
                let names = self.gathers[gather].names.clone();
                let values = (names.iter())
                    .map(|name| {
                        (
                            name.clone(),
                            self.frames[frame]
                                .names
                                .get(name)
                                .cloned()
                                .unwrap_or(Value::None),
                        )
                    })
                    .collect();
                self.gathers[gather].results[index] = values;
                self.gathers[gather].remaining -= 1;
                if self.gathers[gather].remaining > 0 {
                    return Ok(Vec::new());
                }
                let owner = self.gathers[gather].block;
                let results = std::mem::take(&mut self.gathers[gather].results);
                let target = self.blocks[owner].frame;
                for name in names {
                    let items = results
                        .iter()
                        .map(|values| values.get(&name).cloned().unwrap_or(Value::None))
                        .collect();
                    self.frames[target].names.insert(name, Value::Array(items));
                }
                self.blocks[owner].active -= 1;
                self.advance(owner)
            }
            Parent::Branch {
                block: owner,
                names,
            } => {
                let target = self.blocks[owner].frame;
                for name in names {
                    let value = self.frames[frame]
                        .names
                        .get(&name)
                        .cloned()
                        .unwrap_or(Value::None);
                    self.frames[target].names.insert(name, value);
                }
                self.blocks[owner].active -= 1;
                self.advance(owner)
            }
            Parent::Workflow {
                block: owner,
                alias,
                unit,
            } => {
                let Callable::Workflow(workflow) = &unit.callable else {
                    return Ok(Vec::new());
                };
                let outputs = self.outputs_of(&unit.document, workflow.output(), frame)?;
                let target = self.blocks[owner].frame;
                self.frames[target]
                    .names
                    .insert(alias, Value::Object(outputs));
                self.blocks[owner].active -= 1;
                self.advance(owner)
            }
        }
    }

    /// The outputs of a workflow, evaluated in its frame; later outputs may use earlier ones.
    fn outputs_of(
        &mut self,
        document: &Document,
        section: Option<OutputSection>,
        frame: usize,
    ) -> Result<IndexMap<String, Value>, DriverError> {
        let mut outputs = IndexMap::new();
        let Some(section) = section else {
            return Ok(outputs);
        };
        let inner = self.frame(Some(frame));
        for decl in section.declarations() {
            let ty = declared(document, &decl.ty()).map_err(DriverError::new)?;
            let value = self.evaluate(inner, &decl.expr())?;
            let value = value
                .coerce(&ty)
                .map_err(|error| failed(document, &decl, error))?;
            let name = decl.name().text().to_owned();
            self.frames[inner].names.insert(name.clone(), value.clone());
            outputs.insert(name, value);
        }
        Ok(outputs)
    }

    fn evaluate(&mut self, frame: usize, expr: &wdl_ast::v1::Expr) -> Result<Value, DriverError> {
        let mut env = Scope { run: self, frame };
        expression::evaluate(expr, &mut env).map_err(|error| match error {
            Error::Unsupported(message) => DriverError::unsupported(message),
            error => DriverError::new(error.to_string()),
        })
    }
}

/// The names of a frame and its parents, for expressions of the workflow.
struct Scope<'a> {
    run: &'a mut Run,
    frame: usize,
}

impl Environment for Scope<'_> {
    fn lookup(&self, name: &str) -> Option<Value> {
        self.run.lookup(self.frame, name).cloned()
    }

    fn read(&mut self, file: &str) -> Result<String, Error> {
        task::read_location(&*self.run.settings.files, file)
    }

    fn write(&mut self, content: String) -> Result<Value, Error> {
        let folder = self.run.scratch.join("written");
        std::fs::create_dir_all(&folder).map_err(|error| Error::failed(error.to_string()))?;
        let path = folder.join(self.run.written.to_string());
        self.run.written += 1;
        std::fs::write(&path, content).map_err(|error| Error::failed(error.to_string()))?;
        Ok(Value::File(path.to_string_lossy().into_owned()))
    }

    fn size(&mut self, file: &str) -> Result<u64, Error> {
        task::size_location(&*self.run.settings.files, file)
    }
}

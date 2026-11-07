//! Starts the statements of a WDL workflow: declarations, calls, scatters and conditionals,
//! and finds the names each statement needs and defines.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use wdl_analysis::Document;
use wdl_ast::AstToken;
use wdl_ast::v1::{BoundDecl, CallStatement, WorkflowStatement};
use workflow_graph::{DriverError, Task, TaskId};

use super::{Call, Gather, Item, Parent, Run, Step};
use crate::document::{self, Callable, Unit, declared};
use crate::expression::{Error, names};
use crate::task;
use crate::value::Value;

impl Run {
    pub(super) fn run_step(&mut self, block: usize, step: Step) -> Result<Vec<Task>, DriverError> {
        let document = self.blocks[block].document.clone();
        let frame = self.blocks[block].frame;
        let statement = match step {
            Step::Decl(decl) => return self.declare(block, &decl).map(|_| Vec::new()),
            Step::Statement(statement) => statement,
        };
        match statement {
            WorkflowStatement::Declaration(decl) => self.declare(block, &decl).map(|_| Vec::new()),
            WorkflowStatement::Call(call) => self.call(block, &call),
            WorkflowStatement::Scatter(scatter) => {
                let items = match self.evaluate(frame, &scatter.expr())? {
                    Value::Array(items) => items,
                    other => {
                        return Err(failed(
                            &document,
                            &scatter,
                            format!("cannot scatter over a {}", other.kind()),
                        ));
                    }
                };
                let body: Vec<WorkflowStatement> = scatter.statements().collect();
                let names = defined(&body);
                if items.is_empty() {
                    for name in names {
                        self.frames[frame]
                            .names
                            .insert(name, Value::Array(Vec::new()));
                    }
                    return Ok(Vec::new());
                }
                let variable = scatter.variable().text().to_owned();
                self.gathers.push(Gather {
                    block,
                    names,
                    results: vec![BTreeMap::new(); items.len()],
                    remaining: items.len(),
                });
                let gather = self.gathers.len() - 1;
                self.blocks[block].active += 1;
                let mut tasks = Vec::new();
                for (index, value) in items.into_iter().enumerate() {
                    let inner = self.frame(Some(frame));
                    self.frames[inner].names.insert(variable.clone(), value);
                    let mut shard = self.blocks[block].shard.clone();
                    shard.push(index);
                    let path = self.blocks[block].path.clone();
                    let child = self.block(
                        inner,
                        &document,
                        Parent::Scatter { gather, index },
                        shard,
                        path,
                    );
                    self.blocks[child].steps = body
                        .iter()
                        .map(|statement| item(Step::Statement(statement.clone())))
                        .collect();
                    tasks.extend(self.advance(child)?);
                }
                Ok(tasks)
            }
            WorkflowStatement::Conditional(conditional) => {
                if conditional.clauses().count() > 1 {
                    return Err(DriverError::unsupported(format!(
                        "{}: `else` clauses are not supported",
                        document::place(&document, &conditional)
                    )));
                }
                let clause = conditional.if_clause();
                let body: Vec<WorkflowStatement> = clause.statements().collect();
                let names = defined(&body);
                let condition = match clause.expr() {
                    Some(expr) => self.evaluate(frame, &expr)?,
                    None => Value::None,
                };
                match condition {
                    Value::Boolean(true) => {
                        let inner = self.frame(Some(frame));
                        let shard = self.blocks[block].shard.clone();
                        let path = self.blocks[block].path.clone();
                        let child = self.block(
                            inner,
                            &document,
                            Parent::Branch { block, names },
                            shard,
                            path,
                        );
                        self.blocks[child].steps = body
                            .into_iter()
                            .map(|statement| item(Step::Statement(statement)))
                            .collect();
                        self.blocks[block].active += 1;
                        self.advance(child)
                    }
                    Value::Boolean(false) => {
                        for name in names {
                            self.frames[frame].names.insert(name, Value::None);
                        }
                        Ok(Vec::new())
                    }
                    other => Err(failed(
                        &document,
                        &conditional,
                        format!("a condition cannot be a {}", other.kind()),
                    )),
                }
            }
        }
    }

    fn declare(&mut self, block: usize, decl: &BoundDecl) -> Result<(), DriverError> {
        let document = self.blocks[block].document.clone();
        let frame = self.blocks[block].frame;
        let ty = declared(&document, &decl.ty()).map_err(DriverError::new)?;
        let value = self.evaluate(frame, &decl.expr())?;
        let value = value
            .coerce(&ty)
            .map_err(|error| failed(&document, decl, error))?;
        self.frames[frame]
            .names
            .insert(decl.name().text().to_owned(), value);
        Ok(())
    }

    fn call(&mut self, block: usize, call: &CallStatement) -> Result<Vec<Task>, DriverError> {
        let document = self.blocks[block].document.clone();
        let frame = self.blocks[block].frame;
        let unit = document::called(&document, &call.target())
            .map_err(|message| failed(&document, call, message))?;
        let alias = match call.alias() {
            Some(alias) => alias.name().text().to_owned(),
            None => unit.name(),
        };
        let mut given = BTreeMap::new();
        for input in call.inputs() {
            let name = input.name().text().to_owned();
            let value = match input.expr() {
                Some(expr) => self.evaluate(frame, &expr)?,
                None => self.lookup(frame, &name).cloned().unwrap_or(Value::None),
            };
            given.insert(name, value);
        }
        if self.in_main(block) {
            for (name, value) in self.nested.get(&alias).cloned().unwrap_or_default() {
                given.entry(name).or_insert(value);
            }
        }
        let path = self.blocks[block].path.clone();
        let mut name = alias.clone();
        let shard = &self.blocks[block].shard;
        if !shard.is_empty() {
            let positions: Vec<String> = shard.iter().map(usize::to_string).collect();
            name = format!("{alias} ({})", positions.join(", "));
        }
        match &unit.callable {
            Callable::Task(definition) => {
                let definition = definition.clone();
                let step = format!("{path}.{alias}");
                let names = (alias, name, step);
                Ok(vec![self.plan(block, &unit, &definition, given, names)?])
            }
            Callable::Workflow(_) => {
                let inner = self.frame(None);
                let shard = self.blocks[block].shard.clone();
                let parent = Parent::Workflow {
                    block,
                    alias: alias.clone(),
                    unit: unit.clone(),
                };
                let child = self.block(
                    inner,
                    &unit.document,
                    parent,
                    shard,
                    format!("{path}.{alias}"),
                );
                let steps = self.inputs(&unit, inner, given)?;
                self.blocks[child].steps = steps;
                self.blocks[block].active += 1;
                self.advance(child)
            }
        }
    }

    /// Whether a block runs in the main workflow, not in a workflow that it calls.
    fn in_main(&self, block: usize) -> bool {
        let mut current = block;
        loop {
            match &self.blocks[current].parent {
                Parent::Main(_) => return true,
                Parent::Workflow { .. } => return false,
                Parent::Scatter { gather, .. } => current = self.gathers[*gather].block,
                Parent::Branch { block, .. } => current = *block,
            }
        }
    }

    /// Plans a task of a call; the call's outputs go to `alias` in the block once it ran.
    pub(super) fn plan(
        &mut self,
        block: usize,
        unit: &Unit,
        definition: &wdl_ast::v1::TaskDefinition,
        given: BTreeMap<String, Value>,
        (alias, name, step): (String, String, String),
    ) -> Result<Task, DriverError> {
        let id = TaskId(self.next);
        self.next += 1;
        let planned = task::plan(
            &unit.document,
            definition,
            given,
            (id, name.clone(), step),
            &self.settings,
        )
        .map_err(|error| match error {
            Error::Unsupported(message) => DriverError::unsupported(format!("{name}: {message}")),
            error => DriverError::new(format!("{name}: {error}")),
        })?;
        self.calls.insert(
            id,
            Call {
                block,
                alias,
                name,
                state: planned.state,
            },
        );
        self.blocks[block].active += 1;
        Ok(planned.task)
    }
}

pub(super) fn item(step: Step) -> Item {
    let free = match &step {
        Step::Decl(decl) => names(&decl.expr()),
        Step::Statement(statement) => free(statement),
    };
    Item { step, free }
}

/// The names a statement needs from the scope around it.
fn free(statement: &WorkflowStatement) -> BTreeSet<String> {
    match statement {
        WorkflowStatement::Declaration(decl) => names(&decl.expr()),
        WorkflowStatement::Call(call) => {
            let mut found = BTreeSet::new();
            for input in call.inputs() {
                match input.expr() {
                    Some(expr) => found.extend(names(&expr)),
                    None => {
                        found.insert(input.name().text().to_owned());
                    }
                }
            }
            for after in call.after() {
                found.insert(after.name().text().to_owned());
            }
            found
        }
        WorkflowStatement::Scatter(scatter) => {
            let body: Vec<WorkflowStatement> = scatter.statements().collect();
            let mut found = inner(&body);
            found.remove(scatter.variable().text());
            found.extend(names(&scatter.expr()));
            found
        }
        WorkflowStatement::Conditional(conditional) => {
            let mut found = BTreeSet::new();
            for clause in conditional.clauses() {
                let body: Vec<WorkflowStatement> = clause.statements().collect();
                found.extend(inner(&body));
                if let Some(expr) = clause.expr() {
                    found.extend(names(&expr));
                }
            }
            found
        }
    }
}

/// The names that statements of a body need from outside it.
fn inner(body: &[WorkflowStatement]) -> BTreeSet<String> {
    let defined: BTreeSet<String> = defined(body).into_iter().collect();
    (body.iter().flat_map(free))
        .filter(|name| !defined.contains(name))
        .collect()
}

/// The names that statements define for the scope around them.
fn defined(body: &[WorkflowStatement]) -> Vec<String> {
    let mut names = Vec::new();
    for statement in body {
        match statement {
            WorkflowStatement::Declaration(decl) => names.push(decl.name().text().to_owned()),
            WorkflowStatement::Call(call) => names.push(match call.alias() {
                Some(alias) => alias.name().text().to_owned(),
                None => call
                    .target()
                    .names()
                    .last()
                    .map(|name| name.text().to_owned())
                    .unwrap_or_default(),
            }),
            WorkflowStatement::Scatter(scatter) => {
                names.extend(defined(&scatter.statements().collect::<Vec<_>>()))
            }
            WorkflowStatement::Conditional(conditional) => {
                for clause in conditional.clauses() {
                    names.extend(defined(&clause.statements().collect::<Vec<_>>()));
                }
            }
        }
    }
    names
}

/// A failure of a statement, with where it is.
pub(super) fn failed(
    document: &Document,
    node: &impl wdl_ast::AstNode<wdl_ast::SyntaxNode>,
    message: String,
) -> DriverError {
    DriverError::new(format!("{}: {message}", document::place(document, node)))
}

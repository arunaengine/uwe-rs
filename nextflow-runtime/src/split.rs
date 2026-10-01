//! Emits CSV, text, FASTA, FASTQ, and JSON splitter records, singly or in by groups.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::split::{Splitter, decompressed};
use nextflow_evaluator::value::{ChannelId, Closure, PathValue, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};

pub(crate) struct Split {
    pub output: ChannelId,
    pub splitter: Splitter,
    /// The tuple element to split; by default the first path of a tuple, else its first
    /// element.
    pub elem: Option<usize>,
    /// A closure applied to each emitted item.
    pub each: Option<Rc<Closure>>,
    /// Where `file: true` writes the chunks, which are then emitted as files.
    pub files: Option<SplitFiles>,
    /// Whether `.gz` files are decompressed first, which `decompress: false` turns off.
    pub decompress: bool,
    pub count: usize,
    /// For the count operators: the number of records so far, which is emitted at the end.
    pub total: Option<i64>,
    /// The CSV records of a file that are not emitted yet, with the tuple they go into and
    /// their position in it.
    pub pending: Option<(nextflow_evaluator::csv::Records, Option<Vec<Value>>, usize)>,
}

pub(crate) enum SplitFiles {
    Implicit(PathValue),
    Named(PathValue, Option<String>),
}

/// The most records that a split emits before the nodes that read them run.
const BATCH: usize = 10_000;

impl Behaviour for Split {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        loop {
            // The records of a large CSV file go out a batch at a time.
            if let Some((records, tuple, position)) = &mut self.pending {
                for record in records.by_ref().take(BATCH) {
                    let item = match tuple {
                        Some(items) => {
                            let mut items = items.clone();
                            if *position < items.len() {
                                items[*position] = record;
                            }
                            Value::list(items)
                        }
                        None => record,
                    };
                    let item = match &self.each {
                        Some(closure) => context.interpreter.call_closure(closure, vec![item])?,
                        None => item,
                    };
                    context.emit(self.output, item);
                }
                if !records.is_done() {
                    context.again();
                    return Ok(State::Running);
                }
                self.pending = None;
            }
            let Some(message) = context.read(0) else {
                return Ok(State::Running);
            };
            let Message::Value(value) = message else {
                if let Some(total) = self.total {
                    context.emit(self.output, Value::Number(Number::Long(total)));
                }
                context.close(self.output);
                return Ok(State::Stopped);
            };
            let (tuple, position) = match &value {
                Value::List(list) => {
                    let items = list.borrow().items.clone();
                    let position = self
                        .elem
                        .or_else(|| items.iter().position(|item| matches!(item, Value::Path(_))))
                        .unwrap_or(0);
                    (Some(items), position)
                }
                _ => (None, 0),
            };
            let target = match &tuple {
                Some(items) => items.get(position).cloned().unwrap_or(Value::Null),
                None => value.clone(),
            };
            let (text, name) = match &target {
                Value::Path(path) => {
                    let mut bytes = context.interpreter.host().read(path)?;
                    if path.name().ends_with(".gz") && self.decompress {
                        bytes = decompressed(&bytes, path).map_err(thrown)?;
                    }
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    (text, path.base_name().to_owned())
                }
                other => (other.to_string(), "chunk".to_owned()),
            };
            if self.total.is_none() && self.files.is_none() && self.splitter.streams() {
                if let Some(records) = self.splitter.csv_records(text) {
                    if records.missing_header() && !self.splitter.simulated {
                        let exception = nextflow_evaluator::csv::missing_header();
                        return Err(Error::Thrown {
                            exception: Rc::new(exception),
                            span: Default::default(),
                        });
                    }
                    self.pending = Some((records, tuple, position));
                }
                continue;
            }
            let chunks = self.splitter.chunks(&text).map_err(thrown)?;
            if let Some(total) = &mut self.total {
                *total += chunks.len() as i64;
                continue;
            }
            for (index, chunk) in chunks.into_iter().enumerate() {
                let chunk = match (&self.files, &chunk) {
                    (Some(files), Value::String(text)) => {
                        self.count += 1;
                        let path = match files {
                            SplitFiles::Implicit(directory) => {
                                directory.resolve(&format!("{name}.{}.{}", self.count, index + 1))
                            }
                            SplitFiles::Named(directory, named) => {
                                let source = match &target {
                                    Value::Path(path) => {
                                        path.name().strip_suffix(".gz").unwrap_or(path.name())
                                    }
                                    _ => "chunk",
                                };
                                let named = named.as_deref().unwrap_or(source);
                                let name = match named.rsplit_once('.') {
                                    Some((base, extension)) if !base.is_empty() => {
                                        format!("{base}.{}.{extension}", self.count)
                                    }
                                    _ => format!("{named}.{}", self.count),
                                };
                                directory.resolve(&name)
                            }
                        };
                        context.interpreter.host().write(&path, text.as_bytes())?;
                        Value::Path(Rc::new(path))
                    }
                    _ => chunk,
                };
                let item = match &tuple {
                    Some(items) => {
                        let mut items = items.clone();
                        if position < items.len() {
                            items[position] = chunk;
                        }
                        Value::list(items)
                    }
                    None => chunk,
                };
                let item = match &self.each {
                    Some(closure) => context.interpreter.call_closure(closure, vec![item])?,
                    None => item,
                };
                context.emit(self.output, item);
            }
        }
    }
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}

/// `subscribe`: calls a closure for each value, and another at the end.
pub(crate) struct Subscribe {
    pub on_next: Option<Rc<Closure>>,
    pub on_complete: Option<Rc<Closure>>,
    pub on_error: Option<Rc<Closure>>,
}

impl Behaviour for Subscribe {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            match message {
                Message::Value(value) => {
                    if let Some(closure) = &self.on_next
                        && let Err(error) = context.interpreter.call_closure(closure, vec![value])
                    {
                        if let (Some(handler), Some(exception)) =
                            (&self.on_error, error.exception())
                        {
                            context
                                .interpreter
                                .call_closure(handler, vec![Value::Exception(exception.clone())])?;
                            return Ok(State::Stopped);
                        }
                        return Err(error);
                    }
                }
                Message::Stop => {
                    if let Some(closure) = &self.on_complete {
                        context.interpreter.call_closure(closure, Vec::new())?;
                    }
                    return Ok(State::Stopped);
                }
            }
        }
        Ok(State::Running)
    }
}

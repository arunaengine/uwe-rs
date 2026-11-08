//! Emits CSV, text, FASTA, FASTQ, and JSON splitter records, singly or in by groups.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::split::{Chunks, Splitter};
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
    /// The chunks of a value that are not emitted yet.
    pub pending: Option<Pending>,
}

/// The chunks of one value, which are read as they are emitted.
pub(crate) struct Pending {
    chunks: Chunks,
    /// The tuple that the chunks go into, at `position`.
    tuple: Option<Vec<Value>>,
    position: usize,
    /// What chunk files are named after: the base name of the file, and its name without
    /// `.gz`.
    base: String,
    source: String,
    /// The chunks of this value so far.
    index: usize,
}

pub(crate) enum SplitFiles {
    Implicit(PathValue),
    Named(PathValue, Option<String>),
}

/// The most records that a split reads into chunks before the nodes that read them run.
const BATCH: usize = 10_000;

impl Behaviour for Split {
    fn export(&self) -> Result<crate::export::Kind, String> {
        crate::describe::split(self)
    }

    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        loop {
            // The chunks of a large file go out a batch at a time.
            if !self.emit(context)? {
                context.again();
                return Ok(State::Running);
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
            let (chunks, base, source) = match &target {
                Value::Path(path) => {
                    let reader = context.interpreter.host().open(path)?;
                    let chunks = self.splitter.file(reader, path, self.decompress);
                    let source = path.name().strip_suffix(".gz").unwrap_or(path.name());
                    (chunks, path.base_name().to_owned(), source.to_owned())
                }
                other => (
                    self.splitter.text(other.to_string()),
                    "chunk".to_owned(),
                    "chunk".to_owned(),
                ),
            };
            self.pending = Some(Pending {
                chunks: chunks.map_err(thrown)?,
                tuple,
                position,
                base,
                source,
                index: 0,
            });
        }
    }
}

impl Split {
    /// Emits the pending chunks of up to a batch of records, or counts them; whether all
    /// are out.
    fn emit(&mut self, context: &mut Context) -> Result<bool, Error> {
        let Some(mut pending) = self.pending.take() else {
            return Ok(true);
        };
        let start = pending.chunks.records();
        while pending.chunks.records() - start < BATCH {
            let Some(chunk) = pending.chunks.next() else {
                return Ok(true);
            };
            let chunk = chunk.map_err(thrown)?;
            pending.index += 1;
            if let Some(total) = &mut self.total {
                *total += 1;
                continue;
            }
            let chunk = match (&self.files, &chunk) {
                (Some(files), Value::String(text)) => {
                    self.count += 1;
                    let path = match files {
                        SplitFiles::Implicit(directory) => directory.resolve(&format!(
                            "{}.{}.{}",
                            pending.base, self.count, pending.index
                        )),
                        SplitFiles::Named(directory, named) => {
                            let named = named.as_deref().unwrap_or(&pending.source);
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
            let item = match &pending.tuple {
                Some(items) => {
                    let mut items = items.clone();
                    if pending.position < items.len() {
                        items[pending.position] = chunk;
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
        self.pending = Some(pending);
        Ok(false)
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
    fn export(&self) -> Result<crate::export::Kind, String> {
        crate::describe::subscribe(self)
    }

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

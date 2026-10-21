//! `collectFile`: values become the content of files, which are written when the source ends
//! (channels spec, section 5.6).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;
use std::rc::Rc;

use indexmap::IndexMap;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{ChannelId, Closure, Key, PathValue, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};
use crate::operator::Sort;

pub(crate) struct CollectFile {
    pub output: ChannelId,
    pub closure: Option<Rc<Closure>>,
    /// The file name for values that do not name their file, from the `name` option.
    pub name: Option<String>,
    /// Where the files go: `storeDir`, else a directory of this operator below `workDir`.
    pub directory: PathValue,
    pub new_line: bool,
    pub sort: Sort,
    /// Lines to leave out at the start of each entry, and whether the first entry keeps them.
    pub skip: usize,
    pub keep_header: bool,
    /// The initial content of every file.
    pub seed: Option<String>,
    /// The entries of each file, by file name in first-seen order.
    pub files: IndexMap<String, Vec<(Value, String)>>,
}

impl Behaviour for CollectFile {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                return self.finish(context);
            };
            let value = match &self.closure {
                Some(closure) => context.interpreter.call_closure(closure, vec![value])?,
                None => value,
            };
            let (name, items) = match &value {
                Value::Null => continue,
                Value::List(list) if list.borrow().items.len() >= 2 => {
                    let items = list.borrow().items.clone();
                    (items[0].to_string(), items[1..].to_vec())
                }
                Value::Path(path) => {
                    let name = self.name.clone().unwrap_or_else(|| path.name().to_owned());
                    (name, vec![value.clone()])
                }
                other => {
                    let name = self
                        .name
                        .clone()
                        .unwrap_or_else(|| "collect-file.data".into());
                    (name, vec![other.clone()])
                }
            };
            for item in items {
                let text = match &item {
                    Value::Path(path) => {
                        let bytes = context.interpreter.host().read(path)?;
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    Value::String(text) | Value::GString(text) => text.to_string(),
                    other => {
                        let message = format!(
                            "Not a valid file collector argument [{}]: {other}",
                            other.java_class_name()
                        );
                        return Err(Error::Thrown {
                            exception: Rc::new(Exception::illegal_argument(message)),
                            span: Default::default(),
                        });
                    }
                };
                self.files
                    .entry(name.clone())
                    .or_default()
                    .push((item, text));
            }
        }
        Ok(State::Running)
    }
}

impl CollectFile {
    /// Writes the files, then emits them in file name order.
    fn finish(&mut self, context: &mut Context) -> Result<State, Error> {
        let mut files: Vec<_> = std::mem::take(&mut self.files).into_iter().collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entries) in files {
            let mut texts: IndexMap<Key, VecDeque<String>> = IndexMap::new();
            let values = entries
                .into_iter()
                .map(|(value, text)| {
                    texts.entry(Key(value.clone())).or_default().push_back(text);
                    value
                })
                .collect();
            let entries: Vec<_> = self
                .sort
                .apply(context, values)?
                .into_iter()
                .map(|value| texts.get_mut(&Key(value)).unwrap().pop_front().unwrap())
                .collect();
            // Like Nextflow, `newLine` ends the seed and every entry with a new line, even
            // when they end with one already.
            let mut content = self.seed.clone().unwrap_or_default();
            if self.new_line && !content.is_empty() {
                content.push('\n');
            }
            for (index, entry) in entries.iter().enumerate() {
                let skip = if self.keep_header && index == 0 {
                    0
                } else {
                    self.skip
                };
                let mut lines = entry.split_inclusive('\n').skip(skip).collect::<String>();
                if self.new_line {
                    lines.push('\n');
                }
                content.push_str(&lines);
            }
            let path = self.directory.resolve(&name);
            context
                .interpreter
                .host()
                .write(&path, content.as_bytes())?;
            context.emit(self.output, Value::Path(Rc::new(path)));
        }
        context.close(self.output);
        Ok(State::Stopped)
    }
}

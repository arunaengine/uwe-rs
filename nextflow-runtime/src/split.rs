//! Splitters: `splitCsv`, `splitText`, `splitFasta` and `splitFastq` emit the records of a
//! file or text, alone or `by` several (channels spec, section 5.33).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{ChannelId, Closure, PathValue, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};

pub(crate) enum Format {
    Csv {
        separator: char,
        quote: char,
        /// Column names from the first line (`header: true`) or given as a list.
        header: Option<Option<Vec<Value>>>,
        skip: usize,
        strip: bool,
    },
    Text {
        keep_header: bool,
    },
    /// FASTA and FASTQ records as text, or as maps of the named fields.
    Fasta(Option<Vec<String>>),
    Fastq(Option<Vec<String>>),
}

/// The fields of FASTA and FASTQ records, in the order Nextflow puts them in a record map.
const FASTA_FIELDS: &[&str] = &["id", "desc", "header", "text", "seqString", "sequence"];
const FASTQ_FIELDS: &[&str] = &["readHeader", "readString", "qualityHeader", "qualityString"];

/// A FASTA or FASTQ record as a map of the requested fields.
fn record_map(text: &str, fields: &[String], fastq: bool) -> Value {
    let lines: Vec<&str> = text.lines().collect();
    let all = if fastq { FASTQ_FIELDS } else { FASTA_FIELDS };
    let mut entries = Vec::new();
    for field in all
        .iter()
        .filter(|field| fields.iter().any(|wanted| wanted == *field))
    {
        let header = lines.first().map_or("", |line| &line[1.min(line.len())..]);
        let value = match (*field, fastq) {
            ("id", _) => Value::string(header.split_whitespace().next().unwrap_or("")),
            ("desc", _) => header
                .split_once(char::is_whitespace)
                .map_or(Value::Null, |(_, desc)| Value::string(desc.trim())),
            ("header", _) => Value::string(header),
            ("text", _) => Value::string(text),
            ("seqString", _) => Value::string(lines.iter().skip(1).copied().collect::<String>()),
            ("sequence", _) => Value::string(
                lines
                    .iter()
                    .skip(1)
                    .map(|line| format!("{line}\n"))
                    .collect::<String>(),
            ),
            ("readHeader", _) => Value::string(header),
            ("readString", _) => Value::string(lines.get(1).copied().unwrap_or("")),
            ("qualityHeader", _) => {
                Value::string(lines.get(2).map_or("", |line| &line[1.min(line.len())..]))
            }
            _ => Value::string(lines.get(3).copied().unwrap_or("")),
        };
        entries.push((Value::string(*field), value));
    }
    Value::map(entries)
}

pub(crate) struct Split {
    pub output: ChannelId,
    pub format: Format,
    /// Records per emitted chunk.
    pub by: Option<usize>,
    /// The most records to read of each value.
    pub limit: Option<usize>,
    /// The tuple element to split; by default the first path of a tuple, else its first
    /// element.
    pub elem: Option<usize>,
    /// A closure applied to each emitted item.
    pub each: Option<Rc<Closure>>,
    /// Where `file: true` writes the chunks, which are then emitted as files.
    pub files: Option<PathValue>,
    /// Whether `.gz` files are decompressed first, which `decompress: false` turns off.
    pub decompress: bool,
    pub count: usize,
}

impl Behaviour for Split {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
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
                        bytes = decompressed(&bytes, path)?;
                    }
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    (text, path.base_name().to_owned())
                }
                other => (other.to_string(), "chunk".to_owned()),
            };
            for (index, chunk) in self.chunks(&text).into_iter().enumerate() {
                let chunk = match (&self.files, &chunk) {
                    (Some(directory), Value::String(text)) => {
                        self.count += 1;
                        let path =
                            directory.resolve(&format!("{name}.{}.{}", self.count, index + 1));
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
        Ok(State::Running)
    }
}

impl Split {
    /// The chunks of one text: single records, or lists of `by` records for CSV, or the
    /// joined text of `by` records for the other formats.
    fn chunks(&self, text: &str) -> Vec<Value> {
        let mut records = match &self.format {
            Format::Csv { .. } => self.csv(text),
            Format::Text { .. } => text.split_inclusive('\n').map(Value::string).collect(),
            Format::Fasta(_) => records(text, |line, _| line.starts_with('>')),
            Format::Fastq(_) => records(text, |_, index| index % 4 == 0),
        };
        if let Format::Fasta(Some(fields)) | Format::Fastq(Some(fields)) = &self.format {
            let fastq = matches!(self.format, Format::Fastq(_));
            records = records
                .iter()
                .map(|record| record_map(&record.to_string(), fields, fastq))
                .collect();
        }
        if let Some(limit) = self.limit {
            records.truncate(limit);
        }
        let header = match &self.format {
            Format::Text { keep_header: true } => {
                text.split_inclusive('\n').next().map(str::to_owned)
            }
            _ => None,
        };
        let Some(by) = self.by.filter(|by| *by > 0) else {
            return records;
        };
        records
            .chunks(by)
            .map(|chunk| match self.format {
                Format::Csv { .. } | Format::Fasta(Some(_)) | Format::Fastq(Some(_)) => {
                    Value::list(chunk.to_vec())
                }
                _ => {
                    let mut text = header.clone().unwrap_or_default();
                    text.extend(chunk.iter().map(Value::to_string));
                    Value::string(text)
                }
            })
            .collect()
    }

    fn csv(&self, text: &str) -> Vec<Value> {
        let Format::Csv {
            separator,
            quote,
            header,
            skip,
            strip,
        } = &self.format
        else {
            return Vec::new();
        };
        let mut rows = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .skip(*skip)
            .map(|line| fields(line, *separator, *quote, *strip));
        let names: Option<Vec<Value>> = match header {
            Some(None) => rows
                .next()
                .map(|names| names.into_iter().map(Value::string).collect()),
            Some(Some(names)) => Some(names.clone()),
            None => None,
        };
        rows.map(|row| match &names {
            Some(names) => Value::map(names.iter().enumerate().map(|(index, name)| {
                let field = row
                    .get(index)
                    .map_or(Value::Null, |f| Value::string(f.as_str()));
                (name.clone(), field)
            })),
            None => Value::list(row.into_iter().map(Value::string).collect()),
        })
        .collect()
    }
}

/// The bytes of a gzip file, decompressed.
fn decompressed(bytes: &[u8], path: &PathValue) -> Result<Vec<u8>, Error> {
    let mut decoded = Vec::new();
    let mut decoder = flate2::read::MultiGzDecoder::new(bytes);
    std::io::Read::read_to_end(&mut decoder, &mut decoded).map_err(|error| {
        let message = format!("Cannot decompress {}: {error}", path.uri());
        Error::Thrown {
            exception: Rc::new(nextflow_evaluator::exception::Exception::new(
                "ZipException",
                message,
            )),
            span: Default::default(),
        }
    })?;
    Ok(decoded)
}

/// The records of a text: a record starts at each line that `starts` accepts.
fn records(text: &str, starts: impl Fn(&str, usize) -> bool) -> Vec<Value> {
    let mut records: Vec<String> = Vec::new();
    for (index, line) in text.split_inclusive('\n').enumerate() {
        match records.last_mut() {
            Some(record) if !starts(line, index) => record.push_str(line),
            _ => records.push(line.to_owned()),
        }
    }
    records.into_iter().map(Value::string).collect()
}

/// The fields of a CSV line; quotes group text with separators and `""` is a quote.
fn fields(line: &str, separator: char, quote: char, strip: bool) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            c if c == quote && quoted && characters.peek() == Some(&quote) => {
                characters.next();
                fields.last_mut().expect("a field").push(quote);
            }
            c if c == quote => quoted = !quoted,
            c if c == separator && !quoted => fields.push(String::new()),
            other => fields.last_mut().expect("a field").push(other),
        }
    }
    if strip {
        fields
            .iter_mut()
            .for_each(|field| *field = field.trim().to_owned());
    }
    fields
}

/// `subscribe`: calls a closure for each value, and another at the end.
pub(crate) struct Subscribe {
    pub on_next: Option<Rc<Closure>>,
    pub on_complete: Option<Rc<Closure>>,
}

impl Behaviour for Subscribe {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            match message {
                Message::Value(value) => {
                    if let Some(closure) = &self.on_next {
                        context.interpreter.call_closure(closure, vec![value])?;
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

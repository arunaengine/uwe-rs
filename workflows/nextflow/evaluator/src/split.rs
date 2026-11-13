//! Splits CSV, text, FASTA, FASTQ, and JSON into records shared by methods and operators.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read};

use crate::csv::{self, CsvOptions};
use crate::exception::Exception;
use crate::lines::Lines;
use crate::value::{Key, PathValue, Value};

#[derive(Clone, Debug)]
pub enum Format {
    Csv {
        separator: char,
        quote: Option<char>,
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
    /// The elements of the array, or the entries of the object, at a path like `a.b[0].c`;
    /// the empty path is the whole document.
    Json {
        path: String,
    },
}

/// The fields of FASTA and FASTQ records, in the order Nextflow puts them in a record map.
const FASTA_FIELDS: &[&str] = &["id", "desc", "header", "text", "seqString", "sequence"];
const FASTQ_FIELDS: &[&str] = &["readHeader", "readString", "qualityHeader", "qualityString"];

/// How to split a text: its format, the records of each chunk and the most records to read.
#[derive(Clone, Debug)]
pub struct Splitter {
    pub format: Format,
    /// Records per chunk.
    pub by: Option<usize>,
    /// The most records to read of each text.
    pub limit: Option<usize>,
    /// Whether the run is simulated, so that a CSV text may lack its header line.
    pub simulated: bool,
}

impl Splitter {
    /// The splitter of an operator or method, like `splitFasta` or `countLines`, with its
    /// options, like `[by: 10, record: [id: true]]`.
    pub fn new(name: &str, options: Option<&Value>) -> Splitter {
        let format = match name {
            "splitCsv" => Format::Csv {
                separator: option(options, "sep")
                    .and_then(|sep| sep.to_string().chars().next())
                    .unwrap_or(','),
                quote: option(options, "quote").and_then(|quote| quote.to_string().chars().next()),
                header: match option(options, "header") {
                    Some(Value::List(names)) => Some(Some(names.borrow().items.clone())),
                    Some(value) if value.truth() => Some(None),
                    _ => None,
                },
                skip: positive(options, "skip").unwrap_or(0),
                strip: option(options, "strip").is_some_and(|value| value.truth()),
            },
            // There is no `countJson`: Nextflow 26.04.6 fails on it with "Unknown argument
            // 'each' for operator 'JsonSplitter'".
            "splitJson" => Format::Json {
                path: option(options, "path")
                    .map(|path| path.to_string())
                    .unwrap_or_default(),
            },
            "splitText" | "countLines" => Format::Text {
                keep_header: option(options, "keepHeader").is_some_and(|value| value.truth()),
            },
            _ => {
                let fasta = matches!(name, "splitFasta" | "countFasta");
                let all = if fasta { FASTA_FIELDS } else { FASTQ_FIELDS };
                let fields = match option(options, "record") {
                    Some(Value::Map(map)) => Some(
                        map.borrow()
                            .entries
                            .iter()
                            .filter(|(_, wanted)| wanted.truth())
                            .map(|(field, _)| field.0.to_string())
                            .collect(),
                    ),
                    Some(value) if value.truth() => {
                        Some(all.iter().map(|field| field.to_string()).collect())
                    }
                    _ => None,
                };
                if fasta {
                    Format::Fasta(fields)
                } else {
                    Format::Fastq(fields)
                }
            }
        };
        Splitter {
            format,
            by: positive(options, "by"),
            limit: positive(options, "limit"),
            simulated: false,
        }
    }

    /// The chunks of a file, read as they are needed. A `.gz` file is decompressed on the
    /// way when `decompress` is set.
    pub fn file(
        &self,
        reader: Box<dyn Read>,
        path: &PathValue,
        decompress: bool,
    ) -> Result<Chunks, Exception> {
        let compressed = decompress && path.name().ends_with(".gz");
        let reader: Box<dyn BufRead> = match compressed {
            true => Box::new(BufReader::new(flate2::read::MultiGzDecoder::new(reader))),
            false => Box::new(BufReader::new(reader)),
        };
        let origin = Origin {
            uri: path.uri(),
            compressed,
        };
        self.read(Lines::new(reader), Some(origin))
    }

    /// The chunks of a text in memory.
    pub fn text(&self, text: String) -> Result<Chunks, Exception> {
        self.read(Lines::text(text), None)
    }

    fn read(&self, mut lines: Lines, origin: Option<Origin>) -> Result<Chunks, Exception> {
        let failed = |error: std::io::Error| read_error(origin.as_ref(), &error);
        let source = match &self.format {
            Format::Csv {
                separator,
                quote,
                header,
                skip,
                strip,
            } => {
                let options = CsvOptions {
                    separator: *separator,
                    quote: *quote,
                    header: header.clone(),
                    skip: *skip,
                    strip: *strip,
                };
                let mut records = csv::Records::read(lines, &options);
                if let Some(error) = records.error() {
                    return Err(failed(error));
                }
                if records.missing_header() && !self.simulated {
                    return Err(csv::missing_header());
                }
                Source::Csv(records)
            }
            // A JSON document is parsed as a whole.
            Format::Json { path } => {
                let mut text = String::new();
                while let Some(line) = lines.next_line().map_err(failed)? {
                    text.push_str(&line);
                }
                Source::Json(json_records(&text, path)?.into_iter())
            }
            _ => Source::Lines(lines),
        };
        Ok(Chunks {
            splitter: self.clone(),
            source,
            header: None,
            ahead: None,
            line: 0,
            taken: 0,
            origin,
            ended: false,
        })
    }
}

/// The chunks of a text, read a record at a time: single records, or lists of `by` records
/// for CSV and record maps, or the joined text of `by` records for the other formats.
pub struct Chunks {
    splitter: Splitter,
    source: Source,
    /// The first line of the text, which `keepHeader` puts before each chunk.
    header: Option<String>,
    /// The line that starts the next FASTA or FASTQ record, read ahead of that record.
    ahead: Option<String>,
    /// The lines read so far; a FASTQ record starts at every fourth line.
    line: usize,
    /// The records given so far, which `limit` bounds.
    taken: usize,
    origin: Option<Origin>,
    /// Whether the chunks ended, at the end of the text or at an error.
    ended: bool,
}

enum Source {
    Lines(Lines),
    Csv(csv::Records),
    Json(std::vec::IntoIter<Value>),
}

/// The file that a text comes from, which names it in read errors.
struct Origin {
    uri: String,
    compressed: bool,
}

impl Iterator for Chunks {
    type Item = Result<Value, Exception>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let chunk = self.chunk();
        self.ended = !matches!(chunk, Ok(Some(_)));
        chunk.transpose()
    }
}

impl Chunks {
    /// The records read so far.
    pub fn records(&self) -> usize {
        self.taken
    }

    fn chunk(&mut self) -> Result<Option<Value>, Exception> {
        let Some(by) = self.splitter.by.filter(|by| *by > 0) else {
            return self.record();
        };
        let mut records = Vec::new();
        while records.len() < by {
            match self.record()? {
                Some(record) => records.push(record),
                None => break,
            }
        }
        if records.is_empty() {
            return Ok(None);
        }
        Ok(Some(match self.splitter.format {
            Format::Csv { .. } | Format::Fasta(Some(_)) | Format::Fastq(Some(_)) => {
                Value::list(records)
            }
            _ => {
                let mut text = self.header.clone().unwrap_or_default();
                text.extend(records.iter().map(Value::to_string));
                Value::string(text)
            }
        }))
    }

    /// The next record, up to `limit`.
    fn record(&mut self) -> Result<Option<Value>, Exception> {
        if self.splitter.limit.is_some_and(|limit| self.taken >= limit) {
            return Ok(None);
        }
        let record = match &mut self.source {
            Source::Csv(records) => match records.next() {
                Some(record) => Some(record),
                None => match records.error() {
                    Some(error) => return Err(read_error(self.origin.as_ref(), &error)),
                    None => None,
                },
            },
            Source::Json(items) => items.next(),
            Source::Lines(_) => self.text_record()?.map(|text| match &self.splitter.format {
                Format::Fasta(Some(fields)) => record_map(&text, fields, false),
                Format::Fastq(Some(fields)) => record_map(&text, fields, true),
                _ => Value::string(text),
            }),
        };
        self.taken += usize::from(record.is_some());
        Ok(record)
    }

    /// The text of the next record: a line, or the lines of a FASTA or FASTQ record.
    fn text_record(&mut self) -> Result<Option<String>, Exception> {
        let mut record = match self.ahead.take() {
            Some(line) => line,
            None => match self.next_line()? {
                Some((_, line)) => line,
                None => return Ok(None),
            },
        };
        let fasta = match self.splitter.format {
            Format::Fasta(_) => true,
            Format::Fastq(_) => false,
            _ => return Ok(Some(record)),
        };
        while let Some((index, line)) = self.next_line()? {
            if (fasta && line.starts_with('>')) || (!fasta && index % 4 == 0) {
                self.ahead = Some(line);
                break;
            }
            record.push_str(&line);
        }
        Ok(Some(record))
    }

    /// The next line with its index, keeping the first line for `keepHeader`.
    fn next_line(&mut self) -> Result<Option<(usize, String)>, Exception> {
        let Source::Lines(lines) = &mut self.source else {
            return Ok(None);
        };
        let line = (lines.next_line()).map_err(|error| read_error(self.origin.as_ref(), &error))?;
        let index = self.line;
        if let Some(text) = &line {
            self.line += 1;
            if index == 0 && matches!(self.splitter.format, Format::Text { keep_header: true }) {
                self.header = Some(text.clone());
            }
        }
        Ok(line.map(|text| (index, text)))
    }
}

/// The exception of a failed read: Nextflow's for a gzip file it cannot decompress, else
/// that of the file.
fn read_error(origin: Option<&Origin>, error: &std::io::Error) -> Exception {
    match origin {
        Some(origin) if origin.compressed => Exception::new(
            "ZipException",
            format!("Cannot decompress {}: {error}", origin.uri),
        ),
        Some(origin) if error.kind() == std::io::ErrorKind::NotFound => {
            Exception::new("NoSuchFileException", origin.uri.clone())
        }
        Some(origin) => Exception::new("IOException", origin.uri.clone()),
        None => Exception::new("IOException", error.to_string()),
    }
}

/// An option of a map of options.
pub fn option(options: Option<&Value>, name: &str) -> Option<Value> {
    match options {
        Some(Value::Map(map)) => map.borrow().entries.get(&Key(Value::string(name))).cloned(),
        _ => None,
    }
}

/// A number option that is greater than zero, like `by`.
pub fn positive(options: Option<&Value>, name: &str) -> Option<usize> {
    match option(options, name) {
        Some(Value::Number(number)) if number.to_i64() > 0 => Some(number.to_i64() as usize),
        _ => None,
    }
}

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

/// The records of a JSON document for `splitJson`: the elements of the array at `path`, or the
/// entries of the object there as maps `[key: name, value: value]`. Nothing is at a path that
/// the document lacks.
fn json_records(text: &str, path: &str) -> Result<Vec<Value>, Exception> {
    let failed = |cause: String| {
        Exception::new(
            "IllegalStateException",
            format!("Error while parsing JSON content - cause: {cause}"),
        )
    };
    let document = crate::interpreter::parse_json_for_split(text)
        .map_err(|exception| failed(exception.to_string()))?;
    let Some(found) = json_node(&document, "", path) else {
        return Ok(Vec::new());
    };
    match found {
        Value::List(list) => Ok(list.borrow().items.clone()),
        Value::Map(map) => Ok(map
            .borrow()
            .entries
            .iter()
            .map(|(key, value)| {
                Value::map([
                    (Value::string("key"), key.0.clone()),
                    (Value::string("value"), value.clone()),
                ])
            })
            .collect()),
        other => {
            let token = match other {
                Value::String(_) => "STRING",
                Value::Number(_) => "NUMBER",
                Value::Boolean(_) => "BOOLEAN",
                _ => "NULL",
            };
            Err(failed(format!(
                "Expected JSON to be an object or an array but got {token} for json-path \"{path}\"."
            )))
        }
    }
}

/// The first node, in document order, whose path is `wanted`: `key` below the root, `a.key`
/// below an object and `a[0]` below an array.
fn json_node(node: &Value, current: &str, wanted: &str) -> Option<Value> {
    if current == wanted {
        return Some(node.clone());
    }
    match node {
        Value::List(list) => list
            .borrow()
            .items
            .iter()
            .enumerate()
            .find_map(|(index, item)| json_node(item, &format!("{current}[{index}]"), wanted)),
        Value::Map(map) => map.borrow().entries.iter().find_map(|(key, value)| {
            let separator = if current.is_empty() || current.ends_with('.') {
                ""
            } else {
                "."
            };
            json_node(value, &format!("{current}{separator}{}", key.0), wanted)
        }),
        _ => None,
    }
}

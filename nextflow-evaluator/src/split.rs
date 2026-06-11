//! Splitting texts into records as Nextflow's splitters do: CSV rows, lines, FASTA and FASTQ
//! records and JSON elements, alone or `by` several (channels spec, section 5.33). The
//! `splitFasta` operator and the method of paths with that name, and the others, use it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::csv::{self, CsvOptions};
use crate::exception::Exception;
use crate::value::{Key, PathValue, Value};

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
pub struct Splitter {
    pub format: Format,
    /// Records per chunk.
    pub by: Option<usize>,
    /// The most records to read of each text.
    pub limit: Option<usize>,
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
        }
    }

    /// The chunks of one text: single records, or lists of `by` records for CSV and record
    /// maps, or the joined text of `by` records for the other formats.
    pub fn chunks(&self, text: &str) -> Result<Vec<Value>, Exception> {
        let mut records = match &self.format {
            Format::Csv { .. } => self.csv(text),
            Format::Text { .. } => text.split_inclusive('\n').map(Value::string).collect(),
            Format::Fasta(_) => records(text, |line, _| line.starts_with('>')),
            Format::Fastq(_) => records(text, |_, index| index % 4 == 0),
            Format::Json { path } => json_records(text, path)?,
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
            return Ok(records);
        };
        Ok(records
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
            .collect())
    }

    /// Whether the records of a text go one by one: CSV records, each a chunk of its own,
    /// without `by` and `limit`.
    pub fn streams(&self) -> bool {
        matches!(self.format, Format::Csv { .. }) && self.by.is_none() && self.limit.is_none()
    }

    /// The CSV records of a text one by one, when each record is a chunk of its own: without
    /// `by` and `limit`.
    pub fn csv_records(&self, text: String) -> Option<csv::Records> {
        let Format::Csv {
            separator,
            quote,
            header,
            skip,
            strip,
        } = &self.format
        else {
            return None;
        };
        if self.by.is_some() || self.limit.is_some() {
            return None;
        }
        let options = CsvOptions {
            separator: *separator,
            quote: *quote,
            header: header.clone(),
            skip: *skip,
            strip: *strip,
        };
        Some(csv::Records::new(text, &options))
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
        let options = CsvOptions {
            separator: *separator,
            quote: *quote,
            header: header.clone(),
            skip: *skip,
            strip: *strip,
        };
        csv::records(text, &options)
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

/// The bytes of a gzip file, decompressed.
pub fn decompressed(bytes: &[u8], path: &PathValue) -> Result<Vec<u8>, Exception> {
    let mut decoded = Vec::new();
    let mut decoder = flate2::read::MultiGzDecoder::new(bytes);
    std::io::Read::read_to_end(&mut decoder, &mut decoded).map_err(|error| {
        let message = format!("Cannot decompress {}: {error}", path.uri());
        Exception::new("ZipException", message)
    })?;
    Ok(decoded)
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

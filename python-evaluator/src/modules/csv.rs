//! A stand-in for `csv`: readers and writers of delimited text.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;

use super::function;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Dict, Key, Native, Value};

/// Splits one line of CSV into fields, as `csv.reader` does with the default dialect.
pub fn split_csv_line(line: &str, delimiter: char, quote: Option<char>) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    let mut at_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            if Some(c) == quote {
                if chars.peek() == Some(&c) {
                    field.push(c);
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        if at_start && Some(c) == quote {
            quoted = true;
            at_start = false;
            continue;
        }
        if c == delimiter {
            fields.push(std::mem::take(&mut field));
            at_start = true;
            continue;
        }
        at_start = false;
        field.push(c);
    }
    fields.push(field);
    fields
}

/// The lines of a CSV source: a file object, a list of lines, or any iterable of text.
fn csv_rows(
    interpreter: &mut Interpreter,
    source: &Value,
    delimiter: char,
    quote: Option<char>,
) -> Result<Vec<Vec<String>>, Error> {
    let lines = interpreter.iterate(source)?;
    let mut rows = Vec::new();
    let mut pending = String::new();
    for line in lines {
        let text = interpreter.to_str(&line)?;
        pending.push_str(&text);
        // A quoted field may hold line breaks; wait for its closing quote.
        if let Some(quote) = quote
            && pending.matches(quote).count() % 2 == 1
        {
            continue;
        }
        let record = std::mem::take(&mut pending);
        let record = record.strip_suffix('\n').unwrap_or(&record);
        let record = record.strip_suffix('\r').unwrap_or(record);
        rows.push(split_csv_line(record, delimiter, quote));
    }
    if !pending.is_empty() {
        rows.push(split_csv_line(
            pending.trim_end_matches(['\n', '\r']),
            delimiter,
            quote,
        ));
    }
    Ok(rows)
}

fn csv_dialect(
    interpreter: &mut Interpreter,
    args: &mut Args,
) -> Result<(char, Option<char>), Error> {
    let mut delimiter = ',';
    let mut dialect_quote = Some('"');
    if let Some(dialect) = args
        .take_keyword("dialect")
        .or_else(|| args.positional.get(1).cloned())
    {
        if dialect.as_str() == Some("excel-tab") {
            delimiter = '\t';
        }
        // A dialect object, like one that `Sniffer.sniff` gives.
        if let Some(sniffed) = dialect.as_native::<Dialect>() {
            delimiter = sniffed.delimiter;
            dialect_quote = sniffed.quote;
        }
    }
    if let Some(value) = args.take_keyword("delimiter") {
        delimiter = interpreter.to_str(&value)?.chars().next().unwrap_or(',');
    }
    let quote = match args.take_keyword("quotechar") {
        Some(value) if value.is_none() => None,
        Some(value) => interpreter.to_str(&value)?.chars().next(),
        None => dialect_quote,
    };
    let quote = match args.take_keyword("quoting") {
        Some(Value::Int(3)) => None,
        _ => quote,
    };
    args.take_keyword("skipinitialspace");
    args.take_keyword("lineterminator");
    args.take_keyword("strict");
    args.take_keyword("escapechar");
    Ok((delimiter, quote))
}

/// A CSV writer on a file object.
struct CsvWriter {
    file: Value,
    delimiter: char,
    fields: Option<Vec<Value>>,
}

impl CsvWriter {
    fn write_row(&self, interpreter: &mut Interpreter, row: &Value) -> Result<(), Error> {
        let items = match (&self.fields, row.plain()) {
            (Some(fields), Value::Dict(_)) => {
                let mut items = Vec::new();
                for field in fields {
                    items.push(interpreter.get_item(row, field).unwrap_or(Value::str("")));
                }
                items
            }
            _ => interpreter.iterate(row)?,
        };
        let mut texts = Vec::new();
        for item in items {
            let text = if item.is_none() {
                String::new()
            } else {
                interpreter.to_str(&item)?
            };
            if text.contains([self.delimiter, '"', '\n', '\r']) {
                texts.push(format!("\"{}\"", text.replace('"', "\"\"")));
            } else {
                texts.push(text);
            }
        }
        let line = format!("{}\r\n", texts.join(&self.delimiter.to_string()));
        let write = interpreter.get_attribute(&self.file, "write")?;
        interpreter.call(&write, Args::new(vec![Value::str(line)]))?;
        Ok(())
    }
}

impl Native for CsvWriter {
    fn type_name(&self) -> &str {
        "_csv.writer"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        Ok(match name {
            "writerow" => Some(Value::builtin("writerow", move |interpreter, args| {
                let writer = this.as_native::<CsvWriter>().expect("a writer");
                writer.write_row(interpreter, &args.positional[0])?;
                Ok(Value::None)
            })),
            "writerows" => Some(Value::builtin("writerows", move |interpreter, args| {
                let writer = this.as_native::<CsvWriter>().expect("a writer");
                for row in interpreter.iterate(&args.positional[0])? {
                    writer.write_row(interpreter, &row)?;
                }
                Ok(Value::None)
            })),
            "writeheader" => Some(Value::builtin("writeheader", move |interpreter, _| {
                let writer = this.as_native::<CsvWriter>().expect("a writer");
                let header = Value::list(writer.fields.clone().unwrap_or_default());
                let plain = CsvWriter {
                    file: writer.file.clone(),
                    delimiter: writer.delimiter,
                    fields: None,
                };
                plain.write_row(interpreter, &header)?;
                Ok(Value::None)
            })),
            _ => None,
        })
    }
}

pub fn csv() -> Vec<(&'static str, Value)> {
    vec![
        ("QUOTE_MINIMAL", Value::Int(0)),
        ("QUOTE_ALL", Value::Int(1)),
        ("QUOTE_NONNUMERIC", Value::Int(2)),
        ("QUOTE_NONE", Value::Int(3)),
        ("excel", Value::str("excel")),
        ("excel_tab", Value::str("excel-tab")),
        (
            "Error",
            Value::Class(crate::types::library_exception(
                "csv.Error",
                "csv",
                "Exception",
            )),
        ),
        (
            "field_size_limit",
            function("field_size_limit", |_, _| Ok(Value::Int(131072))),
        ),
        (
            "reader",
            function("reader", |interpreter, mut args| {
                let (delimiter, quote) = csv_dialect(interpreter, &mut args)?;
                let source = args
                    .positional
                    .first()
                    .cloned()
                    .ok_or_else(|| Error::type_error("reader() missing file"))?;
                let rows = csv_rows(interpreter, &source, delimiter, quote)?;
                Ok(Value::iterator(rows.into_iter().map(|row| {
                    Value::list(row.into_iter().map(Value::str).collect())
                })))
            }),
        ),
        (
            "DictReader",
            function("DictReader", |interpreter, mut args| {
                let fieldnames = args.take_keyword("fieldnames");
                let restval = args.take_keyword("restval").unwrap_or(Value::None);
                args.take_keyword("restkey");
                let (delimiter, quote) = csv_dialect(interpreter, &mut args)?;
                let source = args
                    .positional
                    .first()
                    .cloned()
                    .ok_or_else(|| Error::type_error("DictReader() missing file"))?;
                let mut rows: VecDeque<Vec<String>> =
                    csv_rows(interpreter, &source, delimiter, quote)?.into();
                // Without a first row, as in an empty file, the reader has no field names.
                let header: Option<Vec<Value>> = match fieldnames {
                    Some(names) if !names.is_none() => Some(interpreter.iterate(&names)?),
                    _ => rows
                        .pop_front()
                        .map(|row| row.into_iter().map(Value::str).collect()),
                };
                rows.retain(|row| !(row.len() == 1 && row[0].is_empty()));
                Ok(Value::native(DictReader {
                    fieldnames: RefCell::new(header),
                    rows: RefCell::new(rows),
                    restval,
                }))
            }),
        ),
        (
            "writer",
            function("writer", |interpreter, mut args| {
                let (delimiter, _) = csv_dialect(interpreter, &mut args)?;
                let file = args
                    .positional
                    .first()
                    .cloned()
                    .ok_or_else(|| Error::type_error("writer() missing file"))?;
                Ok(Value::native(CsvWriter {
                    file,
                    delimiter,
                    fields: None,
                }))
            }),
        ),
        (
            "DictWriter",
            function("DictWriter", |interpreter, mut args| {
                let fields = args
                    .take(1, "fieldnames")
                    .ok_or_else(|| Error::type_error("DictWriter() missing fieldnames"))?;
                if args.positional.len() > 1 {
                    args.positional.truncate(1);
                }
                let (delimiter, _) = csv_dialect(interpreter, &mut args)?;
                let file = args
                    .positional
                    .first()
                    .cloned()
                    .ok_or_else(|| Error::type_error("DictWriter() missing file"))?;
                let fields = interpreter.iterate(&fields)?;
                Ok(Value::native(CsvWriter {
                    file,
                    delimiter,
                    fields: Some(fields),
                }))
            }),
        ),
        (
            "Sniffer",
            function("Sniffer", |_, _| Ok(Value::native(Sniffer))),
        ),
    ]
}

/// `csv.DictReader`: row dicts by the field names, which code may change before it reads.
struct DictReader {
    fieldnames: RefCell<Option<Vec<Value>>>,
    rows: RefCell<VecDeque<Vec<String>>>,
    restval: Value,
}

impl DictReader {
    /// A row as a dict of the current field names.
    fn record(&self, row: Vec<String>) -> Result<Value, Error> {
        let mut dict = Dict::default();
        for (index, name) in self.fieldnames.borrow().iter().flatten().enumerate() {
            let value = row
                .get(index)
                .map_or_else(|| self.restval.clone(), |text| Value::str(text.as_str()));
            dict.entries.insert(Key::new(name.clone())?, value);
        }
        Ok(Value::dict(dict))
    }
}

impl Native for DictReader {
    fn type_name(&self) -> &str {
        "DictReader"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        let rows: Vec<Vec<String>> = self.rows.borrow_mut().drain(..).collect();
        rows.into_iter()
            .map(|row| self.record(row))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        Ok(match name {
            "fieldnames" => Some(
                self.fieldnames
                    .borrow()
                    .clone()
                    .map_or(Value::None, Value::list),
            ),
            "__next__" => Some(Value::builtin("__next__", move |_, _| {
                let Some(reader) = this.as_native::<DictReader>() else {
                    return Err(Error::type_error("not a DictReader"));
                };
                let row = reader.rows.borrow_mut().pop_front();
                match row {
                    Some(row) => reader.record(row),
                    None => Err(Error::stop_iteration()),
                }
            })),
            _ => None,
        })
    }

    fn set_attribute(
        &self,
        interpreter: &mut Interpreter,
        name: &str,
        value: Value,
    ) -> Result<bool, Error> {
        if name != "fieldnames" {
            return Ok(false);
        }
        *self.fieldnames.borrow_mut() = if value.is_none() {
            None
        } else {
            Some(interpreter.iterate(&value)?)
        };
        Ok(true)
    }
}

/// A dialect that `Sniffer.sniff` found.
struct Dialect {
    delimiter: char,
    quote: Option<char>,
}

impl Native for Dialect {
    fn type_name(&self) -> &str {
        "Dialect"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match name {
            "delimiter" => Value::str(self.delimiter.to_string()),
            "quotechar" => self
                .quote
                .map_or(Value::None, |quote| Value::str(quote.to_string())),
            "doublequote" => Value::Bool(true),
            "skipinitialspace" => Value::Bool(false),
            "lineterminator" => Value::str("\r\n"),
            "quoting" => Value::Int(0),
            _ => return Ok(None),
        }))
    }
}

/// The delimiter of a sample of delimited text: of the candidates, the first that occurs the
/// same number of times, and at least once, in each of the first lines.
pub fn sniff_delimiter(sample: &str, candidates: &str) -> Option<char> {
    let lines: Vec<&str> = sample
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(10)
        .collect();
    candidates.chars().find(|candidate| {
        let counts: Vec<usize> = lines
            .iter()
            .map(|line| line.matches(*candidate).count())
            .collect();
        counts.first().is_some_and(|first| *first > 0)
            && counts.iter().all(|count| *count == counts[0])
    })
}

/// `csv.Sniffer`: finds the delimiter of a sample, and whether it starts with a header.
struct Sniffer;

impl Native for Sniffer {
    fn type_name(&self) -> &str {
        "Sniffer"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match name {
            "sniff" => Value::builtin("sniff", |interpreter, mut args| {
                let sample = interpreter.to_str(&args.take(0, "sample").unwrap_or(Value::None))?;
                let candidates = match args.take(1, "delimiters") {
                    Some(value) if !value.is_none() => interpreter.to_str(&value)?,
                    _ => "\t,;| :".to_owned(),
                };
                let delimiter = sniff_delimiter(&sample, &candidates).ok_or_else(|| {
                    let class = crate::types::library_exception("Error", "csv", "Exception");
                    Error::from_value(crate::error::exception(
                        &class,
                        vec![Value::str("Could not determine delimiter")],
                    ))
                })?;
                Ok(Value::native(Dialect {
                    delimiter,
                    quote: Some('"'),
                }))
            }),
            // The first line is a header when some column holds numbers below it but not in it.
            "has_header" => Value::builtin("has_header", |interpreter, args| {
                let sample = interpreter.to_str(&args.get(0).cloned().unwrap_or(Value::None))?;
                let delimiter = sniff_delimiter(&sample, "\t,;| :").unwrap_or(',');
                let rows: Vec<Vec<String>> = sample
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(|line| split_csv_line(line, delimiter, Some('"')))
                    .collect();
                let Some((header, body)) = rows.split_first() else {
                    return Ok(Value::Bool(false));
                };
                let numeric = |text: &str| text.trim().parse::<f64>().is_ok();
                let header_like = (0..header.len()).any(|column| {
                    !numeric(&header[column])
                        && !body.is_empty()
                        && body
                            .iter()
                            .all(|row| row.get(column).is_some_and(|text| numeric(text)))
                });
                Ok(Value::Bool(
                    header_like || header.iter().all(|cell| !numeric(cell)),
                ))
            }),
            _ => return Ok(None),
        }))
    }
}

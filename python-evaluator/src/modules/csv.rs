//! A stand-in for `csv`: readers and writers of delimited text.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::function;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Dict, Native, Value};

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
    if let Some(dialect) = args
        .take_keyword("dialect")
        .or_else(|| args.positional.get(1).cloned())
        && let Some(name) = dialect.as_str()
        && name == "excel-tab"
    {
        delimiter = '\t';
    }
    if let Some(value) = args.take_keyword("delimiter") {
        delimiter = interpreter.to_str(&value)?.chars().next().unwrap_or(',');
    }
    let quote = match args.take_keyword("quotechar") {
        Some(value) if value.is_none() => None,
        Some(value) => interpreter.to_str(&value)?.chars().next(),
        None => Some('"'),
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
                let mut rows = csv_rows(interpreter, &source, delimiter, quote)?.into_iter();
                let header: Vec<String> = match fieldnames {
                    Some(names) if !names.is_none() => interpreter
                        .iterate(&names)?
                        .iter()
                        .map(|name| interpreter.to_str(name))
                        .collect::<Result<_, _>>()?,
                    _ => rows.next().unwrap_or_default(),
                };
                let mut records = Vec::new();
                for row in rows {
                    if row.len() == 1 && row[0].is_empty() {
                        continue;
                    }
                    let mut dict = Dict::default();
                    for (index, name) in header.iter().enumerate() {
                        let value = row
                            .get(index)
                            .map_or_else(|| restval.clone(), |text| Value::str(text.as_str()));
                        dict.insert(name, value);
                    }
                    records.push(Value::dict(dict));
                }
                Ok(Value::native(DictReader {
                    fieldnames: header.into_iter().map(Value::str).collect(),
                    rows: Value::iterator(records),
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
            function("Sniffer", |_, _| Err(Error::unsupported("csv.Sniffer"))),
        ),
    ]
}

/// `csv.DictReader`: an iterator of row dicts that knows its field names.
struct DictReader {
    fieldnames: Vec<Value>,
    rows: Value,
}

impl Native for DictReader {
    fn type_name(&self) -> &str {
        "DictReader"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn iterate(&self, interpreter: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        interpreter.iterate(&self.rows).map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let rows = self.rows.clone();
        Ok(match name {
            "fieldnames" => Some(Value::list(self.fieldnames.clone())),
            "__next__" => Some(Value::builtin("__next__", move |interpreter, _| {
                let next = interpreter.builtins.get("next").unwrap_or(Value::None);
                interpreter.call(&next, Args::new(vec![rows.clone()]))
            })),
            _ => None,
        })
    }
}

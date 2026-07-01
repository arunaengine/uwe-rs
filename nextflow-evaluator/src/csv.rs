//! CSV records as Nextflow's `splitCsv` reads them, for channels, files and text.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::exception::Exception;
use crate::value::{Key, Map, MapKind, Value};

/// How to read CSV text.
pub struct CsvOptions {
    pub separator: char,
    /// The quote character, which groups text with separators; without one, quotes are text.
    pub quote: Option<char>,
    /// Column names from the first line (`header: true`) or given as a list.
    pub header: Option<Option<Vec<Value>>>,
    /// Lines to skip at the start.
    pub skip: usize,
    /// Whether blanks around fields are removed.
    pub strip: bool,
}

/// The records of a text: a list of fields per line, or with a header a map of them. Blank
/// lines are left out. A text without the header line it should have is an error, as in
/// Nextflow, unless the run is `simulated`, whose files are made up.
pub fn records(text: &str, options: &CsvOptions, simulated: bool) -> Result<Vec<Value>, Exception> {
    let records = Records::new(text.to_owned(), options);
    if records.missing_header() && !simulated {
        return Err(missing_header());
    }
    Ok(records.collect())
}

/// The records of a text one by one, so that a large file need not be in records all at once.
pub struct Records {
    text: String,
    /// Where the next line starts.
    position: usize,
    separator: char,
    quote: Option<char>,
    strip: bool,
    names: Option<Vec<Value>>,
    /// A map of the names, which each record copies, so that its keys are hashed once. Names
    /// that repeat have none, and each record hashes its keys.
    template: Option<IndexMap<Key, Value>>,
    /// Whether the first line was to be the header, but the text has no line.
    missing_header: bool,
}

/// The error of Nextflow for a CSV text without the header line it should have.
pub fn missing_header() -> Exception {
    Exception::new("IllegalStateException", "Missing 'header' in CSV file")
}

impl Records {
    pub fn new(text: String, options: &CsvOptions) -> Records {
        let mut records = Records {
            text,
            position: 0,
            separator: options.separator,
            quote: options.quote,
            strip: options.strip,
            names: None,
            template: None,
            missing_header: false,
        };
        for _ in 0..options.skip {
            records.line();
        }
        records.names = match &options.header {
            Some(None) => {
                let names = records.line();
                records.missing_header = names.is_none();
                names.map(|names| names.into_iter().map(Value::string).collect())
            }
            Some(Some(names)) => Some(names.clone()),
            None => None,
        };
        records.template = records.names.as_ref().and_then(|names| {
            let template: IndexMap<Key, Value> = (names.iter())
                .map(|name| (Key(name.clone()), Value::Null))
                .collect();
            (template.len() == names.len()).then_some(template)
        });
        records
    }

    /// Whether the first line was to be the header, but the text has no line. Nextflow then
    /// stops with [`missing_header`].
    pub fn missing_header(&self) -> bool {
        self.missing_header
    }

    /// Whether the whole text is read.
    pub fn is_done(&self) -> bool {
        self.position >= self.text.len()
    }

    /// The fields of the next line that is not blank.
    fn line(&mut self) -> Option<Vec<String>> {
        loop {
            let rest = &self.text[self.position..];
            if rest.is_empty() {
                return None;
            }
            let end = rest.find('\n').unwrap_or(rest.len());
            let line = rest[..end].strip_suffix('\r').unwrap_or(&rest[..end]);
            self.position += (end + 1).min(rest.len());
            if !line.trim().is_empty() {
                return Some(fields(line, self.separator, self.quote, self.strip));
            }
        }
    }
}

impl Iterator for Records {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        let row = self.line()?;
        Some(match (&self.names, &self.template) {
            (Some(_), Some(template)) => {
                let mut entries = template.clone();
                for (field, (_, value)) in row.into_iter().zip(entries.iter_mut()) {
                    *value = Value::string(field);
                }
                Value::Map(Rc::new(RefCell::new(Map {
                    kind: MapKind::LinkedHashMap,
                    entries,
                    default: None,
                })))
            }
            (Some(names), None) => Value::map(names.iter().enumerate().map(|(index, name)| {
                let field = row
                    .get(index)
                    .map_or(Value::Null, |f| Value::string(f.as_str()));
                (name.clone(), field)
            })),
            (None, _) => Value::list(row.into_iter().map(Value::string).collect()),
        })
    }
}

/// The fields of a CSV line; quotes group text with separators and `""` is a quote.
fn fields(line: &str, separator: char, quote: Option<char>, strip: bool) -> Vec<String> {
    // Without quotes, the fields are the parts between separators.
    if quote.is_none_or(|quote| !line.contains(quote)) {
        let part = |field: &str| match strip {
            true => field.trim().to_owned(),
            false => field.to_owned(),
        };
        return line.split(separator).map(part).collect();
    }
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            c if Some(c) == quote && quoted && characters.peek() == quote.as_ref() => {
                characters.next();
                fields.last_mut().expect("a field").push(c);
            }
            c if Some(c) == quote => quoted = !quoted,
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

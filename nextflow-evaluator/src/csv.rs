//! CSV records as Nextflow's `splitCsv` reads them, for channels, files and text.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::value::Value;

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
/// lines are left out.
pub fn records(text: &str, options: &CsvOptions) -> Vec<Value> {
    let mut rows = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .skip(options.skip)
        .map(|line| fields(line, options.separator, options.quote, options.strip));
    let names: Option<Vec<Value>> = match &options.header {
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

/// The fields of a CSV line; quotes group text with separators and `""` is a quote.
fn fields(line: &str, separator: char, quote: Option<char>, strip: bool) -> Vec<String> {
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

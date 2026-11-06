//! nf-csvext 0.2.0: CSV files concatenated, sorted, trimmed and padded, with the plugin's bugs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{PathValue, Value};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "csv_concat",
        "csv_sort",
        "csv_trim",
        "csv_prettyprint",
        "csv_create",
    ],
    call: Some(call),
    script: Some(include_str!("nf_csvext.nf")),
    ..Plugin::new("nf-csvext", Kind::Functions, State::Implemented)
};

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let (options, files) = split(arguments);
    let option = |key: &str| {
        let found = options.iter().find(|(name, _)| name == key);
        found
            .map(|(_, value)| value.clone())
            .filter(|value| !matches!(value, Value::Null))
    };
    let sep = option("sep").map_or(",".to_owned(), |sep| sep.to_string());
    let Some(source) = files.first() else {
        return Err(thrown(format!("`{name}` needs a source file")));
    };
    let lines = read(source, context)?;
    let text = match name {
        "csv_concat" => {
            let appends: Vec<Value> = match files.get(1) {
                Some(Value::List(list)) => list.borrow().items.clone(),
                Some(single) => vec![single.clone()],
                None => Vec::new(),
            };
            let mut appended = Vec::new();
            for append in &appends {
                appended.push(read(append, context)?);
            }
            concat(&lines, &appended, &sep)?
        }
        "csv_sort" => match sort(&lines, option("column"), &sep) {
            Ok(text) => text,
            // The plugin prints the exception and gives null.
            Err(message) => {
                (context.host).print(&format!("java.lang.IllegalArgumentException: {message}\n"));
                return Ok(Value::Null);
            }
        },
        "csv_trim" => trim(&lines, option("column"), option("columns"), &sep)?,
        "csv_prettyprint" => {
            let joint = option("newSep").map_or(sep.clone(), |joint| joint.to_string());
            pretty(&lines, &sep, &joint)
        }
        _ => {
            return Err(Error::Unsupported {
                message: format!("the function `{name}` of the plugin `nf-csvext`"),
                span: Default::default(),
            });
        }
    };
    written(&text, context)
}

/// The named options, which Groovy passes first, and the other arguments.
fn split(arguments: Vec<Value>) -> (Vec<(String, Value)>, Vec<Value>) {
    let mut arguments = arguments.into_iter();
    match arguments.next() {
        Some(options @ Value::Map(_)) => (crate::nf_schema::entries(&options), arguments.collect()),
        Some(first) => (
            Vec::new(),
            std::iter::once(first).chain(arguments).collect(),
        ),
        None => (Vec::new(), Vec::new()),
    }
}

/// The lines of a file, as Java's line reader splits them.
fn read(file: &Value, context: &mut Context) -> Result<Vec<String>, Error> {
    let path = match file {
        Value::Path(path) => (**path).clone(),
        other => PathValue::parse(&other.to_string(), &context.host.launch_dir()),
    };
    let bytes = context.host.read(&path)?;
    let text = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    Ok(text.lines().map(str::to_owned).collect())
}

/// Writes a new file `tmp<digits>csv` in the run's own scratch folder.
fn written(text: &str, context: &mut Context) -> Result<Value, Error> {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let folder = context.host.operator_directory()?;
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let nanos = time.map_or(0, |time| time.subsec_nanos());
    let count = COUNT.fetch_add(1, Ordering::Relaxed);
    let name = format!("tmp{}{nanos}{count}csv", std::process::id());
    let path = PathValue::parse(&format!("{}/{name}", folder.uri()), "/");
    context.host.write(&path, text.as_bytes())?;
    Ok(Value::Path(Rc::new(path)))
}

/// The source, then each append without its first line, after checking that the first lines
/// have equal numbers of fields. The plugin reads its `header` option so that this is always on.
fn concat(lines: &[String], appends: &[Vec<String>], sep: &str) -> Result<String, Error> {
    if appends.is_empty() {
        return Err(thrown("Index 0 out of bounds for length 0"));
    }
    let width = |lines: &[String]| lines.first().map_or(0, |line| fields(line, sep).len());
    let mut text = joined(lines);
    for append in appends {
        if width(lines) != width(append) {
            return Err(thrown("source and append must to be same size"));
        }
        text.push_str(&joined(append.get(1..).unwrap_or_default()));
    }
    Ok(text)
}

/// The header, then the rows in a stable order of the trimmed field of `column`: as numbers
/// when both are numbers, else as text. Fails with the message the plugin prints.
fn sort(lines: &[String], column: Option<Value>, sep: &str) -> Result<String, String> {
    let Some(header) = lines.first() else {
        return Err("El archivo CSV está vacío".to_owned());
    };
    let names = parsed(header, sep);
    let column = column.unwrap_or(Value::Number(Number::Integer(0)));
    let index = match &column {
        Value::Number(Number::Integer(index)) => usize::try_from(*index).ok(),
        Value::Number(Number::Long(index)) => usize::try_from(*index).ok(),
        other => {
            let wanted = other.to_string();
            let named = (names.iter()).position(|name| name.trim().eq_ignore_ascii_case(&wanted));
            named.or_else(|| wanted.parse().ok())
        }
    };
    let Some(index) = index.filter(|index| *index < names.len()) else {
        return Err(format!("Invalid column index {column}"));
    };
    let mut rows: Vec<(String, &String)> = (lines[1..].iter())
        .map(|line| {
            let field = parsed(line, sep).get(index).cloned().unwrap_or_default();
            (field.trim().to_owned(), line)
        })
        .collect();
    rows.sort_by(
        |(left, _), (right, _)| match (left.parse::<f32>(), right.parse::<f32>()) {
            (Ok(left), Ok(right)) => left.total_cmp(&right),
            _ => left.cmp(right),
        },
    );
    let mut text = format!("{header}\n");
    for (_, line) in rows {
        text.push_str(line);
        text.push('\n');
    }
    Ok(text)
}

/// The lines without the columns of `column` or of a `columns` list; `columns` as text removes
/// nothing, as in the plugin, and fields split on the separator even inside quotes.
fn trim(
    lines: &[String],
    column: Option<Value>,
    columns: Option<Value>,
    sep: &str,
) -> Result<String, Error> {
    let names = lines
        .first()
        .map(|line| fields(line, sep))
        .unwrap_or_default();
    let index = |value: &Value| -> Option<usize> {
        let text = value.to_string();
        match text.chars().all(|c| c.is_ascii_digit()) && !text.is_empty() {
            true => text.parse().ok(),
            false => names.iter().position(|name| *name == text),
        }
    };
    let removed: Vec<usize> = match (column, columns) {
        (None, None) => return Err(thrown("Column(s) to trim are required")),
        (Some(_), Some(_)) => {
            return Err(thrown(
                "Only column or columns must to be specified, not both",
            ));
        }
        (Some(column), None) if column.to_string().contains(',') => {
            return Err(thrown(
                "Column argument can't be a list. Use instead `columns`",
            ));
        }
        (Some(column), None) => index(&column).into_iter().collect(),
        (None, Some(Value::List(list))) => list.borrow().items.iter().filter_map(index).collect(),
        (None, Some(_)) => Vec::new(),
    };
    let mut text = String::new();
    for line in lines {
        let kept: Vec<String> = (fields(line, sep).into_iter().enumerate())
            .filter(|(at, _)| !removed.contains(at))
            .map(|(_, field)| field)
            .collect();
        text.push_str(&kept.join(sep));
        text.push('\n');
    }
    Ok(text)
}

/// Every field padded with spaces to the widest value of its column, joined with `joint`.
fn pretty(lines: &[String], sep: &str, joint: &str) -> String {
    let rows: Vec<Vec<String>> = lines.iter().map(|line| parsed(line, sep)).collect();
    let mut widths: Vec<usize> = Vec::new();
    for row in &rows {
        for (at, field) in row.iter().enumerate() {
            let width = field.chars().count();
            match widths.get_mut(at) {
                Some(widest) => *widest = (*widest).max(width),
                None => widths.push(width),
            }
        }
    }
    let mut text = String::new();
    for row in rows {
        let padded: Vec<String> = (row.iter().zip(&widths))
            .map(|(field, width)| format!("{field:<width$}"))
            .collect();
        text.push_str(&padded.join(joint));
        text.push('\n');
    }
    text
}

fn joined(lines: &[String]) -> String {
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// The fields of a line split on the separator, without trailing empty fields, as Java's
/// `String.split` gives them.
fn fields(line: &str, sep: &str) -> Vec<String> {
    let mut fields: Vec<String> = line.split(sep).map(str::to_owned).collect();
    while fields.last().is_some_and(String::is_empty) {
        fields.pop();
    }
    fields
}

/// The fields of the plugin's parser: split on the separator outside double quotes, which it
/// drops.
fn parsed(line: &str, sep: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '"' {
            quoted = !quoted;
            rest = &rest[1..];
        } else if !quoted && !sep.is_empty() && rest.starts_with(sep) {
            fields.push(String::new());
            rest = &rest[sep.len()..];
        } else {
            if let Some(field) = fields.last_mut() {
                field.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    fields
}

fn thrown(message: impl Into<String>) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}

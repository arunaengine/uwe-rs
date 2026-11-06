//! The writer behind `sqlInsert`: it adds each item to a batch and commits full batches.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{Native, Value};

use super::{
    Connection, Datum, Flavour, Source, Type, check_options, datum, option, option_number,
    option_text, source, split_options, target, thrown,
};
use crate::Context;

/// The options of `sqlInsert`, checked when the operator is set up.
const TYPES: [(&str, Type); 7] = [
    ("db", Type::Text),
    ("into", Type::Text),
    ("columns", Type::TextOrList),
    ("statement", Type::Text),
    ("batch", Type::Integer),
    ("batchSize", Type::Integer),
    ("setup", Type::Text),
];

/// `sqlWriter(options)`: checks the options of `sqlInsert` and gives its writer.
pub(super) fn writer(
    flavour: Flavour,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let (options, _) = split_options(arguments);
    check_options("sqlInsert", &options, &TYPES)?;
    let source = source(flavour, &option_text(&options, "db"), context.config, false)?;
    let size = option_number(&options, "batchSize").unwrap_or(0);
    if size < 0 {
        return Err(thrown(format!(
            "SQL batch option must be greater than zero: {size}"
        )));
    }
    if option(&options, "batch").is_some() {
        context.host.print(
            "WARN: The option 'batch' for the 'sqlInsert' operator has been deprecated - Use \
             'batchSize' instead\n",
        );
    }
    let columns = match option(&options, "columns") {
        Some(Value::List(list)) => (list.borrow().items.iter())
            .map(|name| name.to_string())
            .collect(),
        Some(text) => (text.to_string().split(','))
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect(),
        None => Vec::new(),
    };
    let writer = Writer {
        source,
        into: Some(option_text(&options, "into")).filter(|into| !into.is_empty()),
        columns,
        statement: Some(option_text(&options, "statement")).filter(|text| !text.is_empty()),
        setup: Some(option_text(&options, "setup")).filter(|text| !text.is_empty()),
        size: if size == 0 { 10 } else { size as usize },
        connection: None,
        pending: Vec::new(),
    };
    Ok(crate::object("nextflow.sql.InsertWriter", writer))
}

/// Runs `add(item)` and `close()` of a writer.
pub(super) fn methods(
    object: &Value,
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Option<Value>, Error> {
    let Some(mut writer) = crate::native::<Writer>(object) else {
        return Ok(None);
    };
    match (name, arguments.as_slice()) {
        ("add", [item]) => writer.add(item, context).map(|()| Some(Value::Null)),
        ("close", []) => writer.flush().map(|()| {
            writer.connection = None;
            Some(Value::Null)
        }),
        _ => Ok(None),
    }
}

/// The state of one `sqlInsert`: its own connection, opened on the first item, the statement
/// and columns that the first item settles, and the rows not yet committed.
struct Writer {
    source: Source,
    into: Option<String>,
    columns: Vec<String>,
    statement: Option<String>,
    setup: Option<String>,
    size: usize,
    connection: Option<Box<dyn Connection>>,
    pending: Vec<Vec<Datum>>,
}

impl fmt::Debug for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (f.debug_struct("Writer"))
            .field("db", &self.source.name)
            .field("into", &self.into)
            .finish_non_exhaustive()
    }
}

impl Native for Writer {
    fn plugin(&self) -> &'static str {
        "nf-sqldb"
    }

    fn text(&self) -> String {
        format!("sqlInsert({})", self.source.name)
    }
}

impl Writer {
    fn add(&mut self, item: &Value, context: &mut Context) -> Result<(), Error> {
        if matches!(item, Value::Null) {
            return Err(thrown(
                "Invalid sqlInsert entry - It must be a non-null value",
            ));
        }
        if self.connection.is_none() {
            if let Some(setup) = &self.setup {
                connection.execute(setup).map_err(thrown)?;
            }
            self.connection = Some(connection);
        }
        if self.statement.is_none() {
            self.statement = Some(self.prepare(item)?);
        }
        let row = match item {
            Value::List(list) => list.borrow().items.iter().map(datum).collect(),
            Value::Map(_) => {
                let entries = crate::nf_schema::entries(item);
                let lookup = |column: &String| {
                    let found = entries.iter().find(|(key, _)| key == column);
                    found.map_or(Datum::Null, |(_, value)| datum(value))
                };
                self.columns.iter().map(lookup).collect()
            }
            other => vec![datum(other)],
        };
        self.pending.push(row);
        if self.pending.len() >= self.size {
            self.flush()?;
        }
        Ok(())
    }

    /// The insert statement of the target table, with the columns of the option or those
    /// that the first item gives.
    fn prepare(&mut self, item: &Value) -> Result<String, Error> {
        let Some(into) = self.into.clone() else {
            return Err(thrown(
                "Missing sqlInsert target table - Use 'into' option to specify it",
            ));
        };
        if self.columns.is_empty() {
            self.columns = self.inferred(&into, item)?;
        }
        if self.columns.is_empty() {
            return Err(thrown("Missing sqlInsert 'columns' option"));
        }
        let marks = vec!["?"; self.columns.len()].join(",");
        let columns = self.columns.join(",");
        Ok(format!(
            "insert into {into} ( {columns} ) values ( {marks} )"
        ))
    }

    /// The table's columns; for a map, those whose names equal its keys in upper case, which
    /// then look the values up by the column names, as nf-sqldb 0.7.1 does.
    fn inferred(&mut self, into: &str, item: &Value) -> Result<Vec<String>, Error> {
        let Some(connection) = self.connection.as_mut() else {
            return Ok(Vec::new());
        };
        let table = connection.columns(into).map_err(thrown)?;
        if !matches!(item, Value::Map(_)) {
            return Ok(table);
        }
        let keys: Vec<String> = (crate::nf_schema::entries(item).into_iter())
            .map(|(key, _)| key.to_uppercase())
            .collect();
        let unknown: Vec<&str> = (keys.iter())
            .filter(|key| !table.contains(key))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(thrown(format!(
                "Unknown columns for table '{into}': {} - Offending record: {item}",
                unknown.join(",")
            )));
        }
        Ok(table
            .into_iter()
            .filter(|column| keys.contains(column))
            .collect())
    }

    /// Inserts and commits the pending rows.
    fn flush(&mut self) -> Result<(), Error> {
        let (Some(connection), Some(statement)) = (self.connection.as_mut(), &self.statement)
        else {
            return Ok(());
        };
        if self.pending.is_empty() {
            return Ok(());
        }
        let rows = std::mem::take(&mut self.pending);
        connection.insert(statement, &rows).map_err(thrown)
    }
}

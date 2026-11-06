//! nf-sqldb 0.7.1: `fromQuery`, `sqlInsert` and `sqlExecute` on the databases of `sql.db`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod insert;
mod server;
mod sqlite;
pub(crate) mod target;

use std::rc::Rc;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::{BigDecimal, Number};
use nextflow_evaluator::value::{Object, State as ObjectState, Value};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["fromQuery", "sqlInsert", "sqlExecute"],
    call: Some(call),
    methods: Some(insert::methods),
    script: Some(include_str!("operators.nf")),
    factories: &["fromQuery"],
    scopes: &["sql"],
    ..Plugin::new("nf-sqldb", Kind::Functions, State::Partial)
};

/// The plugin whose functions run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flavour {
    Sqldb,
}

/// Runs the functions for nf-sqldb.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    run(Flavour::Sqldb, name, arguments, context)
}

/// Runs `fromQuery`, `sqlExecute` and `sqlWriter`, the helper of the `sqlInsert` script.
pub(crate) fn run(
    flavour: Flavour,
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    match name {
        "fromQuery" => from_query(flavour, arguments, context),
        "sqlExecute" => sql_execute(flavour, arguments, context),
        "sqlWriter" => insert::writer(flavour, arguments, context),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-sqldb`"),
            span: Default::default(),
        }),
    }
}

/// A database of the `sql.db` scope, with the fallbacks of its `default` entry.
#[derive(Clone, Debug)]
pub(crate) struct Source {
    pub name: String,
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
}

/// The value of a database row or an insert parameter, apart from uwe's values so that it can
/// cross to the thread of a network client.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Datum {
    Null,
    Boolean(bool),
    Integer(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
}

/// The result of a query: its column names and its rows.
#[derive(Debug, Default)]
pub(crate) struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Datum>>,
}

/// An open database connection of a client in uwe.
pub(crate) trait Connection {
    fn query(&mut self, sql: &str) -> Result<Table, String>;
    /// Runs a statement and gives the number of rows it changed; 0 for a query.
    fn execute(&mut self, sql: &str) -> Result<u64, String>;
    /// The columns of a table, in table order.
    fn columns(&mut self, table: &str) -> Result<Vec<String>, String>;
    /// Runs the statement once for each row, in one transaction that it commits.
    fn insert(&mut self, statement: &str, rows: &[Vec<Datum>]) -> Result<(), String>;
}

/// `channel.fromQuery(query, [db:, emitColumns:, batchSize:, batchDelay:])`: the rows as lists
/// of values, after the column names when asked for.
fn from_query(
    flavour: Flavour,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let (options, positional) = split_options(arguments);
    let types = [
        ("db", Type::Text),
        ("emitColumns", Type::Boolean),
        ("batchSize", Type::Integer),
        ("batchDelay", Type::Integer),
    ];
    check_options("fromQuery", &options, &types)?;
    let query = match positional.first() {
        Some(Value::Null) | None => String::new(),
        Some(query) => query.to_string(),
    };
    if query.trim().is_empty() {
        return Err(thrown("Missing query argument"));
    }
    let source = source(flavour, &option_text(&options, "db"), context.config, false)?;
    let mut connection = target::connect(&source, &context.host.launch_dir()).map_err(thrown)?;
    let emit = option(&options, "emitColumns").is_some_and(|value| value.truth());
    let size = option_number(&options, "batchSize").unwrap_or(0);
    let delay = option_number(&options, "batchDelay").unwrap_or(100);
    let failed = |message: String| thrown(message);
    let mut items = Vec::new();
    if size <= 0 {
        let table = connection.query(query.trim()).map_err(failed)?;
        if emit {
            items.push(names(&table.columns));
        }
        items.extend(table.rows.into_iter().map(row_value));
        return Ok(Value::list(items));
    }
    let base = paged(&query)?;
    for page in 0.. {
        let sql = format!("{base}\nLIMIT {size} OFFSET {}", page * size);
        let table = connection.query(&sql).map_err(failed)?;
        if emit && page == 0 {
            items.push(names(&table.columns));
        }
        let count = table.rows.len();
        items.extend(table.rows.into_iter().map(row_value));
        std::thread::sleep(Duration::from_millis(u64::try_from(delay).unwrap_or(0)));
        if count < usize::try_from(size).unwrap_or(usize::MAX) {
            break;
        }
    }
    Ok(Value::list(items))
}

/// The query for paging: without trailing blanks and semicolons, refused when it has its own
/// `LIMIT` or `OFFSET` keyword; names that only contain the words pass.
fn paged(query: &str) -> Result<String, Error> {
    let words: Vec<String> = (query.split(|c: char| !c.is_alphanumeric() && c != '_'))
        .map(str::to_ascii_uppercase)
        .collect();
    for keyword in ["LIMIT", "OFFSET"] {
        if words.iter().any(|word| word == keyword) {
            return Err(thrown(format!(
                "Sql query should not include the {keyword} statement when pageSize is \
                 specified: {query}"
            )));
        }
    }
    let mut query = query.to_owned();
    loop {
        query.truncate(
            query
                .trim_end_matches(|c: char| c.is_whitespace() || c == ';')
                .len(),
        );
        let line = query.rfind('\n').map_or(0, |at| at + 1);
        // A `--` after an even number of quotes on the last line starts a comment.
        let comment = query[line..]
            .match_indices("--")
            .map(|(at, _)| line + at)
            .find(|at| query[line..*at].matches('\'').count() % 2 == 0);
        match comment {
            Some(at) => query.truncate(at),
            None => return Ok(query),
        }
    }
}

/// `sqlExecute([db:, statement:])`: `[success:true, result:<rows changed or null>]`, or
/// `[success:false, error:<message>]` for a database error, which it also logs.
fn sql_execute(
    flavour: Flavour,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let (options, _) = split_options(arguments);
    let types = [("db", Type::Text), ("statement", Type::Text)];
    check_options("sqlExecute", &options, &types)?;
    let statement = option_text(&options, "statement");
    if statement.trim().is_empty() {
        return Ok(outcome(
            Err("Missing required parameter 'statement'".into()),
        ));
    }
    let source = match source(flavour, &option_text(&options, "db"), context.config, true) {
        Ok(source) => source,
        Err(Error::Thrown { exception, .. }) if exception.message.starts_with("Unknown db") => {
            return Ok(outcome(Err(exception.message.to_string())));
        }
        Err(error) => return Err(error),
    };
    let launch = context.host.launch_dir();
    let mut connection = match target::connect(&source, &launch) {
        Ok(connection) => connection,
        Err(message) if target::refused(&source.url) => return Err(thrown(message)),
        Err(message) => return Ok(logged(message, context)),
    };
    let statement = statement.trim();
    let lower = statement.to_ascii_lowercase();
    let definition = ["create", "alter", "drop", "truncate"]
        .iter()
        .any(|word| lower.starts_with(word))
        && !statement.contains('\n');
    match connection.execute(statement) {
        Ok(_) if definition => Ok(outcome(Ok(Value::Null))),
        Ok(count) => {
            let count = i64::try_from(count).unwrap_or(i64::MAX);
            Ok(outcome(Ok(integer(count))))
        }
        Err(message) => Ok(logged(message, context)),
    }
}

/// The failure map of a database error, after logging it as the plugin does.
fn logged(message: String, context: &mut Context) -> Value {
    (context.host).print(&format!(
        "ERROR ~ Error executing SQL statement: {message}\n"
    ));
    outcome(Err(message))
}

fn outcome(result: Result<Value, String>) -> Value {
    let (key, value, success) = match result {
        Ok(value) => ("result", value, true),
        Err(message) => ("error", Value::string(message), false),
    };
    Value::map([
        (Value::string("success"), Value::Boolean(success)),
        (Value::string(key), value),
    ])
}

/// The database of this name. `default` always exists; an unknown name fails with the names
/// of the config, as `Did you mean` suggestions in `sqlExecute` and in nf-trino.
pub(crate) fn source(
    flavour: Flavour,
    name: &str,
    config: &Config,
    suggest: bool,
) -> Result<Source, Error> {
    let name = if name.is_empty() { "default" } else { name };
    let sources = sources(flavour, config)?;
    if let Some(found) = sources.iter().find(|source| source.name == name) {
        return Ok(found.clone());
    }
    if name == "default" {
        return Ok(fallback(name, config));
    }
    let names: Vec<&str> = sources.iter().map(|source| source.name.as_str()).collect();
    let mut message = format!("Unknown db name: {name}");
    if names.is_empty() {
        return Err(thrown(message));
    }
    if !suggest && flavour == Flavour::Sqldb {
        message.push_str(&format!(" - Available databases: {}", names.join(", ")));
        return Err(thrown(message));
    }
    let distance = |known: &str| edits(name, known);
    let best = names.iter().map(|known| distance(known)).min().unwrap_or(0);
    let closest: Vec<&str> = (names.iter().copied())
        .filter(|known| distance(known) == best)
        .collect();
    match closest.as_slice() {
        [one] => message.push_str(&format!(" - Did you mean: {one}?")),
        many => {
            message.push_str(" - Did you mean any of these?\n");
            for known in many {
                message.push_str(&format!("  {known}\n"));
            }
        }
    }
    Err(thrown(message))
}

/// The databases of `sql.db` in config order. A URL that is not a JDBC URL fails, and so
/// does, in nf-sqldb, a user or password that still names a secret.
fn sources(flavour: Flavour, config: &Config) -> Result<Vec<Source>, Error> {
    let entries = crate::nf_schema::entries(&config.get(&["sql", "db"]).unwrap_or(Value::Null));
    let mut sources = Vec::new();
    for (name, _) in entries {
        let source = fallback(&name, config);
        if !source.url.starts_with("jdbc:") {
            let url = &source.url;
            return Err(thrown(format!(
                "Invalid database JDBC connection url: {url}"
            )));
        }
        if flavour == Flavour::Sqldb {
            secret_check("user", source.user.as_deref())?;
            secret_check("password", source.password.as_deref())?;
        }
        sources.push(source);
    }
    Ok(sources)
}

/// The settings of a database, else those of `default`, else H2's in-memory database.
fn fallback(name: &str, config: &Config) -> Source {
    let setting = |entry: &str, key: &str| match config.get(&["sql", "db", entry, key]) {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.to_string()).filter(|text| !text.is_empty()),
    };
    let either = |key: &str| setting(name, key).or_else(|| setting("default", key));
    Source {
        name: name.to_owned(),
        url: either("url").unwrap_or_else(|| "jdbc:h2:mem:".to_owned()),
        user: either("user").or_else(|| Some("sa".to_owned())),
        password: either("password"),
    }
}

/// Fails for a value that still looks like a secret reference.
fn secret_check(key: &str, value: Option<&str>) -> Result<(), Error> {
    let Some(value) = value else { return Ok(()) };
    let named = value.contains("secret") && value.contains('[') && value.contains(']');
    if !value.starts_with("secrets.") && !named {
        return Ok(());
    }
    Err(thrown(format!(
        "Unresolved secret detected for {key}: '{value}'. This typically indicates that \
         workspace secrets are not properly configured or accessible. Please verify that:\n\
         1. The secret is defined in your workspace/user secrets\n\
         2. The secret name matches exactly (case-sensitive)\n\
         3. You have proper permissions to access the secret\n\
         4. The Nextflow version supports secrets integration (>=25.04.0)\n\
         See: https://www.nextflow.io/docs/latest/secrets.html"
    )))
}

/// The edit distance of two names.
fn edits(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, a) in left.chars().enumerate() {
        let mut current = vec![row + 1];
        for (column, b) in right.iter().enumerate() {
            let cost = usize::from(a != *b);
            let best = (previous[column] + cost)
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
            current.push(best);
        }
        previous = current;
    }
    previous[right.len()]
}

/// The kinds of option values that the plugin checks.
#[derive(Clone, Copy)]
pub(crate) enum Type {
    Text,
    Integer,
    Boolean,
    TextOrList,
}

/// Fails for an option the operator does not know, or a value of the wrong kind, with the
/// messages of Nextflow's option checker.
pub(crate) fn check_options(
    operator: &str,
    options: &[(String, Value)],
    types: &[(&str, Type)],
) -> Result<(), Error> {
    for (name, value) in options {
        let Some((_, kind)) = types.iter().find(|(known, _)| known == name) else {
            let known: Vec<&str> = types.iter().map(|(known, _)| *known).collect();
            return Err(thrown(format!(
                "Unknown argument '{name}' for operator '{operator}' -- Possible arguments: {}",
                known.join(", ")
            )));
        };
        let text = matches!(value, Value::String(_) | Value::GString(_));
        let fits = match kind {
            Type::Text => text,
            Type::Integer => {
                matches!(value, Value::Number(Number::Integer(_) | Number::Long(_)))
            }
            Type::Boolean => matches!(value, Value::Boolean(_)),
            Type::TextOrList => text || matches!(value, Value::List(_)),
        };
        if fits || matches!(value, Value::Null) {
            continue;
        }
        let expected = match kind {
            Type::Text => "Value don't match: interface java.lang.CharSequence",
            Type::Integer => "Value don't match: class java.lang.Integer",
            Type::Boolean => "Value don't match: class java.lang.Boolean",
            Type::TextOrList => {
                "Possible values: interface java.lang.CharSequence, interface java.util.List"
            }
        };
        return Err(thrown(format!(
            "Value '{value}' cannot be used in in parameter '{name}' for operator \
             '{operator}' -- {expected}"
        )));
    }
    Ok(())
}

/// The named options, which come first or last, and the other arguments.
pub(crate) fn split_options(arguments: Vec<Value>) -> (Vec<(String, Value)>, Vec<Value>) {
    let mut options = Vec::new();
    let mut rest = Vec::new();
    for argument in arguments {
        match argument {
            Value::Map(_) if options.is_empty() => {
                options = crate::nf_schema::entries(&argument);
            }
            other => rest.push(other),
        }
    }
    (options, rest)
}

pub(crate) fn option(options: &[(String, Value)], name: &str) -> Option<Value> {
    let found = options.iter().find(|(key, _)| key == name);
    found
        .map(|(_, value)| value.clone())
        .filter(|value| !matches!(value, Value::Null))
}

pub(crate) fn option_text(options: &[(String, Value)], name: &str) -> String {
    option(options, name)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

pub(crate) fn option_number(options: &[(String, Value)], name: &str) -> Option<i64> {
    match option(options, name)? {
        Value::Number(Number::Integer(value)) => Some(i64::from(value)),
        Value::Number(Number::Long(value)) => Some(value),
        _ => None,
    }
}

fn names(columns: &[String]) -> Value {
    Value::list(
        columns
            .iter()
            .map(|name| Value::string(name.as_str()))
            .collect(),
    )
}

fn row_value(row: Vec<Datum>) -> Value {
    Value::list(row.into_iter().map(value).collect())
}

/// The uwe value of a database value.
pub(crate) fn value(datum: Datum) -> Value {
    match datum {
        Datum::Null => Value::Null,
        Datum::Boolean(value) => Value::Boolean(value),
        Datum::Integer(value) => Value::Number(Number::Integer(value)),
        Datum::Long(value) => Value::Number(Number::Long(value)),
        Datum::Float(value) => Value::Number(Number::Float(value)),
        Datum::Double(value) => Value::Number(Number::Double(value)),
        Datum::Decimal(text) => match BigDecimal::parse(&text) {
            Some(decimal) => Value::Number(Number::BigDecimal(decimal)),
            None => Value::string(text),
        },
        Datum::Text(text) => Value::string(text),
        Datum::Bytes(bytes) => object("[B", ObjectState::Bytes(bytes.into())),
    }
}

/// The database value of a uwe value, for an insert parameter.
pub(crate) fn datum(value: &Value) -> Datum {
    match value {
        Value::Null => Datum::Null,
        Value::Boolean(value) => Datum::Boolean(*value),
        Value::Number(Number::Integer(value)) => Datum::Integer(*value),
        Value::Number(Number::Long(value)) => Datum::Long(*value),
        Value::Number(Number::Float(value)) => Datum::Float(*value),
        Value::Number(Number::Double(value)) => Datum::Double(*value),
        Value::Number(number) => Datum::Decimal(number.to_string()),
        Value::Object(object) => match &object.borrow().state {
            ObjectState::Bytes(bytes) => Datum::Bytes(bytes.to_vec()),
            _ => Datum::Text(value.to_string()),
        },
        other => Datum::Text(other.to_string()),
    }
}

fn integer(count: i64) -> Value {
    match i32::try_from(count) {
        Ok(count) => Value::Number(Number::Integer(count)),
        Err(_) => Value::Number(Number::Long(count)),
    }
}

fn object(class: &'static str, state: ObjectState) -> Value {
    Value::Object(Rc::new(std::cell::RefCell::new(Object { class, state })))
}

pub(crate) fn thrown(message: impl Into<String>) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}

//! The SQLite connection of nf-sqldb, with the values that sqlite-jdbc gives.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use rusqlite::types::{Value as Stored, ValueRef};
use rusqlite::{OpenFlags, params_from_iter};

use super::{Connection, Datum, Table};

pub(crate) struct Sqlite(rusqlite::Connection);

/// Opens a file, a `file:` URI, or an in-memory database for `None`.
pub(crate) fn open(place: Option<&str>) -> Result<Sqlite, String> {
    let flags = OpenFlags::default() | OpenFlags::SQLITE_OPEN_URI;
    let opened = match place {
        None => rusqlite::Connection::open_in_memory(),
        Some(place) => rusqlite::Connection::open_with_flags(place, flags),
    };
    opened.map(Sqlite).map_err(|error| error.to_string())
}

impl Connection for Sqlite {
    fn query(&mut self, sql: &str) -> Result<Table, String> {
        let mut statement = self.0.prepare(sql).map_err(text)?;
        let columns: Vec<String> = (statement.column_names()).map(str::to_owned).collect();
        let width = columns.len();
        let mut rows = Vec::new();
        let mut found = statement.query([]).map_err(text)?;
        while let Some(row) = found.next().map_err(text)? {
            let mut values = Vec::with_capacity(width);
            for index in 0..width {
                values.push(datum(row.get_ref(index).map_err(text)?));
            }
            rows.push(values);
        }
        Ok(Table { columns, rows })
    }

    fn execute(&mut self, sql: &str) -> Result<u64, String> {
        let mut statement = self.0.prepare(sql).map_err(text)?;
        if statement.column_count() > 0 {
            let mut rows = statement.raw_query();
            while rows.next().map_err(text)?.is_some() {}
            return Ok(0);
        }
        let changed = statement.raw_execute().map_err(text)?;
        Ok(u64::try_from(changed).unwrap_or(0))
    }

    fn columns(&mut self, table: &str) -> Result<Vec<String>, String> {
        let sql = "SELECT name FROM pragma_table_info(?1)";
        let mut statement = self.0.prepare(sql).map_err(text)?;
        let names = statement.query_map([table], |row| row.get::<_, String>(0));
        names
            .map_err(text)?
            .map(|name| name.map_err(text))
            .collect()
    }

    fn insert(&mut self, statement: &str, rows: &[Vec<Datum>]) -> Result<(), String> {
        let transaction = self.0.transaction().map_err(text)?;
        {
            let mut prepared = transaction.prepare(statement).map_err(text)?;
            let count = prepared.parameter_count();
            for row in rows {
                if row.len() > count {
                    return Err(format!("Index {count} out of bounds for length {count}"));
                }
                let values = (0..count).map(|index| stored(row.get(index)));
                prepared.execute(params_from_iter(values)).map_err(text)?;
            }
        }
        transaction.commit().map_err(text)
    }
}

/// The value as sqlite-jdbc gives it: integers that fit as `Integer`, else `Long`; dates and
/// booleans as they are stored.
fn datum(value: ValueRef<'_>) -> Datum {
    match value {
        ValueRef::Null => Datum::Null,
        ValueRef::Integer(value) => match i32::try_from(value) {
            Ok(small) => Datum::Integer(small),
            Err(_) => Datum::Long(value),
        },
        ValueRef::Real(value) => Datum::Double(value),
        ValueRef::Text(bytes) => Datum::Text(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Datum::Bytes(bytes.to_vec()),
    }
}

/// A parameter; a missing one is null.
fn stored(value: Option<&Datum>) -> Stored {
    match value {
        None | Some(Datum::Null) => Stored::Null,
        Some(Datum::Boolean(value)) => Stored::Integer(i64::from(*value)),
        Some(Datum::Integer(value)) => Stored::Integer(i64::from(*value)),
        Some(Datum::Long(value)) => Stored::Integer(*value),
        Some(Datum::Float(value)) => Stored::Real(f64::from(*value)),
        Some(Datum::Double(value)) => Stored::Real(*value),
        Some(Datum::Bytes(bytes)) => Stored::Blob(bytes.clone()),
        Some(Datum::Decimal(text) | Datum::Text(text)) => Stored::Text(text.clone()),
    }
}

/// The message as sqlite-jdbc words its two recorded result codes, else SQLite's own.
fn text(error: rusqlite::Error) -> String {
    let rusqlite::Error::SqliteFailure(failure, message) = error else {
        return error.to_string();
    };
    let detail = message.unwrap_or_else(|| failure.to_string());
    match failure.extended_code & 0xff {
        1 => format!("[SQLITE_ERROR] SQL error or missing database ({detail})"),
        5 => format!("[SQLITE_BUSY] The database file is locked ({detail})"),
        _ => detail,
    }
}

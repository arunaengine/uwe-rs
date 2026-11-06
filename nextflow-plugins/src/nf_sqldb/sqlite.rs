//! The SQLite connection of nf-sqldb, with the values that sqlite-jdbc gives.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use rusqlite::OpenFlags;
use rusqlite::types::ValueRef;

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

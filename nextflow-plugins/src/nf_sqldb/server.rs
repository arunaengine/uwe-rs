//! PostgreSQL and MySQL or MariaDB connections over sqlx, with JDBC's mapping of their values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::future::Future;

use sqlx_core::column::Column;
use sqlx_core::connection::{ConnectOptions, Connection as _};
use sqlx_core::executor::Executor;
use sqlx_core::query::query;
use sqlx_core::query_scalar::query_scalar;
use sqlx_core::raw_sql::raw_sql;
use sqlx_core::row::Row;
use sqlx_core::sql_str::{AssertSqlSafe, SqlSafeStr};
use sqlx_core::statement::Statement;
use sqlx_core::type_info::TypeInfo;
use sqlx_core::value::ValueRef;
use sqlx_mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow, MySqlSslMode};
use sqlx_postgres::{PgConnectOptions, PgConnection, PgRow, PgSslMode};

use super::{Connection, Datum, Table};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    Postgres,
    /// MySQL and MariaDB, which share the protocol.
    Mysql,
}

/// How a connection uses TLS, from the URL's parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tls {
    Disable,
    Prefer,
    Require,
    /// TLS with a checked certificate and host name.
    Verify,
}

/// A database server and the account of a connection.
#[derive(Clone, PartialEq)]
pub(crate) struct Server {
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub tls: Tls,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (f.debug_struct("Server"))
            .field("engine", &self.engine)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// An open connection with the runtime that drives it.
pub(crate) struct Open {
    runtime: tokio::runtime::Runtime,
    link: Link,
}

enum Link {
    Postgres(PgConnection),
    Mysql(MySqlConnection),
}

pub(crate) fn open(server: Server) -> Result<Open, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let link = wait(&runtime, async move {
        match server.engine {
            Engine::Postgres => {
                let mut options = PgConnectOptions::new_without_pgpass()
                    .host(&server.host)
                    .port(server.port)
                    .ssl_mode(match server.tls {
                        Tls::Disable => PgSslMode::Disable,
                        Tls::Prefer => PgSslMode::Prefer,
                        Tls::Require => PgSslMode::Require,
                        Tls::Verify => PgSslMode::VerifyFull,
                    });
                if let Some(user) = &server.user {
                    options = options.username(user);
                }
                if let Some(password) = &server.password {
                    options = options.password(password);
                }
                if let Some(database) = &server.database {
                    options = options.database(database);
                }
                options.connect().await.map(Link::Postgres)
            }
            Engine::Mysql => {
                let mut options = MySqlConnectOptions::new()
                    .host(&server.host)
                    .port(server.port)
                    .ssl_mode(match server.tls {
                        Tls::Disable => MySqlSslMode::Disabled,
                        Tls::Prefer => MySqlSslMode::Preferred,
                        Tls::Require => MySqlSslMode::Required,
                        Tls::Verify => MySqlSslMode::VerifyIdentity,
                    });
                if let Some(user) = &server.user {
                    options = options.username(user);
                }
                if let Some(password) = &server.password {
                    options = options.password(password);
                }
                if let Some(database) = &server.database {
                    options = options.database(database);
                }
                options.connect().await.map(Link::Mysql)
            }
        }
    })?
    .map_err(|error| error.to_string())?;
    Ok(Open { runtime, link })
}

/// Runs a future on the connection's runtime in a thread of its own, so that it works also
/// where the caller runs inside another runtime.
fn wait<T: Send>(
    runtime: &tokio::runtime::Runtime,
    future: impl Future<Output = T> + Send,
) -> Result<T, String> {
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| runtime.block_on(future));
        handle
            .join()
            .map_err(|_| "the database client stopped".to_owned())
    })
}

impl Connection for Open {
    fn query(&mut self, sql: &str) -> Result<Table, String> {
        let Open { runtime, link } = self;
        let found = wait(runtime, async move {
            match link {
                Link::Postgres(connection) => {
                    let prepared = connection
                        .prepare(AssertSqlSafe(sql).into_sql_str())
                        .await?;
                    let columns = (prepared.columns().iter())
                        .map(|column| column.name().to_owned())
                        .collect();
                    let rows = prepared.query().fetch_all(&mut *connection).await?;
                    let rows = rows.iter().map(postgres_row).collect();
                    Ok::<_, sqlx_core::Error>((columns, rows))
                }
                Link::Mysql(connection) => {
                    let prepared = connection
                        .prepare(AssertSqlSafe(sql).into_sql_str())
                        .await?;
                    let columns = (prepared.columns().iter())
                        .map(|column| column.name().to_owned())
                        .collect();
                    let rows = prepared.query().fetch_all(&mut *connection).await?;
                    let rows = rows.iter().map(mysql_row).collect();
                    Ok((columns, rows))
                }
            }
        })?;
        let (columns, rows): (Vec<String>, Vec<Result<Vec<Datum>, String>>) =
            found.map_err(message)?;
        let rows = rows.into_iter().collect::<Result<_, _>>()?;
        Ok(Table { columns, rows })
    }

    fn execute(&mut self, sql: &str) -> Result<u64, String> {
        let Open { runtime, link } = self;
        let changed = wait(runtime, async move {
            match link {
                Link::Postgres(connection) => {
                    let done = raw_sql(AssertSqlSafe(sql))
                        .execute(&mut *connection)
                        .await?;
                    Ok::<_, sqlx_core::Error>(done.rows_affected())
                }
                Link::Mysql(connection) => {
                    let done = raw_sql(AssertSqlSafe(sql))
                        .execute(&mut *connection)
                        .await?;
                    Ok(done.rows_affected())
                }
            }
        })?;
        changed.map_err(message)
    }

    fn columns(&mut self, table: &str) -> Result<Vec<String>, String> {
        let Open { runtime, link } = self;
        let names = wait(runtime, async move {
            match link {
                Link::Postgres(connection) => {
                    let sql = "SELECT column_name::text FROM information_schema.columns WHERE \
                               table_name = $1 AND table_schema = current_schema() ORDER BY \
                               ordinal_position";
                    let query = query_scalar::<_, String>(sql).bind(table);
                    query.fetch_all(&mut *connection).await
                }
                Link::Mysql(connection) => {
                    let sql = "SELECT CAST(column_name AS CHAR) FROM information_schema.columns \
                               WHERE table_name = ? AND table_schema = DATABASE() ORDER BY \
                               ordinal_position";
                    let query = query_scalar::<_, String>(sql).bind(table);
                    query.fetch_all(&mut *connection).await
                }
            }
        })?;
        names.map_err(message)
    }

    fn insert(&mut self, statement: &str, rows: &[Vec<Datum>]) -> Result<(), String> {
        let Open { runtime, link } = self;
        let done = wait(runtime, async move {
            match link {
                Link::Postgres(connection) => {
                    let mut transaction = connection.begin().await?;
                    for row in rows {
                        let mut query = query(AssertSqlSafe(numbered(statement)));
                        for value in row {
                            query = match value.clone() {
                                Datum::Null | Datum::List(_) => query.bind(None::<i32>),
                                Datum::Boolean(value) => query.bind(value),
                                Datum::Integer(value) => query.bind(value),
                                Datum::Long(value) => query.bind(value),
                                Datum::Float(value) => query.bind(value),
                                Datum::Double(value) => query.bind(value),
                                Datum::Bytes(value) => query.bind(value),
                                Datum::Decimal(text)
                                | Datum::Text(text)
                                | Datum::Typed(_, text) => query.bind(text),
                            };
                        }
                        query.execute(&mut *transaction).await?;
                    }
                    transaction.commit().await
                }
                Link::Mysql(connection) => {
                    let mut transaction = connection.begin().await?;
                    for row in rows {
                        let mut query = query(AssertSqlSafe(statement));
                        for value in row {
                            query = match value.clone() {
                                Datum::Null | Datum::List(_) => query.bind(None::<String>),
                                Datum::Boolean(value) => query.bind(value),
                                Datum::Integer(value) => query.bind(value),
                                Datum::Long(value) => query.bind(value),
                                Datum::Float(value) => query.bind(value),
                                Datum::Double(value) => query.bind(value),
                                Datum::Bytes(value) => query.bind(value),
                                Datum::Decimal(text)
                                | Datum::Text(text)
                                | Datum::Typed(_, text) => query.bind(text),
                            };
                        }
                        query.execute(&mut *transaction).await?;
                    }
                    transaction.commit().await
                }
            }
        })?;
        done.map_err(message)
    }
}

/// The statement with PostgreSQL's numbered placeholders for JDBC's `?` outside quotes.
fn numbered(statement: &str) -> String {
    let (mut text, mut count, mut quote) = (String::new(), 0, None);
    for c in statement.chars() {
        match (c, quote) {
            ('\'' | '"', None) => quote = Some(c),
            (c, Some(open)) if c == open => quote = None,
            ('?', None) => {
                count += 1;
                text.push_str(&format!("${count}"));
                continue;
            }
            _ => {}
        }
        text.push(c);
    }
    text
}

/// The message of the server, without sqlx's wording around it.
fn message(error: sqlx_core::Error) -> String {
    match error {
        sqlx_core::Error::Database(error) => error.message().to_owned(),
        other => other.to_string(),
    }
}

/// A PostgreSQL row with the classes of the PostgreSQL JDBC driver.
fn postgres_row(row: &PgRow) -> Result<Vec<Datum>, String> {
    let mut values = Vec::with_capacity(row.len());
    for (index, column) in row.columns().iter().enumerate() {
        let failed = |error: sqlx_core::Error| format!("column {}: {error}", column.name());
        let raw = row.try_get_raw(index).map_err(failed)?;
        if raw.is_null() {
            values.push(Datum::Null);
            continue;
        }
        let kind = column.type_info().name().to_owned();
        let get_text = || row.try_get_unchecked::<String, _>(index).map_err(failed);
        let value = match kind.as_str() {
            "BOOL" => Datum::Boolean(row.try_get(index).map_err(failed)?),
            "INT2" => Datum::Integer(i32::from(row.try_get::<i16, _>(index).map_err(failed)?)),
            "INT4" => Datum::Integer(row.try_get(index).map_err(failed)?),
            "INT8" => Datum::Long(row.try_get(index).map_err(failed)?),
            "FLOAT4" => Datum::Float(row.try_get(index).map_err(failed)?),
            "FLOAT8" => Datum::Double(row.try_get(index).map_err(failed)?),
            "NUMERIC" => Datum::Decimal(numeric(raw.as_bytes().map_err(|e| e.to_string())?)?),
            "BYTEA" => Datum::Bytes(row.try_get(index).map_err(failed)?),
            "DATE" => {
                let date: chrono::NaiveDate = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Date", date.format("%Y-%m-%d").to_string())
            }
            "TIMESTAMP" => {
                let time: chrono::NaiveDateTime = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Timestamp", timestamp(time))
            }
            "TIMESTAMPTZ" => {
                let time: chrono::DateTime<chrono::Utc> = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Timestamp", timestamp(time.naive_utc()))
            }
            "TIME" => {
                let time: chrono::NaiveTime = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Time", time.format("%H:%M:%S").to_string())
            }
            "JSON" | "JSONB" => {
                let json: serde_json::Value = row.try_get(index).map_err(failed)?;
                Datum::Text(json.to_string())
            }
            "UUID" => Datum::Typed(
                "java.util.UUID",
                uuid(raw.as_bytes().map_err(|e| e.to_string())?),
            ),
            "INT2[]" | "INT4[]" => {
                let items: Vec<Option<i32>> = row.try_get(index).map_err(failed)?;
                Datum::List(
                    items
                        .into_iter()
                        .map(|item| item.map_or(Datum::Null, Datum::Integer))
                        .collect(),
                )
            }
            "INT8[]" => {
                let items: Vec<Option<i64>> = row.try_get(index).map_err(failed)?;
                Datum::List(
                    items
                        .into_iter()
                        .map(|item| item.map_or(Datum::Null, Datum::Long))
                        .collect(),
                )
            }
            "FLOAT8[]" => {
                let items: Vec<Option<f64>> = row.try_get(index).map_err(failed)?;
                Datum::List(
                    items
                        .into_iter()
                        .map(|item| item.map_or(Datum::Null, Datum::Double))
                        .collect(),
                )
            }
            "TEXT[]" | "VARCHAR[]" => {
                let items: Vec<Option<String>> = row.try_get(index).map_err(failed)?;
                Datum::List(
                    items
                        .into_iter()
                        .map(|item| item.map_or(Datum::Null, Datum::Text))
                        .collect(),
                )
            }
            _ => Datum::Text(get_text().map_err(|_| {
                format!(
                    "uwe cannot read the PostgreSQL type {kind} of column {}",
                    column.name()
                )
            })?),
        };
        values.push(value);
    }
    Ok(values)
}

/// A MySQL or MariaDB row with the classes of MySQL Connector/J.
fn mysql_row(row: &MySqlRow) -> Result<Vec<Datum>, String> {
    let mut values = Vec::with_capacity(row.len());
    for (index, column) in row.columns().iter().enumerate() {
        let failed = |error: sqlx_core::Error| format!("column {}: {error}", column.name());
        let raw = row.try_get_raw(index).map_err(failed)?;
        if raw.is_null() {
            values.push(Datum::Null);
            continue;
        }
        let kind = column.type_info().name().to_owned();
        let whole = |row: &MySqlRow| row.try_get::<i64, _>(index).map_err(failed);
        let value = match kind.as_str() {
            "BOOLEAN" => Datum::Boolean(row.try_get(index).map_err(failed)?),
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "YEAR" => {
                let value = whole(row)?;
                i32::try_from(value).map_or(Datum::Long(value), Datum::Integer)
            }
            "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" => Datum::Integer(
                i32::try_from(row.try_get::<u32, _>(index).map_err(failed)?).unwrap_or(i32::MAX),
            ),
            "INT UNSIGNED" => Datum::Long(i64::from(row.try_get::<u32, _>(index).map_err(failed)?)),
            "BIGINT" => Datum::Long(whole(row)?),
            "BIGINT UNSIGNED" => {
                let value = row.try_get::<u64, _>(index).map_err(failed)?;
                i64::try_from(value).map_or(Datum::Decimal(value.to_string()), Datum::Long)
            }
            "FLOAT" => Datum::Float(row.try_get(index).map_err(failed)?),
            "DOUBLE" => Datum::Double(row.try_get(index).map_err(failed)?),
            "DECIMAL" => Datum::Decimal(row.try_get_unchecked(index).map_err(failed)?),
            "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" | "BIT" => {
                Datum::Bytes(row.try_get_unchecked(index).map_err(failed)?)
            }
            "DATE" => {
                let date: chrono::NaiveDate = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Date", date.format("%Y-%m-%d").to_string())
            }
            "DATETIME" => {
                let time: chrono::NaiveDateTime = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Timestamp", timestamp(time))
            }
            "TIMESTAMP" => {
                let time: chrono::DateTime<chrono::Utc> = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Timestamp", timestamp(time.naive_utc()))
            }
            "TIME" => {
                let time: chrono::NaiveTime = row.try_get(index).map_err(failed)?;
                Datum::Typed("java.sql.Time", time.format("%H:%M:%S").to_string())
            }
            _ => Datum::Text(row.try_get_unchecked(index).map_err(|_| {
                format!(
                    "uwe cannot read the MySQL type {kind} of column {}",
                    column.name()
                )
            })?),
        };
        values.push(value);
    }
    Ok(values)
}

/// The text of `java.sql.Timestamp`: the fraction as stored, at least one digit.
fn timestamp(time: chrono::NaiveDateTime) -> String {
    let base = time.format("%Y-%m-%d %H:%M:%S").to_string();
    let nanos = format!("{:09}", chrono::Timelike::nanosecond(&time) % 1_000_000_000);
    let fraction = nanos.trim_end_matches('0');
    format!(
        "{base}.{}",
        if fraction.is_empty() { "0" } else { fraction }
    )
}

/// The text of a PostgreSQL `numeric` in its binary form: digit count, weight of the first
/// base-10000 digit, sign and display scale, then the digits.
fn numeric(bytes: &[u8]) -> Result<String, String> {
    let word = |at: usize| match bytes.get(at..at + 2) {
        Some(pair) => Ok(i16::from_be_bytes([pair[0], pair[1]])),
        None => Err("a short numeric value".to_owned()),
    };
    let (count, weight, sign, scale) = (word(0)?, word(2)?, word(4)?, word(6)?);
    let negative = match sign as u16 {
        0x0000 => false,
        0x4000 => true,
        0xD000 => return Ok("Infinity".to_owned()),
        0xF000 => return Ok("-Infinity".to_owned()),
        _ => return Ok("NaN".to_owned()),
    };
    let digits: Vec<i16> = (0..usize::try_from(count).unwrap_or(0))
        .map(|index| word(8 + 2 * index))
        .collect::<Result<_, _>>()?;
    let digit = |at: i32| {
        let found = usize::try_from(at).ok().and_then(|at| digits.get(at));
        found.copied().unwrap_or(0)
    };
    let mut whole = String::new();
    for at in 0..=i32::from(weight) {
        match whole.is_empty() {
            true => whole = digit(at).to_string(),
            false => whole.push_str(&format!("{:04}", digit(at))),
        }
    }
    if whole.is_empty() {
        whole.push('0');
    }
    let scale = usize::try_from(scale).unwrap_or(0);
    let mut fraction = String::new();
    let mut group = 1;
    while fraction.len() < scale {
        fraction.push_str(&format!("{:04}", digit(i32::from(weight) + group)));
        group += 1;
    }
    fraction.truncate(scale);
    let sign = if negative { "-" } else { "" };
    Ok(match fraction.is_empty() {
        true => format!("{sign}{whole}"),
        false => format!("{sign}{whole}.{fraction}"),
    })
}

fn uuid(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    match hex.len() {
        32 => format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        ),
        _ => hex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(words: &[i16]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_be_bytes()).collect()
    }

    #[test]
    fn reads_numeric() {
        // 12.34: two digits 12 and 3400, weight 0, scale 2.
        assert_eq!(
            numeric(&encoded(&[2, 0, 0, 2, 12, 3400])),
            Ok("12.34".into())
        );
        // -0.01: one digit 100 at weight -1, scale 2.
        assert_eq!(
            numeric(&encoded(&[1, -1, 0x4000, 2, 100])),
            Ok("-0.01".into())
        );
        // 123456789: digits 1, 2345, 6789 at weight 2.
        assert_eq!(
            numeric(&encoded(&[3, 2, 0, 0, 1, 2345, 6789])),
            Ok("123456789".into())
        );
        // 0.00005: digit 5000 at weight -2, scale 5.
        assert_eq!(
            numeric(&encoded(&[1, -2, 0, 5, 5000])),
            Ok("0.00005".into())
        );
        assert_eq!(numeric(&encoded(&[0, 0, 0, 0])), Ok("0".into()));
    }

    #[test]
    fn numbers_placeholders() {
        let statement = "insert into t ( a,b ) values ( ?,'?' || ? )";
        assert_eq!(
            numbered(statement),
            "insert into t ( a,b ) values ( $1,'?' || $2 )"
        );
    }

    #[test]
    fn formats_timestamps() {
        let time = chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
            .and_then(|day| day.and_hms_milli_opt(3, 4, 5, 120))
            .expect("a time");
        assert_eq!(timestamp(time), "2024-01-02 03:04:05.12");
        let whole = chrono::Timelike::with_nanosecond(&time, 0).expect("a time");
        assert_eq!(timestamp(whole), "2024-01-02 03:04:05.0");
    }
}

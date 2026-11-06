//! PostgreSQL connections over sqlx, with JDBC's mapping of their values.
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
use sqlx_postgres::{PgConnectOptions, PgConnection, PgRow, PgSslMode};

use super::{Connection, Datum, Table};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    Postgres,
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
                                Datum::Null => query.bind(None::<i32>),
                                Datum::Boolean(value) => query.bind(value),
                                Datum::Integer(value) => query.bind(value),
                                Datum::Long(value) => query.bind(value),
                                Datum::Float(value) => query.bind(value),
                                Datum::Double(value) => query.bind(value),
                                Datum::Bytes(value) => query.bind(value),
                                Datum::Decimal(text) | Datum::Text(text) => query.bind(text),
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
            "BYTEA" => Datum::Bytes(row.try_get(index).map_err(failed)?),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_placeholders() {
        let statement = "insert into t ( a,b ) values ( ?,'?' || ? )";
        assert_eq!(
            numbered(statement),
            "insert into t ( a,b ) values ( $1,'?' || $2 )"
        );
    }
}

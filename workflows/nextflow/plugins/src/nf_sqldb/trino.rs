//! Trino's HTTP client protocol for `jdbc:trino:` URLs: a statement, then its `nextUri` pages.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use base64::Engine;
use serde_json::Value as Json;

use super::{Connection, Datum, Table, literal};

/// A Trino coordinator and the session of its requests.
#[derive(Clone, PartialEq)]
pub(crate) struct Trino {
    /// `http://host:port` or `https://host:port`.
    pub base: String,
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for Trino {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trino")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// The coordinator of `//host[:port][/catalog[/schema]][?key=value&...]`, with the user and
/// password of the config, which win over those of the URL.
pub(crate) fn coordinator(
    address: &str,
    user: Option<String>,
    password: Option<String>,
) -> Result<Trino, String> {
    let rest = address.strip_prefix("//").unwrap_or(address);
    let (place, query) = rest.split_once('?').unwrap_or((rest, ""));
    let parameters: Vec<(&str, String)> = (query.split('&'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key, decoded(value))
        })
        .collect();
    let parameter = |name: &str| {
        let found = parameters.iter().find(|(key, _)| *key == name);
        found.map(|(_, value)| value.clone())
    };
    let mut parts = place.split('/');
    let authority = parts.next().unwrap_or_default();
    let tls = parameter("SSL").is_some_and(|value| value == "true");
    let scheme = if tls { "https" } else { "http" };
    let base = match authority.contains(':') {
        true => format!("{scheme}://{authority}"),
        false => format!("{scheme}://{authority}:{}", if tls { 443 } else { 80 }),
    };
    let user = user.or_else(|| parameter("user")).unwrap_or_default();
    let mut headers = vec![
        ("X-Trino-User".to_owned(), user.clone()),
        (
            "X-Trino-Source".to_owned(),
            parameter("source").unwrap_or("trino-jdbc".into()),
        ),
        (
            "Content-Type".to_owned(),
            "text/plain; charset=utf-8".to_owned(),
        ),
    ];
    let mut add = |name: &str, value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            headers.push((name.to_owned(), value));
        }
    };
    add("X-Trino-Catalog", parts.next().map(str::to_owned));
    add("X-Trino-Schema", parts.next().map(str::to_owned));
    add("X-Trino-Client-Tags", parameter("clientTags"));
    add("X-Trino-Time-Zone", parameter("timezone"));
    add("X-Trino-Role", parameter("roles"));
    let session =
        parameter("sessionProperties").map(|text| text.replace(';', ",").replace(':', "="));
    add("X-Trino-Session", session);
    let password = password.or_else(|| parameter("password"));
    if let Some(token) = parameter("accessToken") {
        add("Authorization", Some(format!("Bearer {token}")));
    } else if let Some(password) = password {
        if !tls {
            return Err("Trino needs SSL=true in the URL to send a password".to_owned());
        }
        let pair = format!("{user}:{password}");
        let encoded = base64::engine::general_purpose::STANDARD.encode(pair);
        add("Authorization", Some(format!("Basic {encoded}")));
    }
    Ok(Trino { base, headers })
}

impl Trino {
    /// Runs a statement to its end: the columns, the rows and the update count.
    fn run(&self, sql: &str) -> Result<(Table, Option<u64>), String> {
        let url = format!("{}/v1/statement", self.base);
        let mut answer = self.send("POST", &url, Some(sql.as_bytes().to_vec()))?;
        let mut table = Table::default();
        let mut kinds: Vec<String> = Vec::new();
        loop {
            if let Some(error) = answer.get("error") {
                let message = error["message"].as_str().unwrap_or("the query failed");
                return Err(message.to_owned());
            }
            if kinds.is_empty()
                && let Some(columns) = answer["columns"].as_array()
            {
                for column in columns {
                    table
                        .columns
                        .push(column["name"].as_str().unwrap_or_default().to_owned());
                    kinds.push(column["type"].as_str().unwrap_or_default().to_owned());
                }
            }
            for row in answer["data"].as_array().into_iter().flatten() {
                let cells = row.as_array().into_iter().flatten();
                let values = cells.zip(&kinds).map(|(cell, kind)| datum(cell, kind));
                table.rows.push(values.collect());
            }
            let Some(next) = answer["nextUri"].as_str().map(str::to_owned) else {
                return Ok((table, answer["updateCount"].as_u64()));
            };
            // The credentials go with every page, so only to the coordinator itself.
            if !crate::service::same_origin(&self.base, &next) {
                let shown = crate::service::safe_url(&next);
                return Err(format!(
                    "Trino named a next page on another address, {shown}, which uwe does not \
                     send the credentials to"
                ));
            }
            answer = self.send("GET", &next, None)?;
        }
    }

    fn send(&self, method: &str, url: &str, body: Option<Vec<u8>>) -> Result<Json, String> {
        let answer = storage::exchange(method, url, &self.headers, body)
            .map_err(|error| error.to_string())?;
        if answer.status == 503 {
            return Err("the Trino coordinator is busy (503)".to_owned());
        }
        match serde_json::from_slice(&answer.body) {
            Ok(json) if answer.status < 300 => Ok(json),
            _ => Err(format!(
                "Trino answered {} {}: {}",
                answer.status,
                answer.reason,
                String::from_utf8_lossy(&answer.body).trim()
            )),
        }
    }
}

impl Connection for Trino {
    fn query(&mut self, sql: &str) -> Result<Table, String> {
        self.run(sql).map(|(table, _)| table)
    }

    fn execute(&mut self, sql: &str) -> Result<u64, String> {
        self.run(sql).map(|(_, count)| count.unwrap_or(0))
    }

    fn columns(&mut self, table: &str) -> Result<Vec<String>, String> {
        let (found, _) = self.run(&format!("SHOW COLUMNS FROM {table}"))?;
        let names = found
            .rows
            .into_iter()
            .filter_map(|row| match row.into_iter().next() {
                Some(Datum::Text(name)) => Some(name),
                _ => None,
            });
        Ok(names.collect())
    }

    /// One statement per row with its values as literals, since Trino has no batches.
    fn insert(&mut self, statement: &str, rows: &[Vec<Datum>]) -> Result<(), String> {
        for row in rows {
            self.run(&literal(statement, row)?)?;
        }
        Ok(())
    }

    fn page(&self, size: i64, offset: i64) -> String {
        format!("OFFSET {offset} LIMIT {size}")
    }
}

/// A value of Trino's JSON encoding, by the column type.
pub(crate) fn datum(cell: &Json, kind: &str) -> Datum {
    if cell.is_null() {
        return Datum::Null;
    }
    let base = kind.split(['(', ' ']).next().unwrap_or_default();
    let text = || match cell {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    };
    match base {
        "boolean" => Datum::Boolean(cell.as_bool().unwrap_or(false)),
        "tinyint" | "smallint" | "integer" => {
            let value = cell.as_i64().unwrap_or(0);
            i32::try_from(value).map_or(Datum::Long(value), Datum::Integer)
        }
        "bigint" => Datum::Long(cell.as_i64().unwrap_or(0)),
        "real" => Datum::Float(
            cell.as_f64()
                .map_or_else(|| special(&text()) as f32, |v| v as f32),
        ),
        "double" => Datum::Double(cell.as_f64().unwrap_or_else(|| special(&text()))),
        "decimal" => Datum::Decimal(text()),
        "date" => Datum::Typed("java.sql.Date", text()),
        "timestamp" => Datum::Typed("java.sql.Timestamp", text()),
        "time" => Datum::Typed("java.sql.Time", text()),
        "varbinary" => match base64::engine::general_purpose::STANDARD.decode(text()) {
            Ok(bytes) => Datum::Bytes(bytes),
            Err(_) => Datum::Text(text()),
        },
        "array" => {
            let inner = kind
                .strip_prefix("array(")
                .and_then(|inner| inner.strip_suffix(')'))
                .unwrap_or_default();
            let items = cell.as_array().into_iter().flatten();
            Datum::List(items.map(|item| datum(item, inner)).collect())
        }
        _ => Datum::Text(text()),
    }
}

/// The double of Trino's texts for values that JSON has no number for.
fn special(text: &str) -> f64 {
    match text {
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => f64::NAN,
    }
}

/// The text of a percent-encoded URL parameter.
fn decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let pair = (bytes.get(index + 1..index + 3))
            .and_then(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok());
        match (bytes[index], pair) {
            (b'%', Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn reads_trino_values() {
        let cell = |text: &str| serde_json::from_str::<Json>(text).expect("JSON");
        assert_eq!(datum(&cell("7"), "integer"), Datum::Integer(7));
        assert_eq!(
            datum(&cell("\"12.30\""), "decimal(5,2)"),
            Datum::Decimal("12.30".into())
        );
        assert_eq!(
            datum(&cell("[1, null]"), "array(bigint)"),
            Datum::List(vec![Datum::Long(1), Datum::Null])
        );
        assert_eq!(
            datum(&cell("\"2024-01-02 03:04:05.123\""), "timestamp(3)"),
            Datum::Typed("java.sql.Timestamp", "2024-01-02 03:04:05.123".into())
        );
        assert_eq!(
            datum(&cell("\"AQI=\""), "varbinary"),
            Datum::Bytes(vec![1, 2])
        );
    }

    #[test]
    fn maps_trino_urls() {
        let found = coordinator(
            "//db:8080/hive/web?clientTags=a,b",
            Some("ann".into()),
            None,
        );
        let found = found.expect("a coordinator");
        assert_eq!(found.base, "http://db:8080");
        let header = |name: &str| {
            let found = found.headers.iter().find(|(key, _)| key == name);
            found.map(|(_, value)| value.as_str())
        };
        assert_eq!(header("X-Trino-User"), Some("ann"));
        assert_eq!(header("X-Trino-Catalog"), Some("hive"));
        assert_eq!(header("X-Trino-Schema"), Some("web"));
        assert_eq!(header("X-Trino-Client-Tags"), Some("a,b"));
        let refused = coordinator("//db:8080", Some("ann".into()), Some("pw".into()));
        assert!(refused.is_err());
        let secure = coordinator("//db?SSL=true", None, Some("pw".into())).expect("TLS");
        assert_eq!(secure.base, "https://db:443");
    }

    /// Answers one request on a free local port with `reply`, and marks when it does.
    fn serve(reply: String, heard: Arc<AtomicBool>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            heard.store(true, Ordering::SeqCst);
            let mut reader = BufReader::new(&stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let _ = reader.read_exact(&mut vec![0; length]);
            let text = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = (&stream).write_all(text.as_bytes());
        });
        address
    }

    #[test]
    fn stays_on_coordinator() {
        let elsewhere = Arc::new(AtomicBool::new(false));
        let other = serve(r#"{"id":"q1"}"#.to_owned(), elsewhere.clone());
        let next = format!(r#"{{"id":"q1","nextUri":"{other}/v1/statement/q1/1"}}"#);
        let trino = Trino {
            base: serve(next, Arc::new(AtomicBool::new(false))),
            headers: vec![("Authorization".into(), "Bearer not-a-real-token".into())],
        };
        let error = trino
            .run("SELECT 1")
            .expect_err("a page on another address fails");
        assert!(error.contains("another address"), "{error}");
        assert!(
            !elsewhere.load(Ordering::SeqCst),
            "the other address got a request"
        );
    }
}

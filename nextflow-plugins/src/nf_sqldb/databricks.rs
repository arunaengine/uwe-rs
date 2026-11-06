//! The Databricks SQL Statement Execution API for `jdbc:databricks:` URLs of SQL warehouses:
//! a statement, its polled state and its result chunks, with a personal access token.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use base64::Engine;
use serde_json::{Value as Json, json};

use super::{Connection, Datum, Table};

/// A SQL warehouse and the token, catalog and schema of its statements.
#[derive(Clone, PartialEq)]
pub(crate) struct Databricks {
    /// `https://host[:port]`, or `http://` with `ssl=0`.
    pub base: String,
    pub warehouse: String,
    pub token: String,
    pub catalog: Option<String>,
    pub schema: Option<String>,
}

impl std::fmt::Debug for Databricks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Databricks")
            .field("base", &self.base)
            .field("warehouse", &self.warehouse)
            .finish_non_exhaustive()
    }
}

/// The warehouse of `//host[:port][/schema];key=value;...`, with the password of the config as
/// the token, which wins over the URL's `PWD`.
pub(crate) fn warehouse(address: &str, password: Option<String>) -> Result<Databricks, String> {
    let mut parts = address.split(';');
    let place = parts.next().unwrap_or_default();
    let settings: Vec<(String, String)> = (parts.filter(|part| !part.is_empty()))
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (key.to_ascii_lowercase(), value.to_owned())
        })
        .collect();
    let setting = |name: &str| {
        let found = settings.iter().find(|(key, _)| key == name);
        found
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    };
    let rest = place.strip_prefix("//").unwrap_or(place);
    let (authority, path_schema) = rest.split_once('/').unwrap_or((rest, ""));
    let tls = setting("ssl").is_none_or(|ssl| ssl != "0");
    let base = match (tls, authority.split_once(':')) {
        (true, Some((host, "443"))) => format!("https://{host}"),
        (true, _) => format!("https://{authority}"),
        (false, _) => format!("http://{authority}"),
    };
    let path = setting("httppath").unwrap_or_default();
    let warehouse = (path.strip_prefix("/sql/1.0/warehouses/"))
        .or_else(|| path.strip_prefix("/sql/1.0/endpoints/"))
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .ok_or_else(|| {
            format!(
                "uwe reaches Databricks through its SQL Statement Execution API, which needs a SQL \
                 warehouse; the httpPath '{path}' is not one"
            )
        })?;
    if let Some(mechanism) = setting("authmech").filter(|mechanism| mechanism != "3") {
        return Err(format!(
            "uwe connects to Databricks with a personal access token (AuthMech=3), not with \
             AuthMech={mechanism}"
        ));
    }
    let token = (password.or_else(|| setting("pwd")))
        .ok_or("the Databricks URL needs a personal access token as its password")?;
    Ok(Databricks {
        base,
        warehouse: warehouse.to_owned(),
        token,
        catalog: setting("conncatalog"),
        schema: setting("connschema")
            .or_else(|| Some(path_schema.to_owned()).filter(|schema| !schema.is_empty())),
    })
}

impl Databricks {
    /// Runs a statement to its end: the columns, the rows and the rows it changed.
    fn run(&self, sql: &str, parameters: Vec<Json>) -> Result<(Table, Option<u64>), String> {
        let mut body = json!({
            "warehouse_id": self.warehouse,
            "statement": sql,
            "wait_timeout": "30s",
            "on_wait_timeout": "CONTINUE",
            "disposition": "INLINE",
            "format": "JSON_ARRAY",
        });
        if let Some(catalog) = &self.catalog {
            body["catalog"] = json!(catalog);
        }
        if let Some(schema) = &self.schema {
            body["schema"] = json!(schema);
        }
        if !parameters.is_empty() {
            body["parameters"] = Json::Array(parameters);
        }
        let mut answer = self.send("POST", "/api/2.0/sql/statements/", Some(body))?;
        let id = answer["statement_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let mut pause = Duration::from_millis(100);
        loop {
            match answer["status"]["state"].as_str().unwrap_or_default() {
                "PENDING" | "RUNNING" => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_secs(2));
                    answer = self.send("GET", &format!("/api/2.0/sql/statements/{id}"), None)?;
                }
                "SUCCEEDED" => break,
                state => {
                    let error = &answer["status"]["error"];
                    let message = error["message"].as_str().unwrap_or(state);
                    return Err(message.to_owned());
                }
            }
        }
        let mut table = Table::default();
        let mut kinds = Vec::new();
        for column in answer["manifest"]["schema"]["columns"]
            .as_array()
            .into_iter()
            .flatten()
        {
            table
                .columns
                .push(column["name"].as_str().unwrap_or_default().to_owned());
            kinds.push(column["type_name"].as_str().unwrap_or_default().to_owned());
        }
        let mut chunk = answer["result"].clone();
        loop {
            for row in chunk["data_array"].as_array().into_iter().flatten() {
                let cells = row.as_array().into_iter().flatten();
                table.rows.push(
                    cells
                        .zip(&kinds)
                        .map(|(cell, kind)| datum(cell, kind))
                        .collect(),
                );
            }
            let Some(next) = chunk["next_chunk_index"].as_u64() else {
                break;
            };
            chunk = self.send(
                "GET",
                &format!("/api/2.0/sql/statements/{id}/result/chunks/{next}"),
                None,
            )?;
        }
        let changed = (table
            .columns
            .iter()
            .position(|name| name == "num_affected_rows"))
        .and_then(
            |place| match table.rows.first().and_then(|row| row.get(place)) {
                Some(Datum::Long(count)) => u64::try_from(*count).ok(),
                Some(Datum::Integer(count)) => u64::try_from(*count).ok(),
                _ => None,
            },
        );
        Ok((table, changed))
    }

    fn send(&self, method: &str, path: &str, body: Option<Json>) -> Result<Json, String> {
        let headers = vec![
            ("Authorization".to_owned(), format!("Bearer {}", self.token)),
            ("Content-Type".to_owned(), "application/json".to_owned()),
        ];
        let url = format!("{}{path}", self.base);
        let body = body.map(|body| body.to_string().into_bytes());
        let answer =
            storage::exchange(method, &url, &headers, body).map_err(|error| error.to_string())?;
        let reply: Json = serde_json::from_slice(&answer.body).unwrap_or_default();
        if answer.status < 300 {
            return Ok(reply);
        }
        let message = reply["message"].as_str().unwrap_or(&answer.reason);
        Err(format!("Databricks answered {}: {message}", answer.status))
    }
}

impl Connection for Databricks {
    fn query(&mut self, sql: &str) -> Result<Table, String> {
        self.run(sql, Vec::new()).map(|(table, _)| table)
    }

    fn execute(&mut self, sql: &str) -> Result<u64, String> {
        self.run(sql, Vec::new())
            .map(|(_, count)| count.unwrap_or(0))
    }

    fn columns(&mut self, table: &str) -> Result<Vec<String>, String> {
        self.run(&format!("SELECT * FROM {table} LIMIT 0"), Vec::new())
            .map(|(found, _)| found.columns)
    }

    /// All rows of a batch in one statement, as `VALUES` tuples with named parameters.
    fn insert(&mut self, statement: &str, rows: &[Vec<Datum>]) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let lower = statement.to_ascii_lowercase();
        let at = lower
            .rfind("values")
            .ok_or("the insert statement has no VALUES")?;
        let (head, tuple) = (
            &statement[..at + "values".len()],
            &statement[at + "values".len()..],
        );
        let tuples: Vec<&str> = rows.iter().map(|_| tuple.trim()).collect();
        let mut sql = String::new();
        let mut parameters = Vec::new();
        let mut values = rows.iter().flatten();
        let mut quote = None;
        for c in format!("{head} {}", tuples.join(", ")).chars() {
            match (c, quote) {
                ('\'' | '"', None) => quote = Some(c),
                (c, Some(open)) if c == open => quote = None,
                ('?', None) => {
                    let name = format!("p{}", parameters.len());
                    sql.push_str(&format!(":{name}"));
                    parameters.push(parameter(&name, values.next().unwrap_or(&Datum::Null)));
                    continue;
                }
                _ => {}
            }
            sql.push(c);
        }
        self.run(&sql, parameters).map(drop)
    }
}

/// A named parameter of a statement, with its value as text and its SQL type.
fn parameter(name: &str, value: &Datum) -> Json {
    let (text, kind) = match value {
        Datum::Null => (None, "STRING"),
        Datum::Boolean(value) => (Some(value.to_string()), "BOOLEAN"),
        Datum::Integer(value) => (Some(value.to_string()), "INT"),
        Datum::Long(value) => (Some(value.to_string()), "BIGINT"),
        Datum::Float(value) => (Some(value.to_string()), "FLOAT"),
        Datum::Double(value) => (Some(value.to_string()), "DOUBLE"),
        Datum::Decimal(text) | Datum::Text(text) => (Some(text.clone()), "STRING"),
        Datum::Typed("java.sql.Date", text) => (Some(text.clone()), "DATE"),
        Datum::Typed("java.sql.Timestamp", text) => (Some(text.clone()), "TIMESTAMP"),
        Datum::Typed(_, text) => (Some(text.clone()), "STRING"),
        Datum::Bytes(bytes) => (
            Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            "STRING",
        ),
        Datum::List(_) => (Some(format!("{value:?}")), "STRING"),
    };
    match text {
        Some(text) => json!({ "name": name, "value": text, "type": kind }),
        None => json!({ "name": name, "type": kind }),
    }
}

/// A value of the `JSON_ARRAY` format, which gives every value as text, by its column type.
fn datum(cell: &Json, kind: &str) -> Datum {
    let Some(text) = cell
        .as_str()
        .map(str::to_owned)
        .or_else(|| (!cell.is_null()).then(|| cell.to_string()))
    else {
        return Datum::Null;
    };
    let base = kind
        .split(['(', '<'])
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match base.as_str() {
        "BOOLEAN" => Datum::Boolean(text == "true"),
        "BYTE" | "TINYINT" | "SHORT" | "SMALLINT" | "INT" | "INTEGER" => {
            text.parse().map_or(Datum::Text(text), Datum::Integer)
        }
        "LONG" | "BIGINT" => text.parse().map_or(Datum::Text(text), Datum::Long),
        "FLOAT" => text.parse().map_or(Datum::Text(text), Datum::Float),
        "DOUBLE" => text.parse().map_or(Datum::Text(text), Datum::Double),
        "DECIMAL" => Datum::Decimal(text),
        "DATE" => Datum::Typed("java.sql.Date", text),
        "TIMESTAMP" | "TIMESTAMP_NTZ" => Datum::Typed("java.sql.Timestamp", text),
        "BINARY" => match base64::engine::general_purpose::STANDARD.decode(&text) {
            Ok(bytes) => Datum::Bytes(bytes),
            Err(_) => Datum::Text(text),
        },
        _ => Datum::Text(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_databricks_urls() {
        let address = "//adb-1.azuredatabricks.net:443/default;transportMode=http;ssl=1;AuthMech=3;\
                       httpPath=/sql/1.0/warehouses/abc123;IgnoreTransactions=1";
        let found = warehouse(address, Some("dapi-token".into())).expect("a warehouse");
        assert_eq!(found.base, "https://adb-1.azuredatabricks.net");
        assert_eq!(found.warehouse, "abc123");
        assert_eq!(found.schema.as_deref(), Some("default"));
        let cluster = warehouse(
            "//h:443/default;httpPath=/sql/protocolv1/o/1/2",
            Some("t".into()),
        );
        assert!(cluster.expect_err("a cluster").contains("is not one"));
        let oauth = warehouse(
            "//h/x;httpPath=/sql/1.0/warehouses/w;AuthMech=11",
            Some("t".into()),
        );
        assert!(oauth.expect_err("OAuth").contains("AuthMech=11"));
        assert_eq!(datum(&json!("7"), "INT"), Datum::Integer(7));
        assert_eq!(
            datum(&json!("1.50"), "DECIMAL(5,2)"),
            Datum::Decimal("1.50".into())
        );
        assert_eq!(datum(&Json::Null, "STRING"), Datum::Null);
    }
}

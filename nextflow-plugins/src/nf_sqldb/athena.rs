//! AWS Athena through its HTTP API for `jdbc:awsathena:` and `jdbc:athena:` URLs, signed with
//! Signature Version 4.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use serde_json::{Value as Json, json};
use storage::aws;

use super::{Connection, Datum, Table, literal};

/// An Athena workgroup of a region, where query results go, and the credentials of its calls.
#[derive(Clone, Debug)]
pub(crate) struct Athena {
    endpoint: String,
    region: String,
    output: Option<String>,
    workgroup: Option<String>,
    catalog: Option<String>,
    database: Option<String>,
    credentials: aws::Credentials,
}

impl PartialEq for Athena {
    fn eq(&self, other: &Self) -> bool {
        (&self.endpoint, &self.region, &self.output, &self.workgroup)
            == (
                &other.endpoint,
                &other.region,
                &other.output,
                &other.workgroup,
            )
    }
}

/// The settings of `//Key=Value;Key=Value;...` in the names of the Simba driver (`AwsRegion`,
/// `S3OutputLocation`) or of driver 3 (`Region`, `OutputLocation`). The keys of the config, or
/// else of the URL, sign the calls; without keys, AWS's default chain of the environment does.
pub(crate) fn workgroup(
    address: &str,
    user: Option<String>,
    password: Option<String>,
    variable: &dyn Fn(&str) -> Option<String>,
) -> Result<Athena, String> {
    let rest = address.strip_prefix("//").unwrap_or(address);
    let pairs: Vec<(String, String)> = (rest.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let setting = |names: &[&str]| {
        let found = pairs.iter().find(|(key, _)| names.contains(&key.as_str()));
        found
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    };
    let region = setting(&["region", "awsregion"])
        .or_else(|| variable("AWS_REGION"))
        .or_else(|| variable("AWS_DEFAULT_REGION"))
        .ok_or("the Athena URL names no Region")?;
    let default = match setting(&["endpoint", "endpointoverride"]) {
        Some(endpoint) if !endpoint.starts_with("https://") => {
            return Err(format!(
                "Could not connect: Invalid connection parameter(s): The Athena endpoint \
                 \"{endpoint}\" is not an HTTPS endpoint;"
            ));
        }
        Some(endpoint) => endpoint,
        None => format!("https://athena.{region}.amazonaws.com"),
    };
    let endpoint = aws::endpoint("ATHENA", default, variable);
    let user = user.or_else(|| setting(&["user", "uid", "accesskeyid"]));
    let password = password.or_else(|| setting(&["password", "pwd", "secretaccesskey"]));
    let credentials = match (user, password) {
        (Some(id), Some(secret)) => aws::Credentials::Keys(aws::Keys {
            id,
            secret,
            token: setting(&["sessiontoken"]),
        }),
        _ => aws::Credentials::Chain {
            environment: (aws::VARIABLES.iter())
                .filter_map(|name| Some((name.to_string(), variable(name)?)))
                .collect(),
            home: variable("HOME").map(Into::into),
        },
    };
    Ok(Athena {
        endpoint,
        region,
        output: setting(&["outputlocation", "s3outputlocation"]),
        workgroup: setting(&["workgroup"]),
        catalog: setting(&["catalog"]),
        database: setting(&["database", "schema"]),
        credentials,
    })
}

impl Athena {
    /// Starts the query, waits for its end and reads all its result pages.
    fn run(&self, sql: &str) -> Result<(Table, Option<u64>), String> {
        let mut start = json!({ "QueryString": sql });
        if let Some(output) = &self.output {
            start["ResultConfiguration"] = json!({ "OutputLocation": output });
        }
        if let Some(workgroup) = &self.workgroup {
            start["WorkGroup"] = json!(workgroup);
        }
        let mut context = serde_json::Map::new();
        if let Some(catalog) = &self.catalog {
            context.insert("Catalog".into(), json!(catalog));
        }
        if let Some(database) = &self.database {
            context.insert("Database".into(), json!(database));
        }
        if !context.is_empty() {
            start["QueryExecutionContext"] = Json::Object(context);
        }
        let started = self.call("StartQueryExecution", &start)?;
        let id = started["QueryExecutionId"]
            .as_str()
            .ok_or("Athena gave no query id")?;
        let mut pause = Duration::from_millis(100);
        loop {
            let found = self.call("GetQueryExecution", &json!({ "QueryExecutionId": id }))?;
            let status = &found["QueryExecution"]["Status"];
            match status["State"].as_str().unwrap_or_default() {
                "SUCCEEDED" => break,
                "FAILED" | "CANCELLED" => {
                    let reason = status["StateChangeReason"].as_str().unwrap_or("no reason");
                    return Err(format!("the Athena query {id} failed: {reason}"));
                }
                _ => std::thread::sleep(pause),
            }
            pause = (pause * 2).min(Duration::from_secs(2));
        }
        let mut table = Table::default();
        let mut kinds = Vec::new();
        let mut token: Option<String> = None;
        let mut count = None;
        loop {
            let mut ask = json!({ "QueryExecutionId": id, "MaxResults": 1000 });
            if let Some(token) = &token {
                ask["NextToken"] = json!(token);
            }
            let page = self.call("GetQueryResults", &ask)?;
            count = count.or_else(|| page["UpdateCount"].as_u64());
            let set = &page["ResultSet"];
            let first = kinds.is_empty() && token.is_none();
            if first {
                for column in set["ResultSetMetadata"]["ColumnInfo"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    table
                        .columns
                        .push(column["Name"].as_str().unwrap_or_default().to_owned());
                    kinds.push(column["Type"].as_str().unwrap_or_default().to_owned());
                }
            }
            let rows = set["Rows"].as_array().cloned().unwrap_or_default();
            // The first row of a query's results repeats the column names.
            let header = first && rows.first().is_some_and(|row| names(row) == table.columns);
            for row in rows.iter().skip(usize::from(header)) {
                let cells = row["Data"].as_array().cloned().unwrap_or_default();
                let values = cells
                    .iter()
                    .zip(&kinds)
                    .map(|(cell, kind)| datum(cell, kind));
                table.rows.push(values.collect());
            }
            match page["NextToken"].as_str() {
                Some(next) => token = Some(next.to_owned()),
                None => return Ok((table, count)),
            }
        }
    }

    /// One call of the Athena JSON API.
    fn call(&self, action: &str, body: &Json) -> Result<Json, String> {
        let request = aws::Request {
            service: "athena".to_owned(),
            region: self.region.clone(),
            method: "POST".to_owned(),
            url: format!("{}/", self.endpoint),
            headers: vec![
                (
                    "content-type".to_owned(),
                    "application/x-amz-json-1.1".to_owned(),
                ),
                ("x-amz-target".to_owned(), format!("AmazonAthena.{action}")),
            ],
            body: body.to_string().into_bytes(),
        };
        let answer = aws::send(request, &self.credentials).map_err(|error| error.to_string())?;
        let reply: Json = serde_json::from_slice(&answer.body).unwrap_or_default();
        if answer.status < 300 {
            return Ok(reply);
        }
        let message = (reply.get("Message").or_else(|| reply.get("message")))
            .and_then(Json::as_str)
            .unwrap_or(&answer.reason);
        Err(format!(
            "Athena {action} answered {}: {message}",
            answer.status
        ))
    }
}

impl Connection for Athena {
    fn query(&mut self, sql: &str) -> Result<Table, String> {
        self.run(sql).map(|(table, _)| table)
    }

    fn execute(&mut self, sql: &str) -> Result<u64, String> {
        self.run(sql).map(|(_, count)| count.unwrap_or(0))
    }

    fn columns(&mut self, table: &str) -> Result<Vec<String>, String> {
        let (found, _) = self.run(&format!("SHOW COLUMNS IN {table}"))?;
        let names = found
            .rows
            .into_iter()
            .filter_map(|row| match row.into_iter().next() {
                Some(Datum::Text(name)) => Some(name.trim().to_owned()),
                _ => None,
            });
        Ok(names.collect())
    }

    /// One statement per row with its values as literals; Athena has no prepared batches.
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

fn names(row: &Json) -> Vec<String> {
    let cells = row["Data"].as_array().into_iter().flatten();
    cells
        .map(|cell| cell["VarCharValue"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A value of Athena's text results, by the column type.
fn datum(cell: &Json, kind: &str) -> Datum {
    let Some(text) = cell["VarCharValue"].as_str() else {
        return Datum::Null;
    };
    let text = text.to_owned();
    match kind {
        "boolean" => Datum::Boolean(text == "true"),
        "tinyint" | "smallint" | "integer" => match text.parse::<i64>() {
            Ok(value) => i32::try_from(value).map_or(Datum::Long(value), Datum::Integer),
            Err(_) => Datum::Text(text),
        },
        "bigint" => text.parse().map_or(Datum::Text(text.clone()), Datum::Long),
        "float" | "real" => text.parse().map_or(Datum::Text(text.clone()), Datum::Float),
        "double" => text
            .parse()
            .map_or(Datum::Text(text.clone()), Datum::Double),
        "decimal" => Datum::Decimal(text),
        "date" => Datum::Typed("java.sql.Date", text),
        "timestamp" => Datum::Typed("java.sql.Timestamp", text),
        _ => Datum::Text(text),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The heard requests: target, authorization and body of each.
    type Log = Arc<Mutex<Vec<(String, String, String)>>>;

    /// Answers Athena's calls on a free local port: a query whose results come in two pages.
    fn endpoint() -> (u16, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
        let port = listener.local_addr().expect("an address").port();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let log = heard.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().expect("a handle"));
                let (mut target, mut authorization, mut length) = (String::new(), String::new(), 0);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    let (name, value) = line.trim_end().split_once(": ").unwrap_or_default();
                    match name.to_ascii_lowercase().as_str() {
                        "x-amz-target" => target = value.to_owned(),
                        "authorization" => authorization = value.to_owned(),
                        "content-length" => length = value.parse().unwrap_or(0),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let body = String::from_utf8_lossy(&body).into_owned();
                let columns = r#"{"ColumnInfo":[{"Name":"id","Type":"integer"},{"Name":"day","Type":"date"}]}"#;
                let reply = match target.as_str() {
                    "AmazonAthena.StartQueryExecution" => r#"{"QueryExecutionId":"e1"}"#.to_owned(),
                    "AmazonAthena.GetQueryExecution" => {
                        r#"{"QueryExecution":{"Status":{"State":"SUCCEEDED"}}}"#.to_owned()
                    }
                    _ if body.contains("NextToken") => {
                        r#"{"ResultSet":{"Rows":[{"Data":[{"VarCharValue":"2"},{}]}]}}"#.to_owned()
                    }
                    _ => format!(
                        r#"{{"ResultSet":{{"ResultSetMetadata":{columns},"Rows":[{{"Data":[{{"VarCharValue":"id"}},{{"VarCharValue":"day"}}]}},{{"Data":[{{"VarCharValue":"1"}},{{"VarCharValue":"2024-01-02"}}]}}]}},"NextToken":"t2"}}"#
                    ),
                };
                log.lock()
                    .expect("the log")
                    .push((target, authorization, body));
                let text = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = stream.write_all(text.as_bytes());
            }
        });
        (port, heard)
    }

    #[test]
    fn reads_query_pages() {
        let (port, heard) = endpoint();
        let variables = [(
            "AWS_ENDPOINT_URL_ATHENA",
            format!("http://127.0.0.1:{port}"),
        )];
        let variable = |name: &str| {
            let found = variables.iter().find(|(key, _)| *key == name);
            found.map(|(_, value)| value.clone())
        };
        let address = "//AwsRegion=eu-west-1;S3OutputLocation=s3://bucket/out/;Workgroup=lab";
        let keys = (Some("AKIDEXAMPLE".to_owned()), Some("secret".to_owned()));
        let mut athena = workgroup(address, keys.0, keys.1, &variable).expect("a workgroup");
        let table = athena.query("SELECT id, day FROM t").expect("a table");
        assert_eq!(table.columns, ["id", "day"]);
        assert_eq!(
            table.rows,
            [
                vec![
                    Datum::Integer(1),
                    Datum::Typed("java.sql.Date", "2024-01-02".into())
                ],
                vec![Datum::Integer(2), Datum::Null],
            ]
        );
        let heard = heard.lock().expect("the log").clone();
        let targets: Vec<&str> = heard.iter().map(|(target, _, _)| target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "AmazonAthena.StartQueryExecution",
                "AmazonAthena.GetQueryExecution",
                "AmazonAthena.GetQueryResults",
                "AmazonAthena.GetQueryResults",
            ]
        );
        let (_, authorization, start) = &heard[0];
        assert!(
            authorization.contains("Credential=AKIDEXAMPLE/"),
            "{authorization}"
        );
        assert!(
            authorization.contains("/eu-west-1/athena/aws4_request"),
            "{authorization}"
        );
        let start: Json = serde_json::from_str(start).expect("JSON");
        assert_eq!(start["QueryString"], "SELECT id, day FROM t");
        assert_eq!(
            start["ResultConfiguration"]["OutputLocation"],
            "s3://bucket/out/"
        );
        assert_eq!(start["WorkGroup"], "lab");
    }

    #[test]
    fn refuses_plain_endpoints() {
        let address = "//Region=us-east-1;Endpoint=http://127.0.0.1:9";
        let refused = workgroup(address, None, None, &|_| None).expect_err("refused");
        assert_eq!(
            refused,
            "Could not connect: Invalid connection parameter(s): The Athena endpoint \
             \"http://127.0.0.1:9\" is not an HTTPS endpoint;"

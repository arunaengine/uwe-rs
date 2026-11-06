//! nf-boost 0.8.0: `request` and the cleanup config; `nf_boost.nf` has the functions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{Key, Native, Object, State as ObjectState, Value};

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::Observer;
use crate::{Context, native, object};

/// The functions run as script code of `nf_boost.nf`, `request` here; `scan` is an operator of
/// the runtime.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "fromJson",
        "toJson",
        "fromYaml",
        "mergeCsv",
        "mergeText",
        "request",
        "template",
        "scan",
    ],
    call: Some(call),
    methods: Some(methods),
    script: Some(include_str!("nf_boost.nf")),
    scopes: &["boost"],
    observer: Some(activate),
    ..Plugin::new("nf-boost", Kind::Functions, State::Partial)
};

/// Checks `boost.cleanup`. uwe keeps task results in a store that other runs, the cache and
/// published outputs share, so it never deletes them during a run and skips the cleanup.
fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let on = match config.get(&["boost", "cleanup"]) {
        None | Some(Value::Null | Value::Boolean(false)) => false,
        Some(Value::Boolean(true)) => true,
        Some(other) => match other.to_string().as_str() {
            "false" => false,
            "true" | "v1" | "v2" => true,
            value => return Err(format!("Invalid `boost.cleanup` value -- {value}")),
        },
    };
    if on {
        tracing::warn!(
            "The automatic cleanup of nf-boost (`boost.cleanup`) is skipped: uwe keeps task \
             results in a store that other runs share"
        );
    }
    Ok(None)
}

fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("request", [url]) => request(&url.to_string(), None),
        ("request", [options @ Value::Map(_), url]) => request(&url.to_string(), Some(options)),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-boost` with these arguments"),
            span: Default::default(),
        }),
    }
}

/// A request to a web server and, once it was sent, its answer, as `HttpURLConnection` has.
#[derive(Debug)]
struct Connection {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    answer: Option<storage::Answer>,
}

impl Native for Connection {
    fn plugin(&self) -> &'static str {
        "nf-boost"
    }

    fn text(&self) -> String {
        format!("{}:{}", class(&self.url), self.url)
    }
}

fn class(url: &str) -> &'static str {
    match url.starts_with("https:") {
        true => "sun.net.www.protocol.https.HttpsURLConnectionImpl",
        false => "sun.net.www.protocol.http.HttpURLConnection",
    }
}

/// `request(url, [method:, headers:, body:])`: a connection, which sends the request when the
/// script first reads the answer, or at once when there is a body.
fn request(url: &str, options: Option<&Value>) -> Result<Value, Error> {
    let option = |name: &str| match options {
        Some(Value::Map(map)) => map.borrow().entries.get(&Key(Value::string(name))).cloned(),
        _ => None,
    };
    let method = option("method").map_or_else(|| "GET".into(), |m| m.to_string().to_uppercase());
    const METHODS: [&str; 7] = ["GET", "POST", "HEAD", "OPTIONS", "PUT", "DELETE", "TRACE"];
    if !METHODS.contains(&method.as_str()) {
        let message = format!("Invalid HTTP method: {method}");
        return Err(thrown(Exception::new("ProtocolException", message)));
    }
    let headers = match option("headers") {
        Some(Value::Map(map)) => (map.borrow().entries.iter())
            .map(|(name, value)| (name.0.to_string(), value.to_string()))
            .collect(),
        _ => Vec::new(),
    };
    let body = option("body")
        .map(|body| body.to_string())
        .filter(|body| !body.is_empty());
    let send = body.is_some();
    let connection = Connection {
        url: url.to_owned(),
        method,
        headers,
        body,
        answer: None,
    };
    let value = object(class(url), connection);
    if send && let Some(mut connection) = native::<Connection>(&value) {
        answer(&mut connection)?;
    }
    Ok(value)
}

/// The answer of the server, sent for once.
fn answer(connection: &mut Connection) -> Result<&storage::Answer, Error> {
    if connection.answer.is_none() {
        let body = connection.body.clone().map(String::into_bytes);
        let sent = storage::exchange(
            &connection.method,
            &connection.url,
            &connection.headers,
            body,
        );
        let answer =
            sent.map_err(|error| thrown(Exception::new("IOException", error.to_string())))?;
        connection.answer = Some(answer);
    }
    connection
        .answer
        .as_ref()
        .ok_or_else(|| thrown(Exception::new("IOException", "")))
}

/// Runs a method of a connection, `None` for a method it does not have.
fn methods(
    object: &Value,
    name: &str,
    arguments: Vec<Value>,
    _: &mut Context,
) -> Result<Option<Value>, Error> {
    let Some(mut connection) = native::<Connection>(object) else {
        return Ok(None);
    };
    if let ("getRequestMethod", []) = (name, arguments.as_slice()) {
        return Ok(Some(Value::string(connection.method.as_str())));
    }
    let known = [
        "getResponseCode",
        "getResponseMessage",
        "getInputStream",
        "getErrorStream",
        "getHeaderField",
        "getHeaderFields",
        "getContentType",
    ];
    if !known.contains(&name) {
        return Ok(None);
    }
    let url = connection.url.clone();
    let answer = answer(&mut connection)?;
    let header = |wanted: &str| {
        let found = answer
            .headers
            .iter()
            .rev()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted));
        found.map_or(Value::Null, |(_, value)| Value::string(value.as_str()))
    };
    let failed = answer.status >= 400;
    Ok(Some(match (name, arguments.as_slice()) {
        ("getResponseCode", []) => Value::integer(i32::from(answer.status)),
        ("getResponseMessage", []) => Value::string(answer.reason.as_str()),
        ("getInputStream", []) if answer.status == 404 => {
            return Err(thrown(Exception::new("FileNotFoundException", url)));
        }
        ("getInputStream", []) if failed => {
            let message = format!(
                "Server returned HTTP response code: {} for URL: {url}",
                answer.status
            );
            return Err(thrown(Exception::new("IOException", message)));
        }
        ("getInputStream", []) => stream(&answer.body),
        ("getErrorStream", []) if failed => stream(&answer.body),
        ("getErrorStream", []) => Value::Null,
        ("getHeaderField", [name]) => header(&name.to_string()),
        ("getContentType", []) => header("content-type"),
        ("getHeaderFields", []) => {
            let mut fields: Vec<(String, Vec<Value>)> = Vec::new();
            for (name, value) in &answer.headers {
                let value = Value::string(value.as_str());
                match fields.iter_mut().find(|(known, _)| known == name) {
                    Some((_, values)) => values.push(value),
                    None => fields.push((name.clone(), vec![value])),
                }
            }
            Value::map(
                fields
                    .into_iter()
                    .map(|(n, v)| (Value::string(n), Value::list(v))),
            )
        }
        _ => return Ok(None),
    }))
}

/// An input stream over the bytes of an answer.
fn stream(bytes: &[u8]) -> Value {
    Value::Object(Rc::new(RefCell::new(Object {
        class: "java.io.InputStream",
        state: ObjectState::Stream(bytes.into(), 0),
    })))
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}

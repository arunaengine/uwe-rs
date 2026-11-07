//! nf-boost 0.8.0: `toYaml`, `request` and the cleanup switch; `nf_boost.nf` has the rest.
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
use crate::service::safe_url;
use crate::{Context, native, object};

/// The functions run as script code of `nf_boost.nf`, `toYaml` and `request` here; `scan` is an
/// operator of the runtime.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "fromJson",
        "toJson",
        "fromYaml",
        "toYaml",
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
    ..Plugin::new("nf-boost", Kind::Functions, State::Implemented)
};

/// Whether `boost.cleanup` turns the cleanup on, which the runtime does (see nf-gc): `true`,
/// `'v1'` and `'v2'` do, `false` and none do not.
pub(crate) fn cleanup(config: &Config) -> Result<bool, String> {
    match config.get(&["boost", "cleanup"]) {
        None | Some(Value::Null | Value::Boolean(false)) => Ok(false),
        Some(Value::Boolean(true)) => Ok(true),
        Some(other) => match other.to_string().as_str() {
            "false" => Ok(false),
            "true" | "v1" | "v2" => Ok(true),
            value => Err(format!("Invalid `boost.cleanup` value -- {value}")),
        },
    }
}

/// Checks `boost.cleanup`.
fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    cleanup(config)?;
    Ok(None)
}

fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("toYaml", [value]) => Ok(Value::string(yaml(value))),
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
        format!("{}:{}", class(&self.url), safe_url(&self.url))
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
        let answer = sent.map_err(|error| {
            let message = hidden(&error.to_string(), &connection.url);
            thrown(Exception::new("IOException", message))
        })?;
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
    let url = safe_url(&connection.url);
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

/// A message without the user info and query of an address, which the HTTP client may print
/// in another form than the given address.
fn hidden(message: &str, url: &str) -> String {
    let mut message = message.replace(url, &safe_url(url));
    if let Some(start) = url.find(['?', '#']) {
        message = message.replace(&url[start..], "");
    }
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or_default();
    if let Some((user, _)) = authority.rsplit_once('@') {
        message = message.replace(&format!("{user}@"), "");
    }
    message
}

/// An input stream over the bytes of an answer.
fn stream(bytes: &[u8]) -> Value {
    Value::Object(Rc::new(RefCell::new(Object {
        class: "java.io.InputStream",
        state: ObjectState::Stream(bytes.into(), 0),
    })))
}

/// The YAML text of a value as SnakeYAML dumps it by default: collections of scalars in flow
/// style, other collections in block style, and a final line break.
fn yaml(value: &Value) -> String {
    let mut out = String::new();
    match block(value) {
        true => write_block(value, 0, &mut out),
        false => match value {
            Value::String(text) | Value::GString(text) if text.contains('\n') => {
                out.push_str(&literal(text, 0));
            }
            other => {
                out.push_str(&inline(other));
                out.push('\n');
            }
        },
    }
    out
}

/// The entries of a map or the items of a list, `None` for a scalar.
fn members(value: &Value) -> Option<Vec<(Option<Value>, Value)>> {
    match value {
        Value::Map(map) => Some(
            (map.borrow().entries.iter())
                .map(|(key, value)| (Some(key.0.clone()), value.clone()))
                .collect(),
        ),
        Value::List(list) => Some(
            list.borrow()
                .items
                .iter()
                .map(|v| (None, v.clone()))
                .collect(),
        ),
        Value::Set(set) => Some(
            set.borrow()
                .items
                .iter()
                .map(|k| (None, k.0.clone()))
                .collect(),
        ),
        _ => None,
    }
}

/// Whether a value is a collection that holds a collection, which goes in block style.
fn block(value: &Value) -> bool {
    members(value).is_some_and(|members| members.iter().any(|(_, item)| members_of(item)))
}

fn members_of(value: &Value) -> bool {
    members(value).is_some()
}

fn write_block(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for (key, item) in members(value).unwrap_or_default() {
        let lead = match &key {
            Some(key) => format!("{pad}{}:", scalar(key, false)),
            None => format!("{pad}-"),
        };
        match &item {
            Value::String(text) | Value::GString(text) if text.contains('\n') => {
                out.push_str(&format!("{lead} {}", literal(text, indent + 2)));
            }
            nested if block(nested) && key.is_none() => {
                // The first line of a nested block goes after the dash.
                let mut inner = String::new();
                write_block(nested, indent + 2, &mut inner);
                out.push_str(&format!("{lead} {}", &inner[indent + 2..]));
            }
            nested if block(nested) => {
                out.push_str(&format!("{lead}\n"));
                let deeper = if matches!(nested, Value::Map(_)) {
                    indent + 2
                } else {
                    indent
                };
                write_block(nested, deeper, out);
            }
            other => out.push_str(&format!("{lead} {}\n", inline(other))),
        }
    }
}

/// A multi-line text as a literal block, its lines indented.
fn literal(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent.max(2));
    let (chomp, body) = match text.strip_suffix('\n') {
        Some(body) if !body.ends_with('\n') => ("|", body),
        Some(_) => ("|+", text.trim_end_matches('\n')),
        None => ("|-", text),
    };
    let lines: Vec<String> = body
        .split('\n')
        .map(|line| format!("{pad}{line}\n"))
        .collect();
    format!("{chomp}\n{}", lines.concat())
}

/// A scalar, or a collection of scalars in flow style.
fn inline(value: &Value) -> String {
    match members(value) {
        Some(members) => {
            let map = matches!(value, Value::Map(_));
            let items: Vec<String> = (members.iter())
                .map(|(key, item)| match key {
                    Some(key) => format!("{}: {}", scalar(key, true), scalar(item, true)),
                    None => scalar(item, true),
                })
                .collect();
            match map {
                true => format!("{{{}}}", items.join(", ")),
                false => format!("[{}]", items.join(", ")),
            }
        }
        None => scalar(value, false),
    }
}

/// A scalar, in quotes when it would read as another type or as YAML syntax.
fn scalar(value: &Value, flow: bool) -> String {
    let text = match value {
        Value::Null => return "null".into(),
        Value::Boolean(_) | Value::Number(_) => return value.to_string(),
        other => other.to_string(),
    };
    const IMPLICIT: [&str; 5] = [
        "yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF",
        "~|null|Null|NULL",
        "[-+]?(?:0b[0-1_]+|0[0-7_]+|(?:0|[1-9][0-9_]*)|0x[0-9a-fA-F_]+|[1-9][0-9_]*(?::[0-5]?[0-9])+)",
        "[-+]?(?:\\.[0-9]+|[0-9_]+(?:\\.[0-9_]*)?)(?:[eE][-+]?[0-9]+)?|[-+]?\\.(?:inf|Inf|INF)|\\.(?:nan|NaN|NAN)",
        "[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}(?:(?:[Tt]|[ \\t]+).*)?",
    ];
    let implicit = IMPLICIT.iter().any(|regex| {
        let pattern = nextflow_evaluator::regex::Pattern::new(regex);
        pattern.is_ok_and(|pattern| pattern.matches(&text).unwrap_or(false))
    });
    let first = text.chars().next().unwrap_or(' ');
    let second = text.chars().nth(1).unwrap_or(' ');
    let special = text.is_empty()
        || "#,[]{}&*!|>'\"%@`".contains(first)
        || "-?:".contains(first) && second == ' '
        || text.contains(": ")
        || text.contains(" #")
        || text.ends_with(':')
        || text.starts_with(' ')
        || text.ends_with(' ')
        || flow && text.contains([',', '[', ']', '{', '}'])
        || text.chars().any(char::is_control);
    match implicit || special {
        true => format!("'{}'", text.replace('\'', "''")),
        false => text,
    }
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}

//! Stand-ins for `urllib.parse`, `urllib.error` and `urllib.request`. Parsing and quoting
//! work; downloads while a workflow is planned are unsupported.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::function;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value};

/// The parts of a URL, as `urllib.parse.urlparse` returns them.
struct UrlParts {
    scheme: String,
    netloc: String,
    path: String,
    params: String,
    query: String,
    fragment: String,
    /// Whether the result of `urlsplit`, which has no `params`.
    split: bool,
}

impl UrlParts {
    fn parse(url: &str, split: bool) -> UrlParts {
        let mut rest = url;
        let mut scheme = String::new();
        if let Some(colon) = rest.find(':') {
            let candidate = &rest[..colon];
            if candidate
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
                && candidate
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            {
                scheme = candidate.to_ascii_lowercase();
                rest = &rest[colon + 1..];
            }
        }
        let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (netloc, path) = match rest.strip_prefix("//") {
            Some(after) => match after.find('/') {
                Some(slash) => (&after[..slash], &after[slash..]),
                None => (after, ""),
            },
            None => ("", rest),
        };
        let (path, params) = if split {
            (path, "")
        } else {
            let last = path.rfind('/').map_or(0, |slash| slash + 1);
            match path[last..].find(';') {
                Some(semicolon) => (&path[..last + semicolon], &path[last + semicolon + 1..]),
                None => (path, ""),
            }
        };
        UrlParts {
            scheme,
            netloc: netloc.to_owned(),
            path: path.to_owned(),
            params: params.to_owned(),
            query: query.to_owned(),
            fragment: fragment.to_owned(),
            split,
        }
    }

    fn fields(&self) -> Vec<Value> {
        let mut fields = vec![
            Value::str(self.scheme.as_str()),
            Value::str(self.netloc.as_str()),
            Value::str(self.path.as_str()),
        ];
        if !self.split {
            fields.push(Value::str(self.params.as_str()));
        }
        fields.push(Value::str(self.query.as_str()));
        fields.push(Value::str(self.fragment.as_str()));
        fields
    }

    fn url(&self) -> String {
        let mut url = String::new();
        if !self.scheme.is_empty() {
            url.push_str(&self.scheme);
            url.push(':');
        }
        if !self.netloc.is_empty() || self.scheme == "file" {
            url.push_str("//");
            url.push_str(&self.netloc);
        }
        url.push_str(&self.path);
        if !self.params.is_empty() {
            url.push(';');
            url.push_str(&self.params);
        }
        if !self.query.is_empty() {
            url.push('?');
            url.push_str(&self.query);
        }
        if !self.fragment.is_empty() {
            url.push('#');
            url.push_str(&self.fragment);
        }
        url
    }

    /// The host and port after any user information.
    fn host_port(&self) -> (&str, Option<&str>) {
        let host = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, host)| host);
        match host.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || host.starts_with('[') => {
                (host, Some(port))
            }
            _ => (host, None),
        }
    }
}

impl Native for UrlParts {
    fn type_name(&self) -> &str {
        if self.split {
            "SplitResult"
        } else {
            "ParseResult"
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let (host, port) = self.host_port();
        Ok(Some(match name {
            "scheme" => Value::str(self.scheme.as_str()),
            "netloc" => Value::str(self.netloc.as_str()),
            "path" => Value::str(self.path.as_str()),
            "params" if !self.split => Value::str(self.params.as_str()),
            "query" => Value::str(self.query.as_str()),
            "fragment" => Value::str(self.fragment.as_str()),
            "hostname" if host.is_empty() => Value::None,
            "hostname" => Value::str(host.trim_matches(['[', ']']).to_ascii_lowercase()),
            "port" => match port {
                Some(port) => Value::Int(port.parse().map_err(|_| {
                    Error::value_error(format!(
                        "Port could not be cast to integer value as '{port}'"
                    ))
                })?),
                None => Value::None,
            },
            "username" => match self.netloc.rsplit_once('@') {
                Some((user, _)) => Value::str(user.split(':').next().unwrap_or_default()),
                None => Value::None,
            },
            "geturl" => {
                let this = this.clone();
                Value::builtin(name, move |_, _| {
                    let url = this
                        .as_native::<UrlParts>()
                        .map(UrlParts::url)
                        .unwrap_or_default();
                    Ok(Value::str(url))
                })
            }
            _ => return Ok(None),
        }))
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.fields()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.fields().len())
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        interpreter
            .get_item(&Value::tuple(self.fields()), key)
            .map(Some)
    }

    fn repr(&self, _: &mut Interpreter) -> Result<String, Error> {
        let mut parts = vec![
            format!("scheme={}", crate::format::quote(&self.scheme)),
            format!("netloc={}", crate::format::quote(&self.netloc)),
            format!("path={}", crate::format::quote(&self.path)),
        ];
        if !self.split {
            parts.push(format!("params={}", crate::format::quote(&self.params)));
        }
        parts.push(format!("query={}", crate::format::quote(&self.query)));
        parts.push(format!("fragment={}", crate::format::quote(&self.fragment)));
        Ok(format!("{}({})", self.type_name(), parts.join(", ")))
    }
}

/// Characters that `quote` never escapes.
fn unreserved(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_.-~".contains(c)
}

fn quote_text(text: &str, safe: &str, plus: bool) -> String {
    let mut quoted = String::new();
    for c in text.chars() {
        if unreserved(c) || safe.contains(c) {
            quoted.push(c);
        } else if plus && c == ' ' {
            quoted.push('+');
        } else {
            let mut buffer = [0u8; 4];
            for byte in c.encode_utf8(&mut buffer).bytes() {
                quoted.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    quoted
}

fn unquote_text(text: &str, plus: bool) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && index + 2 < bytes.len()
            && let Some(value) = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            decoded.push(value);
            index += 3;
            continue;
        }
        decoded.push(if plus && byte == b'+' { b' ' } else { byte });
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn text_argument(
    interpreter: &mut Interpreter,
    args: &mut Args,
    index: usize,
    name: &str,
) -> Result<String, Error> {
    match args.take(index, name) {
        Some(value) => interpreter.to_str(&value),
        None => Err(Error::type_error(format!("missing argument '{name}'"))),
    }
}

/// `urljoin(base, url)`: a URL relative to a base, for the usual cases of absolute URLs,
/// absolute paths and relative paths.
fn join(base: &str, url: &str) -> String {
    if url.is_empty() {
        return base.to_owned();
    }
    let target = UrlParts::parse(url, true);
    if !target.scheme.is_empty() {
        return url.to_owned();
    }
    let base = UrlParts::parse(base, true);
    let prefix = format!("{}://{}", base.scheme, base.netloc);
    if url.starts_with("//") {
        return format!("{}:{url}", base.scheme);
    }
    if url.starts_with('/') {
        return format!("{prefix}{url}");
    }
    let directory = match base.path.rfind('/') {
        Some(slash) => &base.path[..=slash],
        None => "/",
    };
    let joined = format!("{directory}{url}");
    let mut segments: Vec<&str> = Vec::new();
    for segment in joined.split('/') {
        match segment {
            ".." => {
                segments.pop();
            }
            "." => {}
            other => segments.push(other),
        }
    }
    let mut path = segments.join("/");
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    format!("{prefix}{path}")
}

pub fn parse() -> Vec<(&'static str, Value)> {
    vec![
        (
            "urlparse",
            function("urlparse", |interpreter, mut args| {
                let url = text_argument(interpreter, &mut args, 0, "url")?;
                Ok(Value::native(UrlParts::parse(&url, false)))
            }),
        ),
        (
            "urlsplit",
            function("urlsplit", |interpreter, mut args| {
                let url = text_argument(interpreter, &mut args, 0, "url")?;
                Ok(Value::native(UrlParts::parse(&url, true)))
            }),
        ),
        (
            "urljoin",
            function("urljoin", |interpreter, mut args| {
                let base = text_argument(interpreter, &mut args, 0, "base")?;
                let url = text_argument(interpreter, &mut args, 1, "url")?;
                Ok(Value::str(join(&base, &url)))
            }),
        ),
        (
            "quote",
            function("quote", |interpreter, mut args| {
                let text = text_argument(interpreter, &mut args, 0, "string")?;
                let safe = match args.take(1, "safe") {
                    Some(value) => interpreter.to_str(&value)?,
                    None => "/".into(),
                };
                Ok(Value::str(quote_text(&text, &safe, false)))
            }),
        ),
        (
            "quote_plus",
            function("quote_plus", |interpreter, mut args| {
                let text = text_argument(interpreter, &mut args, 0, "string")?;
                let safe = match args.take(1, "safe") {
                    Some(value) => interpreter.to_str(&value)?,
                    None => String::new(),
                };
                Ok(Value::str(quote_text(&text, &safe, true)))
            }),
        ),
        (
            "unquote",
            function("unquote", |interpreter, mut args| {
                let text = text_argument(interpreter, &mut args, 0, "string")?;
                Ok(Value::str(unquote_text(&text, false)))
            }),
        ),
        (
            "unquote_plus",
            function("unquote_plus", |interpreter, mut args| {
                let text = text_argument(interpreter, &mut args, 0, "string")?;
                Ok(Value::str(unquote_text(&text, true)))
            }),
        ),
        (
            "urlencode",
            function("urlencode", |interpreter, args| {
                let query = args.get(0).cloned().unwrap_or(Value::None);
                let pairs = match query.plain() {
                    Value::Dict(_) => interpreter.mapping_items(&query)?,
                    _ => {
                        let mut pairs = Vec::new();
                        for item in interpreter.iterate(&query)? {
                            let parts = interpreter.iterate(&item)?;
                            pairs.push((
                                parts.first().cloned().unwrap_or(Value::None),
                                parts.get(1).cloned().unwrap_or(Value::None),
                            ));
                        }
                        pairs
                    }
                };
                let mut encoded = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let key = interpreter.to_str(&key)?;
                    let value = interpreter.to_str(&value)?;
                    encoded.push(format!(
                        "{}={}",
                        quote_text(&key, "", true),
                        quote_text(&value, "", true)
                    ));
                }
                Ok(Value::str(encoded.join("&")))
            }),
        ),
    ]
}

pub fn error() -> Vec<(&'static str, Value)> {
    let url_error = crate::types::library_exception("URLError", "urllib.error", "OSError");
    let http_error = crate::types::library_exception("HTTPError", "urllib.error", "URLError");
    vec![
        ("URLError", Value::Class(url_error)),
        ("HTTPError", Value::Class(http_error)),
        (
            "ContentTooShortError",
            Value::Class(crate::types::library_exception(
                "ContentTooShortError",
                "urllib.error",
                "URLError",
            )),
        ),
    ]
}

pub fn request() -> Vec<(&'static str, Value)> {
    let refuse = |name: &'static str| {
        (
            name,
            Value::builtin(name, move |_, _| {
                Err(Error::unsupported(format!(
                    "urllib.request.{name}: downloads while the workflow is planned"
                )))
            }),
        )
    };
    vec![
        refuse("urlopen"),
        refuse("urlretrieve"),
        refuse("Request"),
        refuse("build_opener"),
    ]
}

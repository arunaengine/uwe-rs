//! IRI handling for Schema Salad: joining references, fragments and file URLs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

/// The parts of an IRI reference, as RFC 3986 section 3 splits them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts<'a> {
    pub scheme: Option<&'a str>,
    pub authority: Option<&'a str>,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub fragment: Option<&'a str>,
}

impl<'a> Parts<'a> {
    pub fn parse(text: &'a str) -> Self {
        let (rest, fragment) = match text.split_once('#') {
            Some((rest, fragment)) => (rest, Some(fragment)),
            None => (text, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((rest, query)) => (rest, Some(query)),
            None => (rest, None),
        };
        let (scheme, rest) = match scheme_end(rest) {
            Some(end) => (Some(&rest[..end]), &rest[end + 1..]),
            None => (None, rest),
        };
        let (authority, path) = match rest.strip_prefix("//") {
            Some(after) => {
                let end = after.find('/').unwrap_or(after.len());
                (Some(&after[..end]), &after[end..])
            }
            None => (None, rest),
        };
        Self {
            scheme,
            authority,
            path,
            query,
            fragment,
        }
    }

    /// Writes the parts back into one IRI, as RFC 3986 section 5.3 does.
    pub fn compose(&self, fragment: Option<&str>) -> String {
        let mut text = String::new();
        if let Some(scheme) = self.scheme {
            text.push_str(scheme);
            text.push(':');
        }
        if let Some(authority) = self.authority {
            text.push_str("//");
            text.push_str(authority);
        }
        text.push_str(self.path);
        if let Some(query) = self.query {
            text.push('?');
            text.push_str(query);
        }
        if let Some(fragment) = fragment {
            text.push('#');
            text.push_str(fragment);
        }
        text
    }
}

/// The end of a valid scheme followed by `:`, so that `C:` or `a b:` are not taken as schemes.
fn scheme_end(text: &str) -> Option<usize> {
    let end = text.find(':')?;
    let scheme = &text[..end];
    let mut bytes = scheme.bytes();
    let first = bytes.next()?;
    let valid = first.is_ascii_alphabetic()
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'));
    (valid && !text[..end].contains('/')).then_some(end)
}

/// Whether the text starts with a scheme such as `file:` or `http:`.
pub fn has_scheme(text: &str) -> bool {
    Parts::parse(text).scheme.is_some()
}

/// The IRI without its fragment, and the fragment.
pub fn split_fragment(text: &str) -> (&str, Option<&str>) {
    match text.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (text, None),
    }
}

/// Resolves a reference against a base IRI, following RFC 3986 section 5.2.
pub fn join(base: &str, reference: &str) -> String {
    let base = Parts::parse(base);
    let target = Parts::parse(reference);
    if target.scheme.is_some() {
        let path = remove_dots(target.path);
        return Parts {
            path: &path,
            ..target
        }
        .compose(target.fragment);
    }
    let (authority, path, query) = if target.authority.is_some() {
        (target.authority, remove_dots(target.path), target.query)
    } else if target.path.is_empty() {
        (
            base.authority,
            base.path.to_owned(),
            target.query.or(base.query),
        )
    } else if target.path.starts_with('/') {
        (base.authority, remove_dots(target.path), target.query)
    } else {
        let merged = merge(&base, target.path);
        (base.authority, remove_dots(&merged), target.query)
    };
    let parts = Parts {
        scheme: base.scheme,
        authority,
        path: &path,
        query,
        fragment: None,
    };
    parts.compose(target.fragment)
}

fn merge(base: &Parts<'_>, path: &str) -> String {
    if base.authority.is_some() && base.path.is_empty() {
        return format!("/{path}");
    }
    match base.path.rfind('/') {
        Some(end) => format!("{}{path}", &base.path[..=end]),
        None => path.to_owned(),
    }
}

/// Removes `.` and `..` segments from a path, as RFC 3986 section 5.2.4 does.
fn remove_dots(path: &str) -> String {
    let mut output: Vec<&str> = Vec::new();
    let absolute = path.starts_with('/');
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    for (index, segment) in segments.iter().enumerate() {
        match *segment {
            "." => {
                if index == last {
                    output.push("");
                }
            }
            ".." => {
                if output.len() > usize::from(absolute) {
                    output.pop();
                }
                if index == last {
                    output.push("");
                }
            }
            _ => output.push(segment),
        }
    }
    let joined = output.join("/");
    if absolute && !joined.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The `file://` URL of an absolute path, with the characters that need it percent-encoded.
pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// The local path of a `file:` URL, with percent-encoding removed.
pub fn file_path(url: &str) -> Option<PathBuf> {
    let parts = Parts::parse(split_fragment(url).0);
    if parts.scheme != Some("file") || !matches!(parts.authority, None | Some("" | "localhost")) {
        return None;
    }
    Some(PathBuf::from(decode(parts.path)))
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes.get(index + 1..index + 3).and_then(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        });
        match (bytes[index], hex) {
            (b'%', Some(value)) => {
                output.push(value);
                index += 3;
            }
            (byte, _) => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
#[path = "iri_tests.rs"]
mod tests;

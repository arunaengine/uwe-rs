//! Public artifact projections, preserving the private records used for replay.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::Serialize;
use serde_json::Value;

/// Masks URL passwords in text while retaining the address's other spelling.
pub fn redact_public_text(text: &str) -> String {
    redacted(text).0
}

fn redacted(text: &str) -> (String, bool) {
    let mut password = false;
    let mut result = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let begin = rest[..at]
            .rfind(|c: char| !c.is_ascii_alphanumeric() && !"+-.".contains(c))
            .map_or(0, |at| {
                at + rest[at..].chars().next().expect("a character").len_utf8()
            });
        let authority = at + 3;
        let end = authority
            + rest[authority..]
                .find(|c: char| c.is_whitespace() || "/?#".contains(c))
                .unwrap_or(rest.len() - authority);
        result.push_str(&rest[..authority]);
        let server = &rest[authority..end];
        if rest[begin..at].starts_with(|c: char| c.is_ascii_alphabetic())
            && let Some((user, host)) = server.rsplit_once('@')
            && let Some((name, _)) = user.split_once(':')
        {
            password = true;
            result.push_str(name);
            result.push_str(":[secret]@");
            result.push_str(host);
        } else {
            result.push_str(server);
        }
        rest = &rest[end..];
    }
    result.push_str(rest);
    (result, password)
}

/// A stable public identity for a private credential-bearing resource.
pub fn public_resource_id(address: &str) -> String {
    if redacted(address).1 || address.starts_with("uwe-resource://") {
        format!(
            "uwe-resource://{}",
            task_model::Digest::sha256(address.as_bytes()).hex()
        )
    } else {
        address.to_owned()
    }
}

/// Serializes readable public values and stable resource keys without changing replay data.
pub fn public_json<T: Serialize>(value: &T) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(value)?;
    project(&mut value, None);
    Ok(value)
}

fn project(value: &mut Value, field: Option<&str>) {
    match value {
        Value::String(text) => {
            *text = if field == Some("@id") {
                public_resource_id(text)
            } else {
                redact_public_text(text)
            };
        }
        Value::Object(object) => {
            for (key, mut value) in std::mem::take(object) {
                project(&mut value, Some(&key));
                object.insert(public_resource_id(&key), value);
            }
        }
        Value::Array(array) => {
            for value in array {
                project(value, field);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_authorities_only() {
        for (private, public) in [
            (
                "ftp://user:password@host/file",
                "ftp://user:[secret]@host/file",
            ),
            (
                "ftp://u:p%40ss@[::1]:21?email=a:b@c#fragment@x",
                "ftp://u:[secret]@[::1]:21?email=a:b@c#fragment@x",
            ),
            (
                "curl 'https://u:password@host'",
                "curl 'https://u:[secret]@host'",
            ),
            ("ftp://host?email=a:b@c", "ftp://host?email=a:b@c"),
            ("ftp://user@host:21/file", "ftp://user@host:21/file"),
            (
                "https://host/a:b@c?x=1#user:pass@fragment",
                "https://host/a:b@c?x=1#user:pass@fragment",
            ),
            ("/local/user:password@file", "/local/user:password@file"),
        ] {
            assert_eq!(redact_public_text(private), public);
        }
    }

    #[test]
    fn preserves_public_identity() {
        use std::collections::BTreeMap;
        let first = "ftp://user:first-password@host/file";
        let second = "ftp://user:second-password@host/file";
        let occupied = public_resource_id(first);
        let producers: BTreeMap<&str, u32> =
            [(first, 1), (second, 2), (occupied.as_str(), 3)].into();
        let initial = public_json(&json!({"producers": producers})).expect("producer projection");
        let subset = public_json(
            &json!({"inputs": [{"location": first}], "input_ids": [public_resource_id(first)]}),
        )
        .expect("task projection");
        let producer_key = subset["input_ids"][0].as_str().expect("frontend identity");
        assert_eq!(initial["producers"][producer_key], 1);
        assert_eq!(
            subset["inputs"][0]["location"],
            "ftp://user:[secret]@host/file"
        );
        assert_eq!(initial["producers"].as_object().expect("map").len(), 3);
        assert_ne!(public_resource_id(first), public_resource_id(second));
        assert_ne!(public_resource_id(first), public_resource_id(&occupied));
        let reference = public_json(&json!({"@id": first})).expect("crate reference");
        assert_eq!(reference["@id"], producer_key);
        assert!(!initial.to_string().contains("password"));
        assert_eq!(
            public_json(&json!({"producers": producers})).expect("deterministic"),
            initial
        );
    }
}

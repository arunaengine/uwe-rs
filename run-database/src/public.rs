//! Public artifact projections, preserving the private records used for replay.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::Serialize;
use serde_json::Value;

/// Query parameters whose values grant access, like the signature of a presigned address.
const SECRET_PARAMETERS: [&str; 13] = [
    "X-Amz-Signature",
    "X-Amz-Credential",
    "X-Amz-Security-Token",
    "X-Goog-Signature",
    "X-Goog-Credential",
    "AWSAccessKeyId",
    "Signature",
    "sig",
    "token",
    "access_token",
    "api_key",
    "apikey",
    "key",
];

/// Masks URL passwords and the values of credential query parameters in text, while
/// retaining the address's other spelling.
pub fn redact_public_text(text: &str) -> String {
    redacted(text).0
}

/// The text with its secrets masked, and whether it held any.
fn redacted(text: &str) -> (String, bool) {
    let (text, password) = without_passwords(text);
    let (text, parameter) = without_parameters(&text);
    (text, password || parameter)
}

/// The values of the query parameters of [`SECRET_PARAMETERS`] as `[secret]`.
fn without_parameters(text: &str) -> (String, bool) {
    let mut masked = false;
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['?', '&']) {
        result.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        let Some((name, _)) = rest.split_once('=') else {
            continue;
        };
        if !(SECRET_PARAMETERS.iter()).any(|secret| secret.eq_ignore_ascii_case(name)) {
            continue;
        }
        let value = name.len() + 1;
        let end = (rest[value..].find(|c: char| c.is_whitespace() || "&#'\"<>".contains(c)))
            .map_or(rest.len(), |end| value + end);
        result.push_str(&rest[..value]);
        result.push_str("[secret]");
        masked = true;
        rest = &rest[end..];
    }
    result.push_str(rest);
    (result, masked)
}

fn without_passwords(text: &str) -> (String, bool) {
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
    fn masks_signed_queries() {
        for (private, public) in [
            (
                "https://b.s3.amazonaws.com/x?X-Amz-Credential=AKIA%2F1&X-Amz-Signature=abc123",
                "https://b.s3.amazonaws.com/x?X-Amz-Credential=[secret]&X-Amz-Signature=[secret]",
            ),
            (
                "curl 'https://host/f?sig=s3cr3t&sv=2022' -o x",
                "curl 'https://host/f?sig=[secret]&sv=2022' -o x",
            ),
            (
                "https://storage.googleapis.com/b/o?X-Goog-Signature=f00#part",
                "https://storage.googleapis.com/b/o?X-Goog-Signature=[secret]#part",
            ),
            (
                "https://api.example/v1?API_KEY=k1 and ?access_token=t2",
                "https://api.example/v1?API_KEY=[secret] and ?access_token=[secret]",
            ),
            (
                "https://host/f?page=2&monkey=1",
                "https://host/f?page=2&monkey=1",
            ),
        ] {
            assert_eq!(redact_public_text(private), public);
        }
        let signed = "https://host/f?token=abc";
        assert_ne!(
            public_resource_id(signed),
            signed,
            "a signed address is private"
        );
        assert_eq!(
            redact_public_text(&redact_public_text(signed)),
            "https://host/f?token=[secret]"
        );
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

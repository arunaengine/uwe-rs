//! Byte arrays, from `text.bytes`, with Groovy's `encodeBase64` and `encodeHex`, and
//! `java.security.MessageDigest` for SHA-2 digests of them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use sha2::Digest;

use crate::exception::Exception;
use crate::value::{Object, State, Value};

/// A new `byte[]`.
pub(in crate::interpreter) fn bytes(bytes: Vec<u8>) -> Value {
    Value::Object(Rc::new(RefCell::new(Object {
        class: "[B",
        state: State::Bytes(bytes.into()),
    })))
}

/// `MessageDigest.getInstance(algorithm)` for the SHA-2 algorithms.
pub(in crate::interpreter) fn message_digest(algorithm: &str) -> Result<Value, Exception> {
    let algorithm = match algorithm {
        "SHA-224" => "SHA-224",
        "SHA-256" => "SHA-256",
        "SHA-384" => "SHA-384",
        "SHA-512" => "SHA-512",
        other => {
            return Err(Exception::new(
                "NoSuchAlgorithmException",
                format!("{other} MessageDigest not available"),
            ));
        }
    };
    Ok(Value::Object(Rc::new(RefCell::new(Object {
        class: "java.security.MessageDigest",
        state: State::Digest(algorithm),
    }))))
}

/// Methods of a byte array.
pub(super) fn bytes_method(bytes: &[u8], name: &str, arguments: &[Value]) -> Option<Value> {
    Some(match (name, arguments) {
        ("encodeBase64", []) => Value::string(base64(bytes)),
        ("encodeHex", []) => {
            Value::string(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
        }
        ("size", []) => Value::integer(bytes.len() as i32),
        _ => return None,
    })
}

/// `digest(bytes)` of a message digest.
pub(super) fn digest_method(algorithm: &str, name: &str, arguments: &[Value]) -> Option<Value> {
    let ("digest", [Value::Object(input)]) = (name, arguments) else {
        return None;
    };
    let State::Bytes(input) = &input.borrow().state else {
        return None;
    };
    let digest = match algorithm {
        "SHA-224" => sha2::Sha224::digest(input).to_vec(),
        "SHA-384" => sha2::Sha384::digest(input).to_vec(),
        "SHA-512" => sha2::Sha512::digest(input).to_vec(),
        _ => sha2::Sha256::digest(input).to_vec(),
    };
    Some(bytes(digest))
}

/// The bytes of Base64 text, with or without padding, as `decodeBase64()` reads it.
pub(super) fn decode_base64(text: &str) -> Result<Vec<u8>, Exception> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let (mut group, mut count) = (0u32, 0);
    for character in text.trim_end_matches('=').bytes() {
        let Some(sextet) = ALPHABET.iter().position(|c| *c == character) else {
            let message = format!("Illegal base64 character {character:x}");
            return Err(Exception::illegal_argument(message));
        };
        group = (group << 6) | sextet as u32;
        count += 1;
        if count == 4 {
            bytes.extend([(group >> 16) as u8, (group >> 8) as u8, group as u8]);
            (group, count) = (0, 0);
        }
    }
    match count {
        2 => bytes.push((group >> 4) as u8),
        3 => bytes.extend([(group >> 10) as u8, (group >> 2) as u8]),
        _ => {}
    }
    Ok(bytes)
}

/// Base64 with padding, as `encodeBase64()` writes it.
pub(super) fn base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

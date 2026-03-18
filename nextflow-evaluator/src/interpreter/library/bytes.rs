//! Byte arrays, from `text.bytes`, with Groovy's `encodeBase64` and `encodeHex`, and
//! `java.security.MessageDigest` for SHA-2 digests of them.
// Copyright (c) 2026 The iunctus Contributors
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

/// Base64 with padding, as `encodeBase64()` writes it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |group, (index, byte)| {
            group | (u32::from(*byte) << (16 - 8 * index))
        });
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (group >> (18 - 6 * index)) & 0x3f;
                out.push(ALPHABET[sextet as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

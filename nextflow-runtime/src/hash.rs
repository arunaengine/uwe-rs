//! Encodes common Nextflow values for CacheHelper's Murmur3 ordering.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::UNIX_EPOCH;

use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

pub(crate) fn key(value: &Value, host: &mut dyn Host, deep: bool) -> Result<i64, Error> {
    let mut bytes = Vec::new();
    encode(value, host, deep, &mut bytes)?;
    Ok(i64::from_le_bytes(murmur(&bytes)[..8].try_into().unwrap()))
}

fn encode(
    value: &Value,
    host: &mut dyn Host,
    deep: bool,
    bytes: &mut Vec<u8>,
) -> Result<(), Error> {
    match value {
        Value::Null => {}
        Value::Boolean(value) => bytes.push(u8::from(*value)),
        Value::String(text) | Value::GString(text) => chars(text, bytes),
        Value::Number(number) => match number {
            Number::Integer(value) => bytes.extend(value.to_le_bytes()),
            Number::Long(value) => bytes.extend(value.to_le_bytes()),
            Number::Float(value) => bytes.extend(value.to_le_bytes()),
            Number::Double(value) => bytes.extend(value.to_le_bytes()),
            Number::BigInteger(_) | Number::BigDecimal(_) => chars(&number.to_string(), bytes),
        },
        Value::List(list) => {
            for value in &list.borrow().items {
                encode(value, host, deep, bytes)?;
            }
        }
        Value::Map(map) => {
            let mut sum = [0u8; 16];
            for (key, value) in &map.borrow().entries {
                let mut entry = Vec::new();
                encode(&key.0, host, deep, &mut entry)?;
                encode(value, host, deep, &mut entry)?;
                for (total, byte) in sum.iter_mut().zip(murmur(&entry)) {
                    *total = total.wrapping_add(byte);
                }
            }
            bytes.extend(sum);
        }
        Value::Path(path)
            if deep
                && host
                    .metadata(path)?
                    .is_some_and(|metadata| !metadata.directory) =>
        {
            bytes.extend(host.read(path)?);
        }
        Value::Path(path) if path.prefix.is_none() => {
            chars(&path.path, bytes);
            if let Ok(metadata) = std::fs::metadata(&path.path) {
                bytes.extend(metadata.len().to_le_bytes());
                if let Ok(modified) = metadata.modified() {
                    let millis = match modified.duration_since(UNIX_EPOCH) {
                        Ok(duration) => duration.as_millis() as i64,
                        Err(error) => -(error.duration().as_millis() as i64),
                    };
                    bytes.extend(millis.to_le_bytes());
                }
            }
        }
        _ => {
            return Err(Error::Unsupported {
                message: format!("CacheHelper hash sorting for {}", value.class_name()),
                span: Default::default(),
            });
        }
    }
    Ok(())
}

fn chars(text: &str, bytes: &mut Vec<u8>) {
    for unit in text.encode_utf16() {
        bytes.extend(unit.to_le_bytes());
    }
}

fn murmur(bytes: &[u8]) -> [u8; 16] {
    let mut first = 0u64;
    let mut second = 0u64;
    let blocks = bytes.chunks_exact(16);
    let tail = blocks.remainder();
    for block in blocks {
        let left = u64::from_le_bytes(block[..8].try_into().unwrap());
        let right = u64::from_le_bytes(block[8..].try_into().unwrap());
        first ^= mix_left(left);
        first = first
            .rotate_left(27)
            .wrapping_add(second)
            .wrapping_mul(5)
            .wrapping_add(0x52dce729);
        second ^= mix_right(right);
        second = second
            .rotate_left(31)
            .wrapping_add(first)
            .wrapping_mul(5)
            .wrapping_add(0x38495ab5);
    }
    let mut left = 0u64;
    let mut right = 0u64;
    for (index, byte) in tail.iter().enumerate() {
        if index < 8 {
            left |= (*byte as u64) << (index * 8);
        } else {
            right |= (*byte as u64) << ((index - 8) * 8);
        }
    }
    if tail.len() > 8 {
        second ^= mix_right(right);
    }
    if !tail.is_empty() {
        first ^= mix_left(left);
    }
    first ^= bytes.len() as u64;
    second ^= bytes.len() as u64;
    first = first.wrapping_add(second);
    second = second.wrapping_add(first);
    first = avalanche(first);
    second = avalanche(second);
    first = first.wrapping_add(second);
    second = second.wrapping_add(first);
    let mut result = [0u8; 16];
    result[..8].copy_from_slice(&first.to_le_bytes());
    result[8..].copy_from_slice(&second.to_le_bytes());
    result
}

fn mix_left(value: u64) -> u64 {
    value
        .wrapping_mul(0x87c37b91114253d5)
        .rotate_left(31)
        .wrapping_mul(0x4cf5ad432745937f)
}

fn mix_right(value: u64) -> u64 {
    value
        .wrapping_mul(0x4cf5ad432745937f)
        .rotate_left(33)
        .wrapping_mul(0x87c37b91114253d5)
}

fn avalanche(mut value: u64) -> u64 {
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51afd7ed558ccd);
    value ^= value >> 33;
    value = value.wrapping_mul(0xc4ceb9fe1a85ec53);
    value ^ (value >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Silent;
    impl Host for Silent {
        fn print(&mut self, _: &str) {}
    }

    #[test]
    fn matches_many_numbers() {
        let mut values: Vec<i32> = (0..64).collect();
        values.sort_by_key(|value| key(&Value::integer(*value), &mut Silent, false).unwrap());
        assert_eq!(
            values,
            [
                34, 20, 1, 55, 17, 21, 32, 29, 49, 47, 53, 39, 44, 25, 6, 46, 36, 30, 40, 15, 0, 2,
                13, 16, 23, 18, 43, 31, 63, 27, 59, 51, 24, 11, 58, 9, 37, 4, 57, 42, 33, 10, 61,
                62, 26, 14, 35, 54, 22, 3, 19, 48, 38, 45, 5, 12, 56, 28, 60, 52, 41, 8, 50, 7
            ]
        );
    }

    #[test]
    fn hashes_path_metadata() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("value");
        std::fs::write(&path, "content").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let value = Value::Path(
            nextflow_evaluator::value::PathValue::parse(&path.to_string_lossy(), "/").into(),
        );
        let encoded = Value::list(vec![
            Value::string(path.to_string_lossy()),
            Value::Number(Number::Long(metadata.len() as i64)),
            Value::Number(Number::Long(
                metadata
                    .modified()
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64,
            )),
        ]);
        assert_eq!(
            key(&value, &mut Silent, false).unwrap(),
            key(&encoded, &mut Silent, false).unwrap()
        );
        assert_eq!(
            key(&value, &mut Silent, true).unwrap(),
            i64::from_le_bytes(murmur(b"content")[..8].try_into().unwrap())
        );
    }

    #[test]
    fn matches_reference_hashes() {
        let cases = [
            (Value::string("a"), -7591212652331886403),
            (Value::string("b"), 315532781288749487),
            (Value::string("alpha"), 4469749617035339534),
            (Value::string("é"), -8822982382499129721),
            (Value::string("😀"), -3649230758951754398),
            (Value::string(""), 0),
            (Value::integer(0), -3485513579396041028),
            (Value::integer(1), -8604791237420463362),
            (Value::integer(-1), 4889297221962843713),
            (Value::integer(42), 2913627637088662735),
            (
                Value::Number(Number::parse_decimal("1.5").unwrap()),
                6827986931352894097,
            ),
            (Value::Number(Number::Long(1)), 19144387141682250),
            (Value::Number(Number::Long(2147483648)), 1998944299520109879),
            (Value::Boolean(true), 8849112093580131862),
            (Value::Boolean(false), 5048724184180415669),
            (Value::Number(Number::Double(1.5)), -981000774749194061),
            (Value::Number(Number::Float(1.5)), -5635888024348523178),
            (
                Value::list(vec![Value::integer(1), Value::integer(2)]),
                -3408911133500361912,
            ),
            (
                Value::map([
                    (Value::string("a"), Value::integer(1)),
                    (Value::string("b"), Value::integer(2)),
                ]),
                -3552940685092144342,
            ),
            (
                Value::map([
                    (Value::string("b"), Value::integer(2)),
                    (Value::string("a"), Value::integer(1)),
                ]),
                -3552940685092144342,
            ),
        ];
        for (value, expected) in cases {
            for deep in [false, true] {
                assert_eq!(key(&value, &mut Silent, deep).unwrap(), expected, "{value}");
            }
        }
    }
}

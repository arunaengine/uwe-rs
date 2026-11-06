//! Content identifiers (CIDv1 with SHA-256, base32 text) and strict DAG-CBOR, as the IPLD
//! specifications describe them, for the blocks of content-addressed stores.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use sha2::{Digest, Sha256};

/// The multicodec of file content.
pub const RAW: u64 = 0x55;
/// The multicodec of DAG-CBOR blocks.
pub const DAG_CBOR: u64 = 0x71;
const SHA2_256: u8 = 0x12;
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A CIDv1 whose multihash is SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cid {
    pub codec: u64,
    pub digest: [u8; 32],
}

impl Cid {
    /// The CID of bytes in a codec.
    pub fn of(codec: u64, bytes: &[u8]) -> Cid {
        Cid {
            codec,
            digest: Sha256::digest(bytes).into(),
        }
    }

    /// The binary form: version 1, the codec, then the multihash.
    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = vec![1];
        varint(self.codec, &mut bytes);
        bytes.extend([SHA2_256, 32]);
        bytes.extend(self.digest);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Cid> {
        let (version, rest) = read_varint(bytes)?;
        let (codec, rest) = read_varint(rest)?;
        match (version, rest) {
            (1, [SHA2_256, 32, digest @ ..]) => Some(Cid {
                codec,
                digest: digest.try_into().ok()?,
            }),
            _ => None,
        }
    }

    /// Parses the text form, multibase base32 lower case with the prefix `b`.
    pub fn parse(text: &str) -> Option<Cid> {
        let body = text.strip_prefix('b')?;
        let mut bytes = Vec::new();
        let (mut buffer, mut bits) = (0u32, 0);
        for character in body.bytes() {
            let value = ALPHABET.iter().position(|letter| *letter == character)? as u32;
            buffer = (buffer << 5) | value;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                bytes.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        Cid::from_bytes(&bytes)
    }
}

impl fmt::Display for Cid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = String::from("b");
        let (mut buffer, mut bits) = (0u32, 0);
        for byte in self.bytes() {
            buffer = (buffer << 8) | u32::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                text.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
            }
            buffer &= (1 << bits) - 1;
        }
        if bits > 0 {
            text.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
        }
        formatter.write_str(&text)
    }
}

/// A value of the IPLD data model.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Null,
    Bool(bool),
    Integer(i128),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    List(Vec<Node>),
    /// Entries in any order; encoding sorts them.
    Map(Vec<(String, Node)>),
    Link(Cid),
}

impl Node {
    /// The value of a key of a map.
    pub fn get(&self, key: &str) -> Option<&Node> {
        let Node::Map(entries) = self else {
            return None;
        };
        entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Node::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The block of the value and its CID.
    pub fn block(&self) -> Result<(Vec<u8>, Cid), String> {
        let mut bytes = Vec::new();
        encode(self, &mut bytes)?;
        let cid = Cid::of(DAG_CBOR, &bytes);
        Ok((bytes, cid))
    }
}

/// Encodes strict DAG-CBOR: smallest heads, definite lengths, 64-bit floats, map keys sorted by
/// length then bytes, links as tag 42.
pub fn encode(node: &Node, out: &mut Vec<u8>) -> Result<(), String> {
    match node {
        Node::Null => out.push(0xf6),
        Node::Bool(flag) => out.push(if *flag { 0xf5 } else { 0xf4 }),
        Node::Integer(number) if *number >= 0 => head(
            0,
            u64::try_from(*number).map_err(|_| "an integer is too large")?,
            out,
        ),
        Node::Integer(number) => {
            let magnitude = u64::try_from(-1 - *number).map_err(|_| "an integer is too small")?;
            head(1, magnitude, out)
        }
        Node::Float(number) if !number.is_finite() => {
            return Err("DAG-CBOR has no NaN or infinity".to_owned());
        }
        Node::Float(number) => {
            out.push(0xfb);
            out.extend(number.to_bits().to_be_bytes());
        }
        Node::Bytes(bytes) => {
            head(2, bytes.len() as u64, out);
            out.extend(bytes);
        }
        Node::Text(text) => {
            head(3, text.len() as u64, out);
            out.extend(text.as_bytes());
        }
        Node::List(items) => {
            head(4, items.len() as u64, out);
            for item in items {
                encode(item, out)?;
            }
        }
        Node::Map(entries) => {
            let mut sorted: Vec<&(String, Node)> = entries.iter().collect();
            sorted.sort_by(|(left, _), (right, _)| {
                (left.len(), left.as_bytes()).cmp(&(right.len(), right.as_bytes()))
            });
            if sorted.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return Err("a map has a key twice".to_owned());
            }
            head(5, sorted.len() as u64, out);
            for (key, value) in sorted {
                head(3, key.len() as u64, out);
                out.extend(key.as_bytes());
                encode(value, out)?;
            }
        }
        Node::Link(cid) => {
            head(6, 42, out);
            let mut bytes = vec![0];
            bytes.extend(cid.bytes());
            head(2, bytes.len() as u64, out);
            out.extend(bytes);
        }
    }
    Ok(())
}

/// Decodes one strict DAG-CBOR block, which must hold exactly one value.
pub fn decode(bytes: &[u8]) -> Result<Node, String> {
    let (node, rest) = item(bytes)?;
    match rest.is_empty() {
        true => Ok(node),
        false => Err("bytes follow the value of the block".to_owned()),
    }
}

fn item(bytes: &[u8]) -> Result<(Node, &[u8]), String> {
    let short = || "the block ends early".to_owned();
    let (&first, rest) = bytes.split_first().ok_or_else(short)?;
    let (major, info) = (first >> 5, first & 31);
    if major == 7 {
        return match info {
            20 => Ok((Node::Bool(false), rest)),
            21 => Ok((Node::Bool(true), rest)),
            22 => Ok((Node::Null, rest)),
            27 => {
                let bits = rest.get(..8).ok_or_else(short)?;
                let number =
                    f64::from_bits(u64::from_be_bytes(bits.try_into().map_err(|_| short())?));
                Ok((Node::Float(number), &rest[8..]))
            }
            _ => Err(format!("DAG-CBOR does not allow the simple value {info}")),
        };
    }
    let (argument, rest) = argument(info, rest)?;
    let length = usize::try_from(argument).map_err(|_| "a length is too large".to_owned())?;
    match major {
        0 => Ok((Node::Integer(i128::from(argument)), rest)),
        1 => Ok((Node::Integer(-1 - i128::from(argument)), rest)),
        2 => {
            let bytes = rest.get(..length).ok_or_else(short)?;
            Ok((Node::Bytes(bytes.to_vec()), &rest[length..]))
        }
        3 => {
            let bytes = rest.get(..length).ok_or_else(short)?;
            let text = String::from_utf8(bytes.to_vec()).map_err(|_| "a text is not UTF-8")?;
            Ok((Node::Text(text), &rest[length..]))
        }
        4 => {
            let (mut items, mut rest) = (Vec::new(), rest);
            for _ in 0..length {
                let (node, after) = item(rest)?;
                items.push(node);
                rest = after;
            }
            Ok((Node::List(items), rest))
        }
        5 => {
            let (mut entries, mut rest) = (Vec::new(), rest);
            for _ in 0..length {
                let (Node::Text(key), after) = item(rest)? else {
                    return Err("a map key is not text".to_owned());
                };
                let (value, after) = item(after)?;
                entries.push((key, value));
                rest = after;
            }
            if entries.iter().any(|(key, _)| key == "/") {
                return Err("a map with the key `/` is refused".to_owned());
            }
            Ok((Node::Map(entries), rest))
        }
        6 if argument == 42 => match item(rest)? {
            (Node::Bytes(bytes), rest) if bytes.first() == Some(&0) => {
                let cid = Cid::from_bytes(&bytes[1..]).ok_or("a link holds no CID")?;
                Ok((Node::Link(cid), rest))
            }
            _ => Err("a link holds no CID".to_owned()),
        },
        _ => Err(format!("DAG-CBOR does not allow the tag {argument}")),
    }
}

/// The argument of a head; indefinite lengths are refused.
fn argument(info: u8, rest: &[u8]) -> Result<(u64, &[u8]), String> {
    let size = match info {
        0..=23 => return Ok((u64::from(info), rest)),
        24 => 1,
        25 => 2,
        26 => 4,
        27 => 8,
        _ => return Err("DAG-CBOR does not allow indefinite lengths".to_owned()),
    };
    let bytes = rest.get(..size).ok_or("the block ends early")?;
    let value = bytes
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    Ok((value, &rest[size..]))
}

/// The smallest head of a major type and its argument.
fn head(major: u8, value: u64, out: &mut Vec<u8>) {
    let major = major << 5;
    match value {
        0..=23 => out.push(major | value as u8),
        24..=0xff => out.extend([major | 24, value as u8]),
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend((value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(major | 26);
            out.extend((value as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend(value.to_be_bytes());
        }
    }
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn read_varint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (at, byte) in bytes.iter().enumerate().take(9) {
        value |= u64::from(byte & 0x7f) << (7 * at);
        if byte & 0x80 == 0 {
            return Some((value, &bytes[at + 1..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_known_contents() {
        let cid = |codec, bytes: &[u8]| Cid::of(codec, bytes).to_string();
        assert_eq!(
            cid(RAW, b""),
            "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"
        );
        assert_eq!(
            cid(RAW, b"hello\n"),
            "bafkreicysg23kiwv34eg2d7qweipxwosdo2py4ldv42nbauguluen5v6am"
        );
        assert_eq!(
            cid(RAW, b"bam-A\n"),
            "bafkreif7gwanofb74dnqxnb5667ubt6si4johxp6om37lwtpzqslahruwm"
        );
        let (bytes, empty) = Node::Map(Vec::new()).block().expect("a block");
        assert_eq!(bytes, [0xa0]);
        assert_eq!(
            empty.to_string(),
            "bafyreigbtj4x7ip5legnfznufuopl4sg4knzc2cof6duas4b3q2fy6swua"
        );
        assert_eq!(Cid::parse(&empty.to_string()), Some(empty));
    }

    #[test]
    fn encodes_strictly() {
        let map = Node::Map(vec![
            ("bb".into(), Node::Integer(-500)),
            ("a".into(), Node::Float(1.5)),
            ("c".into(), Node::Integer(24)),
        ]);
        let mut out = Vec::new();
        encode(&map, &mut out).expect("encodes");
        let expected = [
            0xa3, 0x61, b'a', 0xfb, 0x3f, 0xf8, 0, 0, 0, 0, 0, 0, 0x61, b'c', 0x18, 24, 0x62, b'b',
            b'b', 0x39, 0x01, 0xf3,
        ];
        assert_eq!(out, expected);
        assert_eq!(
            decode(&out),
            Ok(Node::Map(vec![
                ("a".into(), Node::Float(1.5)),
                ("c".into(), Node::Integer(24)),
                ("bb".into(), Node::Integer(-500)),
            ]))
        );
        assert!(encode(&Node::Float(f64::NAN), &mut Vec::new()).is_err());
        assert!(
            decode(&[0x9f, 0xff]).is_err(),
            "indefinite lengths are refused"
        );
        assert!(decode(&[0xc1, 0x00]).is_err(), "other tags are refused");
        assert!(decode(&[0xa1, 0x61, b'/', 0xf6]).is_err());
    }
}

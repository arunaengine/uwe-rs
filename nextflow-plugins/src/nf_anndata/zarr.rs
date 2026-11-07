//! Reads the groups, attributes and one-dimensional arrays of local Zarr v2 and v3 stores.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;
use std::path::Path;

use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;
use serde_json::Value as Json;

use super::{Array, ArrayKind, Attribute, Failure, Node};

/// Whether a directory is the root group of a store.
pub(super) fn is_store(directory: &Path) -> bool {
    directory.join(".zgroup").is_file() || directory.join("zarr.json").is_file()
}

/// The direct children of a group that are Zarr nodes, without hidden names, by name.
pub(super) fn children(root: &Path, group: &str) -> Result<Vec<Node>, Failure> {
    let directory = root.join(group);
    let entries = std::fs::read_dir(&directory).map_err(|error| failed(&directory, error))?;
    let mut nodes = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| failed(&directory, error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if let Some(group) = node(&entry.path()) {
            nodes.push(Node { name, group });
        }
    }
    nodes.sort_by(|a, b| super::java_order(&a.name, &b.name));
    Ok(nodes)
}

/// Whether a directory is a group (`Some(true)`), an array (`Some(false)`) or no node.
fn node(directory: &Path) -> Option<bool> {
    if directory.join(".zgroup").is_file() {
        return Some(true);
    }
    if directory.join(".zarray").is_file() {
        return Some(false);
    }
    let metadata = read_json(&directory.join("zarr.json")).ok()?;
    Some(metadata.get("node_type").and_then(Json::as_str) == Some("group"))
}

pub(super) fn attribute(
    root: &Path,
    group: &str,
    name: &str,
) -> Result<Option<Attribute>, Failure> {
    let directory = root.join(group);
    let attributes = match directory.join("zarr.json").is_file() {
        true => read_json(&directory.join("zarr.json"))?
            .get("attributes")
            .cloned()
            .unwrap_or(Json::Null),
        false if directory.join(".zattrs").is_file() => read_json(&directory.join(".zattrs"))?,
        false => Json::Null,
    };
    Ok(attributes.get(name).map(|value| match value {
        Json::String(text) => Attribute::Text(text.clone()),
        Json::Array(items) if items.iter().all(Json::is_string) => Attribute::Texts(
            (items.iter().filter_map(Json::as_str))
                .map(str::to_owned)
                .collect(),
        ),
        _ => Attribute::Other,
    }))
}

/// How the elements of an array are stored.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Dtype {
    Bool,
    Integer {
        size: usize,
        signed: bool,
        big: bool,
    },
    Float {
        size: usize,
        big: bool,
    },
    /// Variable-length UTF-8 text.
    Text,
    /// Fixed-length UTF-32 text of so many characters.
    Unicode(usize, bool),
    /// Fixed-length bytes.
    Bytes(usize),
}

/// A compressor or a checksum over the bytes of a chunk.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Codec {
    Blosc,
    Zstd,
    Gzip,
    Zlib,
    Checksum,
}

/// What reading an array needs from its metadata.
struct Meta {
    length: usize,
    chunk: usize,
    dtype: Dtype,
    fill: Json,
    /// The byte codecs in the order they encoded.
    codecs: Vec<Codec>,
    /// The name of a chunk file before its number, like `c/` in Zarr v3.
    prefix: String,
}

/// The elements of an array, flattened, as the plugin gives them for Zarr stores: booleans
/// as booleans, and unsigned bytes as signed ones.
pub(super) fn array(root: &Path, path: &str) -> Result<Array, Failure> {
    let directory = root.join(path);
    let meta = match directory.join("zarr.json").is_file() {
        true => version_three(&read_json(&directory.join("zarr.json"))?),
        false => version_two(&read_json(&directory.join(".zarray"))?),
    }
    .map_err(|message| {
        Failure::argument(format!("Cannot read the Zarr array '{path}': {message}"))
    })?;
    let mut values = Vec::with_capacity(meta.length);
    let chunks = meta.length.div_ceil(meta.chunk.max(1));
    for chunk in 0..chunks {
        let wanted = meta.chunk.min(meta.length - chunk * meta.chunk);
        let file = directory.join(format!("{}{chunk}", meta.prefix));
        let decoded = match std::fs::read(&file) {
            Ok(bytes) => decode(&meta, bytes, wanted),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(vec![fill(&meta); wanted])
            }
            Err(error) => return Err(failed(&file, error)),
        };
        let decoded = decoded.map_err(|message| {
            Failure::argument(format!(
                "Cannot read the Zarr chunk {}: {message}",
                file.display()
            ))
        })?;
        values.extend(decoded.into_iter().take(wanted));
    }
    let kind = match meta.dtype {
        Dtype::Text | Dtype::Unicode(..) | Dtype::Bytes(_) => ArrayKind::Text,
        Dtype::Bool => ArrayKind::Other("[Z"),
        Dtype::Integer { size, .. } => super::integer_kind(size),
        Dtype::Float { size: 4, .. } => ArrayKind::Other("[F"),
        Dtype::Float { .. } => ArrayKind::Other("[D"),
    };
    Ok(Array { kind, values })
}

/// The metadata of a `.zarray` file.
fn version_two(meta: &Json) -> Result<Meta, String> {
    let (length, chunk) = extent(meta.get("shape"), meta.get("chunks"))?;
    let filters = meta.get("filters").and_then(Json::as_array);
    let filter_ids = filters
        .into_iter()
        .flatten()
        .filter_map(|filter| filter["id"].as_str());
    let mut text = false;
    for id in filter_ids {
        match id {
            "vlen-utf8" => text = true,
            other => return Err(format!("the filter {other} is not supported")),
        }
    }
    let dtype = match (meta["dtype"].as_str(), text) {
        (_, true) => Dtype::Text,
        (Some(dtype), false) => numpy_dtype(dtype)?,
        (None, false) => return Err("the data type is not a string".into()),
    };
    let codecs = match &meta["compressor"] {
        Json::Null => Vec::new(),
        compressor => vec![codec(compressor["id"].as_str().unwrap_or_default())?],
    };
    Ok(Meta {
        length,
        chunk,
        dtype,
        fill: meta["fill_value"].clone(),
        codecs,
        prefix: String::new(),
    })
}

/// The metadata of the `zarr.json` file of an array.
fn version_three(meta: &Json) -> Result<Meta, String> {
    let shape = meta.get("shape");
    let chunk_shape = meta.pointer("/chunk_grid/configuration/chunk_shape");
    let (length, chunk) = extent(shape, chunk_shape)?;
    let mut dtype = match meta["data_type"].as_str().unwrap_or_default() {
        "bool" => Dtype::Bool,
        "string" => Dtype::Text,
        name => {
            let (kind, bits) = name.split_at(name.find(|c: char| c.is_ascii_digit()).unwrap_or(0));
            let size = bits
                .parse::<usize>()
                .map_err(|_| format!("the data type {name}"))?
                / 8;
            match kind {
                "int" => Dtype::Integer {
                    size,
                    signed: true,
                    big: false,
                },
                "uint" => Dtype::Integer {
                    size,
                    signed: false,
                    big: false,
                },
                "float" => Dtype::Float { size, big: false },
                _ => return Err(format!("the data type {name} is not supported")),
            }
        }
    };
    let mut codecs = Vec::new();
    for entry in meta["codecs"].as_array().into_iter().flatten() {
        match entry["name"].as_str().unwrap_or_default() {
            "bytes" => {
                let big = entry
                    .pointer("/configuration/endian")
                    .and_then(Json::as_str)
                    == Some("big");
                dtype = match dtype {
                    Dtype::Integer { size, signed, .. } => Dtype::Integer { size, signed, big },
                    Dtype::Float { size, .. } => Dtype::Float { size, big },
                    other => other,
                };
            }
            "vlen-utf8" => dtype = Dtype::Text,
            "transpose" => {}
            "crc32c" => codecs.push(Codec::Checksum),
            name => codecs.push(codec(name)?),
        }
    }
    let prefix = match meta
        .pointer("/chunk_key_encoding/name")
        .and_then(Json::as_str)
    {
        Some("v2") => String::new(),
        _ => {
            let separator = meta.pointer("/chunk_key_encoding/configuration/separator");
            format!("c{}", separator.and_then(Json::as_str).unwrap_or("/"))
        }
    };
    Ok(Meta {
        length,
        chunk,
        dtype,
        fill: meta["fill_value"].clone(),
        codecs,
        prefix,
    })
}

/// The length and chunk length of a one-dimensional array; a scalar is one element.
fn extent(shape: Option<&Json>, chunks: Option<&Json>) -> Result<(usize, usize), String> {
    let numbers = |value: Option<&Json>| -> Vec<usize> {
        let items = value.and_then(Json::as_array).into_iter().flatten();
        items.filter_map(Json::as_u64).map(|n| n as usize).collect()
    };
    match (numbers(shape).as_slice(), numbers(chunks).as_slice()) {
        ([], _) => Ok((1, 1)),
        ([length], [chunk]) => Ok((*length, (*chunk).max(1))),
        _ => Err("only one-dimensional arrays are supported".into()),
    }
}

/// A NumPy type string like `<i8`, `|b1`, `<U5` or `|O`.
fn numpy_dtype(text: &str) -> Result<Dtype, String> {
    let mut characters = text.chars();
    let big = characters.next() == Some('>');
    let kind = characters.next();
    let size: usize = characters.as_str().parse().unwrap_or(0);
    Ok(match kind {
        Some('b') => Dtype::Bool,
        Some('i') => Dtype::Integer {
            size,
            signed: true,
            big,
        },
        Some('u') => Dtype::Integer {
            size,
            signed: false,
            big,
        },
        Some('f') => Dtype::Float { size, big },
        Some('U') => Dtype::Unicode(size, big),
        Some('S') => Dtype::Bytes(size),
        _ => return Err(format!("the data type {text} is not supported")),
    })
}

fn codec(name: &str) -> Result<Codec, String> {
    match name {
        "blosc" => Ok(Codec::Blosc),
        "zstd" => Ok(Codec::Zstd),
        "gzip" => Ok(Codec::Gzip),
        "zlib" => Ok(Codec::Zlib),
        other => Err(format!("the codec {other} is not supported")),
    }
}

/// The elements of one chunk.
fn decode(meta: &Meta, mut bytes: Vec<u8>, wanted: usize) -> Result<Vec<Value>, String> {
    for codec in meta.codecs.iter().rev() {
        bytes = match codec {
            Codec::Blosc => super::blosc::decompress(&bytes)?,
            Codec::Zstd => zstd::stream::decode_all(bytes.as_slice()).map_err(|e| e.to_string())?,
            Codec::Gzip => read_all(flate2::read::GzDecoder::new(bytes.as_slice()))?,
            Codec::Zlib => read_all(flate2::read::ZlibDecoder::new(bytes.as_slice()))?,
            Codec::Checksum => bytes[..bytes.len().saturating_sub(4)].to_vec(),
        };
    }
    let values = match meta.dtype {
        Dtype::Text => texts(&bytes)?,
        dtype => (bytes.chunks_exact(element_size(dtype)).take(wanted))
            .map(|element| element_value(dtype, element))
            .collect(),
    };
    match values.len() < wanted {
        true => Err(format!(
            "the chunk holds {} elements, {wanted} are wanted",
            values.len()
        )),
        false => Ok(values),
    }
}

fn read_all(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .read_to_end(&mut output)
        .map_err(|error| error.to_string())?;
    Ok(output)
}

/// The items of a chunk in numcodecs' `vlen-utf8` layout: a count, then each length and text.
fn texts(bytes: &[u8]) -> Result<Vec<Value>, String> {
    let word = |at: usize| -> Result<usize, String> {
        let slice = bytes
            .get(at..at + 4)
            .ok_or("a vlen-utf8 chunk is truncated")?;
        Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]) as usize)
    };
    let count = word(0)?;
    let mut position = 4;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let length = word(position)?;
        position += 4;
        let text = bytes
            .get(position..position + length)
            .ok_or("a vlen-utf8 chunk is truncated")?;
        items.push(Value::string(String::from_utf8_lossy(text)));
        position += length;
    }
    Ok(items)
}

fn element_size(dtype: Dtype) -> usize {
    match dtype {
        Dtype::Bool | Dtype::Text => 1,
        Dtype::Integer { size, .. } | Dtype::Float { size, .. } | Dtype::Bytes(size) => size.max(1),
        Dtype::Unicode(length, _) => (length * 4).max(1),
    }
}

/// One element; unsigned bytes become Java's signed `byte`, as in the plugin.
fn element_value(dtype: Dtype, bytes: &[u8]) -> Value {
    match dtype {
        Dtype::Bool => Value::Boolean(bytes[0] != 0),
        Dtype::Integer { size: 1, .. } => Value::integer(i32::from(bytes[0] as i8)),
        Dtype::Integer { size, signed, big } => {
            let value = super::integer(bytes, big, signed);
            match (size, signed) {
                (2, _) | (4, true) => Value::integer(value as i32),
                _ => Value::Number(Number::Long(value)),
            }
        }
        Dtype::Float { big, .. } => Value::Number(Number::Double(super::float(bytes, big))),
        Dtype::Unicode(_, big) => {
            let units = bytes.as_chunks::<4>().0.iter().map(|unit| match big {
                true => u32::from_be_bytes(*unit),
                false => u32::from_le_bytes(*unit),
            });
            let text: String = (units.take_while(|unit| *unit != 0))
                .filter_map(char::from_u32)
                .collect();
            Value::string(text)
        }
        Dtype::Bytes(_) => {
            let end = bytes
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(bytes.len());
            Value::string(String::from_utf8_lossy(&bytes[..end]))
        }
        Dtype::Text => Value::Null,
    }
}

/// The value of the elements of a chunk that has no file.
fn fill(meta: &Meta) -> Value {
    match (meta.dtype, &meta.fill) {
        (Dtype::Text | Dtype::Unicode(..) | Dtype::Bytes(_), Json::String(text)) => {
            Value::string(text.as_str())
        }
        (Dtype::Text | Dtype::Unicode(..) | Dtype::Bytes(_), _) => Value::string(""),
        (Dtype::Bool, fill) => Value::Boolean(fill.as_bool().unwrap_or(false)),
        (dtype, fill) => {
            let size = element_size(dtype);
            let bytes = match dtype {
                Dtype::Float { size: 4, .. } => {
                    (fill.as_f64().unwrap_or(0.0) as f32).to_le_bytes().to_vec()
                }
                Dtype::Float { .. } => fill.as_f64().unwrap_or(0.0).to_le_bytes().to_vec(),
                _ => fill.as_i64().unwrap_or(0).to_le_bytes()[..size.min(8)].to_vec(),
            };
            let little = match dtype {
                Dtype::Integer { size, signed, .. } => Dtype::Integer {
                    size,
                    signed,
                    big: false,
                },
                Dtype::Float { size, .. } => Dtype::Float { size, big: false },
                other => other,
            };
            element_value(little, &bytes)
        }
    }
}

fn read_json(file: &Path) -> Result<Json, Failure> {
    let text = std::fs::read(file).map_err(|error| failed(file, error))?;
    serde_json::from_slice(&text)
        .map_err(|error| Failure::argument(format!("Cannot read {}: {error}", file.display())))
}

fn failed(file: &Path, error: std::io::Error) -> Failure {
    Failure::argument(format!("Cannot read {}: {error}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::{Dtype, Meta, decode, element_value, numpy_dtype, texts};

    #[test]
    fn rejects_short_chunks() {
        let meta = Meta {
            length: 4,
            chunk: 4,
            dtype: numpy_dtype("<i4").unwrap(),
            fill: serde_json::Value::Null,
            codecs: Vec::new(),
            prefix: String::new(),
        };
        let bytes: Vec<u8> = [1i32, 2, 3].iter().flat_map(|n| n.to_le_bytes()).collect();
        let short = decode(&meta, bytes.clone(), 4).err();
        assert_eq!(short.as_deref(), Some("the chunk holds 3 elements, 4 are wanted"));
        assert_eq!(decode(&meta, bytes, 3).map(|values| values.len()), Ok(3));
    }

    #[test]
    fn reads_vlen_texts() {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for text in ["a", "βγ"] {
            bytes.extend((text.len() as u32).to_le_bytes());
            bytes.extend(text.as_bytes());
        }
        let items: Vec<String> = texts(&bytes)
            .unwrap()
            .iter()
            .map(|v| v.to_string())
            .collect();
        assert_eq!(items, ["a", "βγ"]);
    }

    #[test]
    fn signs_unsigned_bytes() {
        let dtype = numpy_dtype("|u1").unwrap();
        assert_eq!(element_value(dtype, &[175]).to_string(), "-81");
        assert_eq!(
            numpy_dtype("<i8").unwrap(),
            Dtype::Integer {
                size: 8,
                signed: true,
                big: false
            }
        );
    }
}

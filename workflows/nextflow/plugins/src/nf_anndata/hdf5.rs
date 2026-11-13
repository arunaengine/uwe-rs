//! Reads the groups, attributes and arrays of `.h5ad` files, the HDF5 form of AnnData.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use hdf5_pure::{AttrValue, Datatype, DatatypeByteOrder, File};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

use super::{Array, ArrayKind, Attribute, Failure, Node};

/// Opens a file, which must have an HDF5 signature, for reads of the parts that are asked for.
pub(super) fn open(path: &Path) -> Result<File, Failure> {
    match hdf5_pure::is_hdf5(path) {
        Ok(true) => File::open_streaming(path).map_err(failed),
        Ok(false) => Err(Failure::hdf("No valid HDF5 signature found")),
        Err(error) => Err(Failure::hdf(error.to_string())),
    }
}

/// The direct children of a group, `""` for the root, in name order as HDF5 lists links.
pub(super) fn children(file: &File, group: &str) -> Result<Vec<Node>, Failure> {
    let group = match group {
        "" => file.root(),
        path => file.group(path).map_err(failed)?,
    };
    let groups = group.groups().map_err(failed)?;
    let datasets = group.datasets().map_err(failed)?;
    let mut nodes: Vec<Node> = (groups.into_iter().map(|name| Node { name, group: true }))
        .chain(datasets.into_iter().map(|name| Node { name, group: false }))
        .collect();
    nodes.sort_by(|a, b| super::java_order(&a.name, &b.name));
    Ok(nodes)
}

pub(super) fn attribute(
    file: &File,
    group: &str,
    name: &str,
) -> Result<Option<Attribute>, Failure> {
    let attributes = file.group(group).map_err(failed)?.attrs().map_err(failed)?;
    Ok(attributes.get(name).map(|value| match value {
        AttrValue::String(text) | AttrValue::AsciiString(text) | AttrValue::VarLenString(text) => {
            Attribute::Text(text.clone())
        }
        AttrValue::StringSized { value, .. } | AttrValue::AsciiStringSized { value, .. } => {
            Attribute::Text(value.clone())
        }
        AttrValue::StringArray(texts)
        | AttrValue::AsciiStringArray(texts)
        | AttrValue::VarLenStringArray(texts)
        | AttrValue::VarLenAsciiCharArray(texts) => Attribute::Texts(texts.clone()),
        AttrValue::StringArraySized { values, .. }
        | AttrValue::AsciiStringArraySized { values, .. } => Attribute::Texts(values.clone()),
        _ => Attribute::Other,
    }))
}

/// The elements of a dataset, flattened, as the plugin gives them for `.h5ad` files: enums,
/// like the booleans of h5py, by the names of their members, and unsigned bytes as 0 to 255.
pub(super) fn array(file: &File, path: &str) -> Result<Array, Failure> {
    let dataset = file.dataset(path).map_err(failed)?;
    let datatype = dataset.datatype().map_err(failed)?;
    let texts = |texts: Vec<String>| Array {
        kind: ArrayKind::Text,
        values: texts.into_iter().map(Value::string).collect(),
    };
    match datatype {
        Datatype::String { .. }
        | Datatype::VariableLength {
            is_string: true, ..
        } => Ok(texts(dataset.read_string().map_err(failed)?)),
        Datatype::Enumeration { size, members, .. } => {
            let raw = dataset.read_raw().map_err(failed)?;
            let names = raw.chunks(size.max(1) as usize).map(|bytes| {
                let member = members.iter().find(|member| member.value == bytes);
                member.map_or_else(String::new, |member| member.name.clone())
            });
            Ok(texts(names.collect()))
        }
        Datatype::FixedPoint {
            size,
            byte_order,
            layout,
        } => {
            let raw = dataset.read_raw().map_err(failed)?;
            let big = byte_order == DatatypeByteOrder::BigEndian;
            let values = raw.chunks(size.max(1) as usize).map(|bytes| {
                let value = super::integer(bytes, big, layout.signed);
                match (size, layout.signed) {
                    (1 | 2, _) | (4, true) => Value::integer(value as i32),
                    _ => Value::Number(Number::Long(value)),
                }
            });
            Ok(Array {
                kind: super::integer_kind(size as usize),
                values: values.collect(),
            })
        }
        Datatype::FloatingPoint {
            size, byte_order, ..
        } => {
            let raw = dataset.read_raw().map_err(failed)?;
            let big = byte_order == DatatypeByteOrder::BigEndian;
            let values = raw
                .chunks(size.max(1) as usize)
                .map(|bytes| Value::Number(Number::Double(super::float(bytes, big))));
            Ok(Array {
                kind: ArrayKind::Other(if size == 4 { "[F" } else { "[D" }),
                values: values.collect(),
            })
        }
        other => Err(Failure::argument(format!(
            "Unsupported data type at '{path}': {other:?}"
        ))),
    }
}

fn failed(error: hdf5_pure::Error) -> Failure {
    Failure::hdf(error.to_string())
}

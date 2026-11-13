//! nf-anndata 0.5.3: `anndata(path)` reads the frames and names of an `.h5ad` or Zarr store.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod blosc;
mod hdf5;
mod objects;
mod zarr;

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{PathValue, Value};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["anndata"],
    call: Some(call),
    methods: Some(objects::methods),
    ..Plugin::new("nf-anndata", Kind::Functions, State::Implemented)
};

/// The groups that the root of every store has, in the order that errors name them.
const REQUIRED: [&str; 8] = [
    "layers", "obs", "var", "obsm", "varm", "obsp", "varp", "uns",
];
/// The groups whose children count only by name, in the order of the `yaml` map.
const NAMED: [&str; 6] = ["layers", "obsm", "varm", "obsp", "varp", "uns"];

/// A failure with the Java exception class that the plugin throws.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct Failure {
    class: &'static str,
    message: String,
}

impl Failure {
    fn argument(message: impl Into<String>) -> Failure {
        Failure {
            class: "IllegalArgumentException",
            message: message.into(),
        }
    }

    /// The `HdfException` of the jHDF library that the plugin reads `.h5ad` files with.
    fn hdf(message: impl Into<String>) -> Failure {
        Failure {
            class: "HdfException",
            message: message.into(),
        }
    }
}

impl From<Failure> for Error {
    fn from(failure: Failure) -> Error {
        Error::Thrown {
            exception: Rc::new(Exception::new(failure.class, failure.message)),
            span: Default::default(),
        }
    }
}

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let (["anndata"], [target]) = ([name], arguments.as_slice()) else {
        return Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-anndata` with these arguments"
            ),
            span: Default::default(),
        });
    };
    // A string is a local path, even when it looks like a URL.
    let path = match target {
        Value::Path(path) => (**path).clone(),
        other => {
            let launch = PathBuf::from(context.host.launch_dir());
            PathValue::parse(&launch.join(other.to_string()).to_string_lossy(), "/")
        }
    };
    let store = Rc::new(open(&path, context)?);
    Ok(objects::Anndata::Data(Rc::new(read(&store)?)).value())
}

/// A node of a store: its name and whether it is a group.
struct Node {
    name: String,
    group: bool,
}

/// An attribute of a group, as far as the plugin reads attributes.
enum Attribute {
    Text(String),
    Texts(Vec<String>),
    Other,
}

/// The elements of an array, with the kind that decides whether it can be an index.
struct Array {
    kind: ArrayKind,
    values: Vec<Value>,
}

enum ArrayKind {
    Text,
    /// Integers of 32 or 64 bits.
    Integer,
    /// Any other elements, by the name of the Java array class.
    Other(&'static str),
}

fn integer_kind(size: usize) -> ArrayKind {
    match size {
        1 => ArrayKind::Other("[B"),
        2 => ArrayKind::Other("[S"),
        _ => ArrayKind::Integer,
    }
}

/// An integer of one to eight little-endian or big-endian bytes, sign-extended when signed.
fn integer(bytes: &[u8], big: bool, signed: bool) -> i64 {
    let mut ordered = bytes.to_vec();
    if big {
        ordered.reverse();
    }
    let negative = signed && ordered.last().is_some_and(|byte| byte & 0x80 != 0);
    let mut buffer = [if negative { 0xff } else { 0 }; 8];
    for (slot, byte) in buffer.iter_mut().zip(ordered) {
        *slot = byte;
    }
    i64::from_le_bytes(buffer)
}

/// A float of four or eight bytes, widened to a `double`.
fn float(bytes: &[u8], big: bool) -> f64 {
    let mut ordered = bytes.to_vec();
    if big {
        ordered.reverse();
    }
    match ordered.as_slice() {
        [a, b, c, d] => f64::from(f32::from_le_bytes([*a, *b, *c, *d])),
        [a, b, c, d, e, f, g, h] => f64::from_le_bytes([*a, *b, *c, *d, *e, *f, *g, *h]),
        _ => f64::NAN,
    }
}

/// Java's order of strings, by UTF-16 code units.
fn java_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// An open store.
#[derive(Debug)]
enum Store {
    Hdf5(hdf5_pure::File),
    /// The root directory of a local Zarr store.
    Zarr(PathBuf),
}

impl Store {
    fn children(&self, group: &str) -> Result<Vec<Node>, Failure> {
        match self {
            Store::Hdf5(file) => hdf5::children(file, group),
            Store::Zarr(root) => zarr::children(root, group),
        }
    }

    fn attribute(&self, group: &str, name: &str) -> Result<Option<Attribute>, Failure> {
        match self {
            Store::Hdf5(file) => hdf5::attribute(file, group, name),
            Store::Zarr(root) => zarr::attribute(root, group, name),
        }
    }

    fn array(&self, path: &str) -> Result<Array, Failure> {
        match self {
            Store::Hdf5(file) => hdf5::array(file, path),
            Store::Zarr(root) => zarr::array(root, path),
        }
    }

    /// The group as error messages name it: its path in a file, its URI in a Zarr store.
    fn label(&self, group: &str) -> String {
        match self {
            Store::Hdf5(_) => group.into(),
            Store::Zarr(root) => format!("file://{}/{group}", root.display()),
        }
    }
}

/// Opens a store by the last element of its path: `.h5ad` files, else Zarr directories.
fn open(path: &PathValue, context: &mut Context) -> Result<Store, Failure> {
    let name = path.name().to_owned();
    let shown = match path.prefix {
        Some(_) => path.uri(),
        None => path.path.clone(),
    };
    if path.prefix.is_some() && name.ends_with(".zarr") {
        return Err(Failure {
            class: "UnsupportedOperationException",
            message: format!(
                "Remote Zarr stores are not supported. Use a local .zarr directory instead: {shown}"
            ),
        });
    }
    if name.ends_with(".h5ad") {
        let message = format!("Failed to open file '{shown}' . Is it a HDF5 file?");
        let local = match path.prefix {
            Some(_) => context.host.download(path).ok(),
            None => Some(PathBuf::from(&path.path)),
        };
        let local = local.filter(|local| local.is_file());
        return Ok(Store::Hdf5(hdf5::open(
            &local.ok_or(Failure::hdf(message))?,
        )?));
    }
    let directory = PathBuf::from(&path.path);
    if path.prefix.is_none() && (name.ends_with(".zarr") || zarr::is_store(&directory)) {
        return match zarr::is_store(&directory) {
            true => Ok(Store::Zarr(directory)),
            false => Err(Failure::argument(format!(
                "Failed to open Zarr group: {shown}"
            ))),
        };
    }
    Err(Failure::argument(format!(
        "Unsupported AnnData storage format (expected .h5ad file or .zarr directory): {shown}"
    )))
}

/// What `anndata()` reads at once: the frames and the names of the other groups.
#[derive(Debug)]
struct Data {
    obs: Rc<Frame>,
    var: Rc<Frame>,
    /// The children of the groups of `NAMED`, in that order.
    names: Vec<Vec<String>>,
}

/// The `obs` or `var` data frame: its index and column names; columns are read when asked.
#[derive(Debug)]
struct Frame {
    store: Rc<Store>,
    group: &'static str,
    label: String,
    index: String,
    columns: Vec<String>,
    rows: Rc<[String]>,
}

fn read(store: &Rc<Store>) -> Result<Data, Failure> {
    let root = store.children("")?;
    let missing: Vec<&str> = (REQUIRED.iter().copied())
        .filter(|name| !root.iter().any(|node| node.name == *name))
        .collect();
    if !missing.is_empty() {
        let message = format!("Missing fields: [{}]", missing.join(", "));
        return Err(Failure::argument(message));
    }
    for name in REQUIRED {
        if root.iter().any(|node| node.name == name && !node.group) {
            return Err(Failure::argument(format!("Expected group at '{name}'")));
        }
    }
    let obs = Rc::new(frame(store, "obs")?);
    let var = Rc::new(frame(store, "var")?);
    let mut names = Vec::new();
    for group in NAMED {
        let children = store.children(group)?;
        names.push(children.into_iter().map(|node| node.name).collect());
    }
    Ok(Data { obs, var, names })
}

fn frame(store: &Rc<Store>, group: &'static str) -> Result<Frame, Failure> {
    let label = store.label(group);
    let index = match store.attribute(group, "_index")? {
        Some(Attribute::Text(name)) => name,
        _ => "_index".into(),
    };
    let children = store.children(group)?;
    let columns = match store.attribute(group, "column-order")? {
        Some(Attribute::Texts(names)) => names,
        _ => {
            let mut names: Vec<String> = (children.iter())
                .filter(|node| node.name != index)
                .map(|node| node.name.clone())
                .collect();
            names.sort_by(|a, b| java_order(a, b));
            names
        }
    };
    let Some(node) = children.iter().find(|node| node.name == index) else {
        let message = format!("Index '{index}' not found in group: {label}");
        return Err(Failure::argument(message));
    };
    let path = format!("{group}/{index}");
    let text = |value: Value| match value {
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let rows: Vec<String> = match node.group {
        false => {
            let array = store.array(&path)?;
            match array.kind {
                _ if array.values.is_empty() => Vec::new(),
                ArrayKind::Text | ArrayKind::Integer => {
                    array.values.into_iter().map(text).collect()
                }
                ArrayKind::Other(class) => {
                    let message = format!("Unsupported index data type: {class}");
                    return Err(Failure::argument(message));
                }
            }
        }
        true => {
            let encoded = encoded(store, &path, "Unknown index group structure with keys")?;
            encoded.into_iter().map(text).collect()
        }
    };
    Ok(Frame {
        store: store.clone(),
        group,
        label,
        index,
        columns,
        rows: rows.into(),
    })
}

/// The values of a column: an array, a categorical group or a nullable group.
fn column(frame: &Frame, name: &str) -> Result<Vec<Value>, Failure> {
    let children = frame.store.children(frame.group)?;
    let Some(node) = children.iter().find(|node| node.name == name) else {
        let message = format!("Column '{name}' not found in group: {}", frame.label);
        return Err(Failure::argument(message));
    };
    let path = format!("{}/{name}", frame.group);
    match node.group {
        false => Ok(frame.store.array(&path)?.values),
        true => encoded(&frame.store, &path, "Unknown group structure with keys"),
    }
}

/// The values of a group with `categories` and `codes`, where a negative code is null, or with
/// `values` and `mask`, where a set mask is null.
fn encoded(store: &Store, path: &str, unknown: &str) -> Result<Vec<Value>, Failure> {
    let keys: Vec<String> = store
        .children(path)?
        .into_iter()
        .map(|node| node.name)
        .collect();
    let has = |name: &str| keys.iter().any(|key| key == name);
    if has("categories") && has("codes") {
        let categories = store.array(&format!("{path}/categories"))?.values;
        let codes = store.array(&format!("{path}/codes"))?.values;
        let mut values = Vec::with_capacity(codes.len());
        for (index, code) in codes.iter().enumerate() {
            let code = code.to_string().parse::<i64>().unwrap_or(-1);
            values.push(match usize::try_from(code) {
                Err(_) => Value::Null,
                Ok(at) => categories.get(at).cloned().ok_or_else(|| {
                    Failure::argument(format!(
                        "Invalid category code {code} at index {index} (max valid code: {})",
                        categories.len() as i64 - 1
                    ))
                })?,
            });
        }
        return Ok(values);
    }
    if has("values") && has("mask") {
        let values = store.array(&format!("{path}/values"))?.values;
        let mask = store.array(&format!("{path}/mask"))?.values;
        let masked = |flag: Option<&Value>| match flag {
            Some(Value::Boolean(set)) => *set,
            Some(Value::String(text)) => &**text == "TRUE",
            Some(other) => other.truth(),
            None => false,
        };
        let pairs = values.into_iter().enumerate();
        return Ok(pairs
            .map(|(index, value)| {
                if masked(mask.get(index)) {
                    Value::Null
                } else {
                    value
                }
            })
            .collect());
    }
    Err(Failure::argument(format!(
        "{unknown}: [{}]",
        keys.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::{integer, java_order};

    #[test]
    fn extends_signed_integers() {
        assert_eq!(integer(&[0xaf], false, true), -81);
        assert_eq!(integer(&[0xaf], false, false), 175);
        assert_eq!(integer(&[0xff, 0xfe], true, true), -2);
        assert!(java_order("Z", "a").is_lt());
    }
}

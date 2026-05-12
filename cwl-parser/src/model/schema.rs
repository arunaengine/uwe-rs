//! Types of CWL values: primitives, arrays, records, enums, unions and named types.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::model::binding::{
    CommandLineBinding, LoadListing, OutputBinding, read_binding, read_listing, read_output,
};
use crate::model::expression::{Dynamic, Expression};
use crate::model::process::Version;
use crate::model::record::{self, Context, Record};
use crate::node::{Node, Value};

/// The type of a parameter or record field, after the type DSL has been expanded.
#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Null,
    Boolean,
    Int,
    Long,
    Float,
    Double,
    String,
    File,
    Directory,
    Any,
    /// The `stdin` shortcut of command line tool inputs.
    Stdin,
    /// The `stdout` shortcut of command line tool outputs.
    Stdout,
    /// The `stderr` shortcut of command line tool outputs.
    Stderr,
    Array(Box<ArraySchema>),
    Record(Box<RecordSchema>),
    Enum(Box<EnumSchema>),
    Union(Vec<Type>),
    /// A type defined elsewhere, by its resolved IRI, such as a `SchemaDefRequirement` entry.
    Named(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArraySchema {
    pub items: Type,
    pub name: Option<String>,
    pub label: Option<String>,
    pub doc: Vec<String>,
    pub binding: Option<CommandLineBinding>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordSchema {
    pub name: Option<String>,
    pub fields: Vec<RecordField>,
    pub label: Option<String>,
    pub doc: Vec<String>,
    pub binding: Option<CommandLineBinding>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumSchema {
    pub name: Option<String>,
    /// The symbols without the identifier scope that Salad puts in front of them.
    pub symbols: Vec<String>,
    pub label: Option<String>,
    pub doc: Vec<String>,
    pub binding: Option<CommandLineBinding>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordField {
    /// The field name without its identifier scope.
    pub name: String,
    pub value_type: Type,
    pub label: Option<String>,
    pub doc: Vec<String>,
    pub secondary_files: Vec<SecondaryFile>,
    pub streamable: Option<bool>,
    pub format: Vec<Expression>,
    pub load_contents: Option<bool>,
    pub load_listing: Option<LoadListing>,
    pub input_binding: Option<CommandLineBinding>,
    pub output_binding: Option<OutputBinding>,
}

/// A file expected next to a primary file, such as an index.
#[derive(Clone, Debug, PartialEq)]
pub struct SecondaryFile {
    /// A suffix, with a leading `^` for each extension to remove, or an expression.
    pub pattern: Expression,
    /// Absent means the default: required for inputs, optional for outputs.
    pub required: Option<Dynamic<bool>>,
}

/// The last segment of a resolved identifier, such as `path` for `file:///a.cwl#Sample/path`.
pub fn short_name(iri: &str) -> &str {
    let fragment = iri.rsplit_once('#').map_or(iri, |(_, fragment)| fragment);
    fragment.rsplit('/').next().unwrap_or(fragment)
}

/// Reads a `type` value; `outputs` selects the output shortcuts `stdout` and `stderr`.
pub fn read_type(node: &Node, context: &Context, outputs: bool) -> Result<Type, Error> {
    match &node.value {
        Value::String(name) => {
            let primitive = match name.as_str() {
                "null" => Type::Null,
                "boolean" => Type::Boolean,
                "int" => Type::Int,
                "long" => Type::Long,
                "float" => Type::Float,
                "double" => Type::Double,
                "string" => Type::String,
                "File" => Type::File,
                "Directory" => Type::Directory,
                "Any" => Type::Any,
                "stdin" if !outputs => {
                    context.require(Version::V1_1, "the `stdin` type", &node.location)?;
                    Type::Stdin
                }
                "stdout" if outputs => Type::Stdout,
                "stderr" if outputs => Type::Stderr,
                _ if name.contains('#') || name.contains(':') => Type::Named(name.clone()),
                _ => {
                    let message = format!("`{name}` is not a type");
                    return Err(Error::new(&node.location, message));
                }
            };
            Ok(primitive)
        }
        Value::List(items) => {
            let types = items.iter().map(|item| read_type(item, context, outputs));
            Ok(Type::Union(types.collect::<Result<_, _>>()?))
        }
        Value::Map(_) => read_schema(node, context, outputs),
        _ => Err(Error::new(
            &node.location,
            format!(
                "a type must be a string, a list or a map, not {}",
                node.kind()
            ),
        )),
    }
}

fn read_schema(node: &Node, context: &Context, outputs: bool) -> Result<Type, Error> {
    let mut record = Record::new(node, "type")?;
    let kind = record::text(record.required("type")?)?;
    let name = record.optional("name").map(record::text).transpose()?;
    let label = record.optional("label").map(record::text).transpose()?;
    let doc = record::documentation(&mut record)?;
    let binding = record
        .optional("inputBinding")
        .map(|node| read_binding(node, context))
        .transpose()?;
    let schema = match kind.as_str() {
        "array" => {
            let items = read_type(record.required("items")?, context, outputs)?;
            Type::Array(Box::new(ArraySchema {
                items,
                name,
                label,
                doc,
                binding,
            }))
        }
        "record" => {
            let mut fields = Vec::new();
            for field in record
                .optional("fields")
                .map(record::list)
                .transpose()?
                .unwrap_or(&[])
            {
                fields.push(read_field(field, context, outputs)?);
            }
            Type::Record(Box::new(RecordSchema {
                name,
                fields,
                label,
                doc,
                binding,
            }))
        }
        "enum" => {
            let symbols = record::texts(record.required("symbols")?)?;
            let symbols = symbols.iter().map(|symbol| short_name(symbol).to_owned());
            Type::Enum(Box::new(EnumSchema {
                name,
                symbols: symbols.collect(),
                label,
                doc,
                binding,
            }))
        }
        other => {
            let message =
                format!("`{other}` is not a kind of type; expected array, record or enum");
            return Err(Error::new(&node.location, message));
        }
    };
    record.finish()?;
    Ok(schema)
}

fn read_field(node: &Node, context: &Context, outputs: bool) -> Result<RecordField, Error> {
    let mut record = Record::new(node, "record field")?;
    let name = short_name(&record::text(record.required("name")?)?).to_owned();
    let value_type = read_type(record.required("type")?, context, outputs)?;
    let label = record.optional("label").map(record::text).transpose()?;
    let doc = record::documentation(&mut record)?;
    let secondary_files = read_secondary(record.optional("secondaryFiles"), context)?;
    let streamable = record
        .optional("streamable")
        .map(record::boolean)
        .transpose()?;
    let format = match record.optional("format") {
        Some(node) => record::items(node)
            .iter()
            .map(|item| context.expression(item))
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    let load_contents = record
        .optional("loadContents")
        .map(record::boolean)
        .transpose()?;
    let load_listing = record
        .since("loadListing", Version::V1_1, context)?
        .map(read_listing)
        .transpose()?;
    let input_binding = record
        .optional("inputBinding")
        .map(|node| read_binding(node, context))
        .transpose()?;
    let output_binding = record
        .optional("outputBinding")
        .map(|node| read_output(node, context))
        .transpose()?;
    record.finish()?;
    Ok(RecordField {
        name,
        value_type,
        label,
        doc,
        secondary_files,
        streamable,
        format,
        load_contents,
        load_listing,
        input_binding,
        output_binding,
    })
}

/// Reads `secondaryFiles`: patterns from the DSL, expressions, or `{pattern, required}` maps.
pub fn read_secondary(node: Option<&Node>, context: &Context) -> Result<Vec<SecondaryFile>, Error> {
    let Some(node) = node else {
        return Ok(Vec::new());
    };
    let mut files = Vec::new();
    for item in record::items(node) {
        let file = match &item.value {
            Value::String(_) => SecondaryFile {
                pattern: context.expression(item)?,
                required: None,
            },
            _ => {
                context.require(
                    Version::V1_1,
                    "a secondary file with `pattern`",
                    &item.location,
                )?;
                let mut record = Record::new(item, "secondary file")?;
                let pattern = context.expression(record.required("pattern")?)?;
                let required = match record.optional("required") {
                    Some(node) if matches!(node.value, Value::Bool(_)) => {
                        Some(Dynamic::Value(record::boolean(node)?))
                    }
                    Some(node) => Some(Dynamic::Expression(context.code(node)?)),
                    None => None,
                };
                record.finish()?;
                SecondaryFile { pattern, required }
            }
        };
        files.push(file);
    }
    Ok(files)
}

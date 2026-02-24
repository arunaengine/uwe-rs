//! Command line bindings of inputs and output bindings of command line tools.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::expression::{Dynamic, Expression};
use crate::node::{Node, Value};
use crate::process::Version;
use crate::record::{self, Context, Record};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadListing {
    NoListing,
    ShallowListing,
    DeepListing,
}

/// How an input value becomes command line arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandLineBinding {
    pub load_contents: Option<bool>,
    pub position: Option<Dynamic<i64>>,
    pub prefix: Option<String>,
    pub separate: bool,
    pub item_separator: Option<String>,
    pub value_from: Option<Expression>,
    pub shell_quote: bool,
}

/// How output files are collected after a command line tool ran.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputBinding {
    pub glob: Vec<Expression>,
    pub load_contents: Option<bool>,
    pub load_listing: Option<LoadListing>,
    pub output_eval: Option<Expression>,
}

pub fn read_binding(node: &Node, context: &Context) -> Result<CommandLineBinding, Error> {
    let mut record = Record::new(node, "command line binding")?;
    let position = match record.optional("position") {
        Some(node) if matches!(node.value, Value::Integer(_)) => {
            Some(Dynamic::Value(record::integer(node)?))
        }
        Some(node) => {
            context.require(Version::V1_1, "an expression in `position`", &node.location)?;
            Some(Dynamic::Expression(context.code(node)?))
        }
        None => None,
    };
    let binding = CommandLineBinding {
        load_contents: record
            .optional("loadContents")
            .map(record::boolean)
            .transpose()?,
        position,
        prefix: record.optional("prefix").map(record::text).transpose()?,
        separate: record
            .optional("separate")
            .map(record::boolean)
            .transpose()?
            .unwrap_or(true),
        item_separator: record
            .optional("itemSeparator")
            .map(record::text)
            .transpose()?,
        value_from: record
            .optional("valueFrom")
            .map(|node| context.expression(node))
            .transpose()?,
        shell_quote: record
            .optional("shellQuote")
            .map(record::boolean)
            .transpose()?
            .unwrap_or(true),
    };
    record.finish()?;
    Ok(binding)
}

/// The `InputBinding` of workflow and expression tool inputs, which has only `loadContents`.
///
/// In v1.0 it had the full command line binding, which the upgrade removes.
pub fn read_load(node: &Node, context: &Context) -> Result<CommandLineBinding, Error> {
    if context.version == Version::V1_0 {
        return read_binding(node, context);
    }
    let mut record = Record::new(node, "input binding")?;
    let load_contents = record
        .optional("loadContents")
        .map(record::boolean)
        .transpose()?;
    record.finish()?;
    Ok(CommandLineBinding {
        load_contents,
        position: None,
        prefix: None,
        separate: true,
        item_separator: None,
        value_from: None,
        shell_quote: true,
    })
}

pub fn read_output(node: &Node, context: &Context) -> Result<OutputBinding, Error> {
    let mut record = Record::new(node, "output binding")?;
    let glob = match record.optional("glob") {
        Some(node) => record::items(node)
            .iter()
            .map(|item| context.expression(item))
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    let output_eval = match record.optional("outputEval") {
        Some(node) if context.version == Version::V1_0 => Some(context.expression(node)?),
        Some(node) => Some(context.code(node)?),
        None => None,
    };
    let binding = OutputBinding {
        glob,
        load_contents: record
            .optional("loadContents")
            .map(record::boolean)
            .transpose()?,
        load_listing: record
            .since("loadListing", Version::V1_1, context)?
            .map(read_listing)
            .transpose()?,
        output_eval,
    };
    record.finish()?;
    Ok(binding)
}

pub fn read_listing(node: &Node) -> Result<LoadListing, Error> {
    record::symbol(
        node,
        &[
            ("no_listing", LoadListing::NoListing),
            ("shallow_listing", LoadListing::ShallowListing),
            ("deep_listing", LoadListing::DeepListing),
        ],
    )
}

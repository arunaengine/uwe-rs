//! Inputs and outputs of processes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::model::binding::{
    CommandLineBinding, LoadListing, OutputBinding, read_binding, read_listing, read_load,
    read_output,
};
use crate::model::expression::Expression;
use crate::model::process::{Kind, Version};
use crate::model::record::{self, Context, Record};
use crate::model::schema::{SecondaryFile, Type, read_secondary, read_type};
use crate::node::{Entry, Location, Node};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkMerge {
    MergeNested,
    MergeFlattened,
}

/// How to pick from several sources of which some may be null.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickValue {
    FirstNonNull,
    /// `the_only_non_null` in the document.
    OnlyNonNull,
    AllNonNull,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputParameter {
    /// The resolved identifier; its last segment is the input name.
    pub id: String,
    pub label: Option<String>,
    pub doc: Vec<String>,
    pub value_type: Type,
    pub secondary_files: Vec<SecondaryFile>,
    pub streamable: Option<bool>,
    pub format: Vec<Expression>,
    pub load_contents: Option<bool>,
    pub load_listing: Option<LoadListing>,
    /// The default value as written, with `location` and `path` resolved.
    pub default: Option<Node>,
    /// For workflows and expression tools only `load_contents` is set here.
    pub binding: Option<CommandLineBinding>,
    pub extensions: Vec<Entry>,
    pub location: Location,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputParameter {
    pub id: String,
    pub label: Option<String>,
    pub doc: Vec<String>,
    pub value_type: Type,
    pub secondary_files: Vec<SecondaryFile>,
    pub streamable: Option<bool>,
    pub format: Option<Expression>,
    /// Only for command line tools.
    pub binding: Option<OutputBinding>,
    /// Only for workflows: the resolved identifiers of the sources.
    pub output_source: Vec<String>,
    /// Whether `outputSource` is written as a list, which cwltool's type checker merges.
    pub output_is_list: bool,
    pub link_merge: Option<LinkMerge>,
    pub pick_value: Option<PickValue>,
    pub extensions: Vec<Entry>,
    pub location: Location,
}

pub fn read_input(node: &Node, context: &Context, kind: Kind) -> Result<InputParameter, Error> {
    let mut record = Record::new(node, "input parameter")?;
    let id = record::text(record.required("id")?)?;
    let value_type = read_type(record.required("type")?, context, false)?;
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
        .since("loadContents", Version::V1_1, context)?
        .map(record::boolean)
        .transpose()?;
    let load_listing = record
        .since("loadListing", Version::V1_1, context)?
        .map(read_listing)
        .transpose()?;
    let default = record.optional("default").cloned();
    let binding = match record.optional("inputBinding") {
        Some(node) if kind == Kind::CommandLineTool => Some(read_binding(node, context)?),
        Some(node) => Some(read_load(node, context)?),
        None => None,
    };
    let extensions = record.finish()?;
    Ok(InputParameter {
        id,
        label,
        doc,
        value_type,
        secondary_files,
        streamable,
        format,
        load_contents,
        load_listing,
        default,
        binding,
        extensions,
        location: node.location.clone(),
    })
}

pub fn read_output_parameter(
    node: &Node,
    context: &Context,
    kind: Kind,
) -> Result<OutputParameter, Error> {
    let mut record = Record::new(node, "output parameter")?;
    let id = record::text(record.required("id")?)?;
    let value_type = read_type(
        record.required("type")?,
        context,
        kind == Kind::CommandLineTool,
    )?;
    let label = record.optional("label").map(record::text).transpose()?;
    let doc = record::documentation(&mut record)?;
    let secondary_files = read_secondary(record.optional("secondaryFiles"), context)?;
    let streamable = record
        .optional("streamable")
        .map(record::boolean)
        .transpose()?;
    let format = record
        .optional("format")
        .map(|node| context.expression(node))
        .transpose()?;
    let mut parameter = OutputParameter {
        id,
        label,
        doc,
        value_type,
        secondary_files,
        streamable,
        format,
        binding: None,
        output_source: Vec::new(),
        output_is_list: false,
        link_merge: None,
        pick_value: None,
        extensions: Vec::new(),
        location: node.location.clone(),
    };
    match kind {
        Kind::CommandLineTool => {
            let binding = record.optional("outputBinding");
            if binding.is_some() && matches!(parameter.value_type, Type::Stdout | Type::Stderr) {
                let message = "an output of type stdout or stderr cannot have an outputBinding";
                return Err(Error::new(&node.location, message));
            }
            parameter.binding = binding.map(|node| read_output(node, context)).transpose()?;
        }
        Kind::Workflow => {
            if let Some(source) = record.optional("outputSource") {
                parameter.output_source = record::texts(source)?;
                parameter.output_is_list = source.as_list().is_some();
            }
            parameter.link_merge = record.optional("linkMerge").map(read_merge).transpose()?;
            parameter.pick_value = record
                .since("pickValue", Version::V1_2, context)?
                .map(|node| read_pick(node, context.version))
                .transpose()?;
        }
        Kind::ExpressionTool | Kind::Operation => {}
    }
    parameter.extensions = record.finish()?;
    Ok(parameter)
}

pub fn read_merge(node: &Node) -> Result<LinkMerge, Error> {
    let symbols = [
        ("merge_nested", LinkMerge::MergeNested),
        ("merge_flattened", LinkMerge::MergeFlattened),
    ];
    record::symbol(node, &symbols)
}

/// Reads `pickValue`; drafts before v1.2.0-dev3 spelled `the_only_non_null` as `only_non_null`.
pub fn read_pick(node: &Node, version: Version) -> Result<PickValue, Error> {
    let symbols = [
        ("first_non_null", PickValue::FirstNonNull),
        ("the_only_non_null", PickValue::OnlyNonNull),
        ("all_non_null", PickValue::AllNonNull),
    ];
    if version <= Version::V1_2Dev2 && node.as_str() == Some("only_non_null") {
        return Ok(PickValue::OnlyNonNull);
    }
    record::symbol(node, &symbols)
}

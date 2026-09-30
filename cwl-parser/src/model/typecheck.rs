//! Checks workflow source and sink types after value picking, merging and scatter.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use crate::error::Error;
use crate::model::parameter::{LinkMerge, PickValue};
use crate::model::process::Process;
use crate::model::schema::{ArraySchema, RecordSchema, Type, short_name};
use crate::model::workflow::{ScatterMethod, Workflow};
use crate::node::Location;

/// One sink with its sources, as cwltool sees it.
struct Sink<'a> {
    name: &'a str,
    value_type: Type,
    sources: &'a [String],
    /// Whether the sources are written as a list; only a list gets a default `linkMerge`.
    listed: bool,
    link_merge: Option<LinkMerge>,
    pick_value: Option<PickValue>,
    location: &'a Location,
}

/// Fails when a source of a step input or workflow output cannot fit its sink.
pub fn check_types(process: &Process, workflow: &Workflow) -> Result<(), Error> {
    let mut sources: HashMap<&str, Type> = process
        .inputs
        .iter()
        .map(|input| (input.id.as_str(), input.value_type.clone()))
        .collect();
    let mut conditional = Vec::new();
    for step in &workflow.steps {
        let levels = match step.scatter_method {
            _ if step.scatter.is_empty() => 0,
            Some(ScatterMethod::NestedCrossproduct) => step.scatter.len(),
            _ => 1,
        };
        for id in &step.outputs {
            let output = step
                .run
                .process
                .outputs
                .iter()
                .find(|output| short_name(&output.id) == short_name(id));
            if let Some(output) = output {
                let wrapped = (0..levels).fold(output.value_type.clone(), |items, _| array(items));
                sources.insert(id.as_str(), wrapped);
                if step.when.is_some() {
                    conditional.push(id.as_str());
                }
            }
        }
    }
    let mut sinks = Vec::new();
    for step in &workflow.steps {
        for input in &step.inputs {
            if input.source.is_empty() || input.value_from.is_some() {
                continue;
            }
            let name = short_name(&input.id);
            let tool_input = step
                .run
                .process
                .inputs
                .iter()
                .find(|tool_input| short_name(&tool_input.id) == name);
            let mut value_type = tool_input.map_or(Type::Any, |input| input.value_type.clone());
            if step.scatter.iter().any(|scattered| scattered == &input.id) {
                value_type = array(value_type);
            }
            sinks.push(Sink {
                name,
                value_type,
                sources: &input.source,
                listed: input.source_is_list,
                link_merge: input.link_merge,
                pick_value: input.pick_value,
                location: &input.location,
            });
        }
    }
    for output in &process.outputs {
        if output.output_source.is_empty() {
            continue;
        }
        sinks.push(Sink {
            name: short_name(&output.id),
            value_type: output.value_type.clone(),
            sources: &output.output_source,
            listed: output.output_is_list,
            link_merge: output.link_merge,
            pick_value: output.pick_value,
            location: &output.location,
        });
    }
    for sink in sinks {
        check_sink(&sink, &mut sources, &conditional)?;
    }
    Ok(())
}

fn check_sink(
    sink: &Sink<'_>,
    sources: &mut HashMap<&str, Type>,
    conditional: &[&str],
) -> Result<(), Error> {
    let link_merge = if sink.listed {
        let default = (sink.sources.len() > 1).then_some(LinkMerge::MergeNested);
        match sink.pick_value {
            Some(PickValue::FirstNonNull | PickValue::OnlyNonNull) => None,
            Some(PickValue::AllNonNull) => sink.link_merge.or(Some(LinkMerge::MergeNested)),
            None => sink.link_merge.or(default),
        }
    } else {
        None
    };
    for id in sink.sources {
        // cwltool lets a single source from a conditional step be null from then on.
        if !sink.listed
            && conditional.contains(&id.as_str())
            && let Some(value_type) = sources.get_mut(id.as_str())
            && !contains_null(value_type)
        {
            let mut members = vec![Type::Null];
            match &*value_type {
                Type::Union(others) => members.extend(others.iter().cloned()),
                other => members.push(other.clone()),
            }
            *value_type = Type::Union(members);
        }
        let Some(source) = sources.get(id.as_str()) else {
            continue;
        };
        if !fits(source, &sink.value_type, link_merge, sink.pick_value) {
            let mut ways = Vec::new();
            if let Some(pick) = sink.pick_value {
                ways.push(format!("pickValue {}", pick_name(pick)));
            }
            if let Some(merge) = link_merge {
                ways.push(format!("linkMerge {}", merge_name(merge)));
            }
            let with = if ways.is_empty() {
                String::new()
            } else {
                format!(" with {}", ways.join(" and "))
            };
            let message = format!(
                "the source `{}` of type {} does not fit the sink `{}` of type {}{with}",
                short_name(id),
                describe(source),
                sink.name,
                describe(&sink.value_type)
            );
            return Err(Error::new(sink.location, message));
        }
    }
    Ok(())
}

/// The name of a `pickValue` method as documents write it.
fn pick_name(pick: PickValue) -> &'static str {
    match pick {
        PickValue::FirstNonNull => "first_non_null",
        PickValue::OnlyNonNull => "the_only_non_null",
        PickValue::AllNonNull => "all_non_null",
    }
}

/// The name of a `linkMerge` method as documents write it.
fn merge_name(merge: LinkMerge) -> &'static str {
    match merge {
        LinkMerge::MergeNested => "merge_nested",
        LinkMerge::MergeFlattened => "merge_flattened",
    }
}

/// cwltool's `check_types` without its warnings: whether a source can reach its sink.
fn fits(
    source: &Type,
    sink: &Type,
    link_merge: Option<LinkMerge>,
    pick_value: Option<PickValue>,
) -> bool {
    let mut source = source.clone();
    if let (Some(pick), Type::Array(schema)) = (pick_value, &source)
        && let Type::Union(items) = &schema.items
        && items.contains(&Type::Null)
    {
        let present: Vec<Type> = items
            .iter()
            .filter(|item| **item != Type::Null)
            .cloned()
            .collect();
        source = match pick {
            PickValue::AllNonNull => array(Type::Union(present)),
            PickValue::FirstNonNull | PickValue::OnlyNonNull => Type::Union(present),
        };
    }
    let source = match link_merge {
        None => source,
        Some(LinkMerge::MergeNested) => array(source),
        Some(LinkMerge::MergeFlattened) => flattened(source),
    };
    assignable(&source, sink, false)
}

/// cwltool's `can_assign_src_to_sink`: all members of a source union must fit when `strict`,
/// else one that is not null.
fn assignable(source: &Type, sink: &Type, strict: bool) -> bool {
    if *source == Type::Any || *sink == Type::Any {
        return true;
    }
    match (source, sink) {
        (Type::Array(from), Type::Array(to)) => return assignable(&from.items, &to.items, strict),
        (Type::Record(from), Type::Record(to)) => return records(from, to, strict),
        (Type::Array(_) | Type::Record(_) | Type::Enum(_), _)
            if matches!(sink, Type::Array(_) | Type::Record(_) | Type::Enum(_)) =>
        {
            // Two schemas of different kinds, or two enums, which match whatever the symbols.
            return std::mem::discriminant(source) == std::mem::discriminant(sink);
        }
        _ => {}
    }
    if let Type::Union(members) = source {
        return if strict {
            members.iter().all(|member| assignable(member, sink, false))
        } else {
            members
                .iter()
                .any(|member| *member != Type::Null && assignable(member, sink, false))
        };
    }
    if let Type::Union(members) = sink {
        return members
            .iter()
            .any(|member| assignable(source, member, false));
    }
    matches!((name(source), name(sink)), (Some(from), Some(to)) if from == to)
}

/// Whether every field of the sink record can take the field of the same name of the source,
/// or null when the source has none.
fn records(source: &RecordSchema, sink: &RecordSchema, strict: bool) -> bool {
    sink.fields.iter().all(|field| {
        let name = short_name(&field.name);
        let from = source
            .fields
            .iter()
            .find(|candidate| short_name(&candidate.name) == name)
            .map_or(&Type::Null, |candidate| &candidate.value_type);
        assignable(from, &field.value_type, strict)
    })
}

/// The name cwltool compares a type by: its primitive name, or the IRI of a named type; the
/// stream shortcuts are files. Schemas have no name.
fn name(value_type: &Type) -> Option<&str> {
    Some(match value_type {
        Type::Null => "null",
        Type::Boolean => "boolean",
        Type::Int => "int",
        Type::Long => "long",
        Type::Float => "float",
        Type::Double => "double",
        Type::String => "string",
        Type::File | Type::Stdin | Type::Stdout | Type::Stderr => "File",
        Type::Directory => "Directory",
        Type::Any => "Any",
        Type::Named(iri) => iri,
        Type::Array(_) | Type::Record(_) | Type::Enum(_) | Type::Union(_) => return None,
    })
}

fn array(items: Type) -> Type {
    Type::Array(Box::new(ArraySchema {
        items,
        name: None,
        label: None,
        doc: Vec::new(),
        binding: None,
    }))
}

/// The type after `merge_flattened`: arrays stay, other types become arrays of themselves.
fn flattened(value_type: Type) -> Type {
    match value_type {
        Type::Union(members) => Type::Union(members.into_iter().map(flattened).collect()),
        Type::Array(schema) => Type::Array(schema),
        other => array(other),
    }
}

fn contains_null(value_type: &Type) -> bool {
    match value_type {
        Type::Null => true,
        Type::Union(members) => members.contains(&Type::Null),
        _ => false,
    }
}

/// A type for messages, like `File[]`, `string?` or `enum`.
fn describe(value_type: &Type) -> String {
    match value_type {
        Type::Array(schema) => format!("{}[]", describe(&schema.items)),
        Type::Record(_) => "record".into(),
        Type::Enum(_) => "enum".into(),
        Type::Union(members) => {
            let names: Vec<String> = members.iter().map(describe).collect();
            format!("[{}]", names.join(", "))
        }
        other => name(other).map(short_name).unwrap_or_default().to_owned(),
    }
}

#[cfg(test)]
#[path = "typecheck_tests.rs"]
mod tests;

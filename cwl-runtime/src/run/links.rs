//! How values flow between the steps of a workflow: sources that must exist, `linkMerge` and
//! `pickValue` over the sources of a step input, the outputs of a workflow, and the outputs of
//! a scatter, nested by its shape.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};

use cwl_parser::model::{LinkMerge, PickValue, Process, Step, StepInput, short_name};

use super::{ScatterRun, WorkflowRun};
use crate::files;
use crate::value::{Map, Value};

/// Fails when a source or output source names nothing in the workflow.
pub(super) fn check_sources(
    process: &Process,
    workflow: &cwl_parser::model::Workflow,
) -> Result<(), String> {
    let mut known: HashSet<&str> = process
        .inputs
        .iter()
        .map(|input| input.id.as_str())
        .collect();
    for step in &workflow.steps {
        known.extend(step.outputs.iter().map(String::as_str));
    }
    let sources = workflow
        .steps
        .iter()
        .flat_map(|step| step.inputs.iter().flat_map(|input| &input.source))
        .chain(
            process
                .outputs
                .iter()
                .flat_map(|output| &output.output_source),
        );
    for source in sources {
        if !known.contains(source.as_str()) {
            return Err(format!(
                "the source `{}` names no input or step output",
                short_name(source)
            ));
        }
    }
    Ok(())
}

/// The input object of a step from its sources, as cwltool's `object_from_state`: sources
/// merge by `linkMerge`, `pickValue` picks, and `default` stands in for null. `None` while a
/// source has no value yet. `lenient` takes a failed pick as null, for runs on placeholder
/// inputs, where optional inputs are null and conditions follow placeholder values.
pub(super) fn gather(
    values: &HashMap<String, Value>,
    step: &Step,
    lenient: bool,
) -> Option<Result<Map, String>> {
    let mut map = Map::new();
    for input in &step.inputs {
        let mut sources = Vec::new();
        for source in &input.source {
            sources.push(values.get(source)?.clone());
        }
        let value =
            merge(input, sources).and_then(|value| pick(input.pick_value, value, &input.id));
        let value = match value {
            Ok(value) => value,
            Err(_) if lenient => Value::Null,
            Err(error) => return Some(Err(error)),
        };
        let value = match (&value, &input.default) {
            (Value::Null, Some(default)) => {
                let mut value = Value::from_node(default);
                if let Err(error) = files::normalize(&mut value) {
                    return Some(Err(error));
                }
                value
            }
            _ => value,
        };
        map.insert(short_name(&input.id).to_owned(), value);
    }
    Some(Ok(map))
}

fn merge(input: &StepInput, mut sources: Vec<Value>) -> Result<Value, String> {
    let link_merge = input
        .link_merge
        .or((sources.len() > 1).then_some(LinkMerge::MergeNested));
    Ok(match link_merge {
        None => sources.pop().unwrap_or(Value::Null),
        Some(LinkMerge::MergeNested) => Value::List(sources),
        Some(LinkMerge::MergeFlattened) => {
            let mut items = Vec::new();
            for source in sources {
                match source {
                    Value::List(inner) => items.extend(inner),
                    other => items.push(other),
                }
            }
            Value::List(items)
        }
    })
}

/// Applies `pickValue` to a list of values.
fn pick(pick: Option<PickValue>, value: Value, id: &str) -> Result<Value, String> {
    let (Some(pick), Value::List(items)) = (pick, &value) else {
        return Ok(value);
    };
    let present: Vec<&Value> = items.iter().filter(|item| !item.is_null()).collect();
    let name = short_name(id);
    match pick {
        PickValue::FirstNonNull => present
            .first()
            .map(|value| (*value).clone())
            .ok_or_else(|| format!("all sources of `{name}` are null")),
        PickValue::OnlyNonNull => match present.as_slice() {
            [only] => Ok((*only).clone()),
            [] => Err(format!("all sources of `{name}` are null")),
            _ => Err(format!("more than one source of `{name}` is not null")),
        },
        PickValue::AllNonNull => Ok(Value::List(present.into_iter().cloned().collect())),
    }
}

/// The outputs of a skipped step: null for every output.
pub(super) fn null_outputs(step: &Step) -> Map {
    step.outputs
        .iter()
        .map(|id| (short_name(id).to_owned(), Value::Null))
        .collect()
}

/// The output object of a workflow from its output sources.
/// `lenient` takes a failed pick as null, as for steps on placeholder inputs.
pub(super) fn workflow_outputs(run: &WorkflowRun, lenient: bool) -> Result<Map, String> {
    let mut outputs = Map::new();
    for output in &run.process.outputs {
        let sources: Vec<Value> = output
            .output_source
            .iter()
            .map(|source| run.values.get(source).cloned().unwrap_or(Value::Null))
            .collect();
        let link_merge = output
            .link_merge
            .or((sources.len() > 1).then_some(LinkMerge::MergeNested));
        let mut sources = sources;
        let value = match link_merge {
            None => sources.pop().unwrap_or(Value::Null),
            Some(LinkMerge::MergeNested) => Value::List(sources),
            Some(LinkMerge::MergeFlattened) => {
                let mut items = Vec::new();
                for source in sources {
                    match source {
                        Value::List(inner) => items.extend(inner),
                        other => items.push(other),
                    }
                }
                Value::List(items)
            }
        };
        let value = match pick(output.pick_value, value, &output.id) {
            Err(_) if lenient => Value::Null,
            result => result?,
        };
        outputs.insert(short_name(&output.id).to_owned(), value);
    }
    Ok(outputs)
}

/// The outputs of a scatter: a list per output, nested for nested_crossproduct; skipped
/// elements give null.
pub(super) fn assemble(run: &ScatterRun) -> Map {
    let mut outputs = Map::new();
    for name in &run.outputs {
        let flat: Vec<Value> = run
            .results
            .iter()
            .map(|result| {
                result
                    .as_ref()
                    .and_then(|map| map.get(name).cloned())
                    .unwrap_or(Value::Null)
            })
            .collect();
        outputs.insert(name.clone(), nest(&flat, &run.shape));
    }
    outputs
}

/// Folds a flat list into nested lists of the given lengths, the last varying fastest.
fn nest(flat: &[Value], shape: &[usize]) -> Value {
    match shape {
        [] | [_] => Value::List(flat.to_vec()),
        [first, rest @ ..] => {
            let size: usize = rest.iter().product();
            Value::List(
                (0..*first)
                    .map(|index| nest(&flat[index * size..(index + 1) * size], rest))
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;

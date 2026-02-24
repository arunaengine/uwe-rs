//! Workflows: steps, their inputs and outputs, and the processes they run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use crate::binding::{LoadListing, read_listing};
use crate::error::Error;
use crate::expression::Expression;
use crate::node::{Entry, Location, Node, Value};
use crate::parameter::{LinkMerge, PickValue, read_merge, read_pick};
use crate::process::{Process, Version, read_process};
use crate::record::{self, Context, Record};
use crate::requirement::{Requirement, read_requirements};

/// Loads the process that a step's `run` names; the loader implements it.
pub trait Runs {
    fn load(&mut self, url: &str, at: &Location) -> Result<Arc<Process>, Error>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workflow {
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub id: String,
    pub label: Option<String>,
    pub doc: Vec<String>,
    /// The `in` field.
    pub inputs: Vec<StepInput>,
    /// The resolved identifiers of the `out` field.
    pub outputs: Vec<String>,
    pub requirements: Vec<Requirement>,
    pub hints: Vec<Requirement>,
    pub run: Run,
    pub when: Option<Expression>,
    /// The resolved identifiers of the scattered step inputs.
    pub scatter: Vec<String>,
    pub scatter_method: Option<ScatterMethod>,
    pub extensions: Vec<Entry>,
    pub location: Location,
}

/// The process a step runs, loaded and shared between steps that run the same one.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// The resolved IRI of the process, as written or given to an inline process.
    pub reference: String,
    pub process: Arc<Process>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepInput {
    pub id: String,
    /// The resolved identifiers of the sources.
    pub source: Vec<String>,
    pub link_merge: Option<LinkMerge>,
    pub pick_value: Option<PickValue>,
    pub load_contents: Option<bool>,
    pub load_listing: Option<LoadListing>,
    pub label: Option<String>,
    pub default: Option<Node>,
    pub value_from: Option<Expression>,
    pub extensions: Vec<Entry>,
    pub location: Location,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScatterMethod {
    Dotproduct,
    NestedCrossproduct,
    FlatCrossproduct,
}

pub fn read_workflow(
    record: &mut Record<'_>,
    context: &Context,
    runs: &mut dyn Runs,
) -> Result<Workflow, Error> {
    let mut steps = Vec::new();
    for (index, step) in record::list(record.required("steps")?)?.iter().enumerate() {
        steps.push(read_step(step, context, runs, index)?);
    }
    Ok(Workflow { steps })
}

fn read_step(
    node: &Node,
    context: &Context,
    runs: &mut dyn Runs,
    index: usize,
) -> Result<Step, Error> {
    let mut record = Record::new(node, "workflow step")?;
    let id = match record.optional("id") {
        Some(id) => record::text(id)?,
        None => format!("#step{index}"),
    };
    let mut inputs = Vec::new();
    for input in record::list(record.required("in")?)? {
        inputs.push(read_input(input, context)?);
    }
    let mut outputs = Vec::new();
    for output in record::list(record.required("out")?)? {
        let id = match &output.value {
            Value::Map(_) => {
                let mut record = Record::new(output, "step output")?;
                let id = record::text(record.required("id")?)?;
                record.finish()?;
                id
            }
            _ => record::text(output)?,
        };
        outputs.push(id);
    }
    let requirements = read_requirements(record.optional("requirements"), context, false)?;
    let hints = read_requirements(record.optional("hints"), context, true)?;
    let run = read_run(record.required("run")?, context, runs, &id)?;
    let when = record
        .since("when", Version::V1_2, context)?
        .map(|node| context.code(node))
        .transpose()?;
    let scatter = record
        .optional("scatter")
        .map(record::texts)
        .transpose()?
        .unwrap_or_default();
    let scatter_method = record
        .optional("scatterMethod")
        .map(|node| {
            let symbols = [
                ("dotproduct", ScatterMethod::Dotproduct),
                ("nested_crossproduct", ScatterMethod::NestedCrossproduct),
                ("flat_crossproduct", ScatterMethod::FlatCrossproduct),
            ];
            record::symbol(node, &symbols)
        })
        .transpose()?;
    let label = record.optional("label").map(record::text).transpose()?;
    let doc = record::documentation(&mut record)?;
    let extensions = record.finish()?;
    Ok(Step {
        id,
        label,
        doc,
        inputs,
        outputs,
        requirements,
        hints,
        run,
        when,
        scatter,
        scatter_method,
        extensions,
        location: node.location.clone(),
    })
}

/// Loads the process of `run`: a reference goes through the loader, a map is read in place.
fn read_run(node: &Node, context: &Context, runs: &mut dyn Runs, step: &str) -> Result<Run, Error> {
    if let Some(reference) = node.as_str() {
        let process = runs.load(reference, &node.location)?;
        return Ok(Run {
            reference: reference.to_owned(),
            process,
        });
    }
    let fallback = format!("{step}/run");
    let process = read_process(node, context, runs, &fallback)?;
    Ok(Run {
        reference: process.id.clone(),
        process: Arc::new(process),
    })
}

fn read_input(node: &Node, context: &Context) -> Result<StepInput, Error> {
    let mut record = Record::new(node, "step input")?;
    let input = StepInput {
        id: record::text(record.required("id")?)?,
        source: record
            .optional("source")
            .map(record::texts)
            .transpose()?
            .unwrap_or_default(),
        link_merge: record.optional("linkMerge").map(read_merge).transpose()?,
        pick_value: record
            .since("pickValue", Version::V1_2, context)?
            .map(|node| read_pick(node, context.version))
            .transpose()?,
        load_contents: record
            .since("loadContents", Version::V1_1, context)?
            .map(record::boolean)
            .transpose()?,
        load_listing: record
            .since("loadListing", Version::V1_1, context)?
            .map(read_listing)
            .transpose()?,
        label: record.optional("label").map(record::text).transpose()?,
        default: record.optional("default").cloned(),
        value_from: record
            .optional("valueFrom")
            .map(|node| context.expression(node))
            .transpose()?,
        extensions: Vec::new(),
        location: node.location.clone(),
    };
    let extensions = record.finish()?;
    Ok(StepInput {
        extensions,
        ..input
    })
}

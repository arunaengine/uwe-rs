//! Starting the steps of a workflow: the input object of a step from its sources, its scatter
//! into one job per element, `valueFrom`, and `when`, which may skip the step.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{ScatterMethod, Step, short_name};

use super::links::{assemble, null_outputs};
use super::{Event, Job, JobId, Names, Parent, Run, ScatterRun, State};
use crate::files;
use crate::placeholder;
use crate::scope::Scope;
use crate::value::{Map, Value};

impl Run {
    pub(super) fn start_step(
        &mut self,
        workflow: JobId,
        index: usize,
        step: &Step,
        gathered: Map,
        scope: &Scope,
        names: Names,
    ) -> Result<(), String> {
        let at = |error: String| format!("{}: {error}", names.display);
        let step_scope = scope.with(&step.requirements, &step.hints);
        let evaluator = step_scope.evaluator();
        let process = &step.run.process;
        if step.scatter.is_empty() {
            match self
                .step_job(step, gathered, &evaluator, &step_scope)
                .map_err(at)?
            {
                Some(job) => {
                    let parent = Parent::Step {
                        workflow,
                        step: index,
                    };
                    self.launch(process, job, &step_scope, parent, names, false)?;
                }
                None => {
                    let outputs = null_outputs(step);
                    self.events.push_back(Event::Step {
                        workflow,
                        step: index,
                        outputs,
                    });
                }
            }
            return Ok(());
        }
        // Scatter: one job per element of the scattered inputs.
        let keys: Vec<String> = step
            .scatter
            .iter()
            .map(|id| short_name(id).to_owned())
            .collect();
        let mut gathered = gathered;
        if let Some(base) = &self.placeholders {
            // Optional inputs are null in a run on placeholder inputs; a scatter over one gets
            // a list of one placeholder.
            for key in &keys {
                if gathered.get(key).is_none_or(Value::is_null) {
                    let item = process
                        .inputs
                        .iter()
                        .find(|input| short_name(&input.id) == key)
                        .map(|input| {
                            let mut types = step_scope.types();
                            types.add(&input.value_type);
                            let mut value = placeholder::input(input, &types, base);
                            let _ = files::normalize(&mut value);
                            value
                        })
                        .unwrap_or_else(|| Value::string(key.as_str()));
                    gathered.insert(key.clone(), Value::List(vec![item]));
                }
            }
        }
        // In a mock run over files without real content, a null is what real data would fill.
        let mock =
            self.options.mock && placeholder::is_fake(&Value::Map(gathered.clone()), &self.fake);
        let mut lengths = Vec::new();
        for key in &keys {
            match gathered.get(key) {
                Some(Value::List(items)) => lengths.push(items.len()),
                Some(Value::Null) | None if mock => {
                    return Err(at(format!(
                        "the scattered input `{key}` is null, not a list; the mock run has no \
                         real data to fill it"
                    )));
                }
                Some(Value::Null) | None => {
                    return Err(at(format!(
                        "the scattered input `{key}` is null, not a list"
                    )));
                }
                Some(other) => {
                    return Err(at(format!(
                        "the scattered input `{key}` is not a list, but {}",
                        other.summary()
                    )));
                }
            }
        }
        let method = step.scatter_method.unwrap_or(ScatterMethod::Dotproduct);
        let combinations: Vec<Vec<usize>> = match method {
            ScatterMethod::Dotproduct if self.placeholders.is_some() => {
                // Placeholder lists have one item, where the document may expect as many as
                // another list has: shorter lists repeat their items.
                let longest = lengths.iter().copied().max().unwrap_or(0);
                let shortest = lengths.iter().copied().min().unwrap_or(0);
                let count = if shortest == 0 { 0 } else { longest };
                (0..count)
                    .map(|index| lengths.iter().map(|length| index % length).collect())
                    .collect()
            }
            ScatterMethod::Dotproduct => {
                if lengths.iter().any(|length| *length != lengths[0]) {
                    return Err(at("dotproduct scatter needs lists of equal length".into()));
                }
                (0..lengths[0])
                    .map(|index| vec![index; keys.len()])
                    .collect()
            }
            ScatterMethod::NestedCrossproduct | ScatterMethod::FlatCrossproduct => {
                let mut combinations = vec![Vec::new()];
                for length in &lengths {
                    let mut next = Vec::new();
                    for combination in &combinations {
                        for index in 0..*length {
                            let mut extended = combination.clone();
                            extended.push(index);
                            next.push(extended);
                        }
                    }
                    combinations = next;
                }
                combinations
            }
        };
        let shape = match method {
            ScatterMethod::NestedCrossproduct => lengths.clone(),
            _ => vec![combinations.len()],
        };
        let scatter = self.jobs.len();
        self.jobs.push(Job {
            parent: Parent::Step {
                workflow,
                step: index,
            },
            state: State::Scatter(ScatterRun {
                outputs: step
                    .outputs
                    .iter()
                    .map(|id| short_name(id).to_owned())
                    .collect(),
                shape,
                results: vec![None; combinations.len()],
                remaining: combinations.len(),
            }),
        });
        for (element, combination) in combinations.iter().enumerate() {
            let mut inputs = gathered.clone();
            for (key, item) in keys.iter().zip(combination) {
                let value = match gathered.get(key) {
                    Some(Value::List(items)) => items[*item].clone(),
                    _ => Value::Null,
                };
                inputs.insert(key.clone(), value);
            }
            let shown = if method == ScatterMethod::Dotproduct {
                vec![element]
            } else {
                combination.clone()
            };
            let element_names = names.indexed(&shown);
            let job = self
                .step_job(step, inputs, &evaluator, &step_scope)
                .map_err(|error| format!("{}: {error}", element_names.display))?;
            match job {
                Some(job) => {
                    let parent = Parent::Element {
                        scatter,
                        index: element,
                    };
                    self.launch(process, job, &step_scope, parent, element_names, false)?;
                }
                None => {
                    let State::Scatter(run) = &mut self.jobs[scatter].state else {
                        unreachable!("the scatter was just made");
                    };
                    run.results[element] = Some(Map::new());
                    run.remaining -= 1;
                }
            }
        }
        if let State::Scatter(run) = &mut self.jobs[scatter].state
            && run.remaining == 0
        {
            let assembled = assemble(run);
            self.events.push_back(Event::Finished(scatter, assembled));
        }
        Ok(())
    }
}

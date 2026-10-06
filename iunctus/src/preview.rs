//! Previews cache reuse with recorded image pins and simulated uncached tasks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;

use task_runner::Lookup;
use workflow_graph::{Attempt, Executor, Outcome, Running, Simulator, Task};

/// The simulator, with the results of the cache for the tasks that a real run would take from
/// it.
pub struct Previewed {
    simulator: Simulator,
    lookup: Lookup,
    /// The digest that the latest real run pinned each image to.
    pins: HashMap<String, String>,
    /// The image of tasks that name none, as `-default-image` gives it to real runs.
    default_image: Option<String>,
}

impl Previewed {
    pub fn new(
        simulator: Simulator,
        lookup: Lookup,
        pins: HashMap<String, String>,
        default_image: Option<String>,
    ) -> Previewed {
        Previewed {
            simulator,
            lookup,
            pins,
            default_image,
        }
    }

    /// The result that a real run would take from the cache for a task.
    fn cached(&self, task: &Task) -> Option<Outcome> {
        if task.is_no_op() {
            return None;
        }
        // A file that the simulator made up is not what a real task would make, so a task
        // that reads one is not in the cache.
        let made_up = &self.simulator.directory;
        let simulated = |input: &workflow_graph::Input| {
            input.content.is_none() && Path::new(&input.source).starts_with(made_up)
        };
        if task.inputs.iter().any(simulated) {
            return None;
        }
        let image = task.image.as_ref().or(self.default_image.as_ref())?;
        let pinned = preview_image(image, &self.pins)?;
        let runtime = workflow_graph::runtime().ok()?;
        // A store that cannot be read only makes the preview simulate the task.
        let found = runtime.block_on(self.lookup.find(task, Some(pinned)));
        found.ok().flatten()
    }
}

fn preview_image(image: &str, pins: &HashMap<String, String>) -> Option<String> {
    if task_model::conda_image(image) {
        return None;
    }
    match image.contains("@sha256:") {
        true => Some(image.to_owned()),
        false => pins.get(image).cloned(),
    }
}

impl Executor for Previewed {
    fn run(&mut self, attempt: Attempt) -> Running {
        match self.cached(&attempt.task) {
            Some(outcome) => {
                if self.simulator.verbose {
                    println!("[cached] {}", attempt.task.name);
                }
                Running::ended(Ok(Outcome {
                    id: attempt.task.id,
                    ..outcome
                }))
            }
            None => self.simulator.run(attempt),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_conda_pins() {
        for marker in [
            "conda-env:/workflow/environment.yaml",
            "conda-container:debian:stable-slim|/workflow/environment.yaml",
        ] {
            let pinned = format!("sha256:{}", "a".repeat(64));
            let pins = [(marker.into(), pinned)].into();

            assert_eq!(preview_image(marker, &pins), None, "{marker}");
        }
    }
}

//! The entities of `ro-crate-metadata.json`: the workflow with its steps and tools, the engine
//! run, the workflow run, one action per task, the container images, the parameters, the
//! inputs of the workflow and the files. The shapes follow the example crates of the
//! Provenance Run Crate profile 0.5.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet};

use run_database::{Run, RunInputs, TaskDetail};
use serde_json::{Value, json};

use crate::data::Placed;
use crate::entities::{image_entity, language, language_id, one_or_many, push_unique, status};
use crate::formal::{FORMAL_PARAMETER, INPUT_SCHEMA, formal_parameter, input_id, schema_file};
use crate::ids::{encode, parameter_id, run_id, step_id, task_id, time, tool_id};
use crate::pinned::Pinned;

const PROFILES: [(&str, &str, &str); 4] = [
    (
        "https://w3id.org/ro/wfrun/process/0.5",
        "Process Run Crate",
        "0.5",
    ),
    (
        "https://w3id.org/ro/wfrun/workflow/0.5",
        "Workflow Run Crate",
        "0.5",
    ),
    (
        "https://w3id.org/ro/wfrun/provenance/0.5",
        "Provenance Run Crate",
        "0.5",
    ),
    (
        "https://w3id.org/workflowhub/workflow-ro-crate/1.0",
        "Workflow RO-Crate",
        "1.0",
    ),
];

/// The home of iunctus, which ran the workflow.
const ENGINE: &str = "https://github.com/arunaengine/iunctus";

/// The file that tells a reader what the crate holds.
pub(crate) const README: &str = "README.md";

/// What a simulated run says wherever a reader could take its files for results.
pub(crate) const SIMULATED: &str = "Simulated run: iunctus did not run the tasks. Its simulator made up \
    their output files, so the files, digests and times in this crate are not results of the \
    tools. A run with real data gives the real results.";

/// The run and everything the metadata names, gathered by [`crate::export`].
pub(crate) struct Record<'a> {
    pub run: &'a Run,
    /// The id of the workflow file in the crate.
    pub workflow: &'a str,
    /// The tasks that started, in the order they were asked for.
    pub tasks: &'a [TaskDetail],
    /// Where each file of the run is in the crate, by its location in the run; literal inputs
    /// by `content:<digest>`.
    pub placed: &'a HashMap<String, Placed>,
    /// When the crate was made, in ISO 8601.
    pub published: String,
    /// The version of the workflow: its Git revision, else the digest of its main file.
    pub version: Option<String>,
    pub license: Option<&'a str>,
    /// The JSON Schema of the inputs and the values of the run, when the driver described them.
    pub run_inputs: Option<&'a RunInputs>,
    /// What pins the run: the files of its workflow and command line, its remote files, and
    /// the schema of the outputs.
    pub pinned: &'a Pinned,
}

impl Record<'_> {
    /// The `@graph` of the metadata file.
    pub(crate) fn graph(&self) -> Vec<Value> {
        let simulated = self.run.simulated;
        let steps = self.steps();
        let mut graph = vec![
            json!({
                "@id": "ro-crate-metadata.json",
                "@type": "CreativeWork",
                "about": {"@id": "./"},
                "conformsTo": [
                    {"@id": "https://w3id.org/ro/crate/1.1"},
                    {"@id": "https://w3id.org/workflowhub/workflow-ro-crate/1.0"}
                ]
            }),
            self.root(),
        ];
        for (id, name, version) in PROFILES {
            graph.push(
                json!({"@id": id, "@type": "CreativeWork", "name": name, "version": version}),
            );
        }
        graph.push(json!({
            "@id": README,
            "@type": "File",
            "name": "README.md",
            "about": {"@id": "./"},
            "encodingFormat": "text/markdown"
        }));
        graph.push(self.license_entity());
        if simulated {
            graph.push(json!({
                "@id": "#simulated",
                "@type": "PropertyValue",
                "name": "simulated",
                "value": "true",
                "description": SIMULATED
            }));
        }
        graph.push(self.workflow_entity(&steps));
        graph.push(language(&self.run.language));
        if let Some(inputs) = self.run_inputs {
            graph.push(schema_file(self.workflow));
            graph.extend(
                input_schema::properties(&inputs.schema)
                    .iter()
                    .map(formal_parameter),
            );
        }
        if self.run_inputs.is_some() || self.pinned.output_schema.is_some() {
            graph.push(json!({
                "@id": FORMAL_PARAMETER,
                "@type": "CreativeWork",
                "name": "FormalParameter profile",
                "version": "1.0-RELEASE"
            }));
        }
        for (position, step) in steps.iter().enumerate() {
            graph.push(json!({
                "@id": step_id(step),
                "@type": "HowToStep",
                "name": step,
                "position": position.to_string(),
                "workExample": {"@id": tool_id(step)}
            }));
            let mut tool = json!({
                "@id": tool_id(step),
                "@type": "SoftwareApplication",
                "name": step,
                "url": {"@id": self.workflow}
            });
            if let Some(version) = &self.version {
                tool["version"] = json!(version);
            }
            graph.push(tool);
        }
        graph.push(json!({
            "@id": ENGINE,
            "@type": "SoftwareApplication",
            "name": "iunctus",
            "url": ENGINE,
            "softwareVersion": env!("CARGO_PKG_VERSION"),
            "description": "Runs Nextflow, Snakemake, CWL and Argo workflows in containers"
        }));
        graph.push(self.engine_action(&steps));
        graph.push(self.workflow_run());
        let declared = self.declared();
        for (name, value) in &self.run.parameters {
            let mut parameter = json!({
                "@id": parameter_id(name),
                "@type": "PropertyValue",
                "name": name,
                "value": value
            });
            if declared.contains(&name.as_str()) {
                parameter["exampleOfWork"] = json!({"@id": input_id(name)});
            }
            graph.push(parameter);
        }
        for step in &steps {
            let actions: Vec<Value> = self
                .tasks
                .iter()
                .filter(|task| &task.row.step == step)
                .map(|task| json!({"@id": task_id(task.row.id)}))
                .collect();
            graph.push(json!({
                "@id": format!("#control/{}", encode(step)),
                "@type": "ControlAction",
                "name": format!("orchestrate {step}"),
                "instrument": {"@id": step_id(step)},
                "object": one_or_many(actions)
            }));
        }
        // Each image by the reference it ran as, pinned to a digest, and the one the workflow
        // named, which may hold the tag.
        let mut images: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        for task in self.tasks {
            let named = task.description.image.clone();
            let ran = task
                .attempts
                .last()
                .and_then(|attempt| attempt.image.clone());
            let image_id = ran.or_else(|| named.clone()).map(|image| {
                let count = images.len();
                images
                    .entry((image, named))
                    .or_insert_with(|| format!("#image/{count}"))
                    .clone()
            });
            graph.extend(self.task_action(task, image_id.as_deref(), simulated));
        }
        for ((reference, named), id) in &images {
            graph.push(image_entity(id, reference, named.as_deref()));
        }
        graph.extend(self.file_entities());
        let read: Vec<&str> = self.inputs().map(|file| file.location.as_str()).collect();
        graph.extend(self.pinned.entities(self.run, self.workflow, &read));
        graph
    }

    fn root(&self) -> Value {
        let run = self.run;
        let kind = if run.simulated {
            "Simulated run"
        } else {
            "Run"
        };
        let mut description = format!(
            "{kind} {} of the {} workflow {} by iunctus, with the provenance of each task.",
            run.id, run.language, run.workflow
        );
        if run.simulated {
            description = format!("{SIMULATED} {description}");
        }
        let mut parts = vec![json!({"@id": self.workflow}), json!({"@id": README})];
        if self.run_inputs.is_some() {
            parts.push(json!({"@id": INPUT_SCHEMA}));
        }
        parts.extend(self.copied().map(|id| json!({"@id": id})));
        parts.extend(self.pinned.parts().into_iter().map(|id| json!({"@id": id})));
        // Files on the web are data entities of the crate too, as RO-Crate asks.
        parts.extend(self.remote().into_iter().map(|url| json!({"@id": url})));
        let mut mentions = vec![json!({"@id": run_id(&run.id)})];
        mentions.extend(
            self.tasks
                .iter()
                .map(|task| json!({"@id": task_id(task.row.id)})),
        );
        let mut root = json!({
            "@id": "./",
            "@type": "Dataset",
            "name": format!("{kind} {} of {}", run.id, self.workflow),
            "description": description,
            "datePublished": self.published,
            "license": {"@id": self.license_id()},
            "conformsTo": PROFILES.map(|(id, _, _)| json!({"@id": id})),
            "mainEntity": {"@id": self.workflow},
            "hasPart": parts,
            "mentions": mentions
        });
        if run.simulated {
            root["additionalProperty"] = json!({"@id": "#simulated"});
        }
        root
    }

    fn license_id(&self) -> String {
        self.license.unwrap_or("#license").to_owned()
    }

    fn license_entity(&self) -> Value {
        match self.license {
            Some(license) => json!({"@id": license, "@type": "CreativeWork", "name": license}),
            None => json!({
                "@id": "#license",
                "@type": "CreativeWork",
                "name": "No license given",
                "description": "The user who made this crate did not name a license for it."
            }),
        }
    }

    fn workflow_entity(&self, steps: &[String]) -> Value {
        let run = self.run;
        let mut workflow = json!({
            "@id": self.workflow,
            "@type": ["File", "SoftwareSourceCode", "ComputationalWorkflow", "HowTo"],
            "name": self.workflow,
            "programmingLanguage": {"@id": language_id(&run.language)},
            "hasPart": one_or_many(steps.iter().map(|step| json!({"@id": tool_id(step)}))),
            "step": one_or_many(steps.iter().map(|step| json!({"@id": step_id(step)}))),
            "url": format!("file://{}", run.workflow)
        });
        if let Some(version) = &self.version {
            workflow["version"] = json!(version);
        }
        let inputs: Vec<Value> = (self.declared().iter())
            .map(|name| json!({"@id": input_id(name)}))
            .collect();
        if !inputs.is_empty() {
            workflow["input"] = one_or_many(inputs);
        }
        let outputs = self.pinned.outputs();
        if !outputs.is_empty() {
            workflow["output"] = one_or_many(outputs.iter().map(|id| json!({"@id": id})));
        }
        workflow
    }

    /// The names of the inputs that the schema of the run declares.
    fn declared(&self) -> Vec<&str> {
        self.run_inputs.map_or_else(Vec::new, |inputs| {
            input_schema::properties(&inputs.schema)
                .iter()
                .map(|property| property.name)
                .collect()
        })
    }

    fn engine_action(&self, steps: &[String]) -> Value {
        let run = self.run;
        let controls: Vec<Value> = steps
            .iter()
            .map(|step| json!({"@id": format!("#control/{}", encode(step))}))
            .collect();
        let mut action = json!({
            "@id": format!("#engine/{}", run.id),
            "@type": "OrganizeAction",
            "name": format!("Run of iunctus {}", env!("CARGO_PKG_VERSION")),
            "description": format!("iunctus {}", run.arguments.join(" ")),
            "instrument": {"@id": ENGINE},
            "object": one_or_many(controls),
            "result": {"@id": run_id(&run.id)},
            "startTime": time(run.started)
        });
        status(&mut action, run.ended, &run.state, run.error.as_deref());
        action
    }

    fn workflow_run(&self) -> Value {
        let run = self.run;
        let produced: HashSet<&str> = self.outputs().map(|file| file.location.as_str()).collect();
        let consumed: HashSet<&str> = self.inputs().map(|file| file.location.as_str()).collect();
        let mut objects = Vec::new();
        for input in self.inputs() {
            if !produced.contains(input.location.as_str()) {
                push_unique(&mut objects, self.file_id(input));
            }
        }
        objects.extend(
            run.parameters
                .iter()
                .map(|(name, _)| Some(json!({"@id": parameter_id(name)}))),
        );
        // The files that the command line named, and the remote files that the workflow read
        // while it planned, are inputs of the run too.
        objects.extend(self.pinned.named_ids().map(|id| Some(json!({"@id": id}))));
        for address in self.pinned.remote.keys() {
            if !consumed.contains(address.as_str()) {
                push_unique(&mut objects, Some(json!({"@id": address})));
            }
        }
        let mut results = Vec::new();
        for output in self.outputs() {
            if !consumed.contains(output.location.as_str()) {
                push_unique(&mut results, self.file_id(output));
            }
        }
        let kind = if run.simulated {
            "Simulated run"
        } else {
            "Run"
        };
        let mut action = json!({
            "@id": run_id(&run.id),
            "@type": "CreateAction",
            "name": format!("{kind} {} of {}", run.id, self.workflow),
            "instrument": {"@id": self.workflow},
            "object": one_or_many(objects.into_iter().flatten()),
            "result": one_or_many(results.into_iter().flatten()),
            "startTime": time(run.started)
        });
        action["description"] = match run.simulated {
            true => json!(SIMULATED),
            false => json!(format!("iunctus {}", run.arguments.join(" "))),
        };
        status(&mut action, run.ended, &run.state, run.error.as_deref());
        action
    }

    /// The action of a task, with the property values of its environment.
    fn task_action(&self, task: &TaskDetail, image: Option<&str>, simulated: bool) -> Vec<Value> {
        let description = &task.description;
        let mut text = description.command.join(" ");
        if let Some(script) = &description.script {
            text = format!("{text}\n\n{script}");
        }
        if simulated {
            text = format!("Simulated, not run. {text}");
        }
        let id = task_id(task.row.id);
        let objects: Vec<Value> = task
            .inputs
            .iter()
            .filter_map(|file| self.file_id(file))
            .collect();
        let results: Vec<Value> = task
            .outputs
            .iter()
            .filter_map(|file| self.file_id(file))
            .collect();
        let mut action = json!({
            "@id": id,
            "@type": "CreateAction",
            "name": task.row.name,
            "description": text,
            "instrument": {"@id": tool_id(&task.row.step)},
            "object": one_or_many(objects),
            "result": one_or_many(results)
        });
        if let Some(started) = task.row.started {
            action["startTime"] = json!(time(started));
        }
        if let Some(image) = image {
            action["containerImage"] = json!({"@id": image});
        }
        let state = match task.row.state.as_str() {
            "succeeded" => "succeeded",
            "failed" | "stopped" => "failed",
            _ => "running",
        };
        let error = match (task.row.state.as_str(), task.row.exit_code) {
            ("stopped", _) => Some("the run ended before the task finished".to_owned()),
            (_, Some(code)) if code != 0 => Some(format!("exit code {code}")),
            _ => None,
        };
        status(&mut action, task.row.ended, state, error.as_deref());
        let mut entities = Vec::new();
        let mut environment = Vec::new();
        for (name, value) in &description.environment {
            let variable = format!("{id}/environment/{}", encode(name));
            environment.push(json!({"@id": variable}));
            entities.push(json!({
                "@id": variable,
                "@type": "PropertyValue",
                "name": name,
                "value": value
            }));
        }
        if !environment.is_empty() {
            action["environment"] = one_or_many(environment);
        }
        entities.insert(0, action);
        entities
    }

    /// The distinct steps of the tasks, in first-use order.
    fn steps(&self) -> Vec<String> {
        let mut steps: Vec<String> = Vec::new();
        for task in self.tasks {
            if !steps.contains(&task.row.step) {
                steps.push(task.row.step.clone());
            }
        }
        steps
    }
}

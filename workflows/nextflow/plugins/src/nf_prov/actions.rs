//! The actions of the RO-Crate of nf-prov: the modules and steps of the processes, and what
//! each task and each publish made.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

use nextflow_evaluator::interpreter::parse_yaml;

use super::wrroc::{Builder, id_of, reference};
use super::{media_type, normalise, task_hash};
use crate::report_format::{Json, value_json};

const COMPLETED: &str = "http://schema.org/CompletedActionStatus";

impl Builder<'_> {
    /// The module of each process of the recorded tasks, by the process name.
    pub(super) fn modules(&self) -> BTreeMap<String, Module> {
        let start = self.run.start;
        let project = Path::new(&start.project_dir);
        let mut modules = BTreeMap::new();
        for task in self.run.tasks {
            let process_name = task.record["process"].as_str().unwrap_or_default();
            if modules.contains_key(process_name) {
                continue;
            }
            let process = (task.process_id)
                .and_then(|id| start.processes.iter().find(|process| process.id == id));
            let Some((process, script)) =
                process.and_then(|process| Some((process, process.script.as_deref()?)))
            else {
                tracing::warn!(
                    "Could not identify process definition for `{process_name}` -- resulting \
                     RO-Crate may be invalid (hint: define aliased processes in a separate module \
                     script to fix this issue)"
                );
                continue;
            };
            let relative = Path::new(script).strip_prefix(project).map_or_else(
                |_| script.to_owned(),
                |relative| relative.to_string_lossy().into_owned(),
            );
            let id = format!("#module/{relative}/{}", process.definition);
            let meta = Path::new(script).with_file_name("meta.yml");
            let meta = (std::fs::read_to_string(meta).ok())
                .and_then(|text| parse_yaml(&text).ok())
                .map(|value| value_json(&value))
                .unwrap_or_default();
            let mut tools = Vec::new();
            for tool in meta["tools"].as_array().into_iter().flatten() {
                for (tool_name, details) in tool.as_object().into_iter().flatten() {
                    tools.push(Json::object([
                        ("@id", Json::from(format!("{id}/{tool_name}"))),
                        ("@type", "SoftwareApplication".into()),
                        ("name", tool_name.as_str().into()),
                        ("description", Json::from(&details["description"])),
                    ]));
                }
            }
            let entity = Json::object([
                ("@id", Json::from(id.as_str())),
                ("@type", "SoftwareApplication".into()),
                (
                    "name",
                    meta["name"].as_str().unwrap_or(&process.definition).into(),
                ),
                ("url", normalise(script).into()),
                ("description", Json::from(&meta["description"])),
                ("softwareVersion", Json::from(&meta["version"])),
                ("license", Json::from(&meta["license"])),
                ("keywords", Json::from(&meta["keywords"])),
                (
                    "hasPart",
                    (!tools.is_empty())
                        .then(|| Json::List(tools.iter().filter_map(id_of).collect()))
                        .into(),
                ),
            ]);
            modules.insert(process_name.to_owned(), Module { id, entity, tools });
        }
        modules
    }

    /// The HowToStep and the ControlAction of each process, by process step and name.
    pub(super) fn steps(&self, modules: &BTreeMap<String, Module>) -> Vec<(Json, Json)> {
        let mut processes: Vec<(u32, &str)> = Vec::new();
        for task in self.run.tasks {
            let name = task.record["process"].as_str().unwrap_or_default();
            if !processes.iter().any(|(_, known)| *known == name) {
                processes.push((self.run.step(task), name));
            }
        }
        processes.sort();
        (processes.into_iter())
            .map(|(step, name)| {
                let tasks = (self.run.tasks.iter())
                    .filter(|task| task.record["process"] == name)
                    .map(|task| reference(&format!("#task/{}", task_hash(task))));
                let module = modules.get(name).map(|module| reference(&module.id));
                let how = Json::object([
                    ("@id", Json::from(format!("#process-step/{name}"))),
                    ("@type", "HowToStep".into()),
                    ("workExample", module.into()),
                    ("position", u64::from(step).into()),
                ]);
                let control = Json::object([
                    ("@id", Json::from(format!("#process-control/{name}"))),
                    ("@type", "ControlAction".into()),
                    ("instrument", reference(&format!("#process-step/{name}"))),
                    ("name", format!("Orchestrate process {name}").into()),
                    ("object", Json::list(tasks)),
                ]);
                (how, control)
            })
            .collect()
    }

    /// The CreateAction of each task and the entities of its output files.
    pub(super) fn tasks(&self, agent: Option<Json>, modules: &BTreeMap<String, Module>) -> Tasks {
        let mut tasks = Tasks::default();
        for task in self.run.tasks {
            let hash = task_hash(task);
            let objects = (task.inputs.iter()).map(|input| match self.run.producer(&input.path) {
                Some((producer, name)) => reference(&format!(
                    "#task/{}/{name}",
                    task_hash(&self.run.tasks[producer])
                )),
                None => reference(self.ids.get(&input.path).unwrap_or(&normalise(&input.path))),
            });
            let results = (task.outputs.iter())
                .map(|output| reference(&format!("#task/{hash}/{}", output.name)));
            let process = task.record["process"].as_str().unwrap_or_default();
            let failed = task.record["status"] != "COMPLETED" && task.record["status"] != "CACHED";
            let status = if failed {
                "http://schema.org/FailedActionStatus"
            } else {
                COMPLETED
            };
            tasks.actions.push(Json::object([
                ("@id", Json::from(format!("#task/{hash}"))),
                ("@type", "CreateAction".into()),
                ("name", Json::from(&task.record["name"])),
                (
                    "instrument",
                    modules
                        .get(process)
                        .map(|module| reference(&module.id))
                        .into(),
                ),
                ("agent", agent.clone().into()),
                ("object", Json::list(objects)),
                ("result", Json::list(results)),
                ("actionStatus", status.into()),
            ]));
            for output in &task.outputs {
                tasks.outputs.push(Json::object([
                    ("@id", Json::from(format!("#task/{hash}/{}", output.name))),
                    ("@type", "CreativeWork".into()),
                    ("name", output.name.as_str().into()),
                    ("encodingFormat", media_type(&output.path).into()),
                ]));
            }
        }
        tasks
    }

    /// One publish action for each source whose published files are inside the crate.
    pub(super) fn publishes(&self, agent: Option<Json>, engine: &str) -> Vec<Json> {
        let mut sources: Vec<(&str, Vec<String>)> = Vec::new();
        for (target, source) in self.run.published {
            let Some(id) = self.relative(target) else {
                continue;
            };
            match sources.iter_mut().find(|(known, _)| known == source) {
                Some((_, targets)) => targets.push(id),
                None => sources.push((source, vec![id])),
            }
        }
        (sources.into_iter())
            .map(|(source, targets)| {
                let (id, object) = match self.run.producer(source) {
                    Some((producer, name)) => {
                        let hash = task_hash(&self.run.tasks[producer]);
                        (
                            format!("#publish/{hash}/{name}"),
                            format!("#task/{hash}/{name}"),
                        )
                    }
                    None => {
                        let input = self
                            .ids
                            .get(source)
                            .cloned()
                            .unwrap_or_else(|| normalise(source));
                        (format!("#publish/input/{}", targets[0]), input)
                    }
                };
                Json::object([
                    ("@id", Json::from(id)),
                    ("@type", "CreateAction".into()),
                    ("name", "publish".into()),
                    ("agent", agent.clone().into()),
                    ("instrument", reference(engine)),
                    ("object", reference(&object)),
                    (
                        "result",
                        Json::list(targets.iter().map(|target| reference(target))),
                    ),
                    ("actionStatus", COMPLETED.into()),
                ])
            })
            .collect()
    }
}

#[derive(Default)]
pub(super) struct Tasks {
    pub(super) actions: Vec<Json>,
    pub(super) outputs: Vec<Json>,
}

pub(super) struct Module {
    pub(super) id: String,
    pub(super) entity: Json,
    pub(super) tools: Vec<Json>,
}

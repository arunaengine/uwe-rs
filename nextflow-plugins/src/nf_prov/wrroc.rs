//! The `wrroc` report of nf-prov: a Workflow Run RO-Crate in the folder of the report file,
//! with the main script, the resolved config and the inputs that it copies there.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Run, file_name, normalise, save};
use crate::report_format::{Json, NEXTFLOW_VERSION, local_date, now, second_time};

const LANGUAGE: &str = "https://w3id.org/workflowhub/workflow-ro-crate#nextflow";
const WORKFLOW_CRATE: &str = "https://w3id.org/workflowhub/workflow-ro-crate/1.0";
const PROFILES: [(&str, &str, &str); 4] = [
    (
        "https://w3id.org/ro/wfrun/process/0.1",
        "Process Run Crate",
        "0.1",
    ),
    (
        "https://w3id.org/ro/wfrun/workflow/0.1",
        "Workflow Run Crate",
        "0.1",
    ),
    (
        "https://w3id.org/ro/wfrun/provenance/0.1",
        "Provenance Run Crate",
        "0.1",
    ),
    (WORKFLOW_CRATE, "Workflow RO-Crate", "1.0"),
];

/// Writes the crate: copies its files next to the report file, then the metadata file.
pub(super) fn write(run: &Run, options: &Value, file: &Path) -> std::io::Result<()> {
    let folder = file
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    std::fs::create_dir_all(&folder)?;
    let name = file
        .file_name()
        .map_or("ro-crate-metadata.json".into(), |name| {
            name.to_string_lossy().into_owned()
        });
    let mut builder = Builder {
        run,
        options,
        folder,
        files: Vec::new(),
        ids: HashMap::new(),
    };
    let graph = builder.graph(&name)?;
    let document = Json::object([
        (
            "@context",
            Json::from("https://w3id.org/ro/crate/1.1/context"),
        ),
        ("@graph", Json::List(graph)),
    ]);
    save(file, &single(document.without_nulls()).pretty())
}

pub(super) struct Builder<'a> {
    pub(super) run: &'a Run<'a>,
    pub(super) options: &'a Value,
    pub(super) folder: PathBuf,
    /// The entities of the files copied into the crate, in the order they were copied.
    pub(super) files: Vec<Json>,
    /// The entity id of each input path that a param or a copy names.
    pub(super) ids: HashMap<String, String>,
}

impl Builder<'_> {
    fn graph(&mut self, name: &str) -> std::io::Result<Vec<Json>> {
        let start = self.run.start;
        let manifest = &start.manifest;
        let engine =
            format!("https://github.com/nextflow-io/nextflow/releases/tag/v{NEXTFLOW_VERSION}");
        let main = file_name(&start.script).to_owned();
        let license = self.option("license");
        if license.is_none() {
            tracing::warn!(
                "Missing license for Workflow Run RO-Crate -- the resulting crate will be invalid"
            );
        }
        if start.repository.is_none() {
            tracing::warn!(
                "Could not determine pipeline repository URL for Workflow Run RO-Crate -- launch \
                 the pipeline with canonical URL (e.g. `nextflow run nextflow-io/hello`) to \
                 ensure that the pipeline repository URL is recorded in the crate"
            );
        }
        self.copy_project(&main)?;
        let (person, people) = self.people();
        let agent = person.clone().map(|id| reference(&id));
        let (param_formals, param_values, param_files) = self.params()?;
        let inputs = self.inputs(&param_files);
        let (output_formals, output_values, output_files) = self.outputs();
        let modules = self.modules();
        let steps = self.steps(&modules);
        let tasks = self.tasks(agent.clone(), &modules);
        let publishes = self.publishes(agent.clone(), &engine);

        let session = format!("#{}", start.session_id);
        let mut has_part = vec![reference(&main)];
        has_part.extend(license.clone().map(|license| reference(&license)));
        has_part.extend(self.files.iter().filter_map(id_of));
        has_part.extend(inputs.entities.iter().filter_map(id_of));
        has_part.extend(output_files.iter().filter_map(id_of));
        let mut mentions = vec![reference(&session)];
        mentions.extend(inputs.temporary.iter().filter_map(id_of));
        mentions.extend(tasks.actions.iter().filter_map(id_of));
        mentions.extend(tasks.outputs.iter().filter_map(id_of));
        mentions.extend(publishes.iter().filter_map(id_of));
        let title = (manifest["name"].as_str()).unwrap_or(&start.project_name);
        let publisher = self.option("publisher").filter(|publisher| {
            Some(publisher) == person.as_ref() || people.organization.as_ref() == Some(publisher)
        });

        let mut graph = vec![
            Json::object([
                ("@id", Json::from(name)),
                ("@type", "CreativeWork".into()),
                ("about", reference("./")),
                (
                    "conformsTo",
                    Json::list([
                        reference("https://w3id.org/ro/crate/1.1"),
                        reference(WORKFLOW_CRATE),
                    ]),
                ),
            ]),
            Json::object([
                ("@id", Json::from("./")),
                ("@type", "Dataset".into()),
                ("author", agent.clone().into()),
                ("publisher", publisher.as_deref().map(reference).into()),
                ("datePublished", local_date(now()).into()),
                (
                    "conformsTo",
                    Json::list(PROFILES.iter().map(|(id, _, _)| reference(id))),
                ),
                ("name", format!("Workflow run of {title}").into()),
                ("description", Json::from(&manifest["description"])),
                ("hasPart", Json::List(has_part)),
                ("mainEntity", reference(&main)),
                ("mentions", Json::List(mentions)),
                ("license", license.clone().into()),
            ]),
        ];
        graph.extend(PROFILES.iter().map(|(id, name, version)| {
            Json::object([
                ("@id", Json::from(*id)),
                ("@type", "CreativeWork".into()),
                ("name", (*name).into()),
                ("version", (*version).into()),
            ])
        }));
        let mut module_ids: Vec<&String> = modules.values().map(|module| &module.id).collect();
        module_ids.sort();
        module_ids.dedup();
        let creator = match people.creators.is_empty() {
            true => Json::from(&manifest["author"]),
            false => Json::List(people.creators.iter().map(|id| reference(id)).collect()),
        };
        let version = (start.commit_id.clone().map(Json::from))
            .unwrap_or_else(|| Json::from(&manifest["version"]));
        graph.push(Json::object([
            ("@id", Json::from(main.as_str())),
            (
                "@type",
                Json::list([
                    "File",
                    "SoftwareSourceCode",
                    "ComputationalWorkflow",
                    "HowTo",
                ]),
            ),
            (
                "conformsTo",
                reference("https://bioschemas.org/profiles/ComputationalWorkflow/1.0-RELEASE"),
            ),
            ("name", title.into()),
            ("description", Json::from(&manifest["description"])),
            ("programmingLanguage", reference(LANGUAGE)),
            ("creator", creator),
            ("codeRepository", start.repository.as_deref().into()),
            ("version", version),
            ("license", Json::from(&manifest["license"])),
            (
                "url",
                start
                    .repository
                    .as_deref()
                    .map(|_| normalise(&start.script))
                    .into(),
            ),
            ("encodingFormat", "application/nextflow".into()),
            (
                "runtimePlatform",
                format!("Nextflow {NEXTFLOW_VERSION}").into(),
            ),
            (
                "image",
                self.ids.get("diagram").map(|id| reference(id)).into(),
            ),
            (
                "hasPart",
                Json::List(module_ids.iter().map(|id| reference(id)).collect()),
            ),
            (
                "input",
                Json::List(param_formals.iter().filter_map(id_of).collect()),
            ),
            (
                "output",
                Json::List(output_formals.iter().filter_map(id_of).collect()),
            ),
            (
                "step",
                Json::List(steps.iter().filter_map(|step| id_of(&step.0)).collect()),
            ),
        ]));
        graph.push(Json::object([
            ("@id", Json::from(LANGUAGE)),
            ("@type", "ComputerLanguage".into()),
            ("name", "Nextflow".into()),
            ("identifier", "https://www.nextflow.io/".into()),
            ("url", "https://www.nextflow.io/".into()),
            ("version", NEXTFLOW_VERSION.into()),
        ]));
        graph.push(Json::object([
            ("@id", Json::from(engine.as_str())),
            ("@type", "SoftwareApplication".into()),
            ("name", format!("Nextflow {NEXTFLOW_VERSION}").into()),
            ("url", "https://www.nextflow.io/".into()),
            ("softwareVersion", NEXTFLOW_VERSION.into()),
        ]));
        let mut seen = Vec::new();
        for module in modules.values() {
            if !seen.contains(&&module.id) {
                seen.push(&module.id);
            }
        }
        seen.sort();
        for id in seen {
            if let Some(module) = modules.values().find(|module| &module.id == id) {
                graph.push(module.entity.clone());
                graph.extend(module.tools.iter().cloned());
            }
        }
        graph.extend(param_formals);
        graph.extend(output_formals);
        graph.extend(steps.iter().map(|step| step.0.clone()));
        let controls: Vec<Json> = steps.iter().map(|step| step.1.clone()).collect();
        let times = [
            ("startTime", Json::from(second_time(start.start))),
            ("endTime", Json::from(second_time(self.run.end.end))),
        ];
        let mut organize = Json::object([
            ("@id", Json::from(format!("{main}#organize"))),
            ("@type", "OrganizeAction".into()),
            ("agent", agent.clone().into()),
            ("instrument", reference(&engine)),
            ("name", format!("Run of Nextflow {NEXTFLOW_VERSION}").into()),
            (
                "object",
                Json::List(controls.iter().filter_map(id_of).collect()),
            ),
            ("result", reference(&session)),
        ]);
        let mut objects: Vec<Json> = param_values.iter().filter_map(id_of).collect();
        objects.extend(inputs.entities.iter().filter_map(id_of));
        let mut results: Vec<Json> = output_values.iter().filter_map(id_of).collect();
        results.extend(output_files.iter().filter_map(id_of));
        let mut action = Json::object([
            ("@id", Json::from(session.as_str())),
            ("@type", "CreateAction".into()),
            ("agent", agent.into()),
            (
                "name",
                format!("Nextflow workflow run {}", start.session_id).into(),
            ),
            (
                "description",
                format!("Command-line: {}", start.command_line).into(),
            ),
        ]);
        for (key, value) in times.clone() {
            organize.insert(key, value.clone());
            action.insert(key, value);
        }
        action.insert("instrument", reference(&main));
        action.insert("object", Json::List(objects));
        action.insert("result", Json::List(results));
        graph.push(organize);
        graph.push(action);
        if let Some(license) = &license {
            graph.push(Json::object([
                ("@id", Json::from(license.as_str())),
                ("@type", "CreativeWork".into()),
            ]));
        }
        graph.extend(people.entities);
        graph.append(&mut self.files);
        graph.extend(param_values);
        graph.extend(output_values);
        graph.extend(controls);
        graph.extend(inputs.temporary);
        graph.extend(tasks.actions);
        graph.extend(tasks.outputs);
        graph.extend(publishes);
        graph.extend(inputs.entities);
        graph.extend(output_files);
        Ok(graph)
    }

    pub(super) fn option(&self, key: &str) -> Option<String> {
        self.options[key].as_str().map(str::to_owned)
    }

    /// The id of a path inside the crate folder, relative to it; a folder's ends with `/`.
    pub(super) fn relative(&self, path: &str) -> Option<String> {
        let relative = Path::new(path).strip_prefix(&self.folder).ok()?;
        let mut id = relative.to_string_lossy().into_owned();
        if Path::new(path).is_dir() {
            id.push('/');
        }
        Some(id)
    }
}

pub(super) fn reference(id: &str) -> Json {
    Json::object([("@id", Json::from(id))])
}

/// A reference to an entity, by its id.
pub(super) fn id_of(entity: &Json) -> Option<Json> {
    match entity.get("@id") {
        Some(Json::Text(id)) => Some(reference(id)),
        _ => None,
    }
}

/// The document with each list of one item replaced by the item, at every depth.
fn single(value: Json) -> Json {
    match value {
        Json::List(mut items) if items.len() == 1 => single(items.remove(0)),
        Json::List(items) => Json::List(items.into_iter().map(single).collect()),
        Json::Object(entries) => Json::Object(
            (entries.into_iter())
                .map(|(key, value)| (key, single(value)))
                .collect(),
        ),
        other => other,
    }
}

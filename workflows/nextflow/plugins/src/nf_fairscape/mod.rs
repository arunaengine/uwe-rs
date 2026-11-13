//! nf-fairscape 0.2.0: the FAIRSCAPE EVI RO-Crate of a successful run, and its evidence graph.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod files;
mod graph;
mod settings;
mod tasks;

use std::collections::HashMap;
use std::path::Path;

use sha1::{Digest as _, Sha1};

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Start, Task};
use crate::report_format::Json;
use files::{Dataset, size};
use graph::{ends, evidence, link};
use settings::{Settings, activate};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["fairscape"],
    observer: Some(activate),
    ..Plugin::new("nf-fairscape", Kind::Observer, State::Implemented)
};

const EVI: &str = "https://w3id.org/EVI#";
const COMPUTATION: [&str; 2] = ["prov:Activity", "https://w3id.org/EVI#Computation"];
const SOFTWARE: [&str; 2] = ["prov:Entity", "https://w3id.org/EVI#Software"];
const DATASET: [&str; 2] = ["prov:Entity", "https://w3id.org/EVI#Dataset"];

/// What the run told the plugin so far.
struct Fairscape {
    settings: Settings,
    start: Option<Box<Start>>,
    /// The tasks that succeeded or came from the cache, in the order they ended.
    tasks: Vec<Task>,
    /// Each published file that `patterns` lets in, as its source and target.
    published: Vec<(String, String)>,
}

impl Observer for Fairscape {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.start = Some(start.clone()),
            Event::TaskCompleted(task) | Event::TaskCached(task) => self.tasks.push(task.clone()),
            Event::FilePublished { source, target } => {
                let patterns = &self.settings.patterns;
                let glob = nextflow_evaluator::interpreter::glob::matches;
                if patterns.is_empty()
                    || patterns.iter().any(|it| glob(&format!("**/{it}"), target))
                {
                    self.published.push((source.clone(), target.clone()));
                }
            }
            Event::RunEnded(end) if end.success => {
                for output in &end.outputs {
                    if output.index.is_none() && output.files.len() != 1 {
                        tracing::warn!(
                            "Workflow output `{}` should either be a single path or declare an \
                             index file in order to be included in provenance reports",
                            output.name
                        );
                    }
                }
                if let Err(error) = self.write(end) {
                    tracing::warn!(
                        %error,
                        "Error occurred while rendering FAIRSCAPE provenance crate -- see \
                         Nextflow log for details"
                    );
                }
            }
            _ => {}
        }
    }
}

impl Fairscape {
    fn write(&self, end: &End) -> std::io::Result<()> {
        let Some(start) = &self.start else {
            return Ok(());
        };
        let folder = self
            .settings
            .file
            .parent()
            .unwrap_or(Path::new("/"))
            .to_owned();
        std::fs::create_dir_all(&folder)?;
        let mut builder = Builder::new(&self.settings, start, end, &folder);
        builder.copy_workflow();
        let mut entities = builder.entities(&self.tasks, &self.published);
        if self.settings.inverses {
            link(&mut entities);
        }
        let root_id = builder.root.clone();
        let (inputs, outputs) = ends(&entities);
        let root = &mut entities[1];
        if self.settings.evidence {
            root.insert(&format!("{EVI}inputs"), refs(&inputs));
            root.insert(&format!("{EVI}outputs"), refs(&outputs));
        }
        if self.settings.sizes {
            root.insert("contentSize", size(&folder).to_string());
        }
        for (key, value) in &self.settings.metadata {
            root.insert(key, value.clone());
        }
        let document = Json::Object(Vec::new())
            .with("@context", context())
            .with("@graph", Json::List(entities.clone()));
        std::fs::write(&self.settings.file, document.pretty())?;
        if self.settings.evidence {
            let graph = evidence(&entities, &root_id, &outputs, &builder.name);
            let file = folder.join("provenance-graph.json");
            std::fs::write(&file, graph.python())?;
            tracing::info!("FAIRSCAPE provenance graph: {}", file.display());
        }
        Ok(())
    }
}

/// Builds the entities of the crate from what the run told.
struct Builder<'a> {
    settings: &'a Settings,
    start: &'a Start,
    end: &'a End,
    folder: &'a Path,
    /// The name of the run: the manifest's name, else the project's.
    name: String,
    author: String,
    root: String,
    run: String,
    workflow: String,
    engine: String,
    /// The config files copied into `workflow/`, by name.
    configs: Vec<String>,
}

impl<'a> Builder<'a> {
    fn new(settings: &'a Settings, start: &'a Start, end: &'a End, folder: &'a Path) -> Self {
        let manifest = |key: &str| start.manifest[key].as_str().map(str::to_owned);
        let name = manifest("name").unwrap_or_else(|| start.project_name.clone());
        let contributors = start.manifest["contributors"].as_array().map(|people| {
            let names = people.iter().filter_map(|person| person["name"].as_str());
            names.collect::<Vec<_>>().join(", ")
        });
        let author = (settings.author.clone())
            .or_else(|| manifest("author"))
            .or(contributors.filter(|names| !names.is_empty()))
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_default();
        let ark = |prefix: &str, name: &str, seed: &str| ark(&settings.naan, prefix, name, seed);
        let version = manifest("version")
            .or(start.commit_id.clone())
            .unwrap_or_default();
        let script = uri(&start.script);
        Builder {
            root: ark("rocrate", &name, &start.session_id),
            run: ark("computation", &name, &format!("{}#run", start.session_id)),
            workflow: ark("software", &name, &format!("{script}{version}")),
            engine: ark(
                "software",
                "uwe",
                &format!("uwe-{}", env!("CARGO_PKG_VERSION")),
            ),
            name,
            author,
            settings,
            start,
            end,
            folder,
            configs: Vec::new(),
        }
    }

    fn ark(&self, prefix: &str, name: &str, seed: &str) -> String {
        ark(&self.settings.naan, prefix, name, seed)
    }

    fn manifest(&self, key: &str) -> Option<String> {
        self.start.manifest[key].as_str().map(str::to_owned)
    }

    fn version(&self) -> String {
        (self.manifest("version").or(self.start.commit_id.clone())).unwrap_or_default()
    }

    /// The descriptor, the root, the Computations, the Software and the Datasets, in order.
    fn entities(&self, tasks: &[Task], published: &[(String, String)]) -> Vec<Json> {
        let datasets = self.datasets(tasks, published);
        let by_key: HashMap<&str, &Dataset> = (datasets.iter())
            .map(|dataset| (dataset.id.as_str(), dataset))
            .collect();
        let made: Vec<&Dataset> = datasets.iter().filter(|it| it.role == "produced").collect();
        let used = |task: &Task| -> Vec<String> {
            let found = task
                .inputs
                .iter()
                .map(|file| self.dataset_id(&file.path, &file.name));
            found
                .filter(|id| by_key.contains_key(id.as_str()))
                .collect()
        };
        let mut processes: Vec<(String, String)> = Vec::new();
        let mut computations = Vec::new();
        for task in tasks {
            let (process, software) = self.process(task);
            if !processes.iter().any(|(known, _)| *known == process) {
                processes.push((process.clone(), software.clone()));
            }
            computations.push(self.computation(task, &software, used(task), &made));
        }
        let mut run_inputs: Vec<String> = Vec::new();
        for dataset in &datasets {
            let read = tasks.iter().any(|task| used(task).contains(&dataset.id));
            if dataset.role == "used" && (read || dataset.part_of.is_none()) {
                run_inputs.push(dataset.id.clone());
            }
        }
        let generated: Vec<String> = (datasets.iter())
            .filter(|dataset| dataset.published)
            .map(|dataset| dataset.id.clone())
            .collect();
        let mut entities = vec![self.descriptor(), Json::Object(Vec::new())];
        entities.push(self.run_computation(&run_inputs, &generated));
        entities.extend(computations);
        entities.push(self.workflow_software());
        entities.push(self.engine_software());
        for (process, software) in &processes {
            entities.push(self.process_software(process, software, tasks));
        }
        entities.extend(datasets.iter().map(|dataset| self.dataset(dataset)));
        let parts: Vec<String> = (entities[2..].iter())
            .filter_map(|entity| entity.get("@id")?.as_str().map(str::to_owned))
            .collect();
        entities[1] = self.root_entity(&parts);
        entities
    }

    fn descriptor(&self) -> Json {
        Json::Object(Vec::new())
            .with("@id", "ro-crate-metadata.json")
            .with("@type", "CreativeWork")
            .with("conformsTo", reference("https://w3id.org/ro/crate/1.2"))
            .with("about", reference(&self.root))
    }

    fn root_entity(&self, parts: &[String]) -> Json {
        let description = (self.settings.description.clone())
            .or_else(|| self.manifest("description"))
            .filter(|text| text.chars().count() >= 10)
            .unwrap_or_else(|| {
                format!(
                    "RO-Crate of the Nextflow workflow run '{}' (session {})",
                    self.name, self.start.session_id
                )
            });
        let license = (self.settings.license.clone())
            .or_else(|| self.manifest("license"))
            .unwrap_or_else(|| "Apache-2.0".into());
        let mut root = Json::Object(Vec::new())
            .with("@id", self.root.as_str())
            .with("@type", texts(&["Dataset", "https://w3id.org/EVI#ROCrate"]))
            .with(
                "conformsTo",
                reference("https://w3id.org/fairscape/profile/0.1"),
            )
            .with("name", self.name.as_str())
            .with("description", description)
            .with("keywords", texts(&self.settings.keywords))
            .with("version", self.version())
            .with("author", self.author.as_str())
            .with("license", license);
        if let Some(organization) = &self.settings.organization {
            root.insert("publisher", organization.as_str());
        }
        root.with("datePublished", time(self.end.end))
            .with("hasPart", refs(parts))
    }
}

/// The `@context` of the crate.
fn context() -> Json {
    let mut context = Json::Object(Vec::new())
        .with("@vocab", "https://schema.org/")
        .with("evi", EVI)
        .with("rai", "http://mlcommons.org/croissant/RAI/")
        .with("prov", "http://www.w3.org/ns/prov#");
    for term in [
        "usedSoftware",
        "usedDataset",
        "generatedBy",
        "generated",
        "annotates",
        "hasDistribution",
    ] {
        let id = Json::Object(Vec::new())
            .with("@id", format!("{EVI}{term}"))
            .with("@type", "@id");
        context.insert(term, id);
    }
    context.with("localPath", "https://w3id.org/ro/terms#localPath")
}

/// `ark:<naan>/<prefix>-<slug>-<the first 7 hex digits of the SHA-1 of the seed>`.
fn ark(naan: &str, prefix: &str, name: &str, seed: &str) -> String {
    let digest = hex(&Sha1::digest(seed.as_bytes()));
    let mut slug = String::new();
    for character in name.to_lowercase().chars() {
        match character.is_ascii_lowercase() || character.is_ascii_digit() {
            true => slug.push(character),
            false if !slug.ends_with('-') => slug.push('-'),
            false => {}
        }
    }
    format!(
        "ark:{naan}/{prefix}-{}-{}",
        slug.trim_matches('-'),
        &digest[..7]
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A path as a URI, like `file:///data/in.csv`; an address like `s3://b/k` stays as it is.
fn uri(path: &str) -> String {
    match path.contains("://") {
        true => path.to_owned(),
        false => format!(
            "file://{}",
            std::path::absolute(path).unwrap_or_default().display()
        ),
    }
}

fn file_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_owned()
}

/// A time as local ISO 8601 with its offset, like `2026-11-06T10:33:35.061+01:00`.
fn time(millis: i64) -> String {
    let zone = jiff::tz::TimeZone::system();
    jiff::Timestamp::from_millisecond(millis)
        .map(|stamp| {
            stamp
                .to_zoned(zone)
                .strftime("%Y-%m-%dT%H:%M:%S%.f%:z")
                .to_string()
        })
        .unwrap_or_default()
}

fn reference(id: &str) -> Json {
    Json::Object(Vec::new()).with("@id", id)
}

fn refs(ids: &[String]) -> Json {
    Json::List(ids.iter().map(|id| reference(id)).collect())
}

fn texts(items: &[impl AsRef<str>]) -> Json {
    Json::List(items.iter().map(|item| Json::from(item.as_ref())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_arks() {
        // The ids of the recorded crate.
        let session = "cfe26198-d654-474e-bd8e-89988d856eeb";
        assert_eq!(
            ark("59853", "rocrate", "probe-fair", session),
            "ark:59853/rocrate-probe-fair-1acf6e8"
        );
        assert_eq!(
            ark(
                "59853",
                "computation",
                "probe-fair",
                &format!("{session}#run")
            ),
            "ark:59853/computation-probe-fair-6836b4f"
        );
        assert_eq!(
            ark(
                "59853",
                "dataset",
                "nextflow.config",
                "workflow/nextflow.config"
            ),
            "ark:59853/dataset-nextflow-config-f03699b"
        );
        assert_eq!(
            ark(
                "59853",
                "computation",
                "SHOUT (3)",
                "8d168bc859d739f9bcebe39951f4d568"
            ),
            "ark:59853/computation-shout-3-508ced3"
        );
    }
}

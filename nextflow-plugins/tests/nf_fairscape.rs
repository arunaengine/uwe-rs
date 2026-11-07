//! nf-fairscape against the plugin 0.2.0 with Nextflow 26.04.6: a real run of the recorded probe
//! writes a crate and an evidence graph with the entities and links of the recorded ones.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use serde_json::Value as Json;
use workflow_graph::Running;
use workflow_graph::{Attempt, Engine, Executor, Files, Launch, LocalFiles, Outcome, OutputFile};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_fairscape")
}

/// Writes what the scripts of the probe write: SPLIT a part per line, SHOUT the greeting and
/// its part in capitals.
struct Probe(PathBuf);

impl Executor for Probe {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let folder = self.0.join(task.id.to_string());
        std::fs::create_dir_all(&folder).expect("creates");
        let input = (task.inputs.iter())
            .find(|input| input.content.is_none())
            .expect("an input file");
        let text = std::fs::read_to_string(&input.source).expect("reads");
        let made: Vec<(String, String)> = match task.step.as_str() {
            "SPLIT" => (text.lines().enumerate())
                .map(|(place, line)| (format!("part_{place:02}.txt"), format!("{line}\n")))
                .collect(),
            _ => {
                let base = input.path.trim_end_matches(".txt");
                vec![(
                    format!("{base}.up.txt"),
                    format!("hello\n{}", text.to_uppercase()),
                )]
            }
        };
        let files = (made.into_iter())
            .map(|(name, text)| {
                std::fs::write(folder.join(&name), text).expect("writes");
                OutputFile {
                    location: folder.join(&name).to_string_lossy().into_owned(),
                    path: name,
                    ..OutputFile::default()
                }
            })
            .collect();
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }

    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(LocalFiles))
    }
}

/// The entities of a crate by name and kind, with their texts and links by name, sorted. The
/// engine, the session, the command line and where the launch folder is are left out.
fn shape(document: &Json) -> Vec<String> {
    let graph = document["@graph"].as_array().expect("a graph");
    let engine =
        |entity: &Json| ["Nextflow", "uwe"].contains(&entity["name"].as_str().unwrap_or(""));
    let names: HashMap<&str, &str> = (graph.iter())
        .filter_map(|entity| Some((entity["@id"].as_str()?, entity["name"].as_str()?)))
        .collect();
    let named = |value: &Json| -> String {
        let ids = match value {
            Json::Array(items) => items.iter().collect(),
            single => vec![single],
        };
        let mut found: Vec<&str> = (ids.iter())
            .filter_map(|item| item["@id"].as_str())
            .map(|id| match graph.iter().find(|entity| entity["@id"] == id) {
                Some(entity) if engine(entity) => "ENGINE",
                _ => names.get(id).copied().unwrap_or(id),
            })
            .collect();
        found.sort();
        found.join(", ")
    };
    let mut lines = Vec::new();
    for entity in graph.iter().skip(1).filter(|entity| !engine(entity)) {
        let kind = entity["@type"].to_string();
        let mut line = format!("{kind} {}", entity["name"]);
        let config = entity["format"] == "nextflow-config";
        let name = entity["name"].as_str().unwrap_or_default();
        let run = name.starts_with("Nextflow workflow run");
        let description = entity["description"].as_str().unwrap_or_default();
        let description = description.split(" (session").next().unwrap_or_default();
        line.push_str(&format!(" | {description}"));
        for key in ["format", "version", "author", "license", "keywords"] {
            line.push_str(&format!(" | {key} {}", entity[key]));
        }
        if !config {
            line.push_str(&format!(" | {} {}", entity["contentSize"], entity["md5"]));
        }
        // uwe's task scripts lose the indent that Nextflow's keep after the first line.
        if !run && let Some(command) = entity["command"].as_str() {
            let lines: Vec<&str> = command.lines().map(str::trim).collect();
            line.push_str(&format!(" | {}", lines.join("\n")));
        }
        let locator = match &entity["contentUrl"] {
            Json::Null if entity["localPath"].is_string() => "localPath".to_owned(),
            url => url.to_string(),
        };
        line.push_str(&format!(" | {locator}"));
        for key in [
            "generatedBy",
            "usedDataset",
            "usedSoftware",
            "generated",
            "datasetUsedBy",
            "softwareUsedBy",
            "isPartOf",
            "hasPart",
            "https://w3id.org/EVI#inputs",
            "https://w3id.org/EVI#outputs",
            "hasOutputs",
        ] {
            if !entity[key].is_null() {
                line.push_str(&format!(" | {key}: {}", named(&entity[key])));
            }
        }
        lines.push(line);
    }
    lines.sort();
    lines
}

fn read(path: &Path) -> Json {
    serde_json::from_str(&std::fs::read_to_string(path).expect("reads")).expect("JSON")
}

#[test]
fn writes_recorded_crate() {
    let launch = tempfile::tempdir().expect("a folder");
    let folder = launch.path();
    let at = folder.to_string_lossy();
    // The driver launches in the current folder, so the probe names its files absolutely.
    let script = std::fs::read_to_string(fixtures().join("main.nf")).expect("reads");
    let script = script.replace("publishDir '", &format!("publishDir '{at}/"));
    std::fs::write(folder.join("main.nf"), script).expect("writes");
    std::fs::copy(fixtures().join("input.txt"), folder.join("input.txt")).expect("copies");
    let config = format!(
        "plugins {{ id 'nf-fairscape@0.2.0' }}\nmanifest {{\n    name = 'probe-fair'\n    \
         version = '0.0.1'\n    description = 'Tiny probe workflow for nf-fairscape'\n    \
         license = 'MIT'\n    author = 'Probe Author'\n}}\nfairscape {{\n    \
         file = '{at}/results/ro-crate-metadata.json'\n    overwrite = true\n    \
         checksums = true\n    keywords = ['probe', 'test']\n}}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes");
    let run = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Quiet),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&run, settings).expect("a driver");
    let execution = Engine::new(&mut Probe(folder.join("tasks"))).run("main.nf", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let results = folder.join("results");
    let written = read(&results.join("ro-crate-metadata.json"));
    let recorded = read(&fixtures().join("ro-crate-metadata.json"));
    assert_eq!(shape(&written), shape(&recorded));
    assert_eq!(written["@context"], recorded["@context"]);
    assert_eq!(
        written["@graph"][0]["conformsTo"],
        recorded["@graph"][0]["conformsTo"]
    );
    let copied = std::fs::read_to_string(results.join("workflow/main.nf")).expect("reads");
    assert!(copied.contains("process SHOUT"));
    let graph = read(&results.join("provenance-graph.json"));
    let expected = read(&fixtures().join("provenance-graph.json"));
    let nodes = |graph: &Json| {
        let nodes = graph["@graph"]
            .as_object()
            .expect("nodes")
            .values()
            .cloned();
        let entities: Vec<Json> = std::iter::once(Json::Null).chain(nodes).collect();
        shape(&serde_json::json!({ "@graph": entities }))
    };
    assert_eq!(nodes(&graph), nodes(&expected));
    for key in ["@type", "description", "name", "condensation_stats"] {
        assert_eq!(graph[key], expected[key], "{key}");
    }
    assert!(
        !results.join("provenance-graph.html").exists(),
        "a fairscape-cli extra"
    );
}

#[test]
fn refuses_existing_crate() {
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("ro-crate-metadata.json");
    std::fs::write(&file, "{}").expect("writes");
    let loaded = |overwrite: bool| {
        let text = format!(
            "plugins {{ id 'nf-fairscape@0.2.0' }}\nfairscape.file = '{}'\n\
             fairscape.overwrite = {overwrite}\n",
            file.display()
        );
        let config = folder.path().join("nextflow.config");
        std::fs::write(&config, text).expect("writes");
        let options = nextflow_config::Options {
            profiles: None,
            params: nextflow_evaluator::value::Value::map([]),
            project_dir: "/project".into(),
            launch_dir: "/launch".into(),
            work_dir: "/work".into(),
        };
        let config = nextflow_config::load(&[config], &options, &mut Quiet).expect("loads");
        nextflow_plugins::catalogue::observers(&config, folder.path())
            .map(|observers| observers.len())
    };
    let message = format!("Provenance file already exists: {}", file.display());
    assert_eq!(loaded(false), Err(message));
    assert_eq!(loaded(true), Ok(1));
    assert!(!file.exists(), "the old crate goes at the start");
}

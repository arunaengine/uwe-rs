//! nf-cmgg 0.5.0: run observers that write a `DONE` file and YAML sample sheets from worksheets.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod expression;
mod sheets;
mod worksheet;

use std::path::{Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::interpreter::parse_json;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer, Start};
use sheets::Collector;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["cmgg"],
    observer: Some(activate),
    ..Plugin::new("nf-cmgg", Kind::Observer, State::Implemented)
};

/// The pipeline whose worksheet the plugin carries inside. uwe does not ship that file, so
/// such a run needs it in `cmgg.samplesheets.worksheets`.
const BUILT_IN: (&str, &str) = ("nf-cmgg/preprocessing", "nfcmgg_preprocessing.yml");

/// The `cmgg` scope, with wrong types replaced by their defaults.
struct Settings {
    done: bool,
    done_location: Option<String>,
    sheets: bool,
    sheets_location: Option<String>,
    worksheets: Vec<String>,
}

fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let settings = Settings::read(config);
    if !settings.done && !settings.sheets {
        return Ok(None);
    }
    let pipeline = config
        .get(&["manifest", "name"])
        .map(|name| name.to_string());
    Ok(Some(Box::new(Cmgg {
        settings,
        pipeline: pipeline.unwrap_or_default(),
        done: None,
        collector: None,
    })))
}

impl Settings {
    fn read(config: &Config) -> Settings {
        let scope = |name: &str| match config.get(&["cmgg", name]) {
            Some(Value::Map(_)) | None => true,
            Some(other) => {
                wrong(&format!("cmgg.{name}"), "map", &other, "[:]");
                false
            }
        };
        let (done_scope, sheets_scope) = (scope("done"), scope("samplesheets"));
        let flag = |path: [&str; 3], fine: bool| match config.get(&path).filter(|_| fine) {
            Some(Value::Boolean(value)) => value,
            Some(other) => {
                wrong(&path[1..].join("."), "boolean", &other, "false");
                false
            }
            None => false,
        };
        let text = |path: [&str; 3], fine: bool| match config.get(&path).filter(|_| fine) {
            Some(Value::String(text) | Value::GString(text)) => Some(text.to_string()),
            Some(Value::Null) | None => None,
            Some(other) => {
                wrong(&path[1..].join("."), "string", &other, "null");
                None
            }
        };
        let worksheets = match config
            .get(&["cmgg", "samplesheets", "worksheets"])
            .filter(|_| sheets_scope)
        {
            Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
            Some(Value::Null) | None => Vec::new(),
            Some(other) => {
                wrong("samplesheets.worksheets", "list", &other, "[]");
                Vec::new()
            }
        };
        Settings {
            done: flag(["cmgg", "done", "enabled"], done_scope),
            done_location: text(["cmgg", "done", "location"], done_scope),
            sheets: flag(["cmgg", "samplesheets", "enabled"], sheets_scope),
            sheets_location: text(["cmgg", "samplesheets", "location"], sheets_scope),
            worksheets,
        }
    }
}

fn wrong(key: &str, expected: &str, value: &Value, default: &str) {
    let class = value.java_class_name();
    tracing::error!(
        "Expected a {expected} for scope '{key}', but got an object of type {class}, defaulting to {default}"
    );
}

/// The observers of the `cmgg` scope: where `DONE` goes, and the sample sheet collector.
struct Cmgg {
    settings: Settings,
    pipeline: String,
    done: Option<PathBuf>,
    collector: Option<Collector>,
}

impl Observer for Cmgg {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.start(start),
            Event::FilePublished { target, .. } => {
                if let Some(collector) = &mut self.collector
                    && let Err(message) = collector.published(target)
                {
                    tracing::debug!("nf-cmgg skipped the published file {target}: {message}");
                }
            }
            Event::RunEnded(end) if end.success => {
                if let Some(folder) = &self.done {
                    write_done(folder);
                }
                if let Some(collector) = &self.collector {
                    collector.write();
                }
            }
            _ => {}
        }
    }
}

impl Cmgg {
    fn start(&mut self, start: &Start) {
        let launch = Path::new(&start.launch_dir);
        let resolve = |path: &str| launch.join(path);
        let outdir = || match start
            .params
            .get("outdir")
            .and_then(|outdir| outdir.as_str())
        {
            Some(outdir) if !outdir.is_empty() => resolve(outdir),
            _ => {
                tracing::warn!(
                    "No outdir specified, using work directory at {} instead",
                    start.work_dir
                );
                PathBuf::from(&start.work_dir)
            }
        };
        if self.settings.done {
            let location = self.settings.done_location.as_deref();
            self.done = Some(location.map_or_else(outdir, resolve));
        }
        if !self.settings.sheets {
            return;
        }
        if self.pipeline.is_empty() {
            tracing::info!(
                "Cannot determine pipeline name from session manifest, skipping automatic samplesheet generation"
            );
            return;
        }
        tracing::info!(
            "Detected pipeline name: '{}', checking for automatic samplesheet generation",
            self.pipeline
        );
        let Some((file, text)) = self.worksheet(launch) else {
            if self.pipeline == BUILT_IN.0 {
                tracing::error!(
                    "The pipeline {} needs the worksheet {}, which the plugin nf-cmgg carries and uwe does not; give it with `cmgg.samplesheets.worksheets`",
                    BUILT_IN.0,
                    BUILT_IN.1
                );
            }
            tracing::info!(
                "No worksheet found for the current pipeline, skipping automatic samplesheet generation"
            );
            return;
        };
        let worksheet = match worksheet::read(&file, &text) {
            Ok(worksheet) => worksheet,
            Err(message) => {
                tracing::error!(
                    "Invalid worksheet, skipping automatic samplesheet generation: {message}"
                );
                return;
            }
        };
        let location = match self.settings.sheets_location.as_deref() {
            Some(location) => resolve(location),
            None => outdir().join("samplesheets"),
        };
        let input = start.params.get("input").and_then(|input| input.as_str());
        let input = input.filter(|input| !input.is_empty()).map(resolve);
        let params = parse_json(&start.params.to_string()).unwrap_or(Value::map([]));
        tracing::info!("Samplesheets will be generated in '{}'", location.display());
        self.collector = Some(Collector::new(
            worksheet,
            params,
            input.as_deref(),
            location,
        ));
    }

    /// The first configured worksheet for the pipeline: a YAML file whose `name` is its name.
    fn worksheet(&self, launch: &Path) -> Option<(String, String)> {
        for given in &self.settings.worksheets {
            let path = launch.join(given);
            let Ok(text) = std::fs::read_to_string(&path) else {
                tracing::error!("Worksheet '{given}' does not exist, skipping...");
                continue;
            };
            let yaml = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml")
            );
            if yaml && worksheet::pipeline(&text).as_deref() == Some(self.pipeline.as_str()) {
                return Some((given.clone(), text));
            }
        }
        None
    }
}

/// Writes `DONE` with the time of the end of the run in the local time zone.
fn write_done(folder: &Path) {
    let now = jiff::Zoned::now();
    let text = format!(
        "Pipeline completed on {} at {}",
        now.strftime("%d/%m/%Y"),
        now.strftime("%H:%M:%S")
    );
    let written =
        std::fs::create_dir_all(folder).and_then(|()| std::fs::write(folder.join("DONE"), text));
    if let Err(error) = written {
        tracing::error!("Cannot write {}: {error}", folder.join("DONE").display());
    }
}

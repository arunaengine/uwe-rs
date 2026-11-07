//! nf-runinsights 0.2.0: per-process history of runs, findings against it and a Markdown report.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod ai;
mod findings;
mod stats;

use std::path::{Path, PathBuf};
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, End, Event, Observer, Start};
use crate::report_format::Json;
use ai::Access;
use findings::{Finding, history_median};
use stats::{Prior, Sample, Stats};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["sayHello"],
    call: Some(call),
    scopes: &["runinsights"],
    observer: Some(activate),
    ..Plugin::new("nf-runinsights", Kind::Observer, State::Implemented)
};

/// How long the end of a run waits for the report, which may wait for the AI section.
const WAIT: Duration = Duration::from_secs(150);

/// `sayHello(target)`, which the plugin keeps from its template.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("sayHello", [target]) => {
            context.host.print(&format!("Hello, {target}!\n"));
            Ok(Value::Null)
        }
        _ => Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-runinsights` with these arguments"
            ),
            span: Default::default(),
        }),
    }
}

/// The `runinsights` scope.
struct Settings {
    /// As the config names it; a relative one is in the launch directory.
    history: String,
    include_failed: bool,
    /// The model of the AI section, when it is on.
    model: Option<String>,
}

fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let value = |path: &[&str]| config.get(&[&["runinsights"], path].concat());
    let flag = |path: &[&str]| value(path).is_some_and(|value| value.truth());
    let text = |path: &[&str]| {
        value(path)
            .filter(Value::truth)
            .map(|value| value.to_string())
    };
    let settings = Settings {
        history: text(&["history"]).unwrap_or_else(|| "~/.nf-runinsights/history".into()),
        include_failed: flag(&["includeFailed"]),
        model: flag(&["ai", "enabled"])
            .then(|| text(&["ai", "model"]).unwrap_or_else(|| "claude-opus-5".into())),
    };
    let mut insights = Insights {
        settings,
        start: None,
        samples: Vec::new(),
    };
    let worker = Background::start("nf-runinsights", WAIT, move |event| insights.hear(event));
    let worker = worker.map_err(|error| format!("nf-runinsights: cannot start: {error}"))?;
    Ok(Some(Box::new(worker)))
}

/// What the run told the plugin so far.
struct Insights {
    settings: Settings,
    start: Option<Box<Start>>,
    /// The tasks that ran, completed or failed, not those from the cache.
    samples: Vec<Sample>,
}

impl Insights {
    fn hear(&mut self, event: Event) {
        match event {
            Event::RunStarted(start) => self.start = Some(start),
            Event::TaskCompleted(task) | Event::TaskFailed(task) => {
                self.samples.push(Sample::of(&task))
            }
            Event::RunEnded(end) => {
                if let Err(error) = self.finish(&end) {
                    tracing::warn!("nf-runinsights: could not write insights: {error}");
                }
            }
            _ => {}
        }
    }

    /// Records the run in the history, compares it with earlier runs, prints the findings and
    /// writes the report.
    fn finish(&self, end: &End) -> Result<(), String> {
        let Some(start) = &self.start else {
            return Ok(());
        };
        if self.samples.is_empty() {
            tracing::info!("nf-runinsights: no executed tasks recorded (all cached or none ran)");
            return Ok(());
        }
        let launch = Path::new(&start.launch_dir);
        let pipeline = pipeline(start);
        let completed = end.success && self.samples.iter().all(|it| it.completed());
        let mut processes: Vec<(String, Stats)> = Vec::new();
        for sample in &self.samples {
            if !processes.iter().any(|(name, _)| *name == sample.process) {
                let same: Vec<&Sample> = (self.samples.iter())
                    .filter(|other| other.process == sample.process)
                    .collect();
                processes.push((sample.process.clone(), Stats::of(&same)));
            }
        }
        let (folder, legacy) = locations(&self.settings.history, launch);
        let mut prior: Vec<Prior> = read_history(&folder, legacy.as_deref())
            .into_iter()
            .filter(|run| run.pipeline == pipeline)
            .filter(|run| run.completed || self.settings.include_failed)
            .collect();
        prior.sort_by(|one, other| one.ts.cmp(&other.ts));
        let ts = time(end.end);
        let record = Json::Object(Vec::new())
            .with("ts", ts.as_str())
            .with("run_name", start.run_name.as_str())
            .with("session_id", start.session_id.as_str())
            .with("pipeline", pipeline.as_str())
            .with("status", if completed { "completed" } else { "failed" })
            .with(
                "processes",
                Json::Object(
                    processes
                        .iter()
                        .map(|(name, it)| (name.clone(), it.json()))
                        .collect(),
                ),
            );
        let stamp: String = (ts.chars().filter(|it| it.is_ascii_digit() || *it == 'T'))
            .take(15)
            .collect();
        std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let file = folder.join(format!("{stamp}-{}.json", start.run_name));
        std::fs::write(&file, record.compact()).map_err(|error| error.to_string())?;
        let found = findings::findings(&processes, &prior);
        let mut lines = vec![
            String::new(),
            format!(
                "nf-runinsights: {} prior run(s) of '{pipeline}' in history",
                prior.len()
            ),
        ];
        if !completed {
            lines.push(
                "nf-runinsights: run recorded as failed, later runs will not use it as a baseline"
                    .into(),
            );
        } else if prior.is_empty() {
            lines.push(
                "nf-runinsights: baseline recorded, comparisons start on your next run".into(),
            );
        }
        lines.extend(
            found
                .iter()
                .map(|it| format!("nf-runinsights: {}", it.message)),
        );
        if !prior.is_empty() && found.is_empty() {
            lines.push("nf-runinsights: no regressions detected vs history".into());
        }
        let mut report = report(start, &pipeline, &ts, &processes, &prior, &found);
        if let Some(model) = &self.settings.model {
            let payload = payload(start, &pipeline, &processes, &prior, &found);
            match Access::from_environment() {
                None => lines.push(
                    "nf-runinsights: AI analysis skipped: set ANTHROPIC_API_KEY in the launch \
                     environment"
                        .into(),
                ),
                Some(access) => match ai::narrate(&access, model, &ai::data(&payload)) {
                    Ok(text) => {
                        report.push_str(&format!(
                            "\n## AI analysis\n\n{text}\n\n_Written by {model} from the \
                             deterministic metrics above, verify against the numbers before \
                             acting._\n"
                        ));
                        lines.push("nf-runinsights: AI analysis added to report".into());
                    }
                    Err(error) => lines.push(format!(
                        "nf-runinsights: AI analysis skipped ({}), deterministic report unaffected",
                        error.chars().take(120).collect::<String>()
                    )),
                },
            }
        }
        let path = std::path::absolute(launch.join("runinsights-report.md"))
            .map_err(|error| error.to_string())?;
        std::fs::write(&path, report).map_err(|error| error.to_string())?;
        lines.push(format!("nf-runinsights: report: {}", path.display()));
        // The plugin prints its lines on the console, beside the run's own output.
        println!("{}", lines.join("\n"));
        Ok(())
    }
}

/// The local time of the end of the run, ISO 8601 with its offset.
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

/// The id of the pipeline in the history: the project name of a pulled pipeline, else the
/// folder of the project and the script name, like `ri/main.nf`.
fn pipeline(start: &Start) -> String {
    if start.repository.is_some() && !start.project_name.is_empty() {
        return start.project_name.clone();
    }
    let script = Path::new(&start.script)
        .file_name()
        .map(|it| it.to_string_lossy());
    let folder = Path::new(&start.project_dir)
        .file_name()
        .map(|it| it.to_string_lossy());
    match (folder, script) {
        (Some(folder), Some(script)) => format!("{folder}/{script}"),
        (None, Some(script)) => script.into_owned(),
        _ => "unknown".into(),
    }
}

/// The history folder, and a legacy JSON lines file to read: the named file itself, with new
/// runs in `history` beside it, else `history.jsonl` beside the folder.
fn locations(history: &str, launch: &Path) -> (PathBuf, Option<PathBuf>) {
    let home = std::env::var("HOME").unwrap_or_default();
    let named = match history.strip_prefix("~/") {
        Some(rest) => Path::new(&home).join(rest),
        None => launch.join(history),
    };
    let parent = named.parent().unwrap_or(Path::new("/")).to_owned();
    match named.is_file() {
        true => (parent.join("history"), Some(named)),
        false => (named, Some(parent.join("history.jsonl"))),
    }
}

/// Every run of the history folder, in name order, and of the legacy file.
fn read_history(folder: &Path, legacy: Option<&Path>) -> Vec<Prior> {
    let mut files: Vec<PathBuf> = (std::fs::read_dir(folder).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|it| it == "json"))
        .collect();
    files.sort();
    let mut texts: Vec<String> = files
        .iter()
        .filter_map(|it| std::fs::read_to_string(it).ok())
        .collect();
    if let Some(text) = legacy.and_then(|it| std::fs::read_to_string(it).ok()) {
        texts.extend(text.lines().map(str::to_owned));
    }
    (texts.iter())
        .filter_map(|text| serde_json::from_str(text).ok())
        .filter_map(|value: serde_json::Value| Prior::read(&value))
        .collect()
}

fn report(
    start: &Start,
    pipeline: &str,
    ts: &str,
    processes: &[(String, Stats)],
    prior: &[Prior],
    found: &[Finding],
) -> String {
    let findings = match (prior.is_empty(), found.is_empty()) {
        (true, true) => {
            "_First recorded run, baseline established; comparisons start next run._".into()
        }
        (false, true) => "_No regressions or anomalies vs history._".into(),
        _ => (found.iter())
            .map(|it| format!("- **{}** ({}): {}", it.kind, it.severity, it.message))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    let rows: Vec<String> = (processes.iter())
        .map(|(name, stats)| stats.row(name, history_median(name, prior)))
        .collect();
    format!(
        "# nf-runinsights report\n\n- **Pipeline:** {pipeline}\n- **Run:** {}\n- **Time:** {ts}\n\
         - **Prior runs in history:** {}\n\n## Findings\n\n{findings}\n\n\
         ## Processes (this run vs history)\n\n\
         | Process | Tasks | Median time | Hist. median | Peak RSS | Mem req |\n\
         |---|---|---|---|---|---|\n{}\n",
        start.run_name,
        prior.len(),
        rows.join("\n")
    )
}

/// The data of the AI section: the run, its findings and processes, and the last ten runs.
fn payload(
    start: &Start,
    pipeline: &str,
    processes: &[(String, Stats)],
    prior: &[Prior],
    found: &[Finding],
) -> Json {
    let history: Vec<Json> = (prior.iter().rev().take(10).rev())
        .map(|run| {
            let medians = (run.processes.iter())
                .map(|(name, _)| {
                    (
                        name.clone(),
                        stats::number(history_median(name, std::slice::from_ref(run))),
                    )
                })
                .collect();
            Json::Object(Vec::new())
                .with("run_name", run.run_name.as_str())
                .with("ts", run.ts.as_str())
                .with("medians_ms", Json::Object(medians))
        })
        .collect();
    Json::Object(Vec::new())
        .with("pipeline", pipeline)
        .with("run_name", start.run_name.as_str())
        .with("prior_runs", prior.len() as u64)
        .with(
            "findings",
            Json::List(found.iter().map(Finding::json).collect()),
        )
        .with(
            "processes",
            Json::Object(
                processes
                    .iter()
                    .map(|(name, it)| (name.clone(), it.json()))
                    .collect(),
            ),
        )
        .with("history", Json::List(history))
}

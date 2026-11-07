//! nf-fl86 0.6.4: guards and records a run in `<outdir>/pipeline_info`, tells a Notion tracker
//! when it begins and ends, keeps S3 tags on publishing and launches a follow-up Seqera Action.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod secrets;
mod seqera;
mod tags;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_report::files::time;
use crate::nf_schema::get;
use crate::nf_sqldb::thrown;
use crate::observer::{Background, Event, Observer, Start};
use crate::report_format::{Json, NEXTFLOW_VERSION};
use secrets::Access;
use tags::Store;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["getAWSSecret", "call_seqera_action"],
    call: Some(call),
    scopes: &["notion", "seqera_action"],
    observer: Some(activate),
    ..Plugin::new("nf-fl86", Kind::Observer, State::Partial)
};

/// The Seqera Platform API when the call or the config names none.
const PLATFORM: &str = "https://api.cloud.seqera.io";

/// How long the end of the run waits for the Notion update and the Seqera Action.
const WAIT: Duration = Duration::from_secs(300);

/// How long one Notion request may take.
const NOTION_LIMIT: Duration = Duration::from_secs(20);

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let text = |place: usize| match arguments.get(place) {
        None | Some(Value::Null) => String::new(),
        Some(value) => value.to_string(),
    };
    match name {
        "getAWSSecret" => {
            let access = Access::of(context.config, &|name| context.host.environment(name));
            let region = Some(text(1)).filter(|region| !region.is_empty());
            let found = secrets::secret(&text(0), region.as_deref(), &access);
            found.map(Value::string).map_err(thrown)
        }
        "call_seqera_action" => {
            let params = crate::nf_schema::json(arguments.get(2).unwrap_or(&Value::Null));
            let base = Some(text(5)).filter(|base| !base.is_empty());
            let base = base.unwrap_or(PLATFORM.to_owned());
            if text(0).trim().is_empty() {
                tracing::warn!("Seqera action launch was skipped because the token is missing");
                return Ok(Value::map(Vec::new()));
            }
            match seqera::launch(&text(0), &text(1), &params, &text(3), &text(4), &base) {
                Ok(answer) => Ok(parse_json(&answer.to_string()).unwrap_or(Value::Null)),
                Err(reason) => {
                    tracing::warn!("Seqera action launch was not completed: {reason}");
                    Ok(Value::map(Vec::new()))
                }
            }
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-fl86`"),
            span: Default::default(),
        }),
    }
}

/// The output folder: `params.outdir`, else the config's `outputDir`, else `results`. A remote
/// folder is refused, as the plugins of uwe write only local files.
fn outdir(
    params: &serde_json::Value,
    config: &serde_json::Value,
    launch: &Path,
) -> Result<PathBuf, String> {
    let set = |value: &serde_json::Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let folder = set(&params["outdir"]).or_else(|| set(&config["outputDir"]));
    let folder = folder.unwrap_or("results".to_owned());
    match storage::address::scheme(&folder) {
        None => Ok(launch.join(folder)),
        Some(_) => Err(format!(
            "nf-fl86 writes pipeline_info only to a local output folder, not {}",
            crate::service::safe_url(&folder)
        )),
    }
}

/// Before the script: the output folder guard, the start line, and the copies of the sample
/// sheet and the Benchling file in `pipeline_info`.
pub(crate) fn start(context: &mut Context) -> Result<(), Error> {
    let launch = PathBuf::from(context.host.launch_dir());
    let params = crate::nf_schema::json(&context.params);
    let config = context
        .config
        .get(&["outputDir"])
        .map(|value| crate::nf_schema::json(&value));
    let config = serde_json::json!({ "outputDir": config });
    let folder = outdir(&params, &config, &launch).map_err(thrown)?;
    let resumed = get(&context.workflow, "resume").is_some_and(|value| value.truth());
    if folder.exists() && !resumed {
        return Err(thrown(format!(
            "🚨 Terminating execution! Output directory already exists: {} (must be cleared to \
             re-run workflow) 🚨",
            folder.display()
        )));
    }
    let given = |name: &str| {
        params[name]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(|text| launch.join(text))
    };
    let benchling = given("benchling");
    if benchling.as_ref().is_some_and(|file| !file.is_file()) {
        return Err(thrown(
            "The file configured by params.benchling does not exist or is not a regular file",
        ));
    }
    context.host.print("Pipeline is starting (nf-fl86)! 🚀\n");
    let info = folder.join("pipeline_info");
    if let Some(sheet) = given("sample_sheet").or_else(|| given("input")) {
        if !sheet.is_file() {
            return Err(thrown(format!(
                "Sample sheet does not exist or is not a regular file: {}",
                sheet.display()
            )));
        }
        copy(&sheet, &info.join("sample_sheet.csv"))?;
        tracing::info!("Sample sheet copied to pipeline_info/sample_sheet.csv");
    }
    if let Some(benchling) = benchling {
        copy(&benchling, &info.join("benchling.json"))?;
        tracing::info!("Benchling JSON copied to pipeline_info/benchling.json");
    }
    Ok(())
}

fn copy(source: &Path, target: &Path) -> Result<(), Error> {
    let failed =
        |error: std::io::Error| thrown(format!("cannot copy {}: {error}", source.display()));
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder).map_err(failed)?;
    }
    std::fs::copy(source, target).map(drop).map_err(failed)
}

/// At the end of the run: `workflow.json` of a successful run, the end line, and the empty
/// `pipeline.complete` marker in the work directory.
pub(crate) fn finish(context: &mut Context) {
    let workflow = &context.workflow;
    let success = get(workflow, "success").is_some_and(|value| value.truth());
    let launch = PathBuf::from(context.host.launch_dir());
    if success {
        let params = crate::nf_schema::json(&context.params);
        let config = context
            .config
            .get(&["outputDir"])
            .map(|value| crate::nf_schema::json(&value));
        let folder = outdir(
            &params,
            &serde_json::json!({ "outputDir": config }),
            &launch,
        );
        let written = folder.and_then(|folder| {
            let path = folder.join("pipeline_info/workflow.json");
            std::fs::create_dir_all(folder.join("pipeline_info"))
                .and_then(|()| std::fs::write(&path, metadata(context).pretty()))
                .map_err(|error| error.to_string())
        });
        match written {
            Ok(()) => tracing::info!("Workflow metadata written to pipeline_info/workflow.json"),
            Err(error) => tracing::warn!(%error, "Failed to write workflow metadata"),
        }
    }
    context.host.print("Pipeline complete! (nf-fl86)👋\n");
    if success {
        let work = crate::nf_python::work_dir(context, &launch);
        let marker = work.join("pipeline.complete");
        let made = std::fs::create_dir_all(&work).and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&marker)
                .map(drop)
        });
        match made {
            Ok(()) => tracing::info!(
                "Workflow completion marker written to: {}",
                marker.display()
            ),
            Err(error) => tracing::warn!(%error, "Failed to write pipeline completion marker"),
        }
    }
}

/// The content of `workflow.json`.
fn metadata(context: &Context) -> Json {
    let workflow = &context.workflow;
    let property = |key: &str| get(workflow, key).unwrap_or(Value::Null);
    let date = |key: &str| match property(key) {
        Value::Date(millis) => {
            let nanos = i32::try_from(millis.rem_euclid(1000) * 1_000_000).unwrap_or(0);
            Json::from(time(millis.div_euclid(1000), nanos))
        }
        _ => Json::Null,
    };
    let text = |key: &str| match property(key) {
        Value::Null => Json::Null,
        other => Json::from(other.to_string()),
    };
    let setting = |name: &str| {
        context
            .config
            .get(&["manifest", name])
            .filter(|value| !matches!(value, Value::Null))
    };
    let mut manifest = Vec::new();
    for name in [
        "author",
        "contributors",
        "defaultBranch",
        "description",
        "docsUrl",
        "doi",
        "gitmodules",
        "homePage",
        "icon",
        "license",
        "mainScript",
        "name",
        "nextflowVersion",
        "organization",
        "recurseSubmodules",
        "version",
    ] {
        let value = match (name, setting(name)) {
            ("contributors", None) => Json::List(Vec::new()),
            ("mainScript", None) => Json::from("main.nf"),
            ("recurseSubmodules", None) => Json::Bool(false),
            (_, None) => Json::Null,
            (_, Some(value)) => crate::report_format::ordered_json(&value),
        };
        manifest.push((name.to_owned(), value));
    }
    Json::object([
        ("manifest", Json::Object(manifest)),
        ("commandLine", text("commandLine")),
        ("commitId", text("commitId")),
        ("start", date("start")),
        ("complete", date("complete")),
        ("runName", text("runName")),
        ("resume", Json::Bool(property("resume").truth())),
        ("nextflowVersion", Json::from(NEXTFLOW_VERSION)),
    ])
}

/// What the run end of a successful run launches: a Seqera Action.
#[derive(Clone, Debug)]
struct Action {
    token: Option<String>,
    region: Option<String>,
    action: Option<String>,
    workspace: Option<String>,
    run_name: Option<String>,
    suffix: Option<String>,
    params: Option<String>,
    launch: Option<serde_json::Value>,
    base: String,
}

/// The settings that the worker needs.
#[derive(Clone)]
struct Settings {
    beginning: Option<String>,
    completion: Option<String>,
    action: Option<Action>,
    access: Access,
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let text = |path: &[&str]| {
        let found = config
            .get(path)
            .filter(|value| !matches!(value, Value::Null));
        found
            .map(|value| value.to_string().trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let access = Access::of(config, &|name| std::env::var(name).ok());
    let action = config
        .get(&["seqera_action"])
        .filter(|value| value.truth())
        .map(|_| Action {
            token: text(&["seqera_action", "aws_token_name"]),
            region: text(&["seqera_action", "aws_region"]),
            action: text(&["seqera_action", "action"]),
            workspace: text(&["seqera_action", "workspace"]),
            run_name: text(&["seqera_action", "run_name"]),
            suffix: text(&["seqera_action", "run_name_suffix"]),
            params: text(&["seqera_action", "params"]),
            launch: config
                .get(&["seqera_action", "launch_params"])
                .map(|value| crate::nf_schema::json(&value)),
            base: text(&["seqera_action", "api_base"]).unwrap_or(PLATFORM.to_owned()),
        });
    let settings = Settings {
        beginning: text(&["notion", "beginning_endpoint"]),
        completion: text(&["notion", "complete_endpoint"]),
        action,
        access: access.clone(),
    };
    let store = Store {
        access,
        endpoint: text(&["aws", "client", "endpoint"]),
    };
    let mut start: Option<Box<Start>> = None;
    let worker = Background::start("nf-fl86", WAIT, move |event| match event {
        Event::RunStarted(begun) => {
            write_params(&begun);
            notion(settings.beginning.as_deref(), &begun.run_name, "beginning");
            start = Some(begun);
        }
        Event::RunEnded(end) if end.success => {
            let Some(begun) = &start else {
                return;
            };
            notion(
                settings.completion.as_deref(),
                &begun.run_name,
                "completion",
            );
            if let Some(action) = &settings.action {
                follow(action, begun, &settings.access);
            }
        }
        _ => {}
    })
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(Fl86 {
        worker,
        tags: Tags::start(store),
    })))
}

/// `pipeline_info/params.json`: the params, top-level keys sorted.
fn write_params(start: &Start) {
    let folder = outdir(&start.params, &start.config, Path::new(&start.launch_dir));
    let text = Json::from(&start.params).pretty();
    let written = folder.and_then(|folder| {
        let folder = folder.join("pipeline_info");
        std::fs::create_dir_all(&folder)
            .and_then(|()| std::fs::write(folder.join("params.json"), text))
            .map_err(|error| error.to_string())
    });
    match written {
        Ok(()) => tracing::info!("Workflow parameters written to pipeline_info/params.json"),
        Err(_) => {
            tracing::warn!("Failed to write workflow parameters to pipeline_info/params.json")
        }
    }
}

/// The first `NGSRUN<digits>` of a run name, or with `QE` before it when `qe` allows that.
fn identifier(run_name: &str, qe: bool) -> Option<String> {
    for (at, _) in run_name.match_indices("NGSRUN") {
        let digits: String = run_name[at + 6..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            let start = if qe && run_name[..at].ends_with("QE") {
                at - 2
            } else {
                at
            };
            return Some(format!("{}{digits}", &run_name[start..at + 6]));
        }
    }
    None
}

/// Sends the Notion update of a phase; never fails the run.
fn notion(template: Option<&str>, run_name: &str, phase: &str) {
    let Some(template) = template else {
        return;
    };
    let incomplete =
        |reason: &str| tracing::warn!("Notion {phase} update was not completed: {reason}");
    let Some(id) = identifier(run_name, false) else {
        return incomplete("workflow run name does not contain an NGSRUN identifier");
    };
    // The plugin replaces `{NGSRUN}` first, so `${NGSRUN}` keeps its `$`.
    let mut url = template.to_owned();
    for (placeholder, value) in [
        ("{NGSRUN}", id.as_str()),
        ("{ngsrun}", id.as_str()),
        ("${NGSRUN}", id.as_str()),
        ("${ngsrun}", id.as_str()),
        ("{seqeraPlatformUrl}", ""),
        ("${seqeraPlatformUrl}", ""),
    ] {
        url = url.replace(placeholder, value);
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return incomplete("endpoint must use HTTP or HTTPS");
    }
    let (Ok(client), Ok(runtime)) = (
        storage::http_client(),
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build(),
    ) else {
        return incomplete("request could not be completed");
    };
    let request = (client.post(url).timeout(NOTION_LIMIT))
        .header("X-HTTP-Method-Override", "PATCH")
        .header("Content-Type", "application/json")
        .body(Vec::new());
    match runtime.block_on(async {
        request
            .send()
            .await
            .map(|response| response.status().as_u16())
    }) {
        Ok(200..=299) => tracing::info!("Notion {phase} update completed"),
        Ok(status) => incomplete(&format!("HTTP status {status}")),
        Err(_) => incomplete("request could not be completed"),
    }
}

/// Launches the configured Seqera Action after a successful run; never fails the run.
fn follow(action: &Action, start: &Start, access: &Access) {
    if identifier(&start.run_name, true).is_none() {
        tracing::info!(
            "Seqera action launch skipped because the completed workflow run name does not contain \
             an NGSRUN or QENGSRUN identifier"
        );
        return;
    }
    let params = match (&action.params, &action.launch) {
        (Some(text), _) => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .filter(serde_json::Value::is_object),
        (None, Some(launch)) => Some(launch.clone()),
        (None, None) => {
            let folder = outdir(&start.params, &start.config, Path::new(&start.launch_dir));
            let input =
                |folder: PathBuf| serde_json::json!({ "input_dir": folder.to_string_lossy() });
            folder.ok().map(input)
        }
    };
    let (Some(name), Some(id), Some(workspace), Some(params)) =
        (&action.token, &action.action, &action.workspace, params)
    else {
        tracing::warn!("Seqera action configuration was invalid; follow-up launch skipped");
        return;
    };
    let run_name = match (&action.run_name, &action.suffix) {
        (Some(name), _) => name.clone(),
        (None, Some(suffix)) => format!("{}{suffix}", start.run_name),
        (None, None) => start.run_name.clone(),
    };
    let token = match secrets::secret(name, action.region.as_deref(), access) {
        Ok(secret) => {
            tracing::info!("Seqera action AWS secret retrieved successfully");
            match secret.starts_with('{') {
                true => serde_json::from_str::<serde_json::Value>(&secret)
                    .ok()
                    .and_then(|value| value["TOWER_ACCESS_TOKEN"].as_str().map(str::to_owned))
                    .unwrap_or_default(),
                false => secret,
            }
        }
        Err(_) => {
            tracing::warn!(
                "Seqera action AWS secret could not be retrieved; follow-up launch skipped"
            );
            return;
        }
    };
    tracing::info!("Calling configured Seqera action");
    match seqera::launch(&token, &run_name, &params, id, workspace, &action.base) {
        Ok(_) => tracing::info!("Seqera action launch completed successfully"),
        Err(reason) => tracing::warn!("Seqera action launch was not completed: {reason}"),
    }
}

/// The observer: Notion and the Seqera Action on a background worker, S3 tags on a worker
/// whose failures the check at the end of the run reports.
struct Fl86 {
    worker: Background,
    tags: Tags,
}

impl Observer for Fl86 {
    fn notify(&mut self, event: &Event) {
        if let Event::FilePublished { source, target } = event
            && source.starts_with("s3://")
            && target.starts_with("s3://")
        {
            self.tags.send(source, target);
        }
        self.worker.notify(event);
    }

    fn check(&mut self) -> Result<(), String> {
        self.tags.finish()
    }
}

/// A worker thread that copies S3 tags, and keeps the first failure.
struct Tags {
    sender: Option<mpsc::Sender<(String, String)>>,
    worker: Option<JoinHandle<Option<String>>>,
}

impl Tags {
    fn start(store: Store) -> Tags {
        let (sender, published) = mpsc::channel::<(String, String)>();
        let worker = std::thread::Builder::new()
            .name("nf-fl86-tags".to_owned())
            .spawn(move || {
                let mut failure = None;
                for (source, target) in published {
                    if failure.is_none()
                        && let Err(message) = tags::keep(&store, &source, &target)
                    {
                        failure = Some(message);
                    }
                }
                failure
            });
        if let Err(error) = &worker {
            tracing::warn!(%error, "nf-fl86 cannot keep S3 tags");
        }
        Tags {
            sender: Some(sender),
            worker: worker.ok(),
        }
    }

    fn send(&self, source: &str, target: &str) {
        if let Some(sender) = &self.sender {
            let _ = sender.send((source.to_owned(), target.to_owned()));
        }
    }

    /// Waits for the copies that are left; the first failure fails the run.
    fn finish(&mut self) -> Result<(), String> {
        self.sender.take();
        match self.worker.take().map(JoinHandle::join) {
            Some(Ok(Some(message))) => Err(message),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_run_identifiers() {
        assert_eq!(
            identifier("probe_NGSRUN123_x", false).as_deref(),
            Some("NGSRUN123")
        );
        assert_eq!(identifier("QENGSRUN7", true).as_deref(), Some("QENGSRUN7"));
        assert_eq!(identifier("NGSRUN_x", false), None);
    }
}

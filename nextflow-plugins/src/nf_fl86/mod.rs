// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod secrets;
mod seqera;

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_report::files::time;
use crate::nf_schema::get;
use crate::nf_sqldb::thrown;
use crate::report_format::{Json, NEXTFLOW_VERSION};
use secrets::Access;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["getAWSSecret", "call_seqera_action"],
    call: Some(call),
    scopes: &["notion", "seqera_action"],
    ..Plugin::new("nf-fl86", Kind::Observer, State::Partial)
};

/// The Seqera Platform API when the call or the config names none.
const PLATFORM: &str = "https://api.cloud.seqera.io";

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

/// The output folder: `params.outdir`, else the config's `outputDir`, else `results`.
fn outdir(params: &serde_json::Value, config: &serde_json::Value, launch: &Path) -> PathBuf {
    let set = |value: &serde_json::Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let folder = set(&params["outdir"]).or_else(|| set(&config["outputDir"]));
    launch.join(folder.unwrap_or("results".to_owned()))
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
    let folder = outdir(&params, &config, &launch);
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
        let path = folder.join("pipeline_info/workflow.json");
        let written = std::fs::create_dir_all(folder.join("pipeline_info"))
            .and_then(|()| std::fs::write(&path, metadata(context).pretty()));
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

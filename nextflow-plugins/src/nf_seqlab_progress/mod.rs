//! nf-seqlab-progress 0.2.0: a progress dashboard of the stages and source files of an
//! nf-seqlab pipeline, on the console and in `<outdir>/pipeline_info/progress.jsonl`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod board;
mod registry;
mod view;

use std::io::IsTerminal;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_sqldb::thrown;
use crate::observer::Observer;
use board::{Board, Mode};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "registerProgressInputs",
        "registerProgressStages",
        "withProgressIdentity",
    ],
    call: Some(call),
    observer: Some(activate),
    ..Plugin::new("nf-seqlab-progress", Kind::Observer, State::Partial)
};

fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    let argument = |place: usize| arguments.get(place).cloned().unwrap_or(Value::Null);
    let done = match name {
        "registerProgressInputs" => registry::inputs(&argument(0)).map(|()| Value::Null),
        "registerProgressStages" => {
            registry::stages(&argument(0), &argument(1)).map(|()| Value::Null)
        }
        "withProgressIdentity" => registry::identity(&argument(0)),
        _ => {
            return Err(Error::Unsupported {
                message: format!("the function `{name}` of the plugin `nf-seqlab-progress`"),
                span: Default::default(),
            });
        }
    };
    done.map_err(thrown)
}

/// Whether the console is a terminal that the dashboard may draw on.
fn interactive() -> bool {
    let variable = |name: &str| std::env::var(name).unwrap_or_default().to_lowercase();
    std::io::stdout().is_terminal()
        && variable("TERM") != "dumb"
        && !matches!(
            variable("NXF_AGENT_MODE").as_str(),
            "1" | "true" | "yes" | "on"
        )
}

fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    registry::clear();
    let params = config.params();
    let param = |name: &str| {
        crate::nf_schema::get(&params, name).filter(|value| !matches!(value, Value::Null))
    };
    let mode =
        param("progress_mode").map_or("auto".to_owned(), |mode| mode.to_string().to_lowercase());
    let mode = match (mode.as_str(), interactive()) {
        ("auto" | "full" | "compact" | "minimal", true) => Mode::View,
        ("auto" | "full" | "compact" | "minimal" | "plain", _) => Mode::Plain,
        ("json", _) => Mode::Json,
        ("off", _) => Mode::Off,
        (other, _) => {
            return Err(format!(
                "Unknown progress mode '{other}'; expected auto, full, compact, minimal, plain, \
                 json, or off"
            ));
        }
    };
    let limit = param("progress_max_active_files")
        .and_then(|limit| limit.to_string().parse().ok())
        .unwrap_or(4);
    let outdir = param("outdir")
        .or_else(|| config.get(&["outputDir"]).filter(|value| value.truth()))
        .map_or("results".to_owned(), |outdir| outdir.to_string());
    Ok(Some(Box::new(Board::new(mode, limit, outdir))))
}

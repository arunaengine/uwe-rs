//! How nf-seqlab-progress shows a dashboard state: the plugin's plain line, and uwe's own text
//! view for a terminal.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::report_format::Json;

/// The plain line of a dashboard state.
pub(crate) fn plain(board: &Json) -> String {
    let field = |value: Option<&Json>| match value {
        Some(Json::Text(text)) => text.replace(' ', "_"),
        Some(Json::Null) | None => String::new(),
        Some(other) => other.compact(),
    };
    let number = |value: Option<&Json>| match value {
        Some(Json::Number(number)) => number.as_f64().unwrap_or(0.0),
        _ => 0.0,
    };
    let stage = board.get("stage").filter(|stage| **stage != Json::Null);
    let mut line = format!("[nf-seqlab progress] run={}", field(board.get("run_id")));
    match stage {
        Some(stage) => line.push_str(&format!(
            " stage={} files={}/{} stage_percent={:.1} state={}",
            field(stage.get("id")),
            field(stage.get("completed_files")),
            field(stage.get("expected_files")),
            number(stage.get("percent")),
            field(stage.get("state"))
        )),
        None => line.push_str(" stage=none files=0/0 stage_percent=0.0 state=queued"),
    }
    if let Some(Json::List(files)) = board.get("active_files")
        && let Some(file) = files.first()
    {
        let percent = match file.get("percent") {
            Some(Json::Number(number)) => format!("{:.1}", number.as_f64().unwrap_or(0.0)),
            _ => "indeterminate".to_owned(),
        };
        let phase = match file.get("phase") {
            Some(Json::Text(phase)) => phase.replace(' ', "_"),
            _ => "running".to_owned(),
        };
        line.push_str(&format!(
            " file={} file_percent={percent} phase={phase}",
            field(file.get("file_id"))
        ));
    }
    if let Some(publish) = board
        .get("publish")
        .filter(|publish| **publish != Json::Null)
    {
        line.push_str(&format!(
            " publish_in_flight=unknown publish_done={} publish_bytes={}",
            field(publish.get("completed_files")),
            field(publish.get("completed_bytes"))
        ));
    }
    line
}

/// uwe's text view of a dashboard state, for a terminal.
pub(crate) fn text(board: &Json) -> String {
    let run = board
        .get("run_name")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let mut text = format!("nf-seqlab progress: {run}\n");
    if let Some(Json::List(stages)) = board.get("stages") {
        for stage in stages {
            let value = |key: &str| {
                stage
                    .get(key)
                    .map(crate::nf_report::files::groovy)
                    .unwrap_or_default()
            };
            let percent = stage.get("percent").and_then(|percent| match percent {
                Json::Number(number) => number.as_f64(),
                _ => None,
            });
            text.push_str(&format!(
                "  {} [{}] {}/{} files  {:.1}%\n",
                value("label"),
                value("state"),
                value("completed_files"),
                value("expected_files"),
                percent.unwrap_or(0.0)
            ));
        }
    }
    if let Some(Json::List(files)) = board.get("active_files") {
        for file in files {
            let id = file
                .get("file_id")
                .and_then(Json::as_str)
                .unwrap_or_default();
            text.push_str(&format!("    {id}  running  --.-%\n"));
        }
    }
    if let Some(publish) = board
        .get("publish")
        .filter(|publish| **publish != Json::Null)
    {
        let value = |key: &str| {
            publish
                .get(key)
                .map(crate::nf_report::files::groovy)
                .unwrap_or_default()
        };
        let bytes = value("completed_bytes").parse().unwrap_or(0);
        let last = value("last_target");
        let name = last.rsplit('/').next().unwrap_or_default();
        text.push_str(&format!(
            "  Publishing: {} done ({}), last {name}\n",
            value("completed_files"),
            crate::nf_report::files::memory(bytes)
        ));
    }
    text
}

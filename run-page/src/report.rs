//! The report of a run: one HTML file that shows the run as its page does, for a machine where
//! no browser can reach the page, like the login node of a cluster. The file holds everything:
//! the style, the scripts of the page, the pictures of the graph and the data that the page
//! would ask the server for, with the end of each log. It needs no server and no network, so it
//! can be copied anywhere and opened from the disk. `report.js` answers the requests of the
//! page's scripts from the data, and adds a timeline, the resources of each task and where the
//! results went.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use run_database::{Database, DatabaseError, Delivered, Log};
use serde::Serialize;
use workflow_graph::render::{Direction, FOLD, Options, View};

use crate::{api, assets};

/// How many bytes of the end of each log the report holds.
pub const LOG_TAIL: usize = 4096;

/// A run with at most this many tasks has every task in the picture of its graph; a larger one
/// folds its large steps into one box each, as the page does at first.
const OPEN_TASKS: usize = 200;

const REPORT_SCRIPT: &str = include_str!("../assets/report.js");

/// What the report holds besides the page itself.
#[derive(Serialize)]
struct Data {
    run: String,
    /// When the report was written, in milliseconds since the Unix epoch.
    written: i64,
    /// The iunctus that wrote it.
    program: String,
    /// The answers to the requests of the page, by their address.
    answers: BTreeMap<String, serde_json::Value>,
    /// The pictures of the graph, by their address.
    pictures: BTreeMap<String, String>,
    deliveries: Vec<Delivered>,
}

/// The report of a run as one HTML file, or `None` when the database has no such run.
pub fn report(database: &Database, run: &str) -> Result<Option<String>, DatabaseError> {
    let Some(view) = api::run_view(database, run)? else {
        return Ok(None);
    };
    let base = format!("/api/runs/{run}");
    let mut answers = BTreeMap::new();
    answers.insert(base.clone(), serde_json::to_value(&view)?);
    if let Some(time) = database.time_spent(run)? {
        answers.insert(format!("{base}/time"), serde_json::to_value(&time)?);
    }
    let tasks = database.tasks(run)?;
    for row in &tasks {
        let Some(mut task) = api::task_view(database, run, row.id)? else {
            continue;
        };
        tail(&mut task.stdout);
        tail(&mut task.stderr);
        answers.insert(
            format!("{base}/tasks/{}", row.id),
            serde_json::to_value(&task)?,
        );
        // The page asks why a task ran only for tasks that ran.
        if row.reused.is_none() && row.attempts > 0 {
            let why = database.why(run, row.id)?;
            answers.insert(
                format!("{base}/tasks/{}/why", row.id),
                serde_json::to_value(&why)?,
            );
        }
    }
    let mut pictures = BTreeMap::new();
    let fold = if tasks.len() <= OPEN_TASKS { 0 } else { FOLD };
    for (view, view_name) in [(View::Tasks, "tasks"), (View::Steps, "steps")] {
        for (direction, direction_name) in [(Direction::Down, "down"), (Direction::Right, "right")]
        {
            let options = Options {
                view,
                direction,
                fold,
                ..Options::default()
            };
            if let Some(svg) = api::picture(database, run, options)? {
                let address =
                    format!("{base}/graph.svg?view={view_name}&direction={direction_name}");
                pictures.insert(address, svg);
            }
        }
    }
    let data = Data {
        run: run.to_owned(),
        written: run_database::now(),
        program: format!("iunctus {}", env!("CARGO_PKG_VERSION")),
        answers,
        pictures,
        deliveries: database.deliveries(run)?,
    };
    Ok(Some(page(&data)?))
}

/// The end of a log, at most [`LOG_TAIL`] bytes, from the start of a character.
fn tail(log: &mut Option<Log>) {
    let Some(log) = log else {
        return;
    };
    if log.text.len() <= LOG_TAIL {
        return;
    }
    let mut start = log.text.len() - LOG_TAIL;
    while !log.text.is_char_boundary(start) {
        start += 1;
    }
    log.text = log.text[start..].to_owned();
    log.whole = false;
}

/// The HTML file: the frame of the page with its style, the data and the scripts inline.
fn page(data: &Data) -> Result<String, serde_json::Error> {
    // JSON has `<` only inside strings, where `<` means the same. So no text of the data
    // can end the element that holds it.
    let json = serde_json::to_string(&run_database::public_json(data)?)?.replace('<', "\\u003c");
    let scripts = [
        assets::SCRIPT,
        assets::GRAPH_SCRIPT,
        assets::CAMERA_SCRIPT,
        assets::BOXES_SCRIPT,
        assets::TIME_SCRIPT,
        assets::COMPARE_SCRIPT,
        assets::RUN_SCRIPT,
        REPORT_SCRIPT,
    ];
    let mut html = String::from(
        "<!doctype html>\n<!-- The report of a run of iunctus: one file, without a server. -->\n\
         <html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>iunctus report</title>\n<style>\n",
    );
    html.push_str(assets::STYLE);
    html.push_str(
        "</style>\n</head>\n<body>\n<header class=\"bar\">\n  <span class=\"brand\">iunctus</span>\n  \
         <nav id=\"trail\"></nav>\n  <span id=\"live\" class=\"live\" hidden>live</span>\n  \
         <span id=\"written\" class=\"written\"></span>\n</header>\n\
         <main id=\"main\"><p class=\"quiet\">Loading...</p></main>\n\
         <noscript><p>The report needs JavaScript to show the run.</p></noscript>\n\
         <script type=\"application/json\" id=\"report\">",
    );
    html.push_str(&json);
    html.push_str("</script>\n");
    for script in scripts {
        html.push_str("<script>\n");
        html.push_str(script);
        html.push_str("</script>\n");
    }
    html.push_str("</body>\n</html>\n");
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_end_of_a_long_log_starts_at_a_character() {
        let text = format!("{}é{}", "a".repeat(10), "b".repeat(LOG_TAIL - 1));
        let mut log = Some(Log {
            digest: String::new(),
            size: text.len() as u64,
            text,
            whole: true,
        });
        tail(&mut log);
        let log = log.expect("a log");
        assert_eq!(log.text, "b".repeat(LOG_TAIL - 1));
        assert!(!log.whole);
    }

    #[test]
    fn no_script_of_the_page_ends_its_element() {
        let all = [
            assets::SCRIPT,
            assets::GRAPH_SCRIPT,
            assets::CAMERA_SCRIPT,
            assets::BOXES_SCRIPT,
            assets::TIME_SCRIPT,
            assets::COMPARE_SCRIPT,
            assets::RUN_SCRIPT,
            REPORT_SCRIPT,
            assets::STYLE,
        ];
        for script in all {
            assert!(!script.to_lowercase().contains("</script"));
            assert!(!script.to_lowercase().contains("</style"));
        }
    }
}

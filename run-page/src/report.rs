//! Embeds page assets, graphs, logs and run data in a standalone HTML report.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{self, Read};

use run_database::{Database, DatabaseError, Delivered, Log};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use workflow_graph::render::{Direction, FOLD, Options, View};
use workflow_graph::{Files, LocalFiles};

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
    /// The uwe that wrote it.
    program: String,
    /// The answers to the requests of the page, by their address.
    answers: BTreeMap<String, serde_json::Value>,
    /// The pictures of the graph, by their address.
    pictures: BTreeMap<String, String>,
    deliveries: Vec<Delivered>,
}

/// The report of a run as one HTML file, or `None` when the database has no such run.
pub fn report(database: &Database, run: &str) -> Result<Option<String>, DatabaseError> {
    report_with_files(database, run, &LocalFiles)
}

/// Resolves deferred log locations through the request's configured storage providers.
pub fn report_with_files(
    database: &Database,
    run: &str,
    files: &dyn Files,
) -> Result<Option<String>, DatabaseError> {
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
        for log in [&mut task.stdout, &mut task.stderr] {
            if let Some(log) = log {
                log_tail(log, files, LOG_TAIL)?;
            }
            tail(log);
        }
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
        program: format!("uwe {}", env!("CARGO_PKG_VERSION")),
        answers,
        pictures,
        deliveries: database.deliveries(run)?,
    };
    Ok(Some(page(&data)?))
}

/// Reads a deferred log on demand, verifying its bytes while retaining only its bounded tail.
pub fn log_tail(log: &mut Log, files: &dyn Files, limit: usize) -> io::Result<()> {
    if log.whole || !log.text.is_empty() || log.size == 0 {
        return Ok(());
    }
    let Some(location) = &log.location else {
        return Ok(());
    };
    let mut file = std::fs::File::open(files.local(location)?)?;
    let mut hasher = Sha256::new();
    let mut size = 0;
    let mut kept = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
        if read >= limit {
            kept.clear();
            kept.extend_from_slice(&buffer[read - limit..read]);
        } else {
            let remove = (kept.len() + read).saturating_sub(limit);
            kept.drain(..remove);
            kept.extend_from_slice(&buffer[..read]);
        }
    }
    if size != log.size
        || task_model::Digest::from_sha256(&hasher.finalize()).as_str() != log.digest
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained log bytes do not match their recorded digest and size",
        ));
    }
    let start = if size > kept.len() as u64 {
        kept.iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(kept.len())
    } else {
        0
    };
    let text = String::from_utf8_lossy(&kept[start..]);
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    log.whole = size <= limit as u64 && text.len() as u64 == size;
    log.text = text[start..].to_owned();
    Ok(())
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
        "<!doctype html>\n<!-- The report of a run of uwe: one file, without a server. -->\n\
         <html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>uwe report</title>\n<style>\n",
    );
    html.push_str(assets::STYLE);
    html.push_str(
        "</style>\n</head>\n<body>\n<header class=\"bar\">\n  <span class=\"brand\">uwe</span>\n  \
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
    fn verifies_deferred_tail() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let text = format!("{}é{}", "a".repeat(3 * LOG_TAIL), "b".repeat(LOG_TAIL - 1));
        std::fs::write(file.path(), &text).unwrap();
        let mut log = Log {
            digest: task_model::Digest::sha256(text.as_bytes()).to_string(),
            size: text.len() as u64,
            text: String::new(),
            whole: false,
            location: Some(file.path().to_string_lossy().into_owned()),
        };
        log_tail(&mut log, &LocalFiles, LOG_TAIL).unwrap();
        assert_eq!(log.text, "b".repeat(LOG_TAIL - 1));
        assert!(!log.whole);
        log.text.clear();
        std::fs::write(file.path(), text.replace('a', "c")).unwrap();
        assert_eq!(
            log_tail(&mut log, &LocalFiles, LOG_TAIL)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(log.text.is_empty());
        std::fs::write(file.path(), "short").unwrap();
        assert!(log_tail(&mut log, &LocalFiles, LOG_TAIL).is_err());
        let bytes = vec![0xff; 2 * LOG_TAIL];
        std::fs::write(file.path(), &bytes).unwrap();
        log.digest = task_model::Digest::sha256(&bytes).to_string();
        log.size = bytes.len() as u64;
        log_tail(&mut log, &LocalFiles, LOG_TAIL).unwrap();
        assert!(log.text.len() <= LOG_TAIL);
        assert!(!log.whole);
    }

    #[test]
    fn truncates_at_character() {
        let text = format!("{}é{}", "a".repeat(10), "b".repeat(LOG_TAIL - 1));
        let mut log = Some(Log {
            digest: String::new(),
            size: text.len() as u64,
            text,
            whole: true,
            location: None,
        });
        tail(&mut log);
        let log = log.expect("a log");
        assert_eq!(log.text, "b".repeat(LOG_TAIL - 1));
        assert!(!log.whole);
    }

    #[test]
    fn escapes_script_endings() {
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

//! nf-metalog 0.2.0: the trace records of the tasks by the sample of their `meta` map, kept in
//! memory or in SQLite, as a CSV file and an HTML report at the end of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;
use rusqlite::Connection;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Start, Task};
use crate::report_format::{Json, NEXTFLOW_VERSION, offset_time};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["metalog"],
    observer: Some(activate),
    ..Plugin::new("nf-metalog", Kind::Observer, State::Implemented)
};

/// The columns of the CSV of the memory store, in the order nf-metalog wrote them for the
/// fields of the recording; other fields follow in the order of [`FIELDS`].
const RECORDED: [&str; 20] = [
    "process",
    "realtime",
    "native_id",
    "submit",
    "cpus",
    "module",
    "start",
    "task_id",
    "attempt",
    "script",
    "duration",
    "exit",
    "workdir",
    "group_id",
    "name",
    "tag",
    "complete",
    "run_name",
    "hash",
    "status",
];

/// The trace fields in the order of nf-metalog's table after `run_name` and `group_id`, with
/// their types in its SQLite store.
const FIELDS: [(&str, &str); 42] = [
    ("task_id", "TEXT PRIMARY KEY"),
    ("hash", "TEXT"),
    ("native_id", "TEXT"),
    ("process", "TEXT"),
    ("module", "TEXT"),
    ("container", "TEXT"),
    ("tag", "TEXT"),
    ("name", "TEXT"),
    ("status", "TEXT"),
    ("exit", "TEXT"),
    ("submit", "INTEGER"),
    ("start", "INTEGER"),
    ("complete", "INTEGER"),
    ("duration", "INTEGER"),
    ("realtime", "INTEGER"),
    ("cpu_percent", "REAL"),
    ("mem_percent", "REAL"),
    ("rss", "INTEGER"),
    ("vmem", "INTEGER"),
    ("peak_rss", "INTEGER"),
    ("peak_vmem", "INTEGER"),
    ("rchar", "INTEGER"),
    ("wchar", "INTEGER"),
    ("syscr", "INTEGER"),
    ("syscw", "INTEGER"),
    ("read_bytes", "INTEGER"),
    ("write_bytes", "INTEGER"),
    ("attempt", "INTEGER"),
    ("workdir", "TEXT"),
    ("script", "TEXT"),
    ("scratch", "TEXT"),
    ("queue", "TEXT"),
    ("cpus", "INTEGER"),
    ("memory", "INTEGER"),
    ("disk", "INTEGER"),
    ("time", "INTEGER"),
    ("env", "TEXT"),
    ("error_action", "TEXT"),
    ("vol_ctxt", "INTEGER"),
    ("inv_ctxt", "INTEGER"),
    ("hostname", "TEXT"),
    ("cpu_model", "TEXT"),
];

/// The columns that the HTML report leaves out.
const HIDDEN: [&str; 16] = [
    "script",
    "env",
    "native_id",
    "module",
    "vol_ctxt",
    "inv_ctxt",
    "rss",
    "vmem",
    "realtime",
    "queue",
    "time",
    "run_name",
    "error_action",
    "cpu_model",
    "hostname",
    "cpus",
];

/// A stored row: each column with its text.
type Row = Vec<(String, String)>;

fn activate(
    config: &Config,
    launch: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let get = |path: &[&str]| {
        let mut full = vec!["metalog"];
        full.extend_from_slice(path);
        config
            .get(&full)
            .filter(|value| !matches!(value, Value::Null))
    };
    if get(&["enabled"]).is_some_and(|value| !value.truth()) {
        return Ok(None);
    }
    let text = |path: &[&str], default: &str| {
        get(path).map_or(default.to_owned(), |value| value.to_string())
    };
    let launch = launch.to_path_buf();
    let backend = text(&["storageBackend"], "memory");
    let store = match backend.as_str() {
        "memory" => Store::Memory(Vec::new()),
        _ => Store::Sqlite(launch.join(text(&["sqlite", "file"], "metalog.db")), None),
    };
    Ok(Some(Box::new(Metalog {
        key: text(&["groupKey"], "id"),
        csv: launch.join(text(&["report", "csvFile"], "metalog.csv")),
        html: launch.join(text(&["report", "htmlFile"], "metalog.html")),
        overwrite: get(&["report", "overwrite"]).is_some_and(|value| value.truth()),
        store,
        run_name: String::new(),
        start: None,
    })))
}

/// Where the rows are kept: in memory in the order of their first event, or in a SQLite file
/// that may hold the rows of several runs.
enum Store {
    Memory(Vec<(String, Row)>),
    Sqlite(PathBuf, Option<Connection>),
}

struct Metalog {
    key: String,
    csv: PathBuf,
    html: PathBuf,
    overwrite: bool,
    store: Store,
    run_name: String,
    start: Option<Box<Start>>,
}

impl Observer for Metalog {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.started(start),
            Event::TaskSubmitted(task)
            | Event::TaskCompleted(task)
            | Event::TaskCached(task)
            | Event::TaskFailed(task) => self.record(task),
            Event::RunEnded(end) => {
                if let Some(task) = &end.task {
                    self.record(task);
                }
                self.report(end);
            }
            Event::TaskStarted(_) | Event::FilePublished { .. } => {}
        }
    }
}

impl Metalog {
    fn started(&mut self, start: &Start) {
        self.run_name = start.run_name.clone();
        self.start = Some(Box::new(start.clone()));
        match &mut self.store {
            Store::Memory(_) => tracing::info!(
                "Metalog observer initialized with memory backend: runName={}, groupBy={}",
                self.run_name,
                self.key
            ),
            Store::Sqlite(path, connection) => {
                match open(path) {
                    Ok(opened) => *connection = Some(opened),
                    Err(error) => {
                        tracing::error!(%error, "Metalog: {} cannot be opened", path.display())
                    }
                }
                tracing::info!(
                    "Metalog observer initialized with SQLite backend: runName={}, groupBy={}, \
                     dbFile={}",
                    self.run_name,
                    self.key,
                    path.display()
                );
            }
        }
        tracing::info!("Metalog: Workflow created");
    }

    /// Stores the record of a task under its id, in place of an earlier one, when its first
    /// input is a tuple whose first value is a map with the group key.
    fn record(&mut self, task: &Task) {
        let name = task
            .record
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let meta = (task.values.first())
            .and_then(serde_json::Value::as_array)
            .and_then(|tuple| tuple.first())
            .and_then(serde_json::Value::as_object);
        let Some(meta) = meta else {
            tracing::warn!("Could not extract group ID for task {name}, skipping");
            return;
        };
        let Some(group) = meta.get(&self.key) else {
            tracing::warn!(
                "Task {name} meta map does not contain key '{}', skipping",
                self.key
            );
            return;
        };
        let process = task
            .record
            .get("process")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let mut row: Row = vec![
            ("run_name".to_owned(), self.run_name.clone()),
            ("group_id".to_owned(), text(group)),
            (
                "process".to_owned(),
                process.rsplit(':').next().unwrap_or(process).to_owned(),
            ),
        ];
        for (key, value) in &task.record {
            if value.is_null() || key == "process" {
                continue;
            }
            let column = match key.as_str() {
                "%cpu" => "cpu_percent",
                "%mem" => "mem_percent",
                other => other,
            };
            row.push((column.to_owned(), text(value)));
        }
        let id = task.record.get("task_id").map(text).unwrap_or_default();
        match &mut self.store {
            Store::Memory(rows) => match rows.iter_mut().find(|(known, _)| *known == id) {
                Some(entry) => entry.1 = row,
                None => rows.push((id, row)),
            },
            Store::Sqlite(_, Some(connection)) => {
                if let Err(error) = upsert(connection, &row) {
                    tracing::error!(%error, "Metalog: the record of task {name} was not stored");
                }
            }
            Store::Sqlite(_, None) => {}
        }
    }

    /// Writes the CSV and the HTML report from the rows of this run, then closes the store.
    fn report(&mut self, end: &End) {
        let rows = match &mut self.store {
            Store::Memory(rows) => {
                let rows = std::mem::take(rows)
                    .into_iter()
                    .map(|(_, row)| row)
                    .collect();
                memory_order(rows)
            }
            Store::Sqlite(_, connection) => {
                tracing::info!("Metalog: closing the db connection.");
                match connection
                    .take()
                    .map(|connection| read(&connection, &self.run_name))
                {
                    Some(Ok(rows)) => rows,
                    Some(Err(error)) => {
                        tracing::error!(%error, "Error generating and writing the nf-metalog report");
                        return;
                    }
                    None => {
                        tracing::warn!(
                            "Metalog: storage backend already closed, skipping report generation."
                        );
                        return;
                    }
                }
            }
        };
        let columns: Vec<String> = rows
            .first()
            .map(|row| row.iter().map(|(key, _)| key.clone()).collect())
            .unwrap_or_default();
        let cells = |row: &Row, columns: &[String]| -> Vec<String> {
            (columns.iter())
                .map(|column| {
                    (row.iter().find(|(key, _)| key == column))
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default()
                })
                .collect()
        };
        if rows.is_empty() {
            tracing::info!("No data to write in the metalog CSV file");
        } else {
            let mut csv = format!(
                "{}\r\n",
                columns
                    .iter()
                    .map(|cell| quote(cell))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            for row in &rows {
                let line: Vec<String> = cells(row, &columns)
                    .iter()
                    .map(|cell| quote(cell))
                    .collect();
                csv.push_str(&line.join(","));
                csv.push_str("\r\n");
            }
            self.write(&self.csv, &csv);
        }
        tracing::info!("Metalog: generating HTML report.");
        let shown: Vec<String> = columns
            .iter()
            .filter(|column| !HIDDEN.contains(&column.as_str()))
            .cloned()
            .collect();
        let data = Json::object([
            ("cols", Json::list(shown.iter())),
            (
                "rows",
                Json::list(rows.iter().map(|row| Json::list(cells(row, &shown)))),
            ),
        ]);
        let Some(start) = self.start.as_deref() else {
            return;
        };
        let page = self.page(start, end, &rows, &data);
        if self.write(&self.html, &page) {
            tracing::info!("Successfully generated {}", self.html.display());
        }
    }

    /// Writes a report file; an existing file stays unless the config lets it be replaced.
    fn write(&self, path: &Path, text: &str) -> bool {
        if path.exists() && !self.overwrite {
            tracing::error!(
                "Error generating and writing the nf-metalog report: Provenance file already \
                 exists: {}",
                path.display()
            );
            return false;
        }
        let parent = path.parent().map(std::fs::create_dir_all);
        match parent
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(path, text))
        {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(%error, "Error generating and writing the nf-metalog report");
                false
            }
        }
    }

    /// The HTML report: the run, its counts, the tasks of each sample and all tasks.
    fn page(&self, start: &Start, end: &End, rows: &[Row], data: &Json) -> String {
        let title = format!("[{}] nf-metalog report", escape(&self.run_name));
        let status = if end.success { "Completed" } else { "Failed" };
        let mut page = format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}\
             </title>\n<style>body{{font-family:sans-serif;margin:2rem}}table{{border-collapse:\
             collapse}}td,th{{border:1px solid #ccc;padding:.3rem .6rem;text-align:left}}</style>\n\
             </head>\n<body>\n<h1>nf-metalog report</h1>\n<h2>Workflow Overview</h2>\n<p>Status: \
             <strong>{status}</strong>, duration {} s</p>\n<h2>Workflow Metadata</h2>\n<table>\n",
            (end.end - start.start) / 1000,
        );
        let date = |millis: i64| {
            offset_time(millis)
                .replacen('T', " ", 1)
                .chars()
                .take(19)
                .collect::<String>()
        };
        let metadata = [
            ("Command line", start.command_line.clone()),
            ("Start", date(start.start)),
            ("Complete", date(end.end)),
            ("Launch directory", start.launch_dir.clone()),
            ("Work directory", start.work_dir.clone()),
            ("Project directory", start.project_dir.clone()),
            (
                "Script name",
                start
                    .script
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            ("Session id", start.session_id.clone()),
            ("Repository", start.repository.clone().unwrap_or_default()),
            ("Profile", start.profile.clone()),
            ("Nextflow", format!("version {NEXTFLOW_VERSION}")),
        ];
        for (name, value) in metadata {
            let _ = writeln!(page, "<tr><th>{name}</th><td>{}</td></tr>", escape(&value));
        }
        let counts = end.counts;
        let _ = write!(
            page,
            "</table>\n<h2>Execution Summary</h2>\n<p>Succeeded {}, cached {}, failed {}, ignored \
             0</p>\n<h2>Sample Tracking</h2>\n<table>\n<tr><th>Sample</th><th>Tasks</th></tr>\n",
            counts.succeeded, counts.cached, counts.failed
        );
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for row in rows {
            let value = |key: &str| {
                row.iter()
                    .find(|(name, _)| name == key)
                    .map_or("", |(_, value)| value.as_str())
            };
            match groups
                .iter_mut()
                .find(|(group, _)| *group == value("group_id"))
            {
                Some((_, names)) => names.push(value("name")),
                None => groups.push((value("group_id"), vec![value("name")])),
            }
        }
        for (group, names) in groups {
            let _ = writeln!(
                page,
                "<tr><td>{}</td><td>{}</td></tr>",
                escape(group),
                escape(&names.join(", "))
            );
        }
        let _ = write!(
            page,
            "</table>\n<h2>Task Details</h2>\n<table id=\"tasks\"></table>\n<script>\n\
             window.nfMetalogData = {};\nconst data = window.nfMetalogData;\n\
             const table = document.getElementById('tasks');\n\
             const head = table.insertRow();\n\
             data.cols.forEach(c => {{ const th = document.createElement('th'); th.textContent = c; head.appendChild(th); }});\n\
             data.rows.forEach(r => {{ const tr = table.insertRow(); r.forEach(v => {{ tr.insertCell().textContent = v; }}); }});\n\
             </script>\n</body>\n</html>\n",
            data.compact().replace("</", "<\\/")
        );
        page
    }
}

/// The rows with their columns in the order of the recording's CSV, then of the table.
fn memory_order(rows: Vec<Row>) -> Vec<Row> {
    let place = |key: &str| {
        (RECORDED.iter().position(|known| *known == key))
            .or_else(|| {
                FIELDS
                    .iter()
                    .position(|(known, _)| *known == key)
                    .map(|place| 100 + place)
            })
            .unwrap_or(1000)
    };
    (rows.into_iter())
        .map(|mut row| {
            row.sort_by(|a, b| place(&a.0).cmp(&place(&b.0)).then_with(|| a.0.cmp(&b.0)));
            row
        })
        .collect()
}

fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(10))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    let columns: Vec<String> = (FIELDS.iter())
        .map(|(name, kind)| format!("{name} {kind}"))
        .collect();
    connection.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS metalog (run_name TEXT NOT NULL, group_id TEXT NOT NULL, {})",
            columns.join(", ")
        ),
        [],
    )?;
    Ok(connection)
}

/// Stores a row in place of the row of the same task; fields without a column are left out.
fn upsert(connection: &Connection, row: &Row) -> rusqlite::Result<()> {
    let known = |key: &str| {
        key == "run_name" || key == "group_id" || FIELDS.iter().any(|(name, _)| *name == key)
    };
    let row: Vec<&(String, String)> = row.iter().filter(|(key, _)| known(key)).collect();
    let names: Vec<&str> = row.iter().map(|(key, _)| key.as_str()).collect();
    let marks: Vec<String> = (1..=row.len()).map(|index| format!("?{index}")).collect();
    let sql = format!(
        "INSERT OR REPLACE INTO metalog ({}) VALUES ({})",
        names.join(", "),
        marks.join(", ")
    );
    let values: Vec<&str> = row.iter().map(|(_, value)| value.as_str()).collect();
    connection.execute(&sql, rusqlite::params_from_iter(values))?;
    Ok(())
}

/// The rows of a run in the order of the table's columns, without the empty ones.
fn read(connection: &Connection, run_name: &str) -> rusqlite::Result<Vec<Row>> {
    let mut statement = connection.prepare("SELECT * FROM metalog WHERE run_name = ?1")?;
    let names: Vec<String> = statement.column_names().map(str::to_owned).collect();
    let rows = statement.query_map([run_name], |found| {
        let mut row = Row::new();
        for (index, name) in names.iter().enumerate() {
            let value: rusqlite::types::Value = found.get(index)?;
            let text = match value {
                rusqlite::types::Value::Null => continue,
                rusqlite::types::Value::Integer(number) => number.to_string(),
                rusqlite::types::Value::Real(number) => number.to_string(),
                rusqlite::types::Value::Text(text) => text,
                rusqlite::types::Value::Blob(_) => continue,
            };
            row.push((name.clone(), text));
        }
        Ok(row)
    })?;
    rows.collect()
}

/// A trace value as plain text: a text as it is, a number as its digits.
fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A CSV field, quoted when it holds a comma, a quote or a line break.
fn quote(cell: &str) -> String {
    match cell.contains([',', '"', '\n', '\r']) {
        true => format!("\"{}\"", cell.replace('"', "\"\"")),
        false => cell.to_owned(),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

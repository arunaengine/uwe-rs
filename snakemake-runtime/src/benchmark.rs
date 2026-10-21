//! Writes Snakemake TSV or JSONL benchmarks from tool timing and container measurements.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Outcome;

/// The columns, in Snakemake's order.
const HEADER: [&str; 10] = [
    "s",
    "h:m:s",
    "max_rss",
    "max_vms",
    "max_uss",
    "max_pss",
    "io_in",
    "io_out",
    "mean_load",
    "cpu_time",
];

/// A value of a column: a number, or `NA`.
#[derive(Clone, Debug, PartialEq)]
enum Cell {
    Number(f64),
    Text(String),
}

/// The text of the benchmark file of a job that ended with this outcome, for a file of this
/// name.
pub fn text(outcome: &Outcome, name: &str) -> String {
    let cells = cells(outcome);
    if name.ends_with(".jsonl") {
        let mut fields: Vec<(&str, &Cell)> = HEADER.iter().copied().zip(cells.iter()).collect();
        fields.sort_by_key(|(key, _)| *key);
        let object: serde_json::Map<String, serde_json::Value> = fields
            .into_iter()
            .map(|(key, cell)| {
                let value = match cell {
                    Cell::Number(number) => serde_json::json!(number),
                    Cell::Text(text) => serde_json::json!(text),
                };
                (key.to_owned(), value)
            })
            .collect();
        // Python's `json.dumps` separates with `, ` and `: `.
        let line = serde_json::Value::Object(object).to_string();
        return format!("{}\n", python_separators(&line));
    }
    // Snakemake's numbers here are floats, which it writes with two decimals.
    let values: Vec<String> = (cells.iter())
        .map(|cell| match cell {
            Cell::Number(number) => format!("{number:.2}"),
            Cell::Text(text) => text.clone(),
        })
        .collect();
    format!("{}\n{}\n", HEADER.join("\t"), values.join("\t"))
}

/// The values of the columns.
fn cells(outcome: &Outcome) -> Vec<Cell> {
    let seconds = match (outcome.started, outcome.ended) {
        (Some(started), Some(ended)) => ended
            .duration_since(started)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64()),
        _ => 0.0,
    };
    let seconds = (seconds * 10_000.0).round() / 10_000.0;
    let whole = seconds as u64;
    let clock = format!("{}:{:02}:{:02}", whole / 3600, whole / 60 % 60, whole % 60);
    let usage = outcome.usage.unwrap_or_default();
    let mebibytes = |bytes: Option<u64>| {
        bytes.map_or(Cell::Text("NA".into()), |bytes| {
            Cell::Number(bytes as f64 / 1024.0 / 1024.0)
        })
    };
    let cpu = usage.cpu_usec.map(|usec| usec as f64 / 1_000_000.0);
    let load = match cpu {
        Some(cpu) if seconds > 0.0 => Cell::Number(cpu / seconds * 100.0),
        _ => Cell::Text("NA".into()),
    };
    vec![
        Cell::Number(seconds),
        Cell::Text(clock),
        mebibytes(usage.memory_peak),
        Cell::Text("NA".into()),
        Cell::Text("NA".into()),
        Cell::Text("NA".into()),
        mebibytes(usage.read_bytes),
        mebibytes(usage.write_bytes),
        load,
        cpu.map_or(Cell::Text("NA".into()), Cell::Number),
    ]
}

/// JSON with the separators of Python's `json.dumps`, outside of strings.
fn python_separators(line: &str) -> String {
    let mut result = String::with_capacity(line.len() + 32);
    let mut in_string = false;
    let mut escaped = false;
    for character in line.chars() {
        result.push(character);
        if in_string {
            match (escaped, character) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            ',' | ':' => result.push(' '),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use workflow_graph::Usage;

    use super::*;

    fn outcome() -> Outcome {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        Outcome {
            started: Some(started),
            ended: Some(started + Duration::from_millis(3_725_500)),
            usage: Some(Usage {
                cpu_usec: Some(1_862_750_000),
                memory_peak: Some(3 << 20),
                read_bytes: Some(1 << 20),
                write_bytes: None,
            }),
            ..Outcome::default()
        }
    }

    #[test]
    fn matches_benchmark_columns() {
        let text = text(&outcome(), "bench.tsv");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "s\th:m:s\tmax_rss\tmax_vms\tmax_uss\tmax_pss\tio_in\tio_out\tmean_load\tcpu_time"
        );
        assert_eq!(
            lines[1],
            "3725.50\t1:02:05\t3.00\tNA\tNA\tNA\t1.00\tNA\t50.00\t1862.75"
        );
    }

    #[test]
    fn sorts_benchmark_keys() {
        let text = text(&outcome(), "bench.jsonl");
        assert!(
            text.starts_with("{\"cpu_time\": 1862.75, \"h:m:s\": \"1:02:05\", "),
            "{text}"
        );
        assert!(text.ends_with("\"s\": 3725.5}\n"), "{text}");
    }
}

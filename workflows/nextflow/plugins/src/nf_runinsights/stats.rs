//! The per-process statistics of nf-runinsights' history, and how its texts show numbers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::observer::Task;
use crate::report_format::Json;

/// What one task that ran gave, from its trace record.
pub(super) struct Sample {
    pub(super) process: String,
    completed: bool,
    realtime: Option<f64>,
    cpu: Option<f64>,
    peak_rss: Option<f64>,
    cpus: Option<f64>,
    memory: Option<f64>,
    queue: Option<f64>,
    attempt: u64,
    container: Option<String>,
    read_bytes: Option<f64>,
}

impl Sample {
    pub(super) fn completed(&self) -> bool {
        self.completed
    }

    pub(super) fn of(task: &Task) -> Sample {
        let record = &task.record;
        let number = |key: &str| record.get(key).and_then(serde_json::Value::as_f64);
        let queue = match (number("submit"), number("start")) {
            (Some(submit), Some(start)) if start >= submit => Some(start - submit),
            _ => None,
        };
        let container = record.get("container").and_then(|it| it.as_str());
        Sample {
            process: record["process"].as_str().unwrap_or_default().to_owned(),
            completed: record["status"] == "COMPLETED",
            realtime: number("realtime"),
            cpu: number("%cpu"),
            peak_rss: number("peak_rss"),
            cpus: number("cpus"),
            memory: number("memory"),
            queue,
            attempt: record["attempt"].as_u64().unwrap_or(1),
            container: container.filter(|it| !it.is_empty()).map(str::to_owned),
            read_bytes: number("read_bytes"),
        }
    }
}

/// The statistics of one process in one run, as the history keeps them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Stats {
    pub(super) tasks: u64,
    pub(super) failed: u64,
    pub(super) realtime_median: Option<f64>,
    pub(super) realtime_max: Option<f64>,
    pub(super) peak_rss_max: Option<f64>,
    pub(super) cpus: Option<f64>,
    pub(super) memory: Option<f64>,
    pub(super) queue_median: Option<f64>,
    pub(super) cpu_efficiency: Option<f64>,
    pub(super) read_total: Option<f64>,
    pub(super) retried: u64,
    pub(super) container: Option<String>,
}

impl Stats {
    pub(super) fn of(samples: &[&Sample]) -> Stats {
        let values = |pick: fn(&Sample) -> Option<f64>| -> Vec<f64> {
            samples.iter().filter_map(|sample| pick(sample)).collect()
        };
        let efficiency = samples.iter().filter_map(|sample| {
            let cpus = sample.cpus.filter(|cpus| *cpus > 0.0)?;
            Some(sample.cpu? / (100.0 * cpus))
        });
        let reads = values(|sample| sample.read_bytes);
        Stats {
            tasks: samples.len() as u64,
            failed: samples.iter().filter(|sample| !sample.completed).count() as u64,
            realtime_median: median(values(|sample| sample.realtime)),
            realtime_max: values(|sample| sample.realtime)
                .into_iter()
                .reduce(f64::max),
            peak_rss_max: values(|sample| sample.peak_rss)
                .into_iter()
                .reduce(f64::max),
            cpus: samples.first().and_then(|sample| sample.cpus),
            memory: samples.first().and_then(|sample| sample.memory),
            queue_median: median(values(|sample| sample.queue)),
            cpu_efficiency: median(efficiency.collect()),
            read_total: (!reads.is_empty()).then(|| reads.iter().sum()),
            retried: samples.iter().filter(|sample| sample.attempt > 1).count() as u64,
            container: samples.iter().find_map(|sample| sample.container.clone()),
        }
    }

    pub(super) fn json(&self) -> Json {
        Json::Object(Vec::new())
            .with("tasks", self.tasks)
            .with("failed", self.failed)
            .with("realtime_ms_median", number(self.realtime_median))
            .with("realtime_ms_max", number(self.realtime_max))
            .with("peak_rss_max", number(self.peak_rss_max))
            .with("cpus_req", number(self.cpus))
            .with("memory_req", number(self.memory))
            .with("queue_ms_median", number(self.queue_median))
            .with("cpu_eff_median", number(self.cpu_efficiency))
            .with("read_bytes_total", number(self.read_total))
            .with("retried", self.retried)
            .with(
                "container",
                self.container.clone().map_or(Json::Null, Json::from),
            )
    }

    pub(super) fn read(value: &serde_json::Value) -> Stats {
        let number = |key: &str| value.get(key).and_then(serde_json::Value::as_f64);
        let count = |key: &str| value.get(key).and_then(serde_json::Value::as_u64);
        Stats {
            tasks: count("tasks").unwrap_or_default(),
            failed: count("failed").unwrap_or_default(),
            realtime_median: number("realtime_ms_median"),
            realtime_max: number("realtime_ms_max"),
            peak_rss_max: number("peak_rss_max"),
            cpus: number("cpus_req"),
            memory: number("memory_req"),
            queue_median: number("queue_ms_median"),
            cpu_efficiency: number("cpu_eff_median"),
            read_total: number("read_bytes_total"),
            retried: count("retried").unwrap_or_default(),
            container: value["container"].as_str().map(str::to_owned),
        }
    }

    /// The row of the process in the report.
    pub(super) fn row(&self, name: &str, history: Option<f64>) -> String {
        format!(
            "| {name} | {} | {} | {} | {} | {} |",
            self.tasks,
            duration(self.realtime_median),
            duration(history),
            bytes(self.peak_rss_max),
            bytes(self.memory)
        )
    }
}

/// An earlier run of the history.
pub(super) struct Prior {
    pub(super) ts: String,
    pub(super) run_name: String,
    pub(super) pipeline: String,
    pub(super) completed: bool,
    pub(super) processes: Vec<(String, Stats)>,
}

impl Prior {
    pub(super) fn read(value: &serde_json::Value) -> Option<Prior> {
        let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
        let processes = (value["processes"].as_object().into_iter().flatten())
            .map(|(name, stats)| (name.clone(), Stats::read(stats)))
            .collect();
        Some(Prior {
            ts: text("ts")?,
            run_name: text("run_name").unwrap_or_default(),
            pipeline: text("pipeline")?,
            completed: text("status").as_deref() == Some("completed"),
            processes,
        })
    }

    pub(super) fn stats(&self, process: &str) -> Option<&Stats> {
        let mut found = self.processes.iter();
        found
            .find(|(name, _)| name == process)
            .map(|(_, stats)| stats)
    }
}

/// The middle value, or the mean of the two middle values; `None` without values.
pub(super) fn median(mut values: Vec<f64>) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    match values.len() {
        0 => None,
        count if count % 2 == 1 => Some(values[middle]),
        _ => Some((values[middle - 1] + values[middle]) / 2.0),
    }
}

/// A whole number as an integer, else as a decimal; `null` when unknown.
pub(super) fn number(value: Option<f64>) -> Json {
    match value {
        Some(value) if value.fract() == 0.0 && value.abs() < 9.0e15 => Json::from(value as i64),
        Some(value) => serde_json::Number::from_f64(value).map_or(Json::Null, Json::Number),
        None => Json::Null,
    }
}

/// `4.0s` under a minute, else `2m 5s`; `-` when unknown.
pub(super) fn duration(millis: Option<f64>) -> String {
    let Some(millis) = millis else {
        return "-".into();
    };
    match millis < 60_000.0 {
        true => format!("{:.1}s", millis / 1000.0),
        false => {
            let seconds = (millis / 1000.0) as u64;
            format!("{}m {}s", seconds / 60, seconds % 60)
        }
    }
}

/// `900 B`, `2.0 GB`, `512 MB`: one decimal under 10 of a unit; `-` when unknown.
pub(super) fn bytes(value: Option<f64>) -> String {
    let Some(mut value) = value else {
        return "-".into();
    };
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit > 0 && value < 10.0 {
        true => format!("{value:.1} {}", units[unit]),
        false => format!("{value:.0} {}", units[unit]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers() {
        assert_eq!(median(vec![973.0, 981.0]), Some(977.0));
        assert_eq!(median(vec![35.0, 40.0]), Some(37.5));
        assert_eq!(number(Some(977.0)).compact(), "977");
        assert_eq!(number(Some(37.5)).compact(), "37.5");
        assert_eq!(duration(Some(125_400.0)), "2m 5s");
        assert_eq!(bytes(Some(2147483648.0)), "2.0 GB");
        assert_eq!(bytes(Some(512.0 * 1024.0 * 1024.0)), "512 MB");
        assert_eq!(bytes(Some(900.0)), "900 B");
    }
}

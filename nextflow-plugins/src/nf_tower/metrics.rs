//! The resource summaries of each process that the complete request sends.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::observer::Task;
use crate::report_format::Json;
use crate::run_metadata::{trace_number, trace_text};

/// The series of each process, in the order the complete request names them.
const SERIES: [&str; 12] = [
    "cpuUsage",
    "gpuMemAvg",
    "vmem",
    "reads",
    "gpuUsage",
    "cpu",
    "gpuMemPeak",
    "mem",
    "memUsage",
    "timeUsage",
    "time",
    "writes",
];

/// The values of the tasks that ended, by process in the order of their first task.
#[derive(Default)]
pub(super) struct Metrics {
    processes: Vec<(String, Vec<Sample>)>,
}

/// The values of one task, by series; `None` when the task has none.
struct Sample {
    name: String,
    values: [Option<f64>; 12],
}

impl Metrics {
    pub(super) fn add(&mut self, task: &Task) {
        let process = trace_text(task, "process");
        let process = process.rsplit(':').next().unwrap_or(process).to_owned();
        let field = |name: &str| trace_number(task, name);
        let positive = |name: &str| field(name).filter(|value| *value > 0.0);
        let cpus = positive("cpus").unwrap_or(1.0);
        let share = |part: Option<f64>, whole: Option<f64>| Some(part? / whole? * 100.0);
        let values = SERIES.map(|series| match series {
            "cpuUsage" => positive("%cpu").map(|cpu| cpu / cpus),
            "vmem" => field("peak_vmem"),
            "reads" => field("rchar"),
            "cpu" => field("%cpu"),
            "mem" => field("peak_rss"),
            "memUsage" => share(positive("peak_rss"), positive("memory")),
            "timeUsage" => share(positive("realtime"), positive("time")),
            "time" => field("realtime"),
            "writes" => field("wchar"),
            _ => None,
        });
        let sample = Sample {
            name: trace_text(task, "name").to_owned(),
            values,
        };
        match self.processes.iter_mut().find(|(name, _)| *name == process) {
            Some((_, samples)) => samples.push(sample),
            None => self.processes.push((process, vec![sample])),
        }
    }

    pub(super) fn json(&self) -> Json {
        let processes = self.processes.iter().map(|(process, samples)| {
            let mut entries = Vec::new();
            for (place, series) in SERIES.iter().enumerate() {
                if place == 1 {
                    entries.push(("process", Json::from(process)));
                }
                let values: Vec<(f64, &str)> = (samples.iter())
                    .filter_map(|sample| Some((sample.values[place]?, sample.name.as_str())))
                    .collect();
                entries.push((series, summary(values)));
            }
            Json::object(entries)
        });
        Json::List(processes.collect())
    }
}

/// Mean, minimum, quartiles and maximum, with the names of the tasks they come from; null
/// for no values or only zeros.
fn summary(mut values: Vec<(f64, &str)>) -> Json {
    values.sort_by(|left, right| left.0.total_cmp(&right.0));
    let (Some(first), Some(last)) = (values.first(), values.last()) else {
        return Json::Null;
    };
    if first.0 == 0.0 && last.0 == 0.0 {
        return Json::Null;
    }
    let mean = values.iter().map(|value| value.0).sum::<f64>() / values.len() as f64;
    let quartile = |percent: f64| {
        let position = (values.len() - 1) as f64 * percent / 100.0;
        let lower = values[position.floor() as usize];
        let upper = values[position.ceil() as usize];
        let value = lower.0 + (upper.0 - lower.0) * position.fract();
        (value, lower.1)
    };
    let (q1, q2, q3) = (quartile(25.0), quartile(50.0), quartile(75.0));
    Json::object([
        ("mean", number(mean)),
        ("min", number(first.0)),
        ("q1", number(q1.0)),
        ("q2", number(q2.0)),
        ("q3", number(q3.0)),
        ("max", number(last.0)),
        ("minLabel", Json::from(first.1)),
        ("maxLabel", Json::from(last.1)),
        ("q1Label", Json::from(q1.1)),
        ("q2Label", Json::from(q2.1)),
        ("q3Label", Json::from(q3.1)),
    ])
}

/// A value rounded half up to two decimals from its exact binary value, so `69.975` gives
/// `69.97`; whole numbers have no fraction.
fn number(value: f64) -> Json {
    let text = format!("{:.20}", value.abs());
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, "000"));
    let digits = fraction.as_bytes();
    let mut cents = whole.parse::<i64>().unwrap_or_default() * 100
        + i64::from(digits[0] - b'0') * 10
        + i64::from(digits[1] - b'0');
    if digits[2] >= b'5' {
        cents += 1;
    }
    let cents = if value < 0.0 { -cents } else { cents };
    match cents % 100 {
        0 => Json::from(cents / 100),
        _ => Json::from(cents as f64 / 100.0),
    }
}

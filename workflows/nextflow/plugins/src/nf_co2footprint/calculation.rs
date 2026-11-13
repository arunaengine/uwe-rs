//! The Green Algorithms estimate of nf-co2footprint: the energy and CO₂ equivalents of each
//! task, the CPU power per core, and the sums of processes, the workflow and the session.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use crate::observer::Record;

/// The power per core of the fallback rows of the plugin's CPU table, in W: the TDP of the
/// row over its logical cores, as the plugin's documentation gives them.
const FALLBACK: [(&str, f64); 4] = [
    ("default", 11.41),
    ("default local", 12.32),
    ("default compute cluster", 10.77),
    ("default cloud", 1.52),
];

/// What the estimate of a task, or the sum of a group of them, holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Metrics {
    /// The task id, or `None` for a group, which prints `-1`.
    pub(super) task_id: Option<i64>,
    pub(super) status: Option<String>,
    pub(super) name: Option<String>,
    pub(super) process: String,
    /// In kWh.
    pub(super) energy: f64,
    /// In g.
    pub(super) co2: f64,
    pub(super) co2_market: Option<f64>,
    /// In gCO₂e/kWh.
    pub(super) intensity: f64,
    pub(super) intensity_market: Option<f64>,
    /// In percent of one core.
    pub(super) cpu: f64,
    /// In bytes.
    pub(super) memory: f64,
    /// In milliseconds.
    pub(super) realtime: f64,
    pub(super) cpus: f64,
    pub(super) pue: f64,
    /// In W per core and W per GB.
    pub(super) powerdraw_cpu: f64,
    pub(super) powerdraw_memory: f64,
    pub(super) cpu_model: Option<String>,
    /// The energy of the cores and of the memory without the PUE, in kWh.
    pub(super) raw_processor: f64,
    pub(super) raw_memory: f64,
    /// The energy and CO₂e of the tasks that were not cached.
    pub(super) energy_fresh: f64,
    pub(super) co2_fresh: f64,
    /// The trace record of a task, empty for a group.
    pub(super) record: Record,
}

/// The settings that the estimate uses.
#[derive(Clone, Debug, Default)]
pub(super) struct Model {
    pub(super) pue: f64,
    pub(super) powerdraw_memory: f64,
    /// The fallback row that unknown CPUs use, like `default local`.
    pub(super) fallback: String,
    /// The power per core for unknown CPUs that the config gives instead of the row's.
    pub(super) cpu_default: Option<f64>,
    pub(super) ignore_model: bool,
    /// The CPUs of the config's `customCpuTdpFile`: name and power per core.
    pub(super) cpus: Vec<(String, f64)>,
    pub(super) intensity_market: Option<f64>,
}

impl Model {
    /// The power per core of a CPU model, and the model as the estimate records it.
    fn power(&self, model: Option<&str>, warn: &mut Warnings) -> (f64, Option<String>) {
        if self.ignore_model {
            return (self.fallback_power(), Some("Custom value".to_owned()));
        }
        if let Some(found) = model.and_then(|model| self.lookup(model)) {
            return (found, model.map(str::to_owned));
        }
        let power = self.fallback_power();
        let text = match model {
            Some(model) => format!(
                "Could not find CPU model \"{model}\" in given TDP data table. Using {} CPU power \
                 draw value ({power} W).\n\tTo fix this warning, please refer to \
                 https://nextflow-io.github.io/nf-co2footprint/usage/faq/#cpu-model.",
                self.fallback
            ),
            None => format!(
                "No CPU model detected. Using {} CPU power draw value ({power} W).",
                self.fallback
            ),
        };
        warn.once(&text);
        (power, model.map(str::to_owned))
    }

    fn fallback_power(&self) -> f64 {
        self.cpu_default.unwrap_or_else(|| {
            (FALLBACK.iter())
                .find(|(name, _)| *name == self.fallback)
                .map_or(FALLBACK[0].1, |(_, power)| *power)
        })
    }

    /// The power of the only CPU of the table that matches the model, else of the part before
    /// its last `@`.
    fn lookup(&self, model: &str) -> Option<f64> {
        let only = |wanted: &str| {
            let mut matching = (self.cpus.iter()).filter(|(name, _)| normal(name) == wanted);
            match (matching.next(), matching.next()) {
                (Some((_, power)), None) => Some(*power),
                _ => None,
            }
        };
        only(&normal(model)).or_else(|| {
            let (before, _) = model.rsplit_once('@')?;
            only(&normal(before))
        })
    }

    /// The estimate of one task from its trace record, `None` when the record has no memory.
    pub(super) fn task(
        &self,
        record: &Record,
        intensity: f64,
        warn: &mut Warnings,
    ) -> Option<Metrics> {
        let id = record["task_id"].as_i64().unwrap_or_default();
        let number = |key: &str| record.get(key).and_then(serde_json::Value::as_f64);
        let mut missing = |key: &str, default: &str| {
            warn.once(&format!(
                "Missing trace value '{key}' for task {id}, using default: {default}."
            ));
        };
        let realtime = number("realtime").unwrap_or_else(|| {
            missing("realtime", "0");
            0.0
        });
        let requested = number("cpus").unwrap_or_else(|| {
            missing("cpus", "1");
            1.0
        });
        let cpu = match number("%cpu") {
            Some(cpu) => cpu,
            None => {
                let text = format!("100% for all {requested} cores");
                missing("%cpu", &text);
                requested * 100.0
            }
        };
        if cpu == 0.0 {
            warn.once(&format!("The reported CPU usage is 0.0 for task {id}."));
        }
        let memory = match (number("memory"), number("peak_rss")) {
            (Some(memory), _) => memory,
            (None, Some(peak)) => {
                warn.once(&format!(
                    "Requested memory is null for task {id}. Using maximum consumed \
                     memory/`peak_rss` ({} GB) for CO\u{2082}e footprint computation.",
                    peak / 1e9
                ));
                peak
            }
            (None, None) => {
                tracing::error!(
                    "No requested memory and maximum consumed memory found for task {id}."
                );
                return None;
            }
        };
        let model = record.get("cpu_model").and_then(serde_json::Value::as_str);
        let (core, cpu_model) = self.power(model, warn);
        let cores = (cpu / 100.0).ceil().max(requested);
        let usage = if cores > 0.0 {
            cpu / (100.0 * cores)
        } else {
            0.0
        };
        let hours = realtime / 3_600_000.0;
        let raw_processor = hours * cores * core * usage / 1000.0;
        let raw_memory = hours * memory / 1e9 * self.powerdraw_memory / 1000.0;
        let energy = self.pue * (raw_processor + raw_memory);
        let co2 = energy * intensity;
        let fresh = record["status"] != "CACHED";
        let text = |key: &str| {
            record
                .get(key)
                .and_then(|value| value.as_str().map(str::to_owned))
        };
        Some(Metrics {
            task_id: Some(id),
            status: text("status"),
            name: text("name"),
            process: text("process").unwrap_or_default(),
            energy,
            co2,
            co2_market: self.intensity_market.map(|market| energy * market),
            intensity,
            intensity_market: self.intensity_market,
            cpu,
            memory,
            realtime,
            cpus: cores,
            pue: self.pue,
            powerdraw_cpu: core,
            powerdraw_memory: self.powerdraw_memory,
            cpu_model,
            raw_processor,
            raw_memory,
            energy_fresh: if fresh { energy } else { 0.0 },
            co2_fresh: if fresh { co2 } else { 0.0 },
            record: record.clone(),
        })
    }
}

/// The sum of a group of estimates, named `name`: sums, maxima, and means weighted by energy
/// or run time, as the plugin forms them.
pub(super) fn aggregate(name: &str, parts: &[Metrics]) -> Metrics {
    let sum = |value: fn(&Metrics) -> f64| parts.iter().map(value).sum::<f64>();
    let weighted = |value: fn(&Metrics) -> f64, weight: fn(&Metrics) -> f64| {
        let total = sum(weight);
        match total > 0.0 {
            true => {
                parts
                    .iter()
                    .map(|part| value(part) * weight(part))
                    .sum::<f64>()
                    / total
            }
            false if !parts.is_empty() => sum(value) / parts.len() as f64,
            false => 0.0,
        }
    };
    let common = |value: fn(&Metrics) -> Option<&String>| {
        let first = parts.first().and_then(value)?;
        parts
            .iter()
            .all(|part| value(part) == Some(first))
            .then(|| first.clone())
    };
    let optional = |value: fn(&Metrics) -> Option<f64>| {
        let values: Vec<f64> = parts.iter().filter_map(value).collect();
        (!values.is_empty()).then(|| values.iter().sum())
    };
    let energy = |part: &Metrics| part.energy;
    let realtime = |part: &Metrics| part.realtime;
    Metrics {
        task_id: None,
        status: common(|part| part.status.as_ref()),
        name: Some(common(|part| part.name.as_ref()).unwrap_or_else(|| name.to_owned())),
        process: String::new(),
        energy: sum(energy),
        co2: sum(|part| part.co2),
        co2_market: optional(|part| part.co2_market),
        intensity: weighted(|part| part.intensity, energy),
        intensity_market: parts
            .iter()
            .any(|part| part.intensity_market.is_some())
            .then(|| weighted(|part| part.intensity_market.unwrap_or_default(), energy)),
        cpu: weighted(|part| part.cpu, realtime),
        memory: parts.iter().map(|part| part.memory).fold(0.0, f64::max),
        realtime: sum(realtime),
        cpus: weighted(|part| part.cpus, realtime),
        pue: weighted(|part| part.pue, realtime),
        powerdraw_cpu: weighted(|part| part.powerdraw_cpu, energy),
        powerdraw_memory: weighted(|part| part.powerdraw_memory, energy),
        cpu_model: common(|part| part.cpu_model.as_ref()),
        raw_processor: sum(|part| part.raw_processor),
        raw_memory: sum(|part| part.raw_memory),
        energy_fresh: sum(|part| part.energy_fresh),
        co2_fresh: sum(|part| part.co2_fresh),
        record: Record::new(),
    }
}

/// A CPU name for matching: lower case, without `(R)`, `(TM)`, `(C)`, other characters
/// outside ASCII, the words processor and CPU, a part like `8-core`, and spaces.
fn normal(name: &str) -> String {
    let lower = name.to_lowercase();
    let lower = (lower
        .replace("(r)", " ")
        .replace("(tm)", " ")
        .replace("(c)", " "))
    .chars()
    .filter(char::is_ascii)
    .collect::<String>();
    (lower.split_whitespace())
        .filter(|word| !matches!(*word, "processor" | "processors" | "cpu" | "cpus"))
        .filter(|word| {
            let core = word.strip_suffix("-core");
            !core
                .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect()
}

/// Shows a warning once; later ones of the same text go to the debug log, as in the plugin.
#[derive(Default)]
pub(super) struct Warnings {
    shown: HashSet<String>,
}

impl Warnings {
    pub(super) fn once(&mut self, text: &str) {
        if self.shown.insert(text.to_owned()) {
            tracing::warn!("[nf-co2footprint] {text}");
        } else {
            tracing::debug!("[nf-co2footprint] {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model {
            pue: 1.0,
            powerdraw_memory: 0.3725,
            fallback: "default".into(),
            ..Model::default()
        }
    }

    #[test]
    fn estimates_recorded_task() {
        let record = serde_json::json!({
            "task_id": 3, "status": "COMPLETED", "name": "WORK (3)", "process": "WORK",
            "realtime": 1007, "cpus": 1, "%cpu": 2.0, "memory": 1_073_741_824_u64,
        });
        let record = record.as_object().cloned().unwrap_or_default();
        let task = model().task(&record, 333.77, &mut Warnings::default());
        let task = task.expect("an estimate");
        let round = |value: f64| (value * 1e12).round() / 1e6;
        assert_eq!(round(task.raw_processor), 0.063_833);
        assert_eq!(round(task.raw_memory), 0.111_88);
        assert_eq!(round(task.energy), 0.175_713);
        assert_eq!((task.co2 * 1e8).round() / 100.0, 58.65);
    }

    #[test]
    fn matches_cpu_names() {
        let model = Model {
            cpus: vec![
                ("intel xeon e5-2670 v3".into(), 10.0),
                ("amd epyc 7742".into(), 3.4),
            ],
            ..model()
        };
        let mut warn = Warnings::default();
        let power = |name| model.power(Some(name), &mut Warnings::default()).0;
        assert_eq!(power("Intel(R) Xeon(R) CPU E5-2670 v3 @ 2.30GHz"), 10.0);
        assert_eq!(power("AMD EPYC 7742 64-Core Processor"), 3.4);
        assert_eq!(model.power(Some("unknown"), &mut warn).0, 11.41);
    }
}

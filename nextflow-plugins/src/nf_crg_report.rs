//! nf-crg-report 1.1.10: nf-report's reports with the estimated cost of each task, of the head
//! job and of the run, from rates of a price service, a JSON file or the config.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::nf_report::{FUNCTIONS, Finished, Flavour, activate_as, call};
use crate::observer::Observer;
use crate::report_format::Json;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: FUNCTIONS,
    call: Some(call),
    scopes: &["nfreport"],
    observer: Some(activate),
    ..Plugin::new("nf-crg-report", Kind::Observer, State::Partial)
};

/// How long the price service may take to connect and to answer.
const LIMIT: Duration = Duration::from_secs(5);

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    activate_as(config, Flavour::Crg)
}

/// A time in the time zone of this machine as `yyyyMMddHHmmss`, the default file suffix.
pub(crate) fn stamp(millis: i64) -> String {
    let stamp = Timestamp::from_millisecond(millis).unwrap_or(Timestamp::UNIX_EPOCH);
    stamp
        .to_zoned(TimeZone::system())
        .strftime("%Y%m%d%H%M%S")
        .to_string()
}

/// The rates of the run and the resources of the head job.
#[derive(Clone, Debug)]
pub(crate) struct Costs {
    currency: String,
    /// Costs of 1000 CPU hours, 1000 GB-hours of memory and of GPU memory, and of a TB-month.
    cpu: f64,
    memory: f64,
    gpu: f64,
    storage: f64,
    gpu_memory: f64,
    head: Option<[f64; 3]>,
}

impl Costs {
    /// The rates: the price service of `priceAPI`, else the file of `priceJsonPath`, then the
    /// `costs` settings, else 0; loaded once for the whole run.
    pub(crate) fn of(config: &Config) -> Costs {
        let scope = config.get(&["nfreport", "costs"]);
        let scope = match scope {
            None | Some(Value::Null) | Some(Value::Map(_)) => scope,
            Some(_) => {
                tracing::warn!(
                    "Invalid value for 'nfreport.costs', expected a Map. Using default: [:]"
                );
                None
            }
        };
        let setting = |key: &str| {
            let entries = crate::nf_schema::entries(scope.as_ref().unwrap_or(&Value::Null));
            entries
                .into_iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)
        };
        let number = |key: &str, default: f64| {
            setting(key)
                .and_then(|value| value.to_string().parse().ok())
                .unwrap_or(default)
        };
        let prices = match (setting("priceAPI"), setting("priceJsonPath")) {
            (Some(url), _) => fetch(&url.to_string()),
            (None, Some(path)) => read(&path.to_string()),
            (None, None) => {
                tracing::info!(
                    "No pricing API or JSON path configured. Cost estimation will default to 0.0."
                );
                None
            }
        };
        let price = |key: &str| prices.as_ref().and_then(|prices| prices[key].as_f64());
        let costs = Costs {
            currency: (prices
                .as_ref()
                .and_then(|prices| prices["currency"].as_str().map(str::to_owned)))
            .or_else(|| setting("currency").map(|value| value.to_string()))
            .unwrap_or("EUR".to_owned()),
            cpu: price("kCPUHr").unwrap_or(number("kCPUHr", 0.0)),
            memory: price("kGBHr").unwrap_or(number("kGBHr", 0.0)),
            gpu: price("kGPUGBHr").unwrap_or(number("kGPUGBHr", 0.0)),
            storage: price("TBMonth").unwrap_or(number("TBMonth", 0.0)),
            gpu_memory: number("defaultGpuMemGb", 16.0),
            head: setting("launchFile").and_then(|path| head(&path.to_string())),
        };
        tracing::info!(
            "Pricing rates loaded: currency={}, kCPUHr={}, kGBHr={}, kGPUGBHr={}, TBMonth={}",
            costs.currency,
            java(costs.cpu),
            java(costs.memory),
            java(costs.gpu),
            java(costs.storage)
        );
        costs
    }

    /// The compute cost of resources for some hours.
    fn compute(&self, cpus: f64, memory: f64, gpus: f64, hours: f64) -> f64 {
        cpus * hours * self.cpu / 1000.0
            + memory * hours * self.memory / 1000.0
            + gpus * self.gpu_memory * hours * self.gpu / 1000.0
    }

    /// An amount with the currency's symbol and four decimals, like `€0.0012` or `CHF 0.0012`.
    fn money(&self, amount: f64) -> String {
        let symbol = match self.currency.to_uppercase().as_str() {
            "EUR" => "€".to_owned(),
            "USD" => "$".to_owned(),
            "GBP" => "£".to_owned(),
            other => format!("{other} "),
        };
        format!("{symbol}{amount:.4}")
    }

    /// The compute and storage cost of a task.
    fn of_task(&self, task: &Finished) -> (f64, f64) {
        let hours = task.number("realtime").unwrap_or(0) as f64 / 3_600_000.0;
        let (cpus, memory, gpus) = resources(task);
        let bytes: u64 = (task.task.outputs.iter())
            .filter_map(|file| std::fs::metadata(&file.path).ok())
            .filter(std::fs::Metadata::is_file)
            .map(|found| found.len())
            .sum();
        let storage = bytes as f64 / 1024f64.powi(4) * self.storage;
        (
            self.compute(cpus as f64, memory, gpus as f64, hours),
            storage,
        )
    }

    /// The cost fields of a task, which go after its `exit_status`.
    pub(crate) fn task(&self, task: &Finished) -> Vec<(String, Json)> {
        let (compute, storage) = self.of_task(task);
        let (cpus, memory, gpus) = resources(task);
        let bytes: u64 = (task.task.outputs.iter())
            .filter_map(|file| std::fs::metadata(&file.path).ok())
            .filter(std::fs::Metadata::is_file)
            .map(|found| found.len())
            .sum();
        [
            ("compute_cost", Json::from(self.money(compute))),
            (
                "projected_monthly_storage_cost",
                Json::from(self.money(storage)),
            ),
            (
                "total_estimated_cost",
                Json::from(self.money(compute + storage)),
            ),
            ("raw_compute_cost", Json::from(compute)),
            ("raw_storage_cost", Json::from(storage)),
            ("allocated_cpus", Json::from(cpus)),
            ("allocated_mem_gb", Json::from(format!("{memory:.2} GB"))),
            ("allocated_gpus", Json::from(gpus)),
            (
                "output_size",
                Json::from(format!("{:.4} GB", bytes as f64 / 1024f64.powi(3))),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
    }

    /// The cost fields of the summary, and the costs by process; the head job runs from the
    /// start of the run to its end, or to now while it runs.
    pub(crate) fn totals(
        &self,
        tasks: &[Finished],
        start: Option<i64>,
        end: Option<i64>,
    ) -> (Vec<(String, Json)>, Json) {
        let mut processes: Vec<(String, i64, f64, f64)> = Vec::new();
        let (mut compute, mut storage) = (0.0, 0.0);
        for task in tasks {
            let (task_compute, task_storage) = self.of_task(task);
            compute += task_compute;
            storage += task_storage;
            let process = task.text("process").unwrap_or("unknown".to_owned());
            match processes.iter_mut().find(|(name, ..)| *name == process) {
                Some(found) => {
                    found.1 += 1;
                    found.2 += task_compute;
                    found.3 += task_storage;
                }
                None => processes.push((process, 1, task_compute, task_storage)),
            }
        }
        let head = self
            .head
            .filter(|resources| resources.iter().any(|value| *value > 0.0));
        let head_cost = match (head, start) {
            (Some([cpus, memory, gpus]), Some(start)) => {
                let end = end.unwrap_or_else(crate::report_format::now);
                self.compute(
                    cpus,
                    memory,
                    gpus,
                    (end - start).max(0) as f64 / 3_600_000.0,
                )
            }
            _ => 0.0,
        };
        let summary = vec![
            ("has_head_job_cost", Json::from(head.is_some())),
            ("tasks_compute_cost", Json::from(self.money(compute))),
            ("head_job_cost", Json::from(self.money(head_cost))),
            (
                "total_compute_cost",
                Json::from(self.money(compute + head_cost)),
            ),
            (
                "projected_monthly_storage_cost",
                Json::from(self.money(storage)),
            ),
            (
                "total_estimated_cost",
                Json::from(self.money(compute + head_cost + storage)),
            ),
            (
                "pricing_rates",
                Json::object([
                    ("currency", Json::from(self.currency.as_str())),
                    ("kCPUHr", Json::from(self.cpu)),
                    ("kGBHr", Json::from(self.memory)),
                    ("kGPUGBHr", Json::from(self.gpu)),
                    ("TBMonth", Json::from(self.storage)),
                ]),
            ),
        ];
        let by_process = (processes.into_iter()).map(|(name, count, compute, storage)| {
            let costs = Json::object([
                ("count", Json::from(count)),
                ("compute_cost", Json::from(self.money(compute))),
                (
                    "projected_monthly_storage_cost",
                    Json::from(self.money(storage)),
                ),
                (
                    "total_estimated_cost",
                    Json::from(self.money(compute + storage)),
                ),
                ("raw_compute_cost", Json::from(compute)),
                ("raw_storage_cost", Json::from(storage)),
                ("raw_total_estimated_cost", Json::from(compute + storage)),
            ]);
            (name, costs)
        });
        let summary = summary
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        (summary, Json::Object(by_process.collect()))
    }
}

/// The CPUs, memory in GB and GPUs of a task from its trace.
fn resources(task: &Finished) -> (i64, f64, i64) {
    let cpus = task.number("cpus").unwrap_or(1);
    let memory = task.number("memory").unwrap_or(0) as f64 / 1024f64.powi(3);
    let gpus = task.number("accelerator").unwrap_or(0);
    (cpus, memory, gpus)
}

/// A double as Java prints it, like `9.69` or `0.0`.
fn java(value: f64) -> String {
    Value::Number(nextflow_evaluator::number::Number::Double(value)).to_string()
}

/// An address without the user and password it may hold, for log lines.
fn shown(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            match rest.split_once('@').filter(|(user, _)| !user.contains('/')) {
                Some((_, host)) => format!("{scheme}://{host}"),
                None => url.to_owned(),
            }
        }
        None => url.to_owned(),
    }
}

/// The prices of the price service.
fn fetch(url: &str) -> Option<serde_json::Value> {
    let failed = |error: String| {
        tracing::warn!(
            "Pricing API at {} is not working or does not exist: {error}. Cost estimation will \
             default to configured/default rates.",
            shown(url)
        );
        None
    };
    let client = match storage::http_client() {
        Ok(client) => client,
        Err(error) => return failed(error),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return failed(error.to_string()),
    };
    let request = client.get(url).timeout(LIMIT).header("Accept", "*/*");
    let answer = runtime.block_on(async {
        let response = request.send().await.map_err(|error| error.to_string())?;
        response.text().await.map_err(|error| error.to_string())
    });
    match answer {
        Err(error) => failed(error),
        Ok(text) if text.trim().is_empty() => {
            tracing::warn!(
                "Pricing API at {} returned an empty response. Cost estimation will default to \
                 configured/default rates.",
                shown(url)
            );
            None
        }
        Ok(text) => parse(&text),
    }
}

/// The prices of a local JSON file.
fn read(path: &str) -> Option<serde_json::Value> {
    match std::fs::read_to_string(path) {
        Err(_) => {
            tracing::warn!(
                "Pricing JSON file not found at: {path}. Cost estimation will default to 0.0."
            );
            None
        }
        Ok(text) if text.trim().is_empty() => {
            tracing::warn!(
                "Pricing JSON at {path} is empty. Cost estimation will default to configured/default rates."
            );
            None
        }
        Ok(text) => parse(&text),
    }
}

/// Prices without what follows `//` or `#` on each line, as the plugin strips comments.
fn parse(text: &str) -> Option<serde_json::Value> {
    let cleaned: Vec<&str> = (text.lines())
        .map(|line| {
            let cut = [line.find("//"), line.find('#')]
                .into_iter()
                .flatten()
                .min();
            cut.map_or(line, |at| &line[..at])
        })
        .collect();
    match serde_json::from_str(&cleaned.join("\n")) {
        Ok(prices) => Some(prices),
        Err(error) => {
            tracing::warn!("Failed to parse pricing JSON: {error}");
            None
        }
    }
}

/// The CPUs, memory in GB and GPUs of the head job from the scheduler lines of its launch
/// script: `#SBATCH` and `#$`; the last line that sets a value wins.
fn head(path: &str) -> Option<[f64; 3]> {
    let Ok(text) = std::fs::read_to_string(path) else {
        tracing::warn!("Launch script not found at: {path}");
        return None;
    };
    let (mut cpus, mut memory, mut gpus) = (None, 0.0, 0.0);
    let mut scheduled = false;
    for line in text.lines().map(str::trim) {
        let words: Vec<&str> = if let Some(rest) = line.strip_prefix("#SBATCH") {
            rest.split_whitespace().collect()
        } else if let Some(rest) = line.strip_prefix("#$") {
            rest.split_whitespace().collect()
        } else {
            continue;
        };
        scheduled = true;

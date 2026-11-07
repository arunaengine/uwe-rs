//! The settings of nf-co2footprint from the `co2footprint` scope: its output files, the
//! machine type and PUE of the executor, the CPU power table and the carbon intensity.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use super::calculation::Model;

/// The carbon intensity when the config gives none: the world average of Ember's Global
/// Electricity Review 2024, in gCO₂e/kWh.
const GLOBAL: f64 = 480.0;

/// The machine type and power usage effectiveness of each Nextflow executor, and of each
/// machine type, from the surveys and provider pages that the plugin cites.
const EXECUTORS: [(&str, &str, &str); 19] = [
    ("cloud", "cloud", "1.56"),
    ("awsbatch", "cloud", "1.15"),
    ("azurebatch", "cloud", "1.18"),
    ("google-batch", "cloud", "1.1"),
    ("google-lifesciences", "cloud", "1.1"),
    ("compute cluster", "compute cluster", "1.67"),
    ("bridge", "compute cluster", "1.67"),
    ("flux", "compute cluster", "1.67"),
    ("condor", "compute cluster", "1.67"),
    ("hq", "compute cluster", "1.67"),
    ("k8s", "compute cluster", "1.67"),
    ("moab", "compute cluster", "1.67"),
    ("nqsii", "compute cluster", "1.67"),
    ("oar", "compute cluster", "1.67"),
    ("pbs", "compute cluster", "1.67"),
    ("pbspro", "compute cluster", "1.67"),
    ("sge", "compute cluster", "1.67"),
    ("slurm", "compute cluster", "1.67"),
    ("local", "local", "1"),
];

/// An output file of the plugin.
pub(super) struct Output {
    pub(super) enabled: bool,
    pub(super) path: PathBuf,
    /// The path as the config gives it, which the summary shows.
    pub(super) text: String,
    pub(super) overwrite: bool,
}

impl Output {
    pub(super) fn write(&self, text: &str) {
        if !self.enabled {
            return;
        }
        if self.path.exists() && !self.overwrite {
            tracing::warn!("[nf-co2footprint] {} already exists", self.path.display());
            return;
        }
        let parent = self.path.parent().map(std::fs::create_dir_all);
        if let Err(error) = parent
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(&self.path, text))
        {
            tracing::warn!(%error, "[nf-co2footprint] {} was not written", self.path.display());
        }
    }
}

/// What the config of the `co2footprint` scope asks for.
pub(super) struct Settings {
    pub(super) trace: Output,
    pub(super) summary: Output,
    pub(super) report: Output,
    pub(super) provenance: Output,
    pub(super) max_tasks: usize,
    pub(super) metrics_only: bool,
    pub(super) nulls: bool,
    pub(super) model: Model,
    pub(super) intensity: f64,
    pub(super) zone: Option<String>,
    pub(super) token: Option<String>,
    /// The options as the summary and the report show them.
    pub(super) options: Vec<(&'static str, String)>,
}

/// Reads the `co2footprint` scope; a machine type that the plugin does not know is an error,
/// which stops the run.
pub(super) fn read(config: &Config) -> Result<Settings, String> {
    tracing::info!("nf-co2footprint plugin  ~  version 1.4.1");
    let get = |path: &[&str]| {
        let mut full = vec!["co2footprint"];
        full.extend_from_slice(path);
        config
            .get(&full)
            .filter(|value| !matches!(value, Value::Null))
    };
    let text = |path: &[&str]| get(path).map(|value| value.to_string());
    let number = |path: &[&str]| match get(path) {
        Some(Value::Number(number)) => Some(number.to_f64()),
        Some(other) => other.to_string().parse().ok(),
        None => None,
    };
    let flag = |path: &[&str], default: bool| get(path).map_or(default, |value| value.truth());
    let launch = std::env::current_dir().map_err(|error| error.to_string())?;
    let stamp = stamp(crate::report_format::now());
    let output = |name: &str, extension: &str, enabled: bool| {
        let text = text(&[name, "file"])
            .unwrap_or_else(|| format!("co2footprint_{name}_{stamp}.{extension}"));
        Output {
            enabled: flag(&[name, "enabled"], enabled),
            path: launch.join(&text),
            text,
            overwrite: flag(&[name, "overwrite"], true),
        }
    };
    let machine = text(&["machineType"]);
    if let Some(machine) = machine.as_deref()
        && !["local", "compute cluster", "cloud"].contains(&machine)
    {
        return Err(format!(
            "`machineType?` '{machine}' is not supported. Please chose one of [local, compute \
             cluster, cloud]."
        ));
    }
    let executor = config
        .get(&["process", "executor"])
        .map(|value| value.to_string());
    let row = |name: &str| EXECUTORS.iter().find(|(known, _, _)| *known == name);
    let executor_row = executor.as_deref().and_then(row);
    if executor_row.is_none() {
        let name = executor.as_deref().unwrap_or("null");
        tracing::warn!("[nf-co2footprint] Executor '{name}' not mapped.");
    }
    let machine = machine.or_else(|| executor_row.map(|(_, machine, _)| (*machine).to_owned()));
    match machine.as_deref() {
        None => tracing::warn!(
            "[nf-co2footprint] Fallback to: `machineType = null`, `pue = 1.0`. To eliminate this \
             warning you can set `machineType` in the config to one of [local, compute cluster, \
             cloud]."
        ),
        Some("cloud") => tracing::warn!(
            "[nf-co2footprint] Cloud instances are not yet fully supported. We are working on the \
             seamless integration of major cloud providers. In the meantime we recommend following \
             the instructions at \
             https://nextflow-io.github.io/nf-co2footprint/usage/configuration/#cloud-computationsto \
             fully integrate your cloud instances into the plugin."
        ),
        Some(_) => {}
    }
    let pue_text = (text(&["pue"]))
        .or_else(|| executor_row.map(|(_, _, pue)| (*pue).to_owned()))
        .or_else(|| {
            machine
                .as_deref()
                .and_then(row)
                .map(|(_, _, pue)| (*pue).to_owned())
        })
        .unwrap_or_else(|| "1.0".to_owned());
    let ignore_model = flag(&["ignoreCpuModel"], false);
    let custom = text(&["customCpuTdpFile"]);
    let mut cpus = Vec::new();
    match (&custom, ignore_model) {
        (Some(_), true) => tracing::warn!(
            "[nf-co2footprint] Both 'customCpuTdpFile' and 'ignoreCpuModel=true' are set. Note: \
             When 'ignoreCpuModel' is true, the custom TDP file will be ignored."
        ),
        (Some(file), false) => cpus = cpu_table(&launch.join(file))?,
        (None, _) => {}
    }
    if get(&["cpuPowerModel"]).is_some() {
        tracing::warn!(
            "[nf-co2footprint] uwe does not run `cpuPowerModel` closures; the power per core of \
             the CPU table is used"
        );
    }
    let ci = number(&["ci"]);
    let zone = text(&["location"]).map(|zone| zone.to_uppercase());
    let token = text(&["emApiKey"]);
    let (intensity, ci_text) = match (ci, &token) {
        (Some(ci), _) => {
            tracing::info!("[nf-co2footprint] Using given value: {ci} gCO\u{2082}eq/kWh.");
            (ci, text(&["ci"]).unwrap_or_default())
        }
        (None, token) => {
            if let Some(zone) = &zone {
                tracing::warn!(
                    "[nf-co2footprint] Target zone {zone} not found. Attempting to retrieve \
                     carbon intensity for fallback zone GLOBAL."
                );
            }
            tracing::info!(
                "[nf-co2footprint] Using fallback carbon intensity from GLOBAL from CI table: \
                 {GLOBAL:.1} gCO\u{2082}eq/kWh."
            );
            let shown = if token.is_some() {
                "dynamic".to_owned()
            } else {
                format!("{GLOBAL:.1}")
            };
            (GLOBAL, shown)
        }
    };
    match &token {
        Some(_) => tracing::info!(
            "[nf-co2footprint] Electricity Maps API key is set. Attempting to retrieve real-time \
             carbon intensity."
        ),
        None => tracing::info!(
            "[nf-co2footprint] Electricity Maps API key is not set. To retrieve real-time carbon \
             intensity values, please provide a key with the parameter `emApiKey`.\n\tYou can \
             obtain a key for ElectricityMaps at https://portal.electricitymaps.com/auth/login."
        ),
    }
    let shown = |value: Option<String>| value.unwrap_or_else(|| "null".to_owned());
    let powerdraw_memory = number(&["powerdrawMem"]).unwrap_or(0.3725);
    let settings = Settings {
        trace: output("trace", "txt", true),
        summary: output("summary", "txt", false),
        report: output("report", "html", true),
        provenance: output("provenance", "json", true),
        max_tasks: number(&["report", "maxTasks"]).map_or(10_000, |count| count as usize),
        metrics_only: flag(&["provenance", "emissionMetricsOnly"], true),
        nulls: flag(&["provenance", "includeNulls"], false),
        model: Model {
            pue: pue_text.parse().unwrap_or(1.0),
            powerdraw_memory,
            fallback: machine
                .as_ref()
                .map_or("default".to_owned(), |machine| format!("default {machine}")),
            cpu_default: number(&["powerdrawCpuDefault"]),
            ignore_model,
            cpus,
            intensity_market: number(&["ciMarket"]),
        },
        intensity,
        zone: zone.clone(),
        // A given `ci` wins over live values, so the API is not asked then.
        token: token.filter(|_| ci.is_none()),
        options: Vec::new(),
    };
    let mut settings = settings;
    settings.options = vec![
        ("ci", ci_text),
        ("ciMarket", shown(text(&["ciMarket"]))),
        ("ignoreCpuModel", ignore_model.to_string()),
        ("location", shown(zone)),
        ("machineType", shown(machine)),
        ("powerdrawCpuDefault", shown(text(&["powerdrawCpuDefault"]))),
        (
            "powerdrawMem",
            text(&["powerdrawMem"]).unwrap_or_else(|| powerdraw_memory.to_string()),
        ),
        ("pue", pue_text),
        ("customCpuTdpFile", shown(custom)),
        ("provenanceFile", settings.provenance.text.clone()),
        ("reportFile", settings.report.text.clone()),
        ("summaryFile", settings.summary.text.clone()),
        ("traceFile", settings.trace.text.clone()),
    ];
    let outputs = [
        &settings.trace,
        &settings.summary,
        &settings.report,
        &settings.provenance,
    ];
    if !outputs.iter().any(|output| output.enabled) {
        tracing::warn!(
            "[nf-co2footprint] No output files are enabled - to enable, set `enabled: true` in \
             the sections `trace`, `summary`, `report` or `provenance`."
        );
    }
    Ok(settings)
}

/// The CPUs of a TDP table with the columns `name`, `tdp (W)` and `logicalCores`, with their
/// power per core.
fn cpu_table(path: &Path) -> Result<Vec<(String, f64)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("Cannot read the CPU TDP file {}: {error}", path.display()))?;
    let mut lines = text.lines();
    let header: Vec<String> = (lines.next().unwrap_or_default().split(','))
        .map(|name| name.trim().trim_matches('"').to_owned())
        .collect();
    let column = |name: &str| header.iter().position(|known| known == name);
    let (Some(name), Some(tdp), Some(cores)) =
        (column("name"), column("tdp (W)"), column("logicalCores"))
    else {
        return Err(format!(
            "The CPU TDP file {} needs the columns `name`, `tdp (W)` and `logicalCores`",
            path.display()
        ));
    };
    let mut rows = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let cells: Vec<&str> = line
            .split(',')
            .map(|cell| cell.trim().trim_matches('"'))
            .collect();
        let number = |index: usize| cells.get(index).and_then(|cell| cell.parse::<f64>().ok());
        if let (Some(model), Some(tdp), Some(cores)) = (cells.get(name), number(tdp), number(cores))
            && cores.trunc() > 0.0
        {
            rows.push(((*model).to_owned(), tdp / cores.trunc()));
        }
    }
    Ok(rows)
}

/// The launch stamp of default file names: the local date, then the milliseconds since local
/// midnight, like `20261106-37690408`.
fn stamp(millis: i64) -> String {
    let Ok(time) = jiff::Timestamp::from_millisecond(millis) else {
        return String::new();
    };
    let time = time.to_zoned(TimeZone::system());
    let since =
        (i64::from(time.hour()) * 3600 + i64::from(time.minute()) * 60 + i64::from(time.second()))
            * 1000
            + millis.rem_euclid(1000);
    format!("{}{since}", time.strftime("%Y%m%d-"))
}

#[cfg(test)]
mod tests {
    use nextflow_evaluator::interpreter::Host;

    use super::*;

    struct Quiet;

    impl Host for Quiet {
        fn print(&mut self, _: &str) {}
    }

    fn loaded(text: &str) -> Config {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("nextflow.config");
        std::fs::write(&file, text).expect("a config");
        let settings = nextflow_config::Options {
            profiles: None,
            params: nextflow_evaluator::value::Value::map([]),
            project_dir: "/project".into(),
            launch_dir: "/launch".into(),
            work_dir: "/work".into(),
        };
        nextflow_config::load(&[file], &settings, &mut Quiet).expect("loads")
    }

    /// A given `ci` wins over the live values of an API key.
    #[test]
    fn keeps_given_intensity() {
        let given = read(&loaded("co2footprint { ci = 100; emApiKey = 'key' }")).expect("settings");
        assert_eq!((given.intensity, given.token), (100.0, None));
        let live = read(&loaded("co2footprint { emApiKey = 'key' }")).expect("settings");
        assert_eq!(live.token.as_deref(), Some("key"));
    }

    #[test]
    fn stamps_like_nextflow() {
        let text = stamp(crate::report_format::now());
        let (date, millis) = text.split_once('-').expect("a stamp");
        assert_eq!(date.len(), 8);
        assert!(
            millis
                .parse::<u32>()
                .is_ok_and(|millis| millis < 86_400_000)
        );
    }
}

//! Every Nextflow plugin that uwe knows, with what uwe does for it, and the plugins of a config.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::observer::Observer;

/// What a plugin adds to Nextflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Functions, channel factories or operators that scripts include.
    Functions,
    /// Hears the events of a run, like report and notification plugins.
    Observer,
    /// Paths with a URI scheme, like `drs://`.
    FileSystem,
    Executor,
    Other,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Functions => "functions",
            Kind::Observer => "observer",
            Kind::FileSystem => "file system",
            Kind::Executor => "executor",
            Kind::Other => "other",
        }
    }
}

/// What uwe does for a plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Implemented,
    /// Some of its features work, the others are skipped or fail when used.
    Partial,
    /// uwe will implement it.
    Planned,
    /// uwe cannot implement it, for the reason given.
    NotImplementable(&'static str),
    /// uwe leaves it out, for the reason given, like a plugin without a release for a year.
    OutOfScope(&'static str),
}

/// Calls a function of a plugin by its name.
pub type Call = fn(&str, Vec<Value>, &mut Context) -> Result<Value, Error>;

/// Calls a method of an object of a plugin, `None` when the object has no such method.
pub type Methods = fn(&Value, &str, Vec<Value>, &mut Context) -> Result<Option<Value>, Error>;

/// Makes the run observer of a plugin from the config, `None` when the config turns it off.
pub type Activate = fn(&Config) -> Result<Option<Box<dyn Observer>>, String>;

/// A plugin, as the `plugins` block of a config and `include ... from 'plugin/<id>'` name it.
#[derive(Clone, Copy, Debug)]
pub struct Plugin {
    pub id: &'static str,
    pub kind: Kind,
    pub state: State,
    /// The functions that scripts can include from `plugin/<id>`.
    pub functions: &'static [&'static str],
    /// The functions that work, when not all of `functions` do.
    pub supported: Option<&'static [&'static str]>,
    /// Runs the functions that `script` does not define.
    pub call: Option<Call>,
    /// Runs the methods of the objects that its functions give.
    pub methods: Option<Methods>,
    /// Nextflow script that defines some of the functions.
    pub script: Option<&'static str>,
    /// The channel factories it adds, like `fromSamplesheet` for `Channel.fromSamplesheet`.
    pub factories: &'static [&'static str],
    /// The config scopes that it reads, like `prov`.
    pub scopes: &'static [&'static str],
    pub observer: Option<Activate>,
}

impl Plugin {
    /// A plugin with this state and nothing implemented, the start of every entry.
    pub const fn new(id: &'static str, kind: Kind, state: State) -> Plugin {
        Plugin {
            id,
            kind,
            state,
            functions: &[],
            supported: None,
            call: None,
            methods: None,
            script: None,
            factories: &[],
            scopes: &[],
            observer: None,
        }
    }

    /// Whether uwe runs some of its features.
    pub fn works(&self) -> bool {
        matches!(self.state, State::Implemented | State::Partial)
    }
}

const fn planned(id: &'static str, kind: Kind) -> Plugin {
    Plugin::new(id, kind, State::Planned)
}

const fn impossible(id: &'static str, kind: Kind, reason: &'static str) -> Plugin {
    Plugin::new(id, kind, State::NotImplementable(reason))
}

const fn unmaintained(id: &'static str, kind: Kind, reason: &'static str) -> Plugin {
    Plugin::new(id, kind, State::OutOfScope(reason))
}

const CLOSED: &str = "a closed service without a public specification";
const EXTENSION: &str = "a closed enterprise extension without a public specification";
const NEVER: &str = "never released";

/// The plugins of the Nextflow registry, read 2026-11-06. A plugin module replaces its
/// `planned(...)` line with its own `Plugin` value.
pub const PLUGINS: &[Plugin] = &[
    // Plugins of nf-core pipelines.
    crate::NF_SCHEMA_PLUGIN,
    crate::NF_VALIDATION_PLUGIN,
    crate::nf_core_utils::PLUGIN,
    crate::nf_fgbio::PLUGIN,
    crate::nf_anndata::PLUGIN,
    // Functions.
    crate::nf_bactopia::PLUGIN,
    crate::nf_bids::PLUGIN,
    crate::nf_boost::PLUGIN,
    crate::nf_cmgg::PLUGIN,
    crate::nf_dotenv::PLUGIN,
    crate::nf_ped::PLUGIN,
    crate::nf_pregex::PLUGIN,
    crate::nf_versions::PLUGIN,
    // Functions that read data, query services or run code in containers.
    planned("nf-crg-report", Kind::Functions),
    planned("nf-csvext", Kind::Functions),
    planned("nf-fl86", Kind::Functions),
    planned("nf-jev", Kind::Functions),
    planned("nf-labrobotics", Kind::Functions),
    planned("nf-parquet", Kind::Functions),
    planned("nf-python", Kind::Functions),
    planned("nf-r-ipc", Kind::Functions),
    planned("nf-report", Kind::Functions),
    planned("nf-seqlab-progress", Kind::Functions),
    planned("nf-sqldb", Kind::Functions),
    planned("nf-trino", Kind::Functions),
    // Observers that write local files.
    planned("nf-arts", Kind::Observer),
    crate::nf_bigbrother::PLUGIN,
    planned("nf-cgroup-metrics", Kind::Observer),
    crate::nf_co2footprint::PLUGIN,
    planned("nf-emoji", Kind::Observer),
    planned("nf-fairscape", Kind::Observer),
    planned("nf-flowviewer", Kind::Observer),
    planned("nf-gc", Kind::Observer),
    crate::nf_logsaver::PLUGIN,
    planned("nf-metalog", Kind::Observer),
    crate::nf_prov::PLUGIN,
    planned("nf-runinsights", Kind::Observer),
    crate::nf_snapshot::PLUGIN,
    crate::nf_storagemetrics::PLUGIN,
    // Observers that send to services.
    planned("nf-biffix", Kind::Observer),
    planned("nf-crg-genomics-lims", Kind::Observer),
    planned("nf-crukci-support", Kind::Observer),
    planned("nf-lamin", Kind::Observer),
    planned("nf-llm-debugger", Kind::Observer),
    planned("nf-prometheus", Kind::Observer),
    planned("nf-slack", Kind::Observer),
    planned("nf-teams", Kind::Observer),
    planned("nf-tower", Kind::Observer),
    planned("nf-weblog", Kind::Observer),
    // File systems.
    planned("nf-blocks", Kind::FileSystem),
    planned("nf-cloudcache", Kind::FileSystem),
    planned("nf-drs", Kind::FileSystem),
    planned("nf-irods", Kind::FileSystem),
    planned("nf-lakefs", Kind::FileSystem),
    planned("nf-quilt", Kind::FileSystem),
    planned("nf-synapse", Kind::FileSystem),
    // Pipeline sources and images.
    planned("nf-codecommit", Kind::Other),
    planned("nf-diff", Kind::Other),
    planned("nf-kuberun", Kind::Executor),
    planned("nf-wave", Kind::Other),
    // Cloud batch executors.
    planned("nf-amazon", Kind::Executor),
    planned("nf-azure", Kind::Executor),
    planned("nf-google", Kind::Executor),
    // Other executors.
    planned("nf-ccloud", Kind::Executor),
    planned("nf-flyte", Kind::Executor),
    planned("nf-fovus", Kind::Executor),
    planned("nf-ga4gh", Kind::Executor),
    planned("nf-gadi", Kind::Executor),
    planned("nf-iac", Kind::Executor),
    planned("nf-k8s", Kind::Executor),
    planned("nf-nomad", Kind::Executor),
    planned("nf-nomad-s5cmd", Kind::Executor),
    planned("nf-oci", Kind::Executor),
    planned("nf-ospool", Kind::Executor),
    planned("nf-rest", Kind::Executor),
    planned("nf-slurm-no-memory", Kind::Executor),
    planned("nf-snowflake", Kind::Executor),
    // The `agent` primitive.
    planned("nf-agent", Kind::Other),
    planned("nf-agent-pi", Kind::Other),
    // Not implementable.
    impossible(
        "nf-aspera",
        Kind::FileSystem,
        "it needs IBM's closed FASP protocol",
    ),
    impossible("nf-console", Kind::Other, "it is a Groovy GUI console"),
    impossible(
        "nf-itdepends",
        Kind::Functions,
        "it loads Java libraries through `@Grab`, and uwe has no JVM",
    ),
    impossible("nf-seqera", Kind::Executor, CLOSED),
    impossible("xpack-amzn", Kind::Other, EXTENSION),
    impossible("xpack-databricks", Kind::Other, EXTENSION),
    impossible("xpack-google", Kind::Other, EXTENSION),
    impossible("xpack-platform", Kind::Other, EXTENSION),
    impossible("yellowdog", Kind::Executor, CLOSED),
    // Unmaintained: no release in the year before 2026-11-06.
    unmaintained("nf-biofuse", Kind::FileSystem, NEVER),
    unmaintained(
        "nf-cachebrowser",
        Kind::FileSystem,
        "no release since 2025-05-25",
    ),
    unmaintained("nf-command-archiver", Kind::Observer, NEVER),
    unmaintained("nf-cws", Kind::Executor, "no release since 2025-05-28"),
    unmaintained(
        "nf-databricks",
        Kind::Executor,
        "no release since 2025-07-16",
    ),
    unmaintained(
        "nf-datatrail",
        Kind::Observer,
        "no release since 2025-04-11",
    ),
    unmaintained("nf-demo", Kind::Other, NEVER),
    unmaintained("nf-ffq", Kind::Functions, "no release since 2024-03-06"),
    unmaintained("nf-float", Kind::Executor, "no release since 2025-06-24"),
    unmaintained("nf-gpt", Kind::Functions, "no release since 2024-04-15"),
    unmaintained("nf-hello", Kind::Functions, "no release since 2024-03-06"),
    unmaintained("nf-ignite", Kind::Executor, "no release since 2021-05-28"),
    unmaintained(
        "nf-iridanext",
        Kind::Observer,
        "no release since 2025-02-17",
    ),
    unmaintained("nf-jarvice", Kind::Executor, "no release since 2023-05-09"),
    unmaintained("nf-mlflow", Kind::Observer, NEVER),
    unmaintained("nf-nextpie", Kind::Observer, "no release since 2025-06-29"),
    unmaintained("nf-nim", Kind::Functions, "no release since 2025-09-10"),
    unmaintained("nf-notion", Kind::Observer, NEVER),
    unmaintained(
        "nf-pgcache",
        Kind::FileSystem,
        "no release since 2025-01-28",
    ),
    unmaintained("nf-sample-sheet", Kind::Functions, NEVER),
    unmaintained("nf-skypilot", Kind::Executor, NEVER),
    unmaintained(
        "nf-tencentcloud",
        Kind::Executor,
        "no release since 2024-12-04",
    ),
    unmaintained("nf-wr", Kind::Executor, "no release since 2022-02-28"),
];

/// The plugin with this id, without a version like `@1.2.0`.
pub fn find(id: &str) -> Option<&'static Plugin> {
    PLUGINS.iter().find(|plugin| plugin.id == id)
}

/// What uwe does for a plugin, for messages, like "the plugin `nf-prov` is planned ...".
pub fn status(id: &str) -> String {
    let Some(plugin) = find(id) else {
        return format!("the plugin `{id}` is not known to uwe");
    };
    match plugin.state {
        State::Implemented => format!("the plugin `{id}` is implemented in uwe"),
        State::Partial => format!("the plugin `{id}` is partly implemented in uwe"),
        State::Planned => format!("the plugin `{id}` is planned in uwe but not implemented yet"),
        State::NotImplementable(reason) => {
            format!("the plugin `{id}` cannot be implemented in uwe ({reason})")
        }
        State::OutOfScope(reason) => {
            format!("the plugin `{id}` is not supported by uwe ({reason})")
        }
    }
}

/// The ids of the plugins that the `plugins` block of the config names, without versions.
pub fn configured(config: &Config) -> Vec<String> {
    let Some(Value::Set(plugins)) = config.get(&["plugins"]) else {
        return Vec::new();
    };
    let plugins = plugins.borrow();
    let mut ids: Vec<String> = (plugins.items.iter())
        .map(|item| {
            let text = item.0.to_string();
            text.split('@').next().unwrap_or_default().trim().to_owned()
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// One warning for each plugin of the config that uwe does not run, which a run skips.
pub fn warnings(config: &Config) -> Vec<String> {
    (configured(config).iter())
        .filter(|id| !find(id).is_some_and(Plugin::works))
        .map(|id| {
            let status = status(id);
            let status = status.strip_prefix("the ").unwrap_or(&status);
            format!("The {status}; its features are skipped")
        })
        .collect()
}

/// The run observers of the plugins of the config that have one and that the config turns on.
pub fn observers(config: &Config) -> Result<Vec<Box<dyn Observer>>, String> {
    let mut observers = Vec::new();
    for id in configured(config) {
        let activate = find(&id).and_then(|plugin| plugin.observer);
        if let Some(observer) = activate
            .map(|activate| activate(config))
            .transpose()?
            .flatten()
        {
            observers.push(observer);
        }
    }
    Ok(observers)
}

/// The catalogue as JSON, by plugin id: kind, state, the reason uwe leaves a plugin out, and
/// the functions that work and those that do not.
pub fn capabilities() -> serde_json::Value {
    let mut plugins = serde_json::Map::new();
    for plugin in PLUGINS {
        let (state, reason) = match plugin.state {
            State::Implemented => ("implemented", None),
            State::Partial => ("partial", None),
            State::Planned => ("planned", None),
            State::NotImplementable(reason) => ("not implementable", Some(reason)),
            State::OutOfScope(reason) => ("out of scope", Some(reason)),
        };
        let supported = plugin.supported.unwrap_or(plugin.functions);
        let unavailable: Vec<&str> = (plugin.functions.iter())
            .filter(|name| !supported.contains(name))
            .copied()
            .collect();
        let mut entry = serde_json::json!({
            "kind": plugin.kind.name(),
            "state": state,
            "supported": supported,
            "unavailable": unavailable,
        });
        if let Some(reason) = reason {
            entry["reason"] = reason.into();
        }
        if !plugin.scopes.is_empty() {
            entry["scopes"] = plugin.scopes.into();
        }
        plugins.insert(plugin.id.to_owned(), entry);
    }
    serde_json::Value::Object(plugins)
}

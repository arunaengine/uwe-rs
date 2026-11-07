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
    /// uwe will implement it later, for the reason given.
    Planned(&'static str),
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

const fn planned(id: &'static str, kind: Kind, reason: &'static str) -> Plugin {
    Plugin::new(id, kind, State::Planned(reason))
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
    crate::nf_crg_report::PLUGIN,
    crate::nf_csvext::PLUGIN,
    crate::nf_databricks::PLUGIN,
    // nf-fl86: writes pipeline_info only to a local output folder; a remote one fails the run.
    crate::nf_fl86::PLUGIN,
    crate::nf_gpt::PLUGIN,
    crate::nf_jev::PLUGIN,
    crate::nf_labrobotics::PLUGIN,
    crate::nf_parquet::PLUGIN,
    crate::nf_python::PLUGIN,
    crate::nf_r_ipc::PLUGIN,
    crate::nf_report::PLUGIN,
    crate::nf_seqlab_progress::PLUGIN,
    crate::nf_sqldb::PLUGIN,
    crate::nf_trino::PLUGIN,
    // Observers that write local files.
    crate::nf_arts::PLUGIN,
    crate::nf_bigbrother::PLUGIN,
    crate::nf_cgroup_metrics::PLUGIN,
    crate::nf_co2footprint::PLUGIN,
    crate::nf_datatrail::PLUGIN,
    crate::nf_emoji::PLUGIN,
    crate::nf_fairscape::PLUGIN,
    crate::nf_flowviewer::PLUGIN,
    crate::nf_gc::PLUGIN,
    // nf-iridanext: writes its output file and reads sample metadata only on local paths; a
    // remote one fails the run.
    crate::nf_iridanext::PLUGIN,
    crate::nf_logsaver::PLUGIN,
    crate::nf_metalog::PLUGIN,
    crate::nf_prov::PLUGIN,
    crate::nf_runinsights::PLUGIN,
    crate::nf_snapshot::PLUGIN,
    // nf-storagemetrics: measures only the folders of tasks with output files, as uwe gives
    // observers no work folder of a task.
    crate::nf_storagemetrics::PLUGIN,
    // Observers that send to services.
    crate::nf_biffix::PLUGIN,
    crate::crg_lims::PLUGIN,
    crate::nf_crukci_support::PLUGIN,
    crate::nf_lamin::PLUGIN,
    crate::nf_llm_debugger::PLUGIN,
    crate::nf_nextpie::PLUGIN,
    crate::nf_prometheus::PLUGIN,
    crate::nf_slack::PLUGIN,
    crate::nf_teams::PLUGIN,
    crate::nf_tower::PLUGIN,
    crate::nf_weblog::PLUGIN,
    // File systems.
    crate::nf_blocks::PLUGIN,
    crate::nf_cloudcache::PLUGIN,
    crate::nf_drs::PLUGIN,
    crate::nf_irods::PLUGIN,
    crate::nf_oci::PLUGIN,
    crate::nf_lakefs::PLUGIN,
    crate::nf_quilt::PLUGIN,
    crate::nf_synapse::PLUGIN,
    // Pipeline sources and images.
    // nf-codecommit: Git over HTTPS with SigV4 passwords, keys from an https storage provider
    // or AWS's chain; no Nextflow SCM file and no CodeCommit API reads (uwe clones).
    Plugin::new("nf-codecommit", Kind::Other, State::Partial),
    // nf-diff: `uwe compare -report` over uwe's own run records; no log, DAG, published-file
    // or configuration layers, and no archive.
    Plugin::new("nf-diff", Kind::Other, State::Partial),
    // nf-kuberun: `uwe kuberun` runs uwe in a pod with its own service account; no `login`
    // shell, no Job instead of the pod, and only the run options that uwe has.
    Plugin::new("nf-kuberun", Kind::Executor, State::Partial),
    // nf-wave: Wave images for the `container` and `conda` directives when a Wave-only
    // setting or an endpoint asks; no module bundles, layers, Singularity, Fusion or commands.
    crate::nf_wave::PLUGIN,
    // Cloud batch executors. nf-amazon: S3, SES mail and the `aws-batch` executor; no Fusion,
    // array jobs or machine details. nf-google: GCS and the `google-batch` executor; no Fusion,
    // array jobs or machine types per process.
    Plugin::new("nf-amazon", Kind::Executor, State::Partial),
    // nf-azure: Blob Storage and the `azure-batch` executor; no Fusion, file shares or VM size
    // patterns.
    Plugin::new("nf-azure", Kind::Executor, State::Partial),
    Plugin::new("nf-google", Kind::Executor, State::Partial),
    // Other executors.
    // nf-cws: the `cws` scope on uwe's Kubernetes executor, which registers the run, its
    // processes and tasks; no graph edges, batches, memory prediction, WOW or scheduler pod.
    Plugin {
        scopes: &["cws"],
        ..Plugin::new("nf-cws", Kind::Executor, State::Partial)
    },
    // nf-flyte: the `flyte` executor, one Flyte run per attempt with uwe's staging; no child
    // actions of a running Flyte run, token command or Flyte caching.
    Plugin {
        scopes: &["flyte"],
        ..Plugin::new("nf-flyte", Kind::Executor, State::Partial)
    },
    // nf-ga4gh: the `tes` scope on uwe's TES executor, which counts memory in GB of 10^9 bytes
    // as the TES specification does; its `drs://` paths are those of nf-drs.
    Plugin {
        scopes: &["tes"],
        ..Plugin::new("nf-ga4gh", Kind::Executor, State::Partial)
    },
    // nf-gadi: its usage report from the job summaries that uwe's PBS executor reads;
    // `publishTo` only to a local path, and no start and end lines.
    crate::nf_gadi::PLUGIN,
    // nf-jarvice: the `jarvice` executor with nf-jarvice's process settings, on the vault or
    // through a staging root; no banner and no copy of the `NXF_*` variables into the jobs.
    Plugin::new("nf-jarvice", Kind::Executor, State::Implemented),
    // nf-k8s: the `k8s` scope and `pod` options on uwe's Kubernetes executor; no bare pods,
    // `k8s.client` or debug YAML, and no privileged pods.
    Plugin {
        scopes: &["k8s"],
        ..Plugin::new("nf-k8s", Kind::Executor, State::Partial)
    },
    // nf-nomad: the `nomad` executor; no secrets provider, constraint closures, debug JSON or
    // HPC drivers, and no privileged containers.
    Plugin {
        scopes: &["nomad"],
        ..Plugin::new("nf-nomad", Kind::Executor, State::Partial)
    },
    // nf-nomad-s5cmd: its work dir and S3 settings map onto uwe's object-store staging; uwe
    // runs no s5cmd.
    Plugin::new("nf-nomad-s5cmd", Kind::Executor, State::Partial),
    // nf-ospool: `ospool` and `condor` on uwe's HTCondor executor, which stages files itself
    // and ignores the staging settings of `executor.$ospool`.
    Plugin::new("nf-ospool", Kind::Executor, State::Partial),
    // nf-slurm-no-memory: `slurm_no_memory` is uwe's SLURM executor with `memory_per_cpu_gb`.
    Plugin::new("nf-slurm-no-memory", Kind::Executor, State::Implemented),
    // nf-wr: the `wr` executor over the manager's REST API with its token and CA, on shared
    // folders or through a staging root; uwe checks the host name of the certificate.
    Plugin::new("nf-wr", Kind::Executor, State::Implemented),
    // nf-tencentcloud: its executor on uwe's TES executor, `cos://` on an S3 provider, and its
    // metadata file; the shape of that file was not recorded.
    crate::nf_tencentcloud::PLUGIN,
    // nf-snowflake: the `snowflake` executor with object-store staging; no `snowflake://` stage
    // file system, trace upload to a stage or cache store on a stage.
    Plugin {
        scopes: &["snowflake"],
        ..Plugin::new("nf-snowflake", Kind::Executor, State::Partial)
    },
    // nf-agent: the `agent` primitive of Nextflow 26.10, a preview; one chat completions
    // request per task, for text or structured output, without tools or skills.
    Plugin {
        scopes: &["agent"],
        ..Plugin::new("nf-agent", Kind::Other, State::Partial)
    },
    planned(
        "nf-agent-pi",
        Kind::Other,
        "deferred until the Nextflow agent feature leaves preview; needs a container talking gRPC",
    ),
    // Not implementable.
    impossible(
        "nf-aspera",
        Kind::FileSystem,
        "it needs IBM's closed FASP protocol",
    ),
    impossible(
        "nf-ccloud",
        Kind::Executor,
        "closed source, its task protocol is not documented",
    ),
    impossible("nf-console", Kind::Other, "it is a Groovy GUI console"),
    impossible(
        "nf-float",
        Kind::Executor,
        "only MemVerge's command line tool, no public API",
    ),
    impossible(
        "nf-fovus",
        Kind::Executor,
        "only a command line tool, no public API",
    ),
    impossible(
        "nf-ignite",
        Kind::Executor,
        "it needs Java worker processes, and `nextflow node` is gone",
    ),
    impossible(
        "nf-itdepends",
        Kind::Functions,
        "it loads Java libraries through `@Grab`, and uwe has no JVM",
    ),
    impossible(
        "nf-pgcache",
        Kind::FileSystem,
        "it stores Nextflow's binary cache records, which uwe cannot share; uwe has its own cache",
    ),
    impossible("nf-seqera", Kind::Executor, CLOSED),
    impossible("xpack-amzn", Kind::Other, EXTENSION),
    impossible("xpack-databricks", Kind::Other, EXTENSION),
    impossible("xpack-google", Kind::Other, EXTENSION),
    impossible("xpack-platform", Kind::Other, EXTENSION),
    impossible("yellowdog", Kind::Executor, CLOSED),
    // Not re-created, by decision.
    Plugin::new(
        "nf-cachebrowser",
        Kind::FileSystem,
        State::OutOfScope("a viewer of Nextflow's own cache files; only a release candidate"),
    ),
    Plugin::new(
        "nf-ffq",
        Kind::Functions,
        State::OutOfScope("the service it needs (ffq-api) is archived"),
    ),
    Plugin::new(
        "nf-iac",
        Kind::Executor,
        State::OutOfScope(
            "uwe builds no Oracle Cloud machine per task; its Terraform code is not public",
        ),
    ),
    Plugin::new(
        "nf-nim",
        Kind::Functions,
        State::OutOfScope("it ignores the task's inputs and always sends the same downloaded file"),
    ),
    Plugin::new(
        "nf-rest",
        Kind::Executor,
        State::OutOfScope("deactivated by its author (\"move to nf-iac\")"),
    ),
    // Unmaintained: no release in the year before 2026-11-06.
    unmaintained("nf-biofuse", Kind::FileSystem, NEVER),
    unmaintained("nf-command-archiver", Kind::Observer, NEVER),
    unmaintained("nf-demo", Kind::Other, NEVER),
    unmaintained("nf-hello", Kind::Functions, "no release since 2024-03-06"),
    unmaintained("nf-mlflow", Kind::Observer, NEVER),
    unmaintained("nf-notion", Kind::Observer, NEVER),
    unmaintained("nf-sample-sheet", Kind::Functions, NEVER),
    unmaintained("nf-skypilot", Kind::Executor, NEVER),
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
        State::Planned(reason) => {
            format!("the plugin `{id}` is planned in uwe but not implemented yet ({reason})")
        }
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
    // Nextflow loads nf-tower without a `plugins` line when the config turns it on.
    if !configured(config).iter().any(|id| id == "nf-tower")
        && let Some(observer) = crate::nf_tower::implied(config)?
    {
        observers.push(observer);
    }
    Ok(observers)
}

/// The catalogue as JSON, by plugin id: kind, state, the reason for a plugin that uwe does not
/// run, and the functions that work and those that do not.
pub fn capabilities() -> serde_json::Value {
    let mut plugins = serde_json::Map::new();
    for plugin in PLUGINS {
        let (state, reason) = match plugin.state {
            State::Implemented => ("implemented", None),
            State::Partial => ("partial", None),
            State::Planned(reason) => ("planned", Some(reason)),
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

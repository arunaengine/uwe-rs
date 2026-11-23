//! The command line of uwe: the options of each command, how they are read, the groups of
//! commands and their help.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod groups;
mod help;
mod options;
mod parse;

use std::net::IpAddr;
use std::path::PathBuf;

use workflow_graph::render::{Direction, View};
use workflow_graph::{Language, Order};

pub use groups::{group_help, ungroup};
pub(crate) use groups::{subcommands, typed_commands};
pub use help::help;
pub(crate) use options::options_of;
pub(crate) use parse::language;
pub use parse::{parse, parse_command};

/// The options of a command.
#[derive(Clone, Default, PartialEq)]
pub struct Arguments {
    pub help: bool,
    pub version: bool,
    pub verbose: bool,
    pub quiet: bool,
    /// The workflow file or project directory.
    pub target: Option<PathBuf>,
    /// `-r`: the repository tag, branch or full commit id.
    pub revision: Option<String>,
    /// `-latest`: resolve a cached repository reference again.
    pub latest: bool,
    /// `-lang name`; else the language is found from the target.
    pub language: Option<Language>,
    pub stub: bool,
    /// `-keep-going`: after a task failed, run every task that does not depend on it.
    pub keep_going: bool,
    /// `-store folder|address`: where the cache store lives; a folder is made absolute.
    pub store: Option<String>,
    /// `-uwe-config file`: a config file of uwe, read after the site-wide file and
    /// the user's file.
    pub config_file: Option<PathBuf>,
    /// `-profile a,b`
    pub profiles: Option<Vec<String>>,
    /// `-preset name`: a configured workflow and its reference inputs.
    pub preset: Option<String>,
    /// `-c file`, `-configfile file`
    pub configs: Vec<PathBuf>,
    /// `--name value` and `--config name=value`, in the order given.
    pub params: Vec<(String, String)>,
    /// `-inputs file` or `-params-file file`: a CWL job file or a params file.
    pub inputs: Option<PathBuf>,
    /// `-entry name` and the arguments after the target, like Snakemake targets.
    pub targets: Vec<String>,
    /// `-until`: Snakemake rules or files to run up to.
    pub until: Vec<String>,
    /// `-omit-from`: Snakemake rules or files to omit with their downstream jobs.
    pub omit_from: Vec<String>,
    /// `-forcerun`: Snakemake rules or files to rerun with their downstream jobs.
    pub forcerun: Vec<String>,
    /// `-forceall`: rerun every job needed for the Snakemake targets.
    pub forceall: bool,
    /// `-rerun-triggers`: Snakemake checks that can require existing outputs to run again.
    pub rerun_triggers: Option<Vec<String>>,
    /// `-executor simulator|local|tes|slurm|kubernetes|aws-batch|google-batch|azure-batch|nomad`,
    /// `htcondor`, `pbs`, `flyte` or `snowflake`; `None` uses config, else local for runs.
    pub executor: Option<String>,
    /// Whether `executor` is `default` of `[executor]` or `local`, which a Nextflow pipeline's
    /// `process.executor` may override, rather than `-executor` or a preset.
    pub executor_default: bool,
    /// `-graph file`: where to write the execution graph.
    pub graph: Option<PathBuf>,
    /// `-report file`: where `uwe run` writes the report of the run when it ends.
    pub report: Option<PathBuf>,
    /// `-output-dir folder`: where results go that the workflow gives no place of its own.
    pub output_dir: Option<PathBuf>,
    /// `-workdir folder`: the folder of a Snakemake project folder that the run works in, like
    /// Snakemake's `--directory`, in a copy of it so that the run writes nothing into the project.
    pub workdir: Option<PathBuf>,
    /// `-tes-server address`: the TES server of `-executor tes`.
    pub tes_server: Option<String>,
    /// `-slurm-container apptainer|singularity|podman|docker`: how the nodes of
    /// `-executor slurm` run containers.
    pub slurm_container: Option<String>,
    /// `-slurm-options 'options'`: more options of `sbatch`, like `--account=lab`.
    pub slurm_options: Option<String>,
    /// `-kubeconfig file`: the kubeconfig of the cluster of `-executor kubernetes`.
    pub kubeconfig: Option<PathBuf>,
    /// `-kubernetes-namespace name`: the namespace of the Jobs of `-executor kubernetes`.
    pub kubernetes_namespace: Option<String>,
    /// `-head-image image`: the image with uwe that `uwe kuberun` runs in its pod.
    pub head_image: Option<String>,
    /// `-save folder` of `uwe tool extract`: where it writes each tool as a definition file.
    pub save: Option<PathBuf>,
    /// `-name name` and `-image image` of `uwe record`: the tool file and its container.
    pub tool_name: Option<String>,
    pub tool_image: Option<String>,
    /// The words after `--` of `uwe record`: the command it runs and records.
    pub command_line: Vec<String>,
    /// `-head-cpus n` and `-head-memory size`: what the pod of `uwe kuberun` asks for.
    pub head_cpus: Option<String>,
    pub head_memory: Option<String>,
    /// `-volume-mount claim:path,...`: the volume claims of the pod of `uwe kuberun`.
    pub volume_mounts: Vec<String>,
    /// `-background`: `uwe kuberun` ends once the pod has started.
    pub background: bool,
    /// `-fail-on-change`: `uwe compare` ends with exit code 3 when the runs differ.
    pub fail_on_change: bool,
    /// `-format name`: `json`, `dot`, `summary` or `svg` for `uwe graph`; `json`, `text` or
    /// `form` for `uwe inputs`.
    pub format: Option<String>,
    /// `-check`: `uwe inputs` also checks the values of the launch against the schema.
    pub check: bool,
    /// `-view tasks|steps`: the graph with one node per task, or one node per step.
    pub view: Option<View>,
    /// `-direction down|right`: which way a picture of the graph goes.
    pub direction: Option<Direction>,
    /// `-fold n`: in a picture of the graph of tasks, a step with more than n tasks is one
    /// box; 0 draws every task.
    pub fold: Option<usize>,
    /// `-max-pending n`: how many tasks may be pending at once.
    pub max_pending: Option<usize>,
    pub machine_retries: Option<u32>,
    pub retry_delay_ms: Option<u64>,
    /// `-task-minutes n` or `[executor] task_minutes`, in seconds: the time limit of tasks
    /// that set none of their own.
    pub task_seconds: Option<u64>,
    /// Keeps standard output free for an embedding host, without `run::Settings.print`.
    pub no_print: bool,
    /// `-order asked|reversed|finished|shuffled[:seed]`: the order in which pending tasks
    /// finish.
    pub order: Option<Order>,
    /// `-default-image image`: the container image of tasks that name none, for real executors.
    pub default_image: Option<String>,
    /// `-python-image image`: the image of the Python container of decision 19, with the
    /// Python packages that the workflow imports.
    pub python_image: Option<String>,
    /// `-database file`: the run database, instead of the one in the cache directory.
    pub database: Option<PathBuf>,
    /// `-output folder|file`: where `uwe export` writes the crate, `uwe report` the
    /// report, and some other commands their file.
    pub output: Option<PathBuf>,
    /// `-platform name`: the conda platform of a lock, like `linux-64`.
    pub platform: Option<String>,
    /// `-license url`: the license of an exported crate.
    pub license: Option<String>,
    /// `-resume [run]`: continue a run, by default the last run of the workflow.
    pub resume: Option<Resume>,
    pub reason: Option<String>,
    /// `-page`: serve the web page of the run while it goes on.
    pub page: bool,
    /// `-address ip`: where the web page listens; only this machine by default.
    pub address: Option<IpAddr>,
    /// `-port number`: the port of the web page; a free one by default.
    pub port: Option<u16>,
    /// `-keep-days n`: `uwe clean` keeps what runs made or used in the last n days.
    pub keep_days: Option<u64>,
    /// `-keep-run run`, as often as needed: `uwe clean` keeps what the run needs.
    pub keep_runs: Vec<String>,
    /// `-keep-slower-than time`, like `10m`: `uwe clean` keeps the results of tasks that
    /// ran at least this long.
    pub keep_slower_than: Option<std::time::Duration>,
    /// `-remove`: `uwe clean` removes what it lists, instead of only listing it.
    pub remove: bool,
    pub front: bool,
    /// `-check-reproducibility percent`, like `10%`: the share of the tasks whose results come
    /// from the cache that run again, to check that their tools give the same bytes; from 0 to 1.
    pub check_reproducibility: Option<f64>,
    /// `-no-cache`: the run takes no result from the cache and keeps none there; every task
    /// runs.
    pub no_cache: bool,
    /// `-no-cache-steps a,b`: steps whose tasks take no result from the cache and keep none
    /// there, like Nextflow's `cache false`.
    pub no_cache_steps: Vec<String>,
    pub strict_provenance: bool,
    /// `-no-network`: tasks whose workflow does not say that they need the network run
    /// without it.
    pub no_network: bool,
    /// `-no-right-sizing`: tasks ask for the CPUs and memory that their workflow names, not
    /// for what the tasks of their step used in earlier runs.
    pub no_right_sizing: bool,
    /// `-no-grouping`: each task runs in a container of its own, also the short tasks that
    /// would run together.
    pub no_grouping: bool,
    /// The secret values of an embedded run; the command line reads the environment.
    pub secrets: config_file::SecretValues,
    /// The input values of a compiled run that hold a secret value, which reach the wir driver
    /// in memory.
    pub secret_inputs: wir_runtime::SecretInputs,
    /// The run whose launch folder a resumed compiled run starts in, so that its jobs find the
    /// files of the document they continue.
    pub launch_run: Option<String>,
    /// Whether a driver only plans or describes the workflow, so that the code of a Snakefile
    /// writes no files.
    pub planning: bool,
    /// Whether a host runs this through the Rust API, not the command line.
    pub embedded: bool,
    /// Where an embedded run sends what its workflow prints, from `run::Settings.print_sink`.
    pub print_sink: Option<crate::run::PrintSink>,
    /// The static Linux binary of the CWL JavaScript helper, from
    /// `run::Settings.javascript_binary`.
    pub javascript_binary: Option<PathBuf>,
    /// The backend of an embedding host that runs the tasks, from `run::Settings.host_executor`.
    pub host_executor: Option<crate::run::HostExecutor>,
    /// The run store of an embedding host, from `run::Settings.run_store`.
    pub run_store: Option<crate::run::HostRuns>,
    /// The cache namespace of the tenant of a host, from `run::Settings.cache_namespace`.
    pub cache_namespace: Option<String>,
    /// `-grace <time>`: `uwe clean -remove` of a shared store keeps what was written this
    /// recently, at least an hour.
    pub grace: Option<std::time::Duration>,
    /// The cache folder of the run, from `run::Settings.cache_folder` or `[cache] folder`.
    pub cache_folder: Option<PathBuf>,
    /// The folder of the journals of containers, from `run::Settings.journal_folder`.
    pub journal_folder: Option<PathBuf>,
    /// `-json`: inspection commands write JSON instead of text.
    pub json: bool,
    /// `-tools folder`, as often as needed: the tools of the library of `uwe tool`, `uwe serve`
    /// and `uwe page`.
    pub tools: Vec<PathBuf>,
    /// `-workflows folder`, as often as needed: the workflows that the launcher lists.
    pub workflows: Vec<PathBuf>,
    /// `-token token`: the access token of the page, instead of `[page] token`.
    pub token: Option<String>,
    /// `-open`: the browser opens the page of `uwe serve`.
    pub open: bool,
}

/// Shows what the command runs, with the names of its parameters but never their values, the
/// token of the page or other secrets.
impl std::fmt::Debug for Arguments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = (self.params.iter())
            .map(|(name, _)| name.as_str())
            .collect();
        formatter
            .debug_struct("Arguments")
            .field("target", &self.target)
            .field("language", &self.language)
            .field("preset", &self.preset)
            .field("executor", &self.executor)
            .field("profiles", &self.profiles)
            .field("params", &names)
            .field("inputs", &self.inputs)
            .field("targets", &self.targets)
            .field("output_dir", &self.output_dir)
            .field("resume", &self.resume)
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .finish_non_exhaustive()
    }
}

/// Which run `-resume` continues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resume {
    /// The last run of the same workflow.
    Last,
    /// The run with this id.
    Run(String),
}

impl Arguments {
    /// Converts CLI execution options while keeping its input syntax and presentation private.
    pub(crate) fn run_settings(&self) -> crate::run::Settings {
        crate::run::Settings {
            target: self.target.clone().unwrap_or_default(),
            language: self.language,
            profiles: self.profiles.clone().unwrap_or_default(),
            configs: self.configs.clone(),
            config_file: self.config_file.clone(),
            executor: self.executor.clone(),
            resume: self.resume.clone(),
            cache: !self.no_cache,
            no_cache_steps: self.no_cache_steps.clone(),
            strict_provenance: self.strict_provenance,
            targets: self.targets.clone(),
            until: self.until.clone(),
            omit_from: self.omit_from.clone(),
            forcerun: self.forcerun.clone(),
            forceall: self.forceall,
            default_image: self.default_image.clone(),
            max_pending: self.max_pending,
            retries: self.machine_retries,
            retry_delay: self.retry_delay_ms.map(std::time::Duration::from_millis),
            default_time: self.task_seconds.map(std::time::Duration::from_secs),
            output_dir: self.output_dir.clone(),
            workdir: self.workdir.clone(),
            database: self.database.clone(),
            store: self.store.clone(),
            print: true,
            cli: Some(self.clone()),
            ..crate::run::Settings::default()
        }
    }
}

#[cfg(test)]
mod tests;

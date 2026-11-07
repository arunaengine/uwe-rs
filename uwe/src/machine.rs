//! Reports configured machine and executor prerequisites for real runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "machine_local.rs"]
mod local;

use std::fmt;

use crate::Failure;
use crate::arguments::Arguments;
use crate::failure::{SETUP, without_error_numbers};

/// How a part of the machine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Ready,
    /// Missing or broken: a run that needs it stops.
    Missing,
    /// Not set up, and not needed unless a run asks for it.
    NotSetUp,
}

impl fmt::Display for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            State::Ready => "ready",
            State::Missing => "missing",
            State::NotSetUp => "not set up",
        })
    }
}

/// One part of the machine and what the check found.
struct Found {
    state: State,
    part: String,
    text: String,
}

/// What the check found, in order.
#[derive(Default)]
pub(super) struct Report {
    found: Vec<Found>,
}

impl Report {
    pub(super) fn add(&mut self, state: State, part: impl Into<String>, text: impl Into<String>) {
        self.found.push(Found {
            state,
            part: part.into(),
            text: without_error_numbers(&text.into()),
        });
    }

    fn text(&self) -> String {
        let mut text = String::new();
        for found in &self.found {
            text.push_str(&format!(
                "{:<11}{}: {}\n",
                found.state.to_string(),
                found.part,
                found.text
            ));
        }
        let missing = (self.found.iter())
            .filter(|found| found.state == State::Missing)
            .count();
        text.push_str(&match missing {
            0 => "This machine is ready for real runs.\n".to_owned(),
            1 => "1 thing is missing for real runs; the line that starts with `missing` says what \
                  to do.\n"
                .to_owned(),
            count => format!(
                "{count} things are missing for real runs; the lines that start with `missing` \
                 say what to do.\n"
            ),
        });
        text
    }
}

/// `uwe check-machine`: prints what is ready and what is missing, and exits with 3 when
/// something is missing.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let mut report = Report::default();
    let setup = config::config(arguments, &mut report);
    let _registry = setup.registry.enter().map_err(Failure::setup)?;
    let runtime = workflow_graph::runtime().map_err(|error| Failure::setup(error.to_string()))?;
    local::engine(runtime, &setup, arguments, &mut report);
    local::store(runtime, &setup, arguments, &mut report);
    local::database(arguments, &mut report);
    executors::tes_server(&setup.executors, arguments, &mut report);
    executors::slurm(&setup, arguments, &mut report);
    executors::kubernetes(runtime, &setup.executors, arguments, &mut report);
    executors::newer(&setup, arguments, &mut report);
    print!("{}", report.text());
    match report
        .found
        .iter()
        .any(|found| found.state == State::Missing)
    {
        true => Err(Failure::said(SETUP)),
        false => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_missing_requirements() {
        let mut report = Report::default();
        report.add(State::Ready, "store", "/s, 1.0 GB free");
        report.add(State::NotSetUp, "executor tes", "no TES server named");
        assert_eq!(
            report.text(),
            "ready      store: /s, 1.0 GB free\nnot set up executor tes: no TES server \
             named\nThis machine is ready for real runs.\n"
        );
        report.add(
            State::Missing,
            "container engine",
            "no container engine answers",
        );
        assert!(report.text().ends_with("1 thing is missing for real runs; the line that starts with `missing` says what to do.\n"));
    }
}

mod config {
    //! The config files of the machine, their storage providers, and the secrets that they take
    //! from environment variables.
    // Copyright (c) 2026 The uwe Contributors
    // SPDX-License-Identifier: MIT or Apache-2.0

    use config_file::{ConfigFile, Table};

    use super::{Report, State};
    use crate::arguments::Arguments;
    use crate::setup::Setup;

    /// The config files, their storage providers, and the secrets that they take from environment
    /// variables. A config file with a mistake counts as missing, and the rest of the check goes
    /// on without it.
    pub(super) fn config(arguments: &Arguments, report: &mut Report) -> Setup {
        let part = "config file";
        let config = match ConfigFile::load(arguments.config_file.as_deref()) {
            Ok(config) => config,
            Err(error) => {
                report.add(
                    State::Missing,
                    part,
                    format!("{error}; fix it or remove it"),
                );
                return Setup::default();
            }
        };
        let setup = match Setup::load(arguments) {
            Ok(setup) => setup,
            Err(failure) => {
                let message = failure.message.trim_start_matches("uwe: ");
                report.add(State::Missing, part, format!("{message}; fix it"));
                return Setup::default();
            }
        };
        let files: Vec<String> = (config.files().iter())
            .map(|file| file.display().to_string())
            .collect();
        if files.is_empty() {
            let user = config_file::user_file()
                .map_or("~/.config/uwe/config.toml".to_owned(), |file| {
                    file.display().to_string()
                });
            report.add(
                State::NotSetUp,
                part,
                format!(
                    "none, so public files need no provider and the store is a folder in the \
                     cache; {user} can name storage providers and executors"
                ),
            );
            return setup;
        }
        let providers = providers(&config);
        report.add(
            State::Ready,
            part,
            format!("{}; {providers}", files.join(", ")),
        );
        for (origin, variable) in secrets(config.root()) {
            match std::env::var(&variable) {
                Ok(value) if !value.is_empty() => {}
                _ => report.add(
                    State::Missing,
                    "secret",
                    format!(
                        "{origin}: the environment variable {variable}, which holds this secret, is \
                         not set; set it before a run"
                    ),
                ),
            }
        }
        setup
    }

    /// The storage providers of the config file in words, like `storage providers lab (s3) and
    /// portal (https)`.
    fn providers(config: &ConfigFile) -> String {
        let tables = (config.section("storage").ok().flatten())
            .and_then(|section| section.tables().ok())
            .unwrap_or_default();
        let names: Vec<String> = tables
            .iter()
            .map(|table| {
                let name = table.name().trim_start_matches("storage.");
                match table.text("type").ok().flatten() {
                    Some(kind) => format!("{name} ({kind})"),
                    None => name.to_owned(),
                }
            })
            .collect();
        match names.split_last() {
            None => "no storage providers".to_owned(),
            Some((one, [])) => format!("storage provider {one}"),
            Some((last, others)) => format!("storage providers {} and {last}", others.join(", ")),
        }
    }

    /// Every secret of a table and the tables in it that an environment variable holds, with where
    /// it was written: each value `{ environment = "NAME" }`.
    fn secrets(table: Table<'_>) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for key in table.keys() {
            let Ok(Some(inner)) = table.table(key) else {
                continue;
            };
            if inner.keys().eq(["environment"]) {
                if let Ok(Some(secret)) = table.secret(key)
                    && let Some(variable) = secret.variable()
                {
                    found.push((secret.origin().to_string(), variable.to_owned()));
                }
            } else {
                found.extend(secrets(inner));
            }
        }
        found
    }

    #[cfg(test)]
    mod tests {
        use std::path::Path;

        use super::*;

        #[test]
        fn finds_configured_secrets() {
            let text = "[storage.lab]\ntype = \"s3\"\nprefixes = [\"s3://lab/\"]\nsecret_access_key = \
                        { environment = \"LAB_SECRET\" }\n\n[storage.portal]\ntype = \"https\"\n\
                        prefixes = [\"https://x/\"]\nheaders = { X-Key = { environment = \"KEY\" } \
                        }\n";
            let config =
                ConfigFile::parse(text, Path::new("config.toml")).expect("a valid config file");
            let found = secrets(config.root());
            assert_eq!(
                found,
                [
                    ("config.toml:4".to_owned(), "LAB_SECRET".to_owned()),
                    ("config.toml:9".to_owned(), "KEY".to_owned())
                ]
            );
        }
    }
}

mod executors {
    //! The executors that the config file or the command line sets up: the TES server, the tools
    //! of SLURM, HTCondor and PBS, the kubeconfig of a Kubernetes cluster, and the cloud services.
    // Copyright (c) 2026 The uwe Contributors
    // SPDX-License-Identifier: MIT or Apache-2.0

    use std::path::Path;

    use super::{Report, State};
    use crate::arguments::Arguments;
    use crate::setup::{Executors, Setup};
    use crate::tes;

    /// The TES server that `-tes-server` or the config file names, or the default one with
    /// `-executor tes`, asked once whether it answers.
    pub(super) fn tes_server(executors: &Executors, arguments: &Arguments, report: &mut Report) {
        let part = "executor tes";
        let asked = arguments.executor.as_deref() == Some("tes");
        let server = (arguments.tes_server.clone())
            .or_else(|| executors.tes_server.clone())
            .or_else(|| asked.then(|| tes::DEFAULT_SERVER.to_owned()));
        let Some(server) = server else {
            report.add(
                State::NotSetUp,
                part,
                "no TES server named; name one with -tes-server or `server` in [executor.tes]",
            );
            return;
        };
        let token = match arguments.secrets.get("UWE_TES_TOKEN") {
            Some(token) => Some(token),
            None => (executors.tes_token.as_ref()).and_then(|secret| secret.reveal().ok()),
        };
        let answer = executors
            .tes
            .client(&server, token)
            .and_then(|client| tes::answers(&client, &server));
        match answer {
            Ok(()) => report.add(
                State::Ready,
                part,
                format!("the TES server at {server} answers"),
            ),
            Err(problem) => report.add(State::Missing, part, problem),
        }
    }

    /// The tools of SLURM, which the SLURM executor runs on this machine, and the container
    /// runtime of its jobs, which the nodes run. uwe cannot look at the nodes, so it looks for
    /// the runtime here, and names the folders that the nodes must see at the same paths.
    pub(super) fn slurm(setup: &Setup, arguments: &Arguments, report: &mut Report) {
        let executors = &setup.executors;
        let part = "executor slurm";
        let asked = arguments.executor.as_deref() == Some("slurm")
            || executors.slurm_container.is_some()
            || executors.slurm_options.is_some();
        let missing: Vec<&str> = (slurm_executor::TOOLS.into_iter())
            .filter(|tool| !slurm_executor::executable(Path::new(tool)))
            .collect();
        let needed: Vec<&str> = missing
            .iter()
            .copied()
            .filter(|tool| *tool != "sacct")
            .collect();
        let runtime = setup.slurm_container(arguments);
        let shared = format!(
            "the nodes must see {} and {} at the same paths",
            crate::cache_directory().join("scratch/slurm").display(),
            setup.store_location(arguments)
        );
        match (needed.is_empty(), asked) {
            (true, _) if !slurm_executor::executable(Path::new(runtime)) => report.add(
                State::Missing,
                part,
                format!(
                    "the SLURM tools are on the PATH, but the container runtime `{runtime}` is not; \
                     the jobs run each task with it on a node, so install it, or name another with \
                     -slurm-container"
                ),
            ),
            (true, _) if missing.is_empty() => report.add(
                State::Ready,
                part,
                format!(
                    "sbatch, squeue, sacct and scancel are on the PATH; the jobs run tasks with \
                     {runtime}; {shared}"
                ),
            ),
            (true, _) => report.add(
                State::Ready,
                part,
                format!(
                    "sbatch, squeue and scancel are on the PATH; without sacct, a job that SLURM has \
                     forgotten ends as a failure of the machine; the jobs run tasks with {runtime}; \
                     {shared}"
                ),
            ),
            (false, true) => report.add(
                State::Missing,
                part,
                format!(
                    "{} not on the PATH; run uwe on a node of the SLURM cluster, like its login \
                     node",
                    tools(&needed)
                ),
            ),
            (false, false) => report.add(
                State::NotSetUp,
                part,
                "the tools of SLURM are not on the PATH; -executor slurm runs on a node of a SLURM \
                 cluster",
            ),
        }
    }

    /// The newer executors, each built as a run builds it: that reads its tools, staging root,
    /// address and credentials, but asks no service.
    pub(super) fn newer(setup: &Setup, arguments: &Arguments, report: &mut Report) {
        let names = [
            "aws-batch",
            "google-batch",
            "azure-batch",
            "nomad",
            "htcondor",
            "pbs",
            "flyte",
            "snowflake",
            "jarvice",
            "wr",
        ];
        let store = (setup.store(arguments))
            .map_err(|failure| failure.message.trim_start_matches("uwe: ").to_owned());
        for name in names {
            let asked = arguments.executor.as_deref() == Some(name);
            let built =
                (store.clone()).and_then(|store| built(name, asked, store, setup, arguments));
            let (state, text) = match built {
                Ok(text) => (State::Ready, text),
                Err(problem) if asked => (State::Missing, problem),
                Err(problem) => (State::NotSetUp, problem),
            };
            report.add(state, format!("executor {name}"), text);
        }
    }

    /// What a run of the executor `name` would use, or why it cannot start.
    fn built(
        name: &str,
        asked: bool,
        store: content_store::Store,
        setup: &Setup,
        arguments: &Arguments,
    ) -> Result<String, String> {
        let scratch = crate::cache_directory().join("scratch");
        let image = arguments.default_image.clone();
        let run = "check-machine";
        let cloud = |service: &str| {
            format!(
                "the settings, staging root and credentials of {service} are complete; uwe asks \
                 {service} only when a run starts"
            )
        };
        let grid = |scheduler: &str, config: &crate::grid::Config| {
            let runtime = config.container.as_deref().unwrap_or("apptainer");
            format!("the {scheduler} tools are on the PATH; the jobs run tasks with {runtime}")
        };
        let executors = &setup.executors;
        Ok(match name {
            "aws-batch" => {
                crate::aws_batch::executor(store, &scratch, run, image, setup, &arguments.secrets)?;
                cloud("AWS Batch")
            }
            "google-batch" => {
                crate::google_batch::executor(store, &scratch, run, image, setup)?;
                cloud("Google Batch")
            }
            "azure-batch" => {
                crate::azure_batch::executor(store, &scratch, run, image, setup)?;
                cloud("Azure Batch")
            }
            "nomad" => {
                let named = (executors.nomad.settings.address.clone()).or_else(|| {
                    std::env::var("NOMAD_ADDR")
                        .ok()
                        .filter(|text| !text.is_empty())
                });
                if named.is_none() && !asked {
                    return Err("no Nomad agent named; name one with `address` in \
                                [executor.nomad] or NOMAD_ADDR"
                        .into());
                }
                crate::nomad::executor(store, &scratch, run, image, setup, &arguments.secrets)?;
                let address = named.unwrap_or_else(|| nomad_executor::ADDRESS.into());
                format!(
                    "the jobs go to the Nomad agent at {address}; uwe asks it only when a run starts"
                )
            }
            "htcondor" => {
                crate::grid::htcondor(store, &scratch, run, image, setup)?;
                grid("HTCondor", &executors.htcondor)
            }
            "flyte" => {
                crate::flyte::executor(store, &scratch, run, image, setup, &arguments.secrets)?;
                cloud("Flyte")
            }
            "snowflake" => {
                crate::snowflake::executor(store, &scratch, run, image, setup)?;
                cloud("Snowflake")
            }
            "jarvice" => {
                crate::jarvice::executor(store, &scratch, run, image, setup)?;
                let address = &executors.jarvice.settings.api_url;
                format!(
                    "the jobs go to the JARVICE API at {address}; uwe asks it only when a run starts"
                )
            }
            "wr" => {
                crate::wr::executor(store, &scratch, run, image, setup)?;
                let runtime = executors
                    .wr
                    .jobs
                    .container
                    .as_deref()
                    .unwrap_or("apptainer");
                format!(
                    "the token and CA certificate of the wr manager are readable; the jobs run \
                     tasks with {runtime}; uwe asks wr only when a run starts"
                )
            }
            _ => {
                crate::grid::pbs(store, &scratch, run, image, setup)?;
                grid("PBS", &executors.pbs)
            }
        })
    }

    /// `a is`, `a and b are`, `a, b and c are`.
    fn tools(names: &[&str]) -> String {
        match names.split_last() {
            Some((one, [])) => format!("`{one}` is"),
            Some((last, others)) => format!("`{}` and `{last}` are", others.join("`, `")),
            None => String::new(),
        }
    }

    /// The kubeconfig that `-kubeconfig` or the config file names, read without asking the cluster.
    pub(super) fn kubernetes(
        runtime: &tokio::runtime::Runtime,
        executors: &Executors,
        arguments: &Arguments,
        report: &mut Report,
    ) {
        let part = "executor kubernetes";
        let kubeconfig = (arguments.kubeconfig.clone()).or_else(|| executors.kubeconfig.clone());
        let Some(kubeconfig) = kubeconfig else {
            report.add(
                if arguments.executor.as_deref() == Some("kubernetes") {
                    State::Missing
                } else {
                    State::NotSetUp
                },
                part,
                "no kubeconfig named; name one with -kubeconfig or `kubeconfig` in \
                 [executor.kubernetes]; uwe never takes the default one",
            );
            return;
        };
        let namespace = (arguments.kubernetes_namespace.clone())
            .or_else(|| executors.kubernetes_namespace.clone());
        if !kubeconfig.is_file() {
            report.add(
                State::Missing,
                part,
                format!("the kubeconfig {} does not exist", kubeconfig.display()),
            );
            return;
        }
        let connected = runtime.block_on(kubernetes_executor::Cluster::connect(
            &kubeconfig,
            namespace,
        ));
        match connected {
            Ok(cluster) => report.add(
                State::Ready,
                part,
                format!(
                    "the kubeconfig {} is readable; Jobs go to the namespace {}",
                    kubeconfig.display(),
                    cluster.namespace()
                ),
            ),
            Err(error) => report.add(
                State::Missing,
                part,
                error.lines().next().unwrap_or_default().to_owned(),
            ),
        }
    }
}

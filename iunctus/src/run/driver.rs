//! Driver construction shared by the CLI and embedding API.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, cache_directory, detect, failure, nextflow, repository};
use std::path::{Path, PathBuf};
use workflow_graph::{CancellationToken, Driver, DriverError, Language, Launch};

/// The workflow of a command and its language: `-lang`, else what the target tells.
pub(crate) fn target_and_language(arguments: &Arguments) -> Result<(PathBuf, Language), Failure> {
    let Some(target) = &arguments.target else {
        return Err(Failure::usage(
            "iunctus: name a workflow file or a project directory to run",
        ));
    };
    let revision = arguments.revision.as_deref();
    let target = repository::prepare(target, revision, arguments.latest, arguments)?;
    let remote = target.to_string_lossy();
    let remote = remote.starts_with("https://") || remote.starts_with("http://");
    let cwl = remote
        && (arguments.language == Some(Language::Cwl)
            || (arguments.language.is_none()
                && target
                    .to_string_lossy()
                    .split('#')
                    .next()
                    .unwrap_or_default()
                    .ends_with(".cwl")));
    let target = if remote && !cwl {
        fetch_workflow(
            &target.to_string_lossy(),
            &cache_directory().join("downloads"),
        )?
    } else {
        target
    };
    let language = match arguments.language.or(cwl.then_some(Language::Cwl)) {
        Some(language) => language,
        None => {
            detect::language(&target).map_err(|message| failure(format!("iunctus: {message}")))?
        }
    };
    let target = if remote && language == Language::Cwl {
        arguments.target.clone().unwrap()
    } else {
        target
    };
    if language != Language::Snakemake
        && (!arguments.until.is_empty()
            || !arguments.omit_from.is_empty()
            || !arguments.forcerun.is_empty()
            || arguments.forceall)
    {
        return Err(DriverError::unsupported(
            "-until, -omit-from, -forcerun and -forceall require a Snakemake workflow",
        )
        .into());
    }
    Ok((target, language))
}

/// A copy of a workflow file on the web. The storage that fetched it holds the downloads
/// until the process ends, so that `iunctus clean` leaves the file alone while the run reads it.
fn fetch_workflow(url: &str, downloads: &Path) -> Result<PathBuf, Failure> {
    static FETCHING: std::sync::OnceLock<storage::Storage> = std::sync::OnceLock::new();
    let storage = FETCHING.get_or_init(storage::Storage::default);
    (storage.blocking().fetch(url, downloads)).map_err(|error| failure(error.to_string()))
}

/// Where `iunctus resume` leaves its requests for the suspended Argo steps of a run.
pub(crate) fn resume_folder(run: &str) -> PathBuf {
    cache_directory().join("scratch").join("resume").join(run)
}

/// The driver of a language.
pub(crate) fn driver(
    language: Language,
    launch: &Launch,
    run: &str,
    quiet: bool,
    arguments: &Arguments,
    setup: &Setup,
    cancel: &CancellationToken,
) -> Result<Box<dyn Driver>, DriverError> {
    match language {
        Language::Nextflow => nextflow::driver(launch, run, quiet, arguments, setup),
        Language::Snakemake => snakemake_runtime::driver_with_options(
            launch,
            snakemake(arguments, setup, cancel),
            snakemake_runtime::RunSettings {
                image: arguments.default_image.clone(),
                offline: super::backend::network_rule(arguments, setup).offline(
                    &workflow_graph::Task {
                        step: "command".into(),
                        ..Default::default()
                    },
                ),
                rerun_triggers: arguments.rerun_triggers.clone(),
                conda: setup.conda.is_some(),
                shared_workdir: shared_workdir(arguments, setup),
                local: arguments.executor.as_deref().unwrap_or("local") == "local",
            },
        ),
        Language::Cwl => {
            let downloads = cache_directory().join("downloads");
            let options = cwl_runtime::Options {
                image: arguments
                    .default_image
                    .clone()
                    .unwrap_or_else(|| cwl_runtime::tool::DEFAULT_IMAGE.into()),
                javascript: cwl_runtime::JavaScriptSettings::from_binary(
                    setup.engine(),
                    cancel.clone(),
                ),
                local_engine: (arguments.executor.as_deref().unwrap_or("local") == "local")
                    .then(|| setup.engine()),
                registry: setup.cwl_registry.clone(),
                // Apptainer on SLURM runs a saved image archive; it needs no registry.
                archives: (arguments.executor.as_deref() == Some("slurm")
                    && matches!(
                        setup.slurm_container(arguments),
                        "apptainer" | "singularity"
                    ))
                .then(|| cache_directory().join("cwl-images")),
                ..cwl_runtime::Options::default()
            };
            cwl_runtime::driver_with_options(launch, &setup.storage, downloads, options)
        }
        Language::Argo => {
            let endpoints = (setup.storage.s3_providers().into_iter())
                .map(|(provider, prefixes, address)| argo_runtime::Endpoint {
                    provider,
                    prefixes,
                    address,
                })
                .collect();
            let storage = setup.storage.clone();
            let git = move |source: &argo_runtime::GitSource, folder: &Path| {
                if source.account && !source.repo.starts_with("https://") {
                    return Err(format!(
                        "{} signs in over plain HTTP, which would send the account in clear text; use https",
                        source.repo
                    ));
                }
                let storage = storage.clone();
                let lookup =
                    move |url: &str| storage.account(url).map_err(|error| error.to_string());
                if source.account && lookup(&source.repo)?.is_none() {
                    return Err(format!(
                        "{} signs in; give it an account with an https storage provider of the config file",
                        source.repo
                    ));
                }
                let fetch = crate::git::Fetch {
                    url: source.repo.clone(),
                    reference: source.reference.clone(),
                    depth: source.depth,
                    refspecs: source.fetch.clone(),
                    submodules: source.submodules,
                    accounts: Some(crate::git::Accounts(std::sync::Arc::new(lookup))),
                };
                let stop = std::sync::atomic::AtomicBool::new(false);
                crate::git::checkout(&fetch, folder, &stop).map(|_| ())
            };
            let services = argo_runtime::Services {
                endpoints,
                resume: Some(resume_folder(run)),
                git: Some(argo_runtime::GitClient(std::sync::Arc::new(git))),
            };
            argo_runtime::driver_with(launch, services)
        }
    }
}

/// Whether the executor sees the folders of this machine at their paths, so that Snakemake
/// jobs run in the working directory itself: the local one, and SLURM without object stores.
fn shared_workdir(arguments: &Arguments, setup: &Setup) -> bool {
    match arguments.executor.as_deref().unwrap_or("local") {
        "local" => true,
        "slurm" => setup.executors.slurm_staging.mode == crate::staging::Mode::SharedPath,
        _ => false,
    }
}

/// The settings of Snakemake runs: the Python container of decision 19, with the engine of
/// the local executor, which keeps what uv downloads and installs in the cache, and the
/// storage providers.
fn snakemake(
    arguments: &Arguments,
    setup: &Setup,
    cancel: &CancellationToken,
) -> snakemake_runtime::Settings {
    snakemake_runtime::Settings {
        python: Some(snakemake_runtime::PythonContainers {
            engine: setup.engine(),
            image: arguments.python_image.clone(),
            cache: cache_directory().join("python"),
            show_output: true,
        }),
        storage: setup.storage.clone(),
        cancel: cancel.clone(),
        git: Some(snakemake_git(setup)),
    }
}

/// The Git client for Snakefiles of `gitfile(...)` on the web, signed in with the account of
/// an https storage provider when the config file gives one.
fn snakemake_git(setup: &Setup) -> snakemake_runtime::GitClient {
    let storage = setup.storage.clone();
    let checkout = move |url: &str, reference: &str, folder: &Path| {
        let storage = storage.clone();
        let lookup = move |url: &str| storage.account(url).map_err(|error| error.to_string());
        // A shallow fetch cannot reach a commit by its ID.
        let commit = reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit());
        let fetch = crate::git::Fetch {
            url: url.to_owned(),
            reference: Some(reference.to_owned()),
            depth: (!commit).then_some(1),
            accounts: Some(crate::git::Accounts(std::sync::Arc::new(lookup))),
            ..crate::git::Fetch::default()
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        crate::git::checkout(&fetch, folder, &stop).map(|_| ())
    };
    snakemake_runtime::GitClient(std::sync::Arc::new(checkout))
}

/// What the command line asks to run, in the terms all languages share.
pub(crate) fn launch(arguments: &Arguments, target: PathBuf) -> Launch {
    Launch {
        target,
        params: arguments.params.clone(),
        inputs: arguments.inputs.clone(),
        profiles: arguments.profiles.clone().unwrap_or_default(),
        configs: arguments.configs.clone(),
        targets: arguments.targets.clone(),
        until: arguments.until.clone(),
        omit_from: arguments.omit_from.clone(),
        forcerun: arguments.forcerun.clone(),
        forceall: arguments.forceall,
        scratch: cache_directory().join("scratch"),
        output_dir: arguments.output_dir.clone(),
        stub: arguments.stub,
        // The simulator makes up the outputs of tasks, so its runs also assume that the
        // source files of the workflow exist, as the graph needs them rather than their content.
        simulated: arguments.executor.as_deref().unwrap_or("local") == "simulator",
        keep_going: arguments.keep_going,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn holds_fetched_workflows() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|count| count > 2) {
                    line.clear();
                }
                let body = "process A { script: 'true' }\n";
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                let mut stream = stream;
                let _ = stream.write_all(format!("{head}{body}").as_bytes());
            }
        });
        let downloads = tempfile::tempdir().unwrap();
        let url = format!("http://{address}/main.nf");
        let copy = super::fetch_workflow(&url, downloads.path()).unwrap();
        assert!(copy.is_file());
        // Clean-up cannot take the downloads alone while the run may read the file.
        let alone = storage::hold_downloads(downloads.path()).unwrap();
        assert!(alone.is_none(), "the run holds its downloads");
    }
}

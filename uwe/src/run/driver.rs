//! Driver construction shared by the CLI and embedding API.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::arguments::Arguments;
use crate::hosted::Isolation;
use crate::setup::Setup;
use crate::{Failure, detect, failure, nextflow, repository};
use std::path::{Path, PathBuf};
use storage::Storage;
use workflow_graph::{CancellationToken, Driver, DriverError, Language, Launch};

/// The workflow of a command and its language: `-lang`, else what the target tells. Git signs
/// in with the accounts of the storage of the run, which also fetches a workflow on the web.
pub(crate) fn target_and_language(
    arguments: &Arguments,
    setup: &Setup,
) -> Result<(PathBuf, Language), Failure> {
    let storage = &setup.storage;
    let Some(target) = &arguments.target else {
        return Err(Failure::usage(
            "uwe: name a workflow file or a project directory to run",
        ));
    };
    let revision = arguments.revision.as_deref();
    let target = repository::prepare(target, revision, arguments.latest, arguments, Some(storage))?;
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
        let downloads = setup.cache_directory().join("downloads");
        fetch_workflow(&target.to_string_lossy(), storage, &downloads)?
    } else {
        target
    };
    let language = match arguments.language.or(cwl.then_some(Language::Cwl)) {
        Some(language) => language,
        None => detect::language(&target).map_err(|message| failure(format!("uwe: {message}")))?,
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

/// A copy of a workflow file on the web, fetched through the storage of the run, with its
/// accounts, proxy and isolation. That storage holds the downloads while the run lasts, so that
/// `uwe clean` leaves the file alone while the run reads it.
fn fetch_workflow(url: &str, storage: &Storage, downloads: &Path) -> Result<PathBuf, Failure> {
    (storage.blocking().fetch(url, downloads)).map_err(|error| failure(error.to_string()))
}

/// Where `uwe resume` leaves its requests for the suspended Argo steps of a run, in the cache
/// folder `cache`.
pub(crate) fn resume_folder(cache: &Path, run: &str) -> PathBuf {
    cache.join("scratch").join("resume").join(run)
}

/// The isolation of a run, with its output folder among the folders that workflow code may
/// reach; `None` when the run is not isolated.
pub(crate) fn isolation(arguments: &Arguments, setup: &Setup) -> Option<Isolation> {
    let mut isolation = setup.isolation.clone()?;
    isolation.roots.extend(arguments.output_dir.clone());
    Some(isolation)
}

/// The cache folder and the local store, which other runs share: isolated workflow code reads
/// their files by path, but may not list or change them.
pub(crate) fn shared_folders(arguments: &Arguments, setup: &Setup) -> Vec<PathBuf> {
    let store = setup.store_location(arguments);
    let store = (!store.contains("://")).then(|| PathBuf::from(store));
    [setup.cache_directory()].into_iter().chain(store).collect()
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
        Language::Snakemake => {
            let options = snakemake_options(arguments, setup, run);
            if let Some(folder) = &options.workdir {
                std::fs::create_dir_all(folder)
                    .map_err(|error| DriverError::new(format!("{}: {error}", folder.display())))?;
            }
            let settings = snakemake(arguments, setup, cancel);
            snakemake_runtime::driver_with_options(launch, settings, options)
        }
        Language::Cwl => {
            let downloads = setup.cache_directory().join("downloads");
            let options = cwl_options(arguments, setup, cancel);
            cwl_runtime::driver_with_options(launch, &setup.storage, downloads, options)
        }
        Language::Argo => argo_runtime::driver_with(launch, argo_services(run, setup)),
        Language::Wdl => {
            // The expressions of an isolated run read files where its workflow code may.
            let mut options = wdl_options(arguments, setup);
            options.roots = isolation(arguments, setup).map(|isolation| {
                let shared = shared_folders(arguments, setup);
                let own = isolation.launch.clone();
                nextflow::run_roots(&launch.target, launch, (&isolation, &shared), own)
            });
            wdl_runtime::driver_with(launch, options)
        }
        Language::Wir => {
            let mut settings = wir_runtime::Settings {
                // Only a block that is the whole run hears `uwe resume`, whose requests name
                // the steps of the run.
                argo: argo_services(run, setup),
                cwl: cwl_options(arguments, setup, cancel),
                nextflow: nextflow::blocks(run, quiet, arguments, setup),
                snakemake: wir_runtime::snakemake::Options {
                    settings: snakemake(arguments, setup, cancel),
                    run: snakemake_options(arguments, setup, run),
                },
                wdl: wdl_options(arguments, setup),
                galaxy: galaxy_options(arguments),
                annotations: Default::default(),
                base: None,
                print: !arguments.no_print,
                print_sink: {
                    let secrets = arguments.secrets.clone();
                    match arguments.print_sink.clone() {
                        Some(sink) => wir_runtime::Printer::new(move |text| {
                            sink.print(&nextflow::hidden(text, &secrets));
                        }),
                        None if quiet => wir_runtime::Printer::new(|text| eprint!("{text}")),
                        None => wir_runtime::Printer::default(),
                    }
                },
                adapters: wir_runtime::argo::register(
                    Default::default(),
                    argo_services(run, setup),
                ),
                secret_inputs: arguments.secret_inputs.clone(),
                // File ops reach what the code of the workflow reaches.
                roots: isolation(arguments, setup).map(|isolation| {
                    let workflow = arguments.target.clone().unwrap_or(launch.target.clone());
                    let own = (isolation.launch.clone())
                        .or_else(|| arguments.embedded.then(|| own(setup, run)));
                    let shared = shared_folders(arguments, setup);
                    nextflow::run_roots(&workflow, launch, (&isolation, &shared), own)
                }),
                repositories: {
                    // A block fetches its repository as `uwe run <address> -r <commit>` does.
                    let (arguments, storage) = (arguments.clone(), setup.storage.clone());
                    wir_runtime::Repositories::new(move |location, commit| {
                        let location = Path::new(location);
                        repository::prepare(
                            location,
                            Some(commit),
                            false,
                            &arguments,
                            Some(&storage),
                        )
                        .map_err(|failure| DriverError::new(failure.message))
                    })
                },
            };
            wir_runtime::snakemake::register(&mut settings.adapters, settings.snakemake.clone());
            wir_runtime::nextflow::register(&mut settings.adapters, settings.nextflow.clone());
            let downloads = setup.cache_directory().join("downloads");
            // A composed workflow or a wir document in YAML runs as the JSON document of it.
            let folder = setup.cache_directory().join("scratch").join("composed");
            let prepared =
                crate::tools::prepare(&launch.target, &folder).map_err(DriverError::new)?;
            let launch = match prepared {
                Some(prepared) => {
                    settings.base = Some(prepared.base);
                    settings.annotations = prepared.annotations;
                    &Launch {
                        target: prepared.path,
                        ..launch.clone()
                    }
                }
                None => launch,
            };
            wir_runtime::driver(launch, settings.with_storage(&setup.storage, downloads))
        }
        Language::Galaxy => galaxy_runtime::driver_with(launch, galaxy_options(arguments)),
    }
}

/// The options of Galaxy runs, and of the Galaxy blocks of wir runs.
fn galaxy_options(arguments: &Arguments) -> galaxy_runtime::Options {
    let defaults = galaxy_runtime::Options::default();
    galaxy_runtime::Options {
        image: arguments.default_image.clone().unwrap_or(defaults.image),
        tools: defaults.tools,
        print: !arguments.no_print,
    }
}

/// The services of Argo runs, and of the Argo blocks of wir runs.
fn argo_services(run: &str, setup: &Setup) -> argo_runtime::Services {
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
        let lookup = move |url: &str| storage.account(url).map_err(|error| error.to_string());
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
    argo_runtime::Services {
        policy: setup.argo_policy.clone(),
        locks: setup.argo_locks.clone(),
        endpoints,
        resume: Some(resume_folder(&setup.cache_directory(), run)),
        git: Some(argo_runtime::GitClient(std::sync::Arc::new(git))),
    }
}

/// The options of WDL runs, and of the WDL blocks of wir runs.
pub(crate) fn wdl_options(arguments: &Arguments, setup: &Setup) -> wdl_runtime::Options {
    wdl_runtime::Options {
        image: (arguments.default_image.clone())
            .unwrap_or_else(|| wdl_runtime::task::DEFAULT_IMAGE.into()),
        print: !arguments.no_print,
        imports: setup.isolation.is_some().then(|| wdl_imports(setup)),
        roots: None,
    }
}

/// Reads each document at a web address that a WDL document of an isolated run imports
/// through the storage of the run, which refuses an address the run may not reach.
fn wdl_imports(setup: &Setup) -> wdl_runtime::document::Imports {
    let downloads = setup.cache_directory().join("downloads");
    let storage = setup.storage.blocking();
    wdl_runtime::document::Imports::new(move |url| {
        let copy = storage
            .fetch(url, &downloads)
            .map_err(|error| error.to_string())?;
        std::fs::read_to_string(copy).map_err(|error| error.to_string())
    })
}

/// The options of CWL runs, and of the CWL blocks of wir runs.
fn cwl_options(
    arguments: &Arguments,
    setup: &Setup,
    cancel: &CancellationToken,
) -> cwl_runtime::Options {
    cwl_runtime::Options {
        image: arguments
            .default_image
            .clone()
            .unwrap_or_else(|| cwl_runtime::tool::DEFAULT_IMAGE.into()),
        javascript: {
            let engine = setup.engine();
            let mut javascript =
                cwl_runtime::JavaScriptSettings::from_binary(engine, cancel.clone());
            javascript.journals = Some(setup.journal_folder());
            javascript.binary = arguments.javascript_binary.clone();
            javascript
        },
        local_engine: (arguments.executor.as_deref().unwrap_or("local") == "local")
            .then(|| setup.engine()),
        registry: setup.cwl_registry.clone(),
        software: setup.conda.is_some(),
        shm: match arguments.executor.as_deref().unwrap_or("local") {
            "local" | "kubernetes" => true,
            "slurm" => matches!(setup.slurm_container(arguments), "docker" | "podman"),
            _ => false,
        },
        // Apptainer on SLURM runs a saved image archive; it needs no registry.
        archives: (arguments.executor.as_deref() == Some("slurm")
            && matches!(
                setup.slurm_container(arguments),
                "apptainer" | "singularity"
            ))
        .then(|| crate::cwl_archives(&setup.cache_directory())),
        print: !arguments.no_print,
        ..cwl_runtime::Options::default()
    }
}

/// The options of Snakemake runs, and of the Snakemake blocks of wir runs.
pub(crate) fn snakemake_options(
    arguments: &Arguments,
    setup: &Setup,
    run: &str,
) -> snakemake_runtime::RunSettings {
    let run = arguments.launch_run.as_deref().unwrap_or(run);
    snakemake_runtime::RunSettings {
        image: arguments.default_image.clone(),
        offline: super::backend::network_rule(arguments, setup).offline(&workflow_graph::Task {
            step: "command".into(),
            ..Default::default()
        }),
        rerun_triggers: arguments.rerun_triggers.clone(),
        conda: setup.conda.is_some(),
        shared_workdir: shared_workdir(arguments, setup),
        local: arguments.executor.as_deref().unwrap_or("local") == "local",
        // An isolated or embedded run without a launch folder of the host starts in one of its
        // own, or that of the run it resumes, never in the working directory of the process.
        workdir: match &setup.isolation {
            Some(isolation) => Some(isolation.launch.clone().unwrap_or_else(|| own(setup, run))),
            None => arguments.embedded.then(|| own(setup, run)),
        },
        directory: arguments.workdir.clone(),
        planning: arguments.planning,
    }
}

/// The launch folder of its own of the run `run`.
fn own(setup: &Setup, run: &str) -> PathBuf {
    setup
        .cache_directory()
        .join("scratch")
        .join("launch")
        .join(run)
}

/// Whether the executor sees the folders of this machine at their paths, so that Snakemake
/// jobs run in the working directory itself: the local one, and SLURM without object stores.
fn shared_workdir(arguments: &Arguments, setup: &Setup) -> bool {
    match arguments.executor.as_deref().unwrap_or("local") {
        "local" => true,
        "slurm" => {
            setup.executors.slurm_staging.mode == crate::executors::staging::Mode::SharedPath
        }
        _ => false,
    }
}

/// The settings of Snakemake runs: the Python container of decision 19, with the engine of
/// the local executor, which keeps what uv downloads and installs in the cache, and the
/// storage providers.
pub(crate) fn snakemake(
    arguments: &Arguments,
    setup: &Setup,
    cancel: &CancellationToken,
) -> snakemake_runtime::Settings {
    let isolation = isolation(arguments, setup);
    snakemake_runtime::Settings {
        python: Some(snakemake_runtime::PythonContainers {
            engine: setup.engine(),
            image: arguments.python_image.clone(),
            cache: setup.cache_directory().join("python"),
            show_output: true,
            journals: Some(setup.journal_folder()),
        }),
        storage: setup.storage.clone(),
        cancel: cancel.clone(),
        git: Some(snakemake_git(setup)),
        environment: Some(arguments.secrets.environment()),
        roots: isolation.clone().map(|isolation| {
            let mut roots = isolation.roots;
            roots.extend(isolation.launch);
            roots
        }),
        read_only: shared_folders(arguments, setup),
        readable: isolation
            .map(|isolation| isolation.read_only)
            .unwrap_or_default(),
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

/// What the command line asks to run, in the terms all languages share, with the scratch
/// folder of the run of `setup`.
pub(crate) fn launch(arguments: &Arguments, setup: &Setup, target: PathBuf) -> Launch {
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
        scratch: setup.cache_directory().join("scratch"),
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
    fn passes_javascript_binary() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let mut settings = crate::run::Settings::new(root.path().join("main.cwl"));
        settings.config_file = Some(config);
        settings.javascript_binary = Some(root.path().join("uwe-static"));
        let (arguments, _, _inputs) = settings.prepare().unwrap();
        let setup = crate::setup::Setup::default();
        let cancel = workflow_graph::CancellationToken::new();
        let options = super::cwl_options(&arguments, &setup, &cancel);
        let expected = root.path().join("uwe-static");
        assert_eq!(options.javascript.binary, Some(expected));
    }

    #[test]
    fn uses_journal_folder() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let cancel = workflow_graph::CancellationToken::new();
        for chosen in [None, Some(root.path().join("journals"))] {
            let _scope = crate::run::support::CacheScope::new(false);
            let mut settings = crate::run::Settings::new(root.path().join("main.cwl"));
            settings.config_file = Some(config.clone());
            settings.cache_folder = Some(root.path().join("cache"));
            settings.journal_folder = chosen.clone();
            let (arguments, _, _inputs) = settings.prepare().unwrap();
            let mut setup = crate::setup::Setup::default();
            setup.use_folders(&arguments);
            let expected = chosen.unwrap_or_else(|| root.path().join("cache/containers"));
            // Another thread, like one of the workers of the run, sees the same folders.
            let (setup, arguments, cancel) = (&setup, &arguments, &cancel);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let options = super::cwl_options(arguments, setup, cancel);
                    assert_eq!(options.javascript.journals.as_ref(), Some(&expected));
                    let python = super::snakemake(arguments, setup, cancel).python;
                    assert_eq!(
                        python.and_then(|python| python.journals),
                        Some(expected.clone())
                    );
                });
            });
        }
    }

    /// Two runs of one thread with other cache folders keep their own folders and downloads.
    #[test]
    fn separates_run_folders() {
        let root = tempfile::tempdir().unwrap();
        let mut setups = Vec::new();
        for name in ["one", "two"] {
            let arguments = crate::arguments::Arguments {
                cache_folder: Some(root.path().join(name)),
                ..Default::default()
            };
            let mut setup = crate::setup::Setup::default();
            setup.use_folders(&arguments);
            setups.push(setup);
        }
        for (setup, name) in setups.iter().zip(["one", "two"]) {
            let context = setup.context();
            assert_eq!(context.cache_folder, root.path().join(name));
            assert_eq!(
                context.downloads(),
                root.path().join(name).join("downloads")
            );
            assert_eq!(
                setup.journal_folder(),
                root.path().join(name).join("containers")
            );
            let launch = super::launch(&Default::default(), setup, "main.nf".into());
            assert_eq!(launch.scratch, root.path().join(name).join("scratch"));
        }
    }

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
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut stream = stream;
                let _ = stream.write_all(format!("{head}{body}").as_bytes());
            }
        });
        let downloads = tempfile::tempdir().unwrap();
        let url = format!("http://{address}/main.nf");
        let storage = storage::Storage::default().with_proxy(None);
        let copy = super::fetch_workflow(&url, &storage, downloads.path()).unwrap();
        assert!(copy.is_file());
        // Clean-up cannot take the downloads alone while the run may read the file.
        let alone = storage::hold_downloads(downloads.path()).unwrap();
        assert!(alone.is_none(), "the run holds its downloads");
        // The storage of the run holds them until the run ends.
        drop(storage);
        let alone = storage::hold_downloads(downloads.path()).unwrap();
        assert!(alone.is_some(), "the downloads go free with the run");
    }
}

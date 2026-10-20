//! Driver construction shared by the CLI and embedding API.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, cache_directory, detect, failure, nextflow, repository};
use std::path::PathBuf;
use workflow_graph::{CancellationToken, Driver, DriverError, Language, Launch};

/// The workflow of a command and its language: `-lang`, else what the target tells.
pub(crate) fn target_and_language(arguments: &Arguments) -> Result<(PathBuf, Language), Failure> {
    let Some(target) = &arguments.target else {
        return Err(Failure::usage(
            "iunctus: name a workflow file or a project directory to run",
        ));
    };
    let target = repository::prepare(target, arguments.revision.as_deref(), arguments.latest)?;
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
        storage::Storage::default()
            .blocking()
            .fetch(
                &target.to_string_lossy(),
                &cache_directory().join("downloads"),
            )
            .map_err(|error| failure(error.to_string()))?
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
                ..cwl_runtime::Options::default()
            };
            cwl_runtime::driver_with_options(launch, &setup.storage, downloads, options)
        }
        Language::Argo => argo_runtime::driver(launch),
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
    }
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

//! Complete workflow runs, including recording and resume.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::application::{self, Description, Policy};
use crate::arguments::Arguments;
use crate::repeat::Pinned;
use crate::setup::Setup;
use crate::{
    Failure, driver, executor, failure, fetched, inputs, insight, launch, network_rule, page,
    progress, record, report, resources, resume, run_id, sources, stop_on_interrupt,
    target_and_language, terminal,
};
use run_database::Recorder;
use run_page::Server;
use workflow_graph::{CancellationToken, Engine, Execution, Order, RunOptions};

/// A workflow submission. `settings` uses the same configuration as `iunctus run`.
/// Strict admission checks typed JSON without CLI coercion or simulated assumptions.
pub struct RunRequest {
    pub settings: crate::RunSettings,
    pub policy: Policy,
    /// Supplies execution setup without loading its config files or applying global defaults.
    /// The optional local viewer still reads its own page configuration.
    pub setup: Option<Setup>,
}

impl RunRequest {
    /// Selects a workflow with strict admission. Set `settings.inputs` to its JSON input file.
    pub fn new(target: impl Into<std::path::PathBuf>) -> Self {
        Self {
            settings: Arguments {
                target: Some(target.into()),
                ..Arguments::default()
            },
            policy: Policy::Strict,
            setup: None,
        }
    }

    /// Runs safely inside Tokio. Drivers and the blocking engine stay on the worker thread.
    /// Dropping this future requests cancellation; the worker continues cleanup and recording.
    /// Await completion after cancelling the token to observe the final result.
    pub async fn run(self, cancel: CancellationToken) -> Result<Run, Failure> {
        let cancel = cancel.child_token();
        let _stop = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || self.run_blocking(cancel))
            .await
            .map_err(|error| Failure::setup(format!("iunctus: run worker failed: {error}")))?
    }

    /// Blocking equivalent of `run`. Call outside an async runtime.
    /// Workflow failures are retained in `Run::execution`, together with the partial graph.
    pub fn run_blocking(mut self, cancel: CancellationToken) -> Result<Run, Failure> {
        if self.setup.is_none() {
            crate::setup::Defaults::load(&self.settings)?.apply("run", &mut self.settings);
        }
        execute_with(
            &self.settings,
            true,
            &Pinned::default(),
            cancel.child_token(),
            Some(self.policy),
            self.setup,
        )
    }

    /// Builds and describes the configured driver without starting tasks or recording a run.
    /// Native planning can still read files and use configured container helpers.
    pub async fn describe(mut self, cancel: CancellationToken) -> Result<Description, Failure> {
        let cancel = cancel.child_token();
        let _stop = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            if self.setup.is_none() {
                crate::setup::Defaults::load(&self.settings)?.apply("run", &mut self.settings);
            }
            let setup = match self.setup {
                Some(setup) => setup,
                None => Setup::load(&self.settings)?,
            };
            let (target, language) = target_and_language(&self.settings)?;
            let launch = launch(&self.settings, target);
            let mut driver = driver(
                language,
                &launch,
                &run_id(),
                true,
                &self.settings,
                &setup,
                &cancel,
            )?;
            application::describe(driver.as_mut()).map_err(|error| failure(error.to_string()))
        })
        .await
        .map_err(|error| Failure::setup(format!("iunctus: discovery worker failed: {error}")))?
    }
}

/// Runs the target with its language's driver and the chosen executor, and records the run
/// in the run database. `quiet` keeps standard output free for the graph. With `-page`, the
/// page of the run serves from the start; the result holds it.
pub(crate) fn execute(arguments: &Arguments, quiet: bool, pinned: &Pinned) -> Result<Ran, Failure> {
    execute_with(arguments, quiet, pinned, stop_on_interrupt(), None, None)
}

fn execute_with(
    arguments: &Arguments,
    quiet: bool,
    pinned: &Pinned,
    interrupt: CancellationToken,
    admission: Option<Policy>,
    setup: Option<Setup>,
) -> Result<Run, Failure> {
    let _stop = interrupt.clone().drop_guard();
    let (target, language) = target_and_language(arguments)?;
    let target = &target;
    let launch = launch(arguments, target.clone());
    let run = run_id();
    let name = arguments.executor.as_deref().unwrap_or("simulator");
    let setup = match setup {
        Some(setup) => setup,
        None => Setup::load(arguments)?,
    };
    for file in &pinned.files {
        setup.storage.pin(file.clone());
    }
    let store = setup.store_location(arguments);
    let real = name != "simulator";
    // A real run shows its steps as it goes; in a terminal they are drawn in place, so the
    // executors print no line per task there.
    let shown = real && !quiet;
    let verbose = !quiet && !(shown && terminal::possible());
    let runtime =
        workflow_graph::runtime().map_err(|error| failure(format!("iunctus: {error}")))?;
    let sources = runtime.block_on(sources::collect(target, arguments));
    let invocation = admission.map(|_| {
        let mut words = record::run_arguments(arguments);
        for (option, targets) in [
            ("-until", &arguments.until),
            ("-omit-from", &arguments.omit_from),
            ("-forcerun", &arguments.forcerun),
        ] {
            for target in targets {
                words.extend([option.into(), target.clone()]);
            }
        }
        if arguments.forceall {
            words.push("-forceall".into());
        }
        words
    });
    let started = record::Started {
        id: &run,
        target,
        language,
        executor: name,
        simulated: !real,
        store: real.then_some(store.as_str()),
        sources: &sources,
        repeats: pinned.repeats.as_deref(),
        invocation: invocation.as_deref(),
    };
    // Read before this run is in the database.
    let measured = resources::measured(arguments);
    let record::Recording {
        mut recorder,
        reusable,
    } = record::start(arguments, &started)?;
    let page = match arguments.page {
        true => Some(page::start(arguments, &format!("/runs/{run}"))?),
        false => None,
    };
    // The inputs are checked before any task starts (decision 24).
    let prepared = executor(
        arguments,
        &setup,
        &run,
        verbose,
        !arguments.no_cache,
        pinned,
        &measured,
    )
    .and_then(|executor| {
        let mut driver = driver(
            language, &launch, &run, quiet, arguments, &setup, &interrupt,
        )?;
        let inputs = match admission {
            Some(policy) => {
                let inputs = driver.inputs()?;
                let admission = application::validate_json(
                    Description {
                        language,
                        inputs,
                        outputs: None,
                    },
                    started.simulated,
                    policy,
                )
                .map_err(|error| failure(error.to_string()))?;
                if let Some(inputs) = &admission.inputs {
                    driver.use_inputs(inputs.values.clone())?;
                }
                admission.inputs
            }
            None => inputs::check(driver.as_mut(), started.simulated, quiet)?,
        };
        if let Some(inputs) = inputs {
            recorder.inputs(&inputs.schema, &inputs.values);
        }
        // A workflow whose outputs cannot be described still runs.
        if let Ok(Some(schema)) = driver.outputs() {
            recorder.outputs(&schema);
        }
        Ok((executor, driver))
    });
    let (executor, mut driver) = match prepared {
        Ok(prepared) => prepared,
        Err(failure) => {
            recorder.ended(Some(&failure.message));
            close(recorder);
            return Err(failure);
        }
    };
    // A real run keeps the files of its workflow and the remote files it read in its store,
    // so that a repeat of the run has them years later.
    let kept = match real {
        true => setup.store(arguments).ok(),
        false => None,
    };
    if let Some(store) = &kept
        && let Err(message) = runtime.block_on(sources::keep(store, &sources))
    {
        tracing::warn!("iunctus: the store does not keep the files of the workflow: {message}");
    }
    if let (true, Some(reason)) = (real, &sources.skipped) {
        tracing::info!("iunctus: the run records no files of its workflow, as {reason}");
    }
    let taken = resume::Taken::default();
    let mut executor = match reusable {
        Some(reusable) => {
            let mode = if real {
                resume::Mode::Real
            } else {
                resume::Mode::Simulated
            };
            let resumed = resume::Resumed::new(
                executor,
                reusable,
                taken.clone(),
                mode,
                network_rule(arguments, &setup),
                verbose,
            );
            Box::new(resumed)
        }
        None => executor,
    };
    // A real executor runs every task that can run, and the pool of its machine or cluster
    // sets how many run at once. The simulator runs one at a time, in the order asked.
    let options = RunOptions {
        max_pending: (arguments.max_pending).unwrap_or(if real { 1_024 } else { 1 }),
        order: (arguments.order).unwrap_or(if real {
            Order::Finished
        } else {
            Order::AsAsked
        }),
    };
    let workflow = target.to_string_lossy();
    let progress = shown.then(|| {
        progress::Progress::start(progress::Header {
            target,
            language,
            executor: name,
            run: &run,
        })
    });
    let mut noting = resume::Noting::new(&mut recorder, taken);
    let mut watching;
    let observer: &mut dyn workflow_graph::Observer = match &progress {
        Some(progress) => {
            watching = progress.observer(&mut noting);
            &mut watching
        }
        None => &mut noting,
    };
    let mut execution = Engine::new(executor.as_mut())
        .options(options)
        .retries(arguments.machine_retries.unwrap_or(2))
        .retry_delay(std::time::Duration::from_millis(
            arguments.machine_retry_delay_ms.unwrap_or(1_000),
        ))
        .observer(observer)
        .cancel(interrupt.clone())
        .run(&workflow, driver.as_mut());
    if !real {
        execution.graph.simulate();
    }
    // The run is over: a later Ctrl-C, like the one that closes the page, stops nothing.
    interrupt.cancel();
    let read = fetched::read(&setup.storage, &store);
    recorder.remote_files(&fetched::rows(&read));
    if let Some(store) = &kept
        && let Err(message) = runtime.block_on(fetched::keep(store, &read))
    {
        tracing::warn!("iunctus: the store does not keep the remote files of the run: {message}");
    }
    close(recorder);
    // A failed run leaves its error as the first line on standard error.
    if execution.error.is_none() && !arguments.no_right_sizing {
        for line in resources::notes(&execution.graph, &measured, !real) {
            eprintln!("{line}");
        }
    }
    // Traces of simulated runs would only fill dashboards with made-up times.
    if real {
        insight::telemetry::after_run(arguments, &setup, &run);
    }
    let report = report::after_run(arguments, &run);
    match progress {
        Some(progress) => progress.finish(&run, Some(&store), report.as_deref()),
        None => {
            if let Some(report) = &report {
                eprintln!("iunctus: the report of the run is in {}", report.display());
            }
        }
    }
    Ok(Ran {
        execution,
        page,
        id: run,
    })
}

/// A run that ended: what the engine did, the page that serves it, and its id in the run
/// database.
pub struct Run {
    pub execution: Execution,
    /// Optional local viewer requested in the settings; the host owns its lifetime.
    pub page: Option<Server>,
    pub id: String,
}

pub(crate) type Ran = Run;

/// Ends the record of a run. A run goes on when the database fails; it only says so.
pub(crate) fn close(recorder: Recorder) {
    if let Err(error) = recorder.close() {
        tracing::warn!("iunctus: the run database did not record the whole run: {error}");
    }
}

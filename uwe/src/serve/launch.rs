//! Describes, checks and starts the workflows of the launcher, and starts recorded runs again.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use run_page::{Described, LaunchRequest, Profiles, Refusal, ResumeFrom, RunStarted};
use serde_json::{Map, Value, json};

use super::Backend;
use crate::Resume;
use crate::application::Policy;
use crate::forms::Form;
use crate::run::{RunRequest, Settings};

/// A text that is given and not empty.
fn given(text: &Option<String>) -> Option<&str> {
    text.as_deref().filter(|text| !text.trim().is_empty())
}

impl Backend {
    /// The settings of a run of `request`, with the config, database and store of the server.
    fn settings(&self, request: &LaunchRequest) -> Result<Settings, Refusal> {
        let target = given(&request.target);
        let preset = given(&request.preset);
        if target.is_none() && preset.is_none() {
            return Err(Refusal::invalid(
                "give a workflow, a path or address, or a preset",
            ));
        }
        let mut settings = Settings::new(target.unwrap_or_default());
        settings.preset = preset.map(str::to_owned);
        settings.revision = given(&request.revision).map(str::to_owned);
        settings.config_file.clone_from(&self.options.config_file);
        settings.database = Some(self.options.database.clone());
        settings.store.clone_from(&self.options.store);
        settings.run_store.clone_from(&self.options.run_store);
        settings.cache_folder.clone_from(&self.options.cache_folder);
        settings.setup = self.options.setup.as_ref().map(|make| make());
        if let Some(name) = given(&request.language) {
            let language = crate::arguments::language(name)
                .ok_or_else(|| Refusal::invalid(format!("uwe runs no language {name}")))?;
            settings.language = Some(language);
        }
        settings.profiles = (request.profile.as_ref())
            .map(Profiles::names)
            .unwrap_or_default();
        settings
            .targets
            .extend(given(&request.entry).map(str::to_owned));
        settings.inputs = request.values.clone().map(Value::Object);
        if let Some(executor) = given(&request.executor) {
            if !crate::setup::EXECUTORS.contains(&executor) {
                return Err(Refusal::invalid(format!("uwe has no executor {executor}")));
            }
            settings.executor = Some(executor.to_owned());
        }
        settings.resume = match request.resume.as_ref().and_then(ResumeFrom::run) {
            None => None,
            Some(None) => Some(Resume::Last),
            Some(Some(run)) => Some(Resume::Run(run.to_owned())),
        };
        if request.no_network == Some(true) {
            settings.network = Some(task_runner::NetworkRule {
                none_by_default: true,
                allowed_steps: Vec::new(),
            });
        }
        settings.cache = request.no_cache != Some(true);
        settings.default_image = given(&request.default_image).map(str::to_owned);
        settings.output_dir = given(&request.output_dir).map(Into::into);
        // The run may write its output folder, so an isolated one names only a folder that
        // it may write already.
        let isolation = settings
            .setup
            .as_ref()
            .and_then(|setup| setup.isolation.as_ref());
        if let (Some(folder), Some(isolation)) = (&settings.output_dir, isolation) {
            let roots: Vec<PathBuf> = (isolation.roots.iter().cloned())
                .chain(isolation.launch.clone())
                .collect();
            if !super::contained(folder, &roots) {
                return Err(Refusal::invalid(
                    "the output folder must be inside a folder that this server lets runs write",
                ));
            }
        }
        Ok(settings)
    }
}

/// The language, the input form, the support report and the outputs of a workflow, from one
/// preparation of the workflow. A form that cannot be made is `None`, with why.
pub(super) async fn describe(
    backend: &Backend,
    request: &LaunchRequest,
) -> Result<Described, Refusal> {
    let overview = RunRequest::new(backend.settings(request)?).overview().await;
    let (description, support) = overview.map_err(|failure| Refusal::failed(failure.message))?;
    let language = description.language.to_string();
    let outputs = json!(description.outputs);
    let (form, form_error) = match Form::new(description, Policy::Strict) {
        Ok(form) => (Some(json!(form.response())), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let (editable, executor) = defaults(backend, request);
    Ok(Described {
        language,
        form,
        form_error,
        support: Some(json!(support)),
        support_error: None,
        outputs,
        editable,
        executor,
    })
}

/// The inputs that a run of `request` may set, `None` for all, and the executor that it uses
/// when it names none: those of its preset, else the default of the config file. A preset
/// that cannot be read lets a run set none of its inputs.
fn defaults(backend: &Backend, request: &LaunchRequest) -> (Option<Vec<String>>, Option<String>) {
    let config = backend.options.config_file.as_deref();
    if let Some(name) = given(&request.preset) {
        let catalogue = crate::catalogue::Catalogue::load(config).ok();
        return match catalogue.and_then(|catalogue| catalogue.editable(name)) {
            Some((editable, executor)) => (Some(editable), executor),
            None => (Some(Vec::new()), None),
        };
    }
    let arguments = crate::arguments::Arguments {
        config_file: config.map(Path::to_owned),
        ..crate::arguments::Arguments::default()
    };
    let configured = crate::setup::Defaults::load(&arguments).ok();
    let executor = configured.and_then(|defaults| defaults.executor);
    (None, executor.or_else(|| Some("local".into())))
}

/// Checks the values of `request` against the form of its workflow, without starting a task.
pub(super) async fn validate(backend: &Backend, request: &LaunchRequest) -> Result<Value, Refusal> {
    let described = RunRequest::new(backend.settings(request)?).describe().await;
    let description = described.map_err(|failure| Refusal::failed(failure.message))?;
    let form =
        Form::new(description, Policy::Strict).map_err(|e| Refusal::failed(e.to_string()))?;
    let values = Value::Object(request.values.clone().unwrap_or_default());
    Ok(json!(form.validate_response(values)))
}

/// Starts a run of `request` on a thread of the backend. It answers once the run is in the run
/// store, or with why it failed before; the request stays with the run for WES.
pub(super) async fn launch(
    backend: &Backend,
    request: LaunchRequest,
) -> Result<RunStarted, Refusal> {
    let mut settings = backend.settings(&request)?;
    if let Some(allowed) = &request.allowed {
        settings.resume = match settings.resume.take() {
            Some(Resume::Run(run)) if !allowed.contains(&run) => {
                return Err(Refusal::missing("no authorized run"));
            }
            Some(Resume::Last) => last_allowed(backend, allowed, &request).await?,
            resume => resume,
        };
    }
    let run = crate::run_id();
    settings.run_id = Some(run.clone());
    let limit = backend.options.run_limit;
    let admitted = backend.registry.admit(&run, request, limit)?;
    settings.cancel = admitted.cancel;
    settings.listener = Some(admitted.listener);
    settings.print_sink = Some(admitted.sink);
    let ending = backend.registry.ending(&run);
    let thread = std::thread::Builder::new()
        .name("uwe-studio-run".into())
        .spawn(move || {
            let error = match RunRequest::new(settings).run_blocking() {
                Ok(ran) => ran.execution.error.map(|error| error.to_string()),
                Err(failure) => Some(failure.message),
            };
            ending.finish(error);
        });
    // A thread that does not start drops its guard, which records the end of the run.
    let thread =
        thread.map_err(|error| Refusal::failed(format!("the run cannot start: {error}")))?;
    backend.registry.keep(&run, thread);
    match admitted.started.await {
        Ok(Err(message)) => Err(Refusal::failed(message)),
        Ok(Ok(())) | Err(_) => Ok(RunStarted::running(run)),
    }
}

/// Starts a run again: with the request that started it in this server, else with the
/// workflow, values, executor and options that the run store recorded. The fields of
/// `changes`, like `executor`, replace those of the run.
pub(super) async fn repeat(
    backend: &Backend,
    run: &str,
    changes: LaunchRequest,
) -> Result<RunStarted, Refusal> {
    let request = match backend.registry.launched(run) {
        Some(launched) => launched.request,
        None => recorded(backend, run).await?,
    };
    let request = LaunchRequest {
        wes: None,
        resume: None,
        ..request
    };
    launch(backend, request.changed(changes)).await
}

/// The last run of the workflow of `request` among the runs that a page for some runs may
/// resume, as `-resume` without a run would take it; `None` starts the run anew.
async fn last_allowed(
    backend: &Backend,
    allowed: &BTreeSet<String>,
    request: &LaunchRequest,
) -> Result<Option<Resume>, Refusal> {
    let Some(target) = given(&request.target) else {
        return Ok(None);
    };
    let workflow = match target.contains("://") {
        true => target.to_owned(),
        false => (std::path::absolute(target).map(|path| path.to_string_lossy().into_owned()))
            .unwrap_or_else(|_| target.to_owned()),
    };
    let simulated = given(&request.executor) == Some("simulator");
    let (store, allowed) = (backend.store()?, allowed.clone());
    let found = tokio::task::spawn_blocking(move || {
        let mut runs = Vec::new();
        for id in &allowed {
            runs.extend(store.run(id)?);
        }
        Ok::<_, run_database::DatabaseError>(runs)
    });
    let runs = (found
        .await
        .map_err(|error| Refusal::failed(error.to_string()))?)
    .map_err(|error| Refusal::failed(error.to_string()))?;
    let last = (runs.into_iter())
        .filter(|run| run.workflow == workflow && run.replays.is_none() && run.state != "running")
        .filter(|run| simulated || !run.simulated)
        .max_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
    Ok(last.map(|run| Resume::Run(run.id)))
}

/// A launch request of a recorded run.
async fn recorded(backend: &Backend, id: &str) -> Result<LaunchRequest, Refusal> {
    let store = backend.store()?;
    let owned = id.to_owned();
    let found = tokio::task::spawn_blocking(move || {
        let run = store.run(&owned)?;
        let inputs = store.run_inputs(&owned)?;
        Ok::<_, run_database::DatabaseError>((run, inputs))
    })
    .await
    .map_err(|error| Refusal::failed(error.to_string()))?;
    let (run, inputs) = found.map_err(|error| Refusal::failed(error.to_string()))?;
    let run = run.ok_or_else(|| Refusal::missing(format!("no run `{id}`")))?;
    if run.state == "running" {
        return Err(Refusal::failed(format!("run {id} is still running")));
    }
    let target = match (Path::new(&run.workflow).is_relative(), &run.launch) {
        (true, Some(launch)) if !run.workflow.contains("://") => Path::new(launch)
            .join(&run.workflow)
            .to_string_lossy()
            .into_owned(),
        _ => run.workflow.clone(),
    };
    let words = run.arguments.get(1..).unwrap_or_default();
    let given = crate::arguments::parse(words).unwrap_or_default();
    let executor = match run.simulated {
        true => "simulator",
        false => run.executor.as_str(),
    };
    let values = match inputs.map(|inputs| inputs.values) {
        Some(Value::Object(values)) => values,
        Some(_) => Map::new(),
        // Without typed values the parameters of the command line are texts.
        None => (given.params.into_iter())
            .map(|(name, value)| (name, Value::String(value)))
            .collect(),
    };
    // The options of the run come back as it recorded them, so that a repeat of a run without
    // the network never gets it.
    Ok(LaunchRequest {
        target: Some(target),
        language: Some(run.language),
        revision: run.revision,
        executor: (crate::setup::EXECUTORS.contains(&executor)).then(|| executor.to_owned()),
        profile: given.profiles.map(Profiles::List),
        entry: given.targets.into_iter().next(),
        values: Some(values),
        no_network: Some(given.no_network),
        no_cache: Some(given.no_cache),
        default_image: given.default_image,
        output_dir: (given.output_dir).map(|folder| folder.to_string_lossy().into_owned()),
        ..LaunchRequest::default()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use run_database::{Change, MemoryStore, RunStart, RunStore};

    use super::*;
    use crate::serve::Options;

    /// A studio on `store` whose runs may write only `outputs` below `root`.
    fn backend(root: &Path, store: Arc<MemoryStore>) -> Backend {
        let mut options = Options::new(root.join("runs.sqlite"), root.join("studio"));
        options.run_store = Some(store);
        let writable = root.join("outputs");
        options.setup = Some(Arc::new(move || crate::setup::Setup {
            isolation: Some(crate::hosted::Isolation {
                roots: vec![writable.clone()],
                ..crate::hosted::Isolation::default()
            }),
            ..crate::setup::Setup::default()
        }));
        Backend::new(options)
    }

    fn record(store: &MemoryStore, id: &str, arguments: &[&str]) {
        let start = RunStart {
            id: id.into(),
            resumes: None,
            workflow: "/work/main.nf".into(),
            language: workflow_graph::Language::Nextflow,
            revision: None,
            arguments: arguments.iter().map(|word| word.to_string()).collect(),
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: None,
            program: None,
            launch: Some("/work".into()),
            workflow_folder: None,
            repeats: None,
        };
        store.begin(&start).unwrap();
        let ended = Change::Ended {
            error: None,
            stopped: false,
        };
        store.apply(id, ended).unwrap();
    }

    #[test]
    fn keeps_outputs_inside() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("outputs")).unwrap();
        let backend = backend(root.path(), Arc::new(MemoryStore::new().unwrap()));
        let request = |folder: &Path| LaunchRequest {
            target: Some("main.nf".into()),
            output_dir: Some(folder.to_string_lossy().into_owned()),
            ..LaunchRequest::default()
        };
        let inside = root.path().join("outputs/run-1");
        assert!(backend.settings(&request(&inside)).is_ok());
        std::os::unix::fs::symlink(root.path(), root.path().join("outputs/back")).unwrap();
        for outside in [
            root.path().join("elsewhere"),
            root.path().join("outputs/../elsewhere"),
            root.path().join("outputs/back/elsewhere"),
            PathBuf::from("relative"),
        ] {
            let refused = backend.settings(&request(&outside)).err();
            let status = refused.map(|refused| refused.status.as_u16());
            assert_eq!(status, Some(400), "{outside:?}");
        }
    }

    #[test]
    fn repeats_recorded_options() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new().unwrap());
        let words = [
            "run",
            "/work/main.nf",
            "-no-network",
            "-no-cache",
            "-default-image",
            "debian:12",
            "-output-dir",
            "/out",
        ];
        record(&store, "offline", &words);
        let backend = backend(root.path(), store);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let request = runtime.block_on(recorded(&backend, "offline")).unwrap();
        assert_eq!(
            (request.no_network, request.no_cache),
            (Some(true), Some(true))
        );
        assert_eq!(request.default_image.as_deref(), Some("debian:12"));
        assert_eq!(request.output_dir.as_deref(), Some("/out"));
    }

    #[test]
    fn resumes_allowed_runs() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new().unwrap());
        record(&store, "mine", &["run", "/work/main.nf"]);
        record(&store, "theirs", &["run", "/work/main.nf"]);
        let backend = backend(root.path(), store);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let request = LaunchRequest {
            target: Some("/work/main.nf".into()),
            ..LaunchRequest::default()
        };
        let allowed = BTreeSet::from(["mine".to_owned()]);
        let last = runtime.block_on(last_allowed(&backend, &allowed, &request));
        assert_eq!(last.unwrap(), Some(Resume::Run("mine".into())));
        let unknown = BTreeSet::from(["unknown".to_owned()]);
        let none = runtime.block_on(last_allowed(&backend, &unknown, &request));
        assert_eq!(none.unwrap(), None);
    }
}

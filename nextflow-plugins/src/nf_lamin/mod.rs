//! nf-lamin 0.9.0: records the run, its transform and its remote input and output files in a
//! LaminDB instance, and hands the `lamin` scope to the provider of `lamin://` paths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod client;
mod settings;
mod tracker;

use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, Event, Observer};
use client::Client;
use settings::Settings;
use tracker::Tracker;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "annotateArtifact",
        "getInstanceSlug",
        "getRunUid",
        "getTransformUid",
    ],
    call: Some(call),
    scopes: &["lamin"],
    observer: Some(activate),
    ..Plugin::new("nf-lamin", Kind::Observer, State::Partial)
};

/// How long the end of the run waits for the requests that are left.
const WAIT: Duration = Duration::from_secs(120);
/// How long a function or the end of the run waits for the run record to exist.
const READY: Duration = Duration::from_secs(60);

/// What the functions of a script share with the worker of their run, by session id.
static RUNS: LazyLock<Mutex<HashMap<String, Arc<Shared>>>> = LazyLock::new(Default::default);

#[derive(Default)]
struct Shared {
    status: Mutex<Status>,
    changed: Condvar,
}

#[derive(Default)]
struct Status {
    /// The worker heard of the run, so the run record comes.
    tracked: bool,
    /// The run record exists, or making it failed.
    ready: bool,
    run_uid: Option<String>,
    transform_uid: Option<String>,
    /// Why the run could not be recorded, which fails the run.
    failure: Option<String>,
    annotations: Vec<Annotation>,
}

/// What `annotateArtifact` adds to the artifact of a file.
#[derive(Clone, Debug, Default, PartialEq)]
struct Annotation {
    path: String,
    kind: Option<String>,
    description: Option<String>,
    labels: Vec<String>,
    projects: Vec<String>,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut Status)) {
        if let Ok(mut status) = self.status.lock() {
            change(&mut status);
            self.changed.notify_all();
        }
    }

    /// The status once the run record exists or failed, waiting at most [`READY`] when the
    /// worker heard of the run.
    fn ready<T>(&self, read: impl FnOnce(&Status) -> T) -> Option<T> {
        let status = self.status.lock().ok()?;
        let waiting = |status: &mut Status| status.tracked && !status.ready;
        let (status, _) = self
            .changed
            .wait_timeout_while(status, READY, waiting)
            .ok()?;
        Some(read(&status))
    }
}

/// The shared state of a run, made by whoever needs it first.
fn shared(session: &str) -> Arc<Shared> {
    let mut runs = RUNS.lock().unwrap_or_else(|poison| poison.into_inner());
    runs.entry(session.to_owned()).or_default().clone()
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    storage::lamin::use_scope(options(config));
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let Some(settings) = Settings::read(config, &variable)? else {
        return Ok(None);
    };
    // The sign-in runs in a thread of its own, outside any runtime of the caller.
    let hub = settings.hub.clone();
    let connected = std::thread::spawn(move || Client::connect(&hub)).join();
    let client = connected
        .map_err(|_| "the LaminDB sign-in stopped".to_owned())?
        .map_err(|error| format!("Could not initialize Lamin run: {error}"))?;
    if settings.dry_run {
        tracing::info!("nf-lamin: Dry-run mode enabled");
        return Ok(None);
    }
    let shared = Arc::new(Shared::default());
    let mut tracker = Tracker::new(settings, client, shared.clone());
    let background = Background::start("nf-lamin", WAIT, move |event| tracker.handle(&event))
        .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(Lamin {
        background,
        shared,
        session: None,
    })))
}

/// The run observer: shares the run with the functions of its script.
struct Lamin {
    background: Background,
    shared: Arc<Shared>,
    session: Option<String>,
}

impl Observer for Lamin {
    fn notify(&mut self, event: &Event) {
        if let Event::RunStarted(start) = event {
            let mut runs = RUNS.lock().unwrap_or_else(|poison| poison.into_inner());
            let earlier = runs.insert(start.session_id.clone(), self.shared.clone());
            let annotations = earlier.map(|earlier| {
                let mut status = earlier.status.lock().unwrap_or_else(|p| p.into_inner());
                std::mem::take(&mut status.annotations)
            });
            self.shared.update(|status| {
                status.tracked = true;
                status.annotations.extend(annotations.unwrap_or_default());
            });
            self.session = Some(start.session_id.clone());
        }
        self.background.notify(event);
        if let (Event::RunEnded(_), Some(session)) = (event, &self.session) {
            let mut runs = RUNS.lock().unwrap_or_else(|poison| poison.into_inner());
            runs.remove(session);
        }
    }

    /// A run that could not be recorded fails, as the plugin stops it.
    fn check(&mut self) -> Result<(), String> {
        let failure = self.shared.ready(|status| status.failure.clone());
        failure.flatten().map_or(Ok(()), Err)
    }
}

/// The functions of `plugin/nf-lamin`.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let instance = settings::instance(context.config, &variable);
    let session = crate::nf_schema::get(&context.workflow, "sessionId")
        .map(|id| id.to_string())
        .unwrap_or_default();
    match name {
        "getInstanceSlug" => Ok(instance.map_or(Value::Null, Value::string)),
        "getRunUid" | "getTransformUid" => {
            let runs = RUNS.lock().unwrap_or_else(|poison| poison.into_inner());
            let found = runs.get(&session).cloned();
            drop(runs);
            let Some(shared) = instance.and(found) else {
                return Ok(Value::Null);
            };
            let uid = shared.ready(|status| match name {
                "getRunUid" => status.run_uid.clone(),
                _ => status.transform_uid.clone(),
            });
            Ok(uid.flatten().map_or(Value::Null, Value::string))
        }
        "annotateArtifact" => {
            let (named, mut rest) = split(arguments);
            let target = if rest.is_empty() {
                Value::Null
            } else {
                rest.remove(0)
            };
            if instance.is_some() {
                let found = annotations(&target, &named);
                shared(&session).update(|status| status.annotations.extend(found));
            }
            Ok(target)
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-lamin`"),
            span: Default::default(),
        }),
    }
}

/// The named options, which Groovy passes first, and the other arguments.
fn split(arguments: Vec<Value>) -> (Vec<(String, Value)>, Vec<Value>) {
    let mut arguments = arguments.into_iter();
    match arguments.next() {
        Some(options @ Value::Map(_)) => (crate::nf_schema::entries(&options), arguments.collect()),
        Some(first) => (
            Vec::new(),
            std::iter::once(first).chain(arguments).collect(),
        ),
        None => (Vec::new(), Vec::new()),
    }
}

/// One annotation for each file of a path or a list of paths.
fn annotations(target: &Value, named: &[(String, Value)]) -> Vec<Annotation> {
    let option = |key: &str| {
        (named.iter())
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    let text = |key: &str| option(key).filter(|value| !matches!(value, Value::Null));
    let list = |key: &str| option(key).map(settings::texts).unwrap_or_default();
    let mut paths = Vec::new();
    collect(target, &mut paths);
    (paths.into_iter())
        .map(|path| Annotation {
            path,
            kind: text("kind").map(Value::to_string),
            description: text("description").map(Value::to_string),
            labels: list("ulabel_uids"),
            projects: list("project_uids"),
        })
        .collect()
}

fn collect(value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::Path(path) => paths.push(path.uri()),
        Value::List(list) => (list.borrow().items.iter()).for_each(|item| collect(item, paths)),
        Value::Null => {}
        other => paths.push(other.to_string()),
    }
}

fn options(config: &Config) -> storage::lamin::Options {
    let text = |path: &[&str]| {
        (config.get(path))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
            .filter(|text| !text.is_empty())
    };
    let hub = text(&["lamin", "api", "supabase_api_url"]);
    let environment = text(&["lamin", "env"]).or_else(|| std::env::var("LAMIN_ENV").ok());
    if hub.is_none() && environment.as_deref().is_some_and(|name| name != "prod") {
        tracing::warn!(
            "nf-lamin: uwe knows only the prod hub; set lamin.api.supabase_api_url for another"
        );
    }
    storage::lamin::Options {
        api_key: text(&["lamin", "api_key"]),
        hub,
        anon_key: text(&["lamin", "api", "supabase_anon_key"]),
        hub_credentials: text(&["lamin", "features", "manage_s3_credentials"])
            .map(|flag| flag == "true"),
    }
}

#[cfg(test)]
mod tests {
    use nextflow_evaluator::interpreter::Host;

    use super::*;

    struct Quiet;

    impl Host for Quiet {
        fn print(&mut self, _: &str) {}
    }

    #[test]
    fn scope_names_options() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("nextflow.config");
        let text = "lamin {\n    instance = 'demo-org/demo-inst'\n    api_key = 'key'\n    \
                    api {\n        supabase_api_url = 'http://127.0.0.1:1'\n        \
                    supabase_anon_key = 'anon'\n    }\n    \
                    features { manage_s3_credentials = false }\n}\n";
        std::fs::write(&file, text).expect("a config");
        let settings = nextflow_config::Options {
            profiles: None,
            params: Value::map([]),
            project_dir: "/project".into(),
            launch_dir: "/launch".into(),
            work_dir: "/work".into(),
        };
        let config = nextflow_config::load(&[file], &settings, &mut Quiet).expect("loads");
        let found = options(&config);
        assert_eq!(
            (
                found.api_key,
                found.hub,
                found.anon_key,
                found.hub_credentials
            ),
            (
                Some("key".into()),
                Some("http://127.0.0.1:1".into()),
                Some("anon".into()),
                Some(false)
            )
        );
    }
}

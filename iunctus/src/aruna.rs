//! Aruna execution for embedding hosts with an authorized immutable object adapter.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

pub use aruna_executor::ArunaBackend;
use aruna_executor::client::Client;
pub use aruna_executor::client::Object;
pub use aruna_executor::objects::{Objects, Reference, Transfer};
use task_runner::{Runner, RunnerExecutor};

use crate::Failure;
use crate::arguments::Arguments;
use crate::setup::Setup;

pub(crate) fn executor(
    arguments: &Arguments,
    setup: &Setup,
    scratch: &Path,
    run: &str,
) -> Result<RunnerExecutor<ArunaBackend>, Failure> {
    let settings = &setup.executors;
    let required = |value: Option<&String>, key: &str| {
        value
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| {
                Failure::setup(format!(
                    "iunctus: Aruna needs -aruna-{key} or `{key}` in [executor.aruna]"
                ))
            })
    };
    let server = required(
        arguments
            .aruna_server
            .as_ref()
            .or(settings.aruna_server.as_ref()),
        "server",
    )?;
    let group_id = required(
        arguments
            .aruna_group
            .as_ref()
            .or(settings.aruna_group.as_ref()),
        "group",
    )?;
    let output_bucket = required(
        arguments
            .aruna_bucket
            .as_ref()
            .or(settings.aruna_bucket.as_ref()),
        "bucket",
    )?;
    let scope = required(
        arguments
            .aruna_scope
            .as_ref()
            .or(settings.aruna_scope.as_ref()),
        "scope",
    )?;
    let objects = setup.aruna_objects.clone().ok_or_else(|| Failure::setup(
        "iunctus: Aruna needs host-supplied immutable object transfer in Setup.aruna_objects; the native jobs API models no upload or download routes"))?;
    let token = match std::env::var("IUNCTUS_ARUNA_TOKEN") {
        Ok(token) if !token.is_empty() => Some(token),
        _ => settings
            .aruna_token
            .as_ref()
            .map(|secret| {
                if secret.variable().is_none() {
                    return Err(Failure::setup(
                        "iunctus: Aruna token must reference an environment variable",
                    ));
                }
                secret
                    .reveal()
                    .map_err(|error| Failure::setup(format!("iunctus: {error}")))
            })
            .transpose()?,
    };
    let client =
        Client::new(&server, token).map_err(|error| Failure::setup(format!("iunctus: {error}")))?;
    let backend = ArunaBackend {
        client,
        scratch: scratch.join("aruna"),
        scope,
        group_id,
        output_bucket,
        objects,
        configs: Default::default(),
    };
    let runner = Runner::new(
        setup.store(arguments)?,
        backend,
        run,
        crate::runner_settings(setup.storage.clone(), true),
    );
    Ok(RunnerExecutor::new(runner, arguments.default_image.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bound;
    impl Objects for Bound {
        fn write<'a>(&'a self, _: &'a str, _: &'a str, _: &'a Path) -> Transfer<'a, Reference> {
            panic!("executor construction must not upload or submit work")
        }
        fn read<'a>(&'a self, _: &'a Object, _: u64) -> Transfer<'a, Vec<u8>> {
            panic!("executor construction must not download outputs")
        }
    }

    #[test]
    fn embedding_requires_objects_and_cli_settings_win() {
        let root = tempfile::tempdir().unwrap();
        let arguments = Arguments {
            executor: Some("aruna".into()),
            aruna_server: Some("https://selected.example/api/v1".into()),
            aruna_group: Some("selected-group".into()),
            aruna_bucket: Some("selected-bucket".into()),
            aruna_scope: Some("selected-scope".into()),
            store: Some(root.path().join("store").to_string_lossy().into_owned()),
            ..Arguments::default()
        };
        let mut setup = Setup::default();
        setup.executors.aruna_server = Some("https://configured.example/api/v1".into());
        let Err(error) = executor(&arguments, &setup, root.path(), "run") else {
            panic!("missing transfer must fail")
        };
        assert!(error.message.contains("Setup.aruna_objects"));
        setup.aruna_objects = Some(std::sync::Arc::new(Bound));
        let executor = executor(&arguments, &setup, root.path(), "run").unwrap();
        let backend = executor.runner().backend();
        assert_eq!(backend.client.server(), "https://selected.example/api/v1/");
        assert_eq!(backend.group_id, "selected-group");
        assert_eq!(backend.output_bucket, "selected-bucket");
        assert_eq!(backend.scope, "selected-scope");
        let offline = Arguments {
            no_network: true,
            ..arguments
        };
        assert!(crate::run_backend::offline_possible(&offline, &setup).is_err());
    }
}

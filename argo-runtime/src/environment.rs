//! The environment of a container: plain values, fields of the pod as the run makes them up,
//! and the keys of ConfigMaps and Secrets (decision 18).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::Container;
use argo_parser::{Node, Value as Yaml};

use crate::objects::{KeyRef, config_map, config_map_value};
use crate::run::{Failure, Result, Run, scalar_text};

impl Run {
    /// The environment of a container: `envFrom` first, then `env`, which wins, as in
    /// Kubernetes. Fields of the pod get the run's values (`fields`), ConfigMaps come from the
    /// loaded documents, and Secrets give placeholders in a simulated run.
    pub(crate) fn environment(
        &mut self,
        container: &Container,
        fields: &BTreeMap<&'static str, String>,
    ) -> Result<BTreeMap<String, String>> {
        let mut environment = BTreeMap::new();
        if let Some(Node {
            value: Yaml::Sequence(sources),
            ..
        }) = &container.env_from
        {
            for source in sources {
                let prefix = source.get("prefix").map(scalar_text).unwrap_or_default();
                self.environment_from(source, &prefix, &mut environment)?;
            }
        }
        for variable in &container.env {
            let name = &variable.name;
            let Some(from) = &variable.value_from else {
                environment.insert(name.clone(), variable.value.clone().unwrap_or_default());
                continue;
            };
            let what = format!("environment variable {name}");
            if let Some(path) = from
                .get("fieldRef")
                .and_then(|reference| reference.get("fieldPath"))
                .and_then(Node::as_str)
            {
                match fields.get(path) {
                    Some(value) => {
                        environment.insert(name.clone(), value.clone());
                    }
                    None => self.warn(format!(
                        "{what} reads the pod field {path}, which a run without Kubernetes does not have; it is left out"
                    )),
                }
            } else if let Some(reference) = from.get("configMapKeyRef").map(KeyRef::of) {
                let found = config_map_value(&self.library, &self.document.path, &reference);
                match found {
                    Some(value) => {
                        environment.insert(name.clone(), value);
                    }
                    None if reference.optional => {}
                    None => {
                        let value = self.config_map_key(&what, &reference)?;
                        environment.insert(name.clone(), value);
                    }
                }
            } else if let Some(reference) = from.get("secretKeyRef").map(KeyRef::of) {
                if reference.optional && !self.options.assume_inputs {
                    continue;
                }
                let value = self.secret(&what, &reference)?;
                environment.insert(name.clone(), value);
            } else {
                self.warn(format!(
                    "{what} reads a resource of its container, which a run without Kubernetes does not have; it is left out"
                ));
            }
        }
        Ok(environment)
    }

    /// The variables of one `envFrom` entry: every key of a ConfigMap, or of a Secret, whose
    /// keys only a secret store knows.
    fn environment_from(
        &mut self,
        source: &Node,
        prefix: &str,
        environment: &mut BTreeMap<String, String>,
    ) -> Result<()> {
        if let Some(reference) = source.get("configMapRef").map(KeyRef::of) {
            match config_map(&self.library, &self.document.path, &reference.name) {
                Some(entries) => {
                    for (key, value) in entries {
                        environment.insert(format!("{prefix}{key}"), value);
                    }
                }
                None if reference.optional => {}
                None if self.options.assume_inputs => self.warn(format!(
                    "envFrom reads ConfigMap {}, which none of the loaded documents holds; a simulated run leaves it out",
                    reference.name
                )),
                None => {
                    return Err(Failure::error(format!(
                        "envFrom reads ConfigMap {}, which none of the loaded documents (the workflow file, the configs and the repository) holds",
                        reference.name
                    )));
                }
            }
        }
        if let Some(reference) = source.get("secretRef").map(KeyRef::of) {
            if !self.options.assume_inputs && !reference.optional {
                return Err(Failure::error(format!(
                    "envFrom reads Secret {}; uwe has no secret store yet, so a real run cannot give it to a task",
                    reference.name
                )));
            }
            self.warn(format!(
                "envFrom reads Secret {}, whose keys only a secret store knows; a simulated run leaves it out",
                reference.name
            ));
        }
        Ok(())
    }
}

//! Resource templates that manage a PersistentVolumeClaim (decision 18). They run no task: a
//! claim becomes a volume of the run, which the module `volumes` shares between tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::model::Template;
use argo_parser::{Node, Value as Yaml, parse_yaml};

use crate::items::yaml_value;
use crate::node::{NodeOutputs, Phase};
use crate::run::{Result, Run, scalar_text};
use crate::value::Value;

impl Run {
    /// Runs a resource template that manages a PersistentVolumeClaim: `create` and `apply`
    /// make an empty volume of the claim's name, `delete` drops it. Output parameters read the
    /// manifest with `jsonPath`.
    pub(crate) fn start_claim(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let template = &processed.template;
        let Some(resource) = &template.resource else {
            return Ok(());
        };
        let manifest = parse_yaml(&resource.manifest)
            .ok()
            .flatten()
            .unwrap_or_else(|| Node::new(Yaml::Mapping(Vec::new()), Default::default()));
        let name = manifest
            .get("metadata")
            .and_then(|metadata| metadata.get("name"))
            .map(scalar_text)
            .unwrap_or_default();
        let key = format!("claim:{name}");
        if resource.action == "delete" {
            self.volumes.remove(&key);
            self.claims.remove(&name);
        } else {
            self.volumes.entry(key).or_default();
            self.claims.insert(name);
        }
        let outputs = self.resource_outputs(template, &yaml_value(&manifest));
        self.nodes[index].outputs = outputs;
        self.set_phase(index, Phase::Succeeded, String::new());
        Ok(())
    }

    /// The output parameters of a resource template, which read the resource with `jsonPath`.
    pub(crate) fn resource_outputs(
        &mut self,
        template: &Template,
        document: &Value,
    ) -> NodeOutputs {
        let mut outputs = NodeOutputs::default();
        for parameter in &template.outputs.parameters {
            let from = parameter.value_from.as_ref();
            let path = from.map(|from| from.json_path.clone()).unwrap_or_default();
            let found = (!path.is_empty())
                .then(|| {
                    let path = format!("${}", path.trim_start_matches('{').trim_end_matches('}'));
                    crate::jsonpath::get(&path, document).ok()
                })
                .flatten()
                .map(|value| match value.as_str() {
                    Some(text) => text.to_owned(),
                    None => value.to_go_string(),
                });
            let default = from.and_then(|from| from.default.clone());
            let value = match (found, default) {
                (Some(value), _) => value,
                (None, Some(default)) => default.0,
                (None, None) => {
                    self.warn(format!(
                        "output parameter {} of template {} reads the resource with something other than jsonPath; it is empty",
                        parameter.name, template.name
                    ));
                    String::new()
                }
            };
            if !parameter.global_name.is_empty() {
                self.globals.insert(
                    format!("workflow.outputs.parameters.{}", parameter.global_name),
                    value.clone(),
                );
            }
            outputs.parameters.push((parameter.name.clone(), value));
        }
        outputs
    }
}

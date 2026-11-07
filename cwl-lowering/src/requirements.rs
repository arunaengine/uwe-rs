//! The image, network and attributes of a task from the CWL requirements in scope.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Dynamic, RequirementClass};
use wir::{Attributes, Cache, Resources, Task};

use crate::Scope;

/// Sets the image and network of the task from the requirements in scope, and gives the
/// attributes they set. Requirements that change the command or its folder have no lowering.
pub fn apply(scope: &Scope, task: &mut Task) -> Result<Attributes, String> {
    for entry in &scope.requirements {
        match &entry.class {
            RequirementClass::Software { .. } => {
                return Err("a SoftwareRequirement".into());
            }
            class if !known(class) => return Err(format!("the requirement {}", name(class))),
            _ => {}
        }
    }
    if let Some(class) = scope.hints.iter().map(|entry| &entry.class).find(|class| {
        !known(class)
            && !matches!(class, RequirementClass::Software { .. })
            && !matches!(class, RequirementClass::Other { .. })
    }) {
        return Err(format!("the hint {}", name(class)));
    }
    let mut attributes = Attributes::default();
    if let Some(docker) = scope.find(|class| match class {
        RequirementClass::Docker(docker) => Some(docker),
        _ => None,
    }) {
        let pull = docker
            .docker_pull
            .as_ref()
            .or(docker.docker_image_id.as_ref());
        match (pull, &docker.docker_output_directory) {
            (Some(image), None) if docker.docker_file.is_none() && docker.docker_load.is_none() => {
                task.image = Some(image.clone());
            }
            _ => return Err("a DockerRequirement other than dockerPull".into()),
        }
    }
    let value = |setting: &Dynamic<bool>| match setting {
        Dynamic::Value(value) => Ok(*value),
        Dynamic::Expression(_) => Err("a requirement set by an expression".to_owned()),
    };
    if let Some(network) = scope.find(|class| match class {
        RequirementClass::NetworkAccess { network_access } => Some(network_access),
        _ => None,
    }) {
        task.network = value(network)?;
    }
    if let Some(reuse) = scope.find(|class| match class {
        RequirementClass::WorkReuse { enable_reuse } => Some(enable_reuse),
        _ => None,
    }) && !value(reuse)?
    {
        attributes.cache = Some(Cache::Never);
    }
    if let Some(limit) = scope.find(|class| match class {
        RequirementClass::ToolTimeLimit { timelimit } => Some(timelimit),
        _ => None,
    }) {
        match limit {
            Dynamic::Value(seconds) if *seconds > 0 => attributes.time = Some(*seconds as u64),
            Dynamic::Value(0) => {}
            // CWL refuses a negative limit when the tool runs; the native op keeps that.
            Dynamic::Value(_) => return Err("a negative time limit".into()),
            Dynamic::Expression(_) => return Err("a time limit set by an expression".into()),
        }
    }
    if let Some(resource) = scope.find(|class| match class {
        RequirementClass::Resource(resource) => Some(resource),
        _ => None,
    }) {
        let number = |setting: &Option<Dynamic<f64>>| match setting {
            Some(Dynamic::Value(value)) => Ok(Some(*value)),
            Some(Dynamic::Expression(_)) => Err("a resource set by an expression".to_owned()),
            None => Ok(None),
        };
        let cpus = number(&resource.cores_min)?.map(|cores| cores.ceil().max(1.0) as u32);
        let memory = number(&resource.ram_min)?.map(|mebibytes| (mebibytes * 1_048_576.0) as u64);
        if cpus.is_some() || memory.is_some() {
            attributes.resources = Some(Resources {
                cpus,
                memory,
                ..Resources::default()
            });
        }
    }
    Ok(attributes)
}

/// Requirements that a task can follow: they set its image, network or attributes, or
/// change nothing without expressions.
fn known(class: &RequirementClass) -> bool {
    matches!(
        class,
        RequirementClass::InlineJavascript { .. }
            | RequirementClass::LoadListing { .. }
            | RequirementClass::Docker(_)
            | RequirementClass::Resource(_)
            | RequirementClass::WorkReuse { .. }
            | RequirementClass::NetworkAccess { .. }
            | RequirementClass::ToolTimeLimit { .. }
            | RequirementClass::SubworkflowFeature
            | RequirementClass::ScatterFeature
            | RequirementClass::MultipleInputFeature
            | RequirementClass::StepInputExpression
    )
}

fn name(class: &RequirementClass) -> String {
    let text = format!("{class:?}");
    let end = text.find([' ', '(', '{']).unwrap_or(text.len());
    format!("{}Requirement", &text[..end])
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// The scope of a fixture tool, and the task that its requirements set.
    fn applied(name: &str) -> (Result<Attributes, String>, Task) {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let document = cwl_parser::load_file(folder.join(name), Default::default()).unwrap();
        let process = &document.process;
        let scope = Scope::default().with(&process.requirements, &process.hints);
        let mut task = Task {
            image: None,
            command: Vec::new(),
            captures: Vec::new(),
            success_codes: Vec::new(),
            network: false,
            stdout: None,
            stderr: None,
        };
        (apply(&scope, &mut task), task)
    }

    #[test]
    fn sets_image_resources() {
        let (attributes, task) = applied("count.cwl");
        let resources = Resources {
            cpus: Some(2),
            memory: Some(64 * 1_048_576),
            ..Resources::default()
        };
        assert_eq!(attributes.unwrap().resources, Some(resources));
        assert_eq!(
            task.image.as_deref(),
            Some("docker.io/library/debian:12-slim")
        );
        assert!(!task.network, "a v1.2 tool has no network");
    }

    #[test]
    fn refuses_environment_variables() {
        let (attributes, _) = applied("environment.cwl");
        assert_eq!(attributes.unwrap_err(), "the requirement EnvVarRequirement");
    }
}

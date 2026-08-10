//! Conda environment files, like `environment.yaml` of a Snakemake rule or of an nf-core
//! module: a name, the channels, the packages as match specs, and packages of `pip`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::{Node, Value, parse_yaml};

use crate::spec::{MatchSpec, SpecError};

/// A conda environment file.
#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
    pub name: Option<String>,
    /// The channels, in the order the file gives them, which is their priority.
    pub channels: Vec<String>,
    pub dependencies: Vec<MatchSpec>,
    /// The packages that `pip` installs, as the file gives them.
    pub pip: Vec<String>,
}

/// An environment file that cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    #[error("line {line}: {message}")]
    Syntax { line: u32, message: String },
    #[error("line {line}: {message}")]
    Shape { line: u32, message: String },
    #[error("line {line}: {error}")]
    Spec { line: u32, error: SpecError },
}

impl Environment {
    pub fn parse(text: &str) -> Result<Environment, EnvironmentError> {
        let root = parse_yaml(text).map_err(|error| EnvironmentError::Syntax {
            line: error.position.line,
            message: error.message,
        })?;
        let Some(root) = root.filter(|root| matches!(root.value, Value::Mapping(_))) else {
            return Err(shape(
                1,
                "the file holds no mapping of `channels` and `dependencies`",
            ));
        };
        let mut environment = Environment {
            name: root.get("name").and_then(scalar),
            channels: Vec::new(),
            dependencies: Vec::new(),
            pip: Vec::new(),
        };
        if let Some(channels) = root.get("channels") {
            for channel in items(channels, "channels")? {
                let Some(name) = scalar(channel) else {
                    return Err(shape(channel.position.line, "a channel is no name"));
                };
                environment.channels.push(name);
            }
        }
        let Some(dependencies) = root.get("dependencies") else {
            return Ok(environment);
        };
        for dependency in items(dependencies, "dependencies")? {
            let line = dependency.position.line;
            if let Some(pip) = dependency.get("pip") {
                for package in items(pip, "pip")? {
                    environment.pip.extend(scalar(package));
                }
                continue;
            }
            let Some(text) = scalar(dependency) else {
                return Err(shape(line, "a dependency is no match spec"));
            };
            let spec =
                MatchSpec::parse(&text).map_err(|error| EnvironmentError::Spec { line, error })?;
            environment.dependencies.push(spec);
        }
        Ok(environment)
    }
}

/// The items of a sequence; an empty value has none, and a single text is one item.
fn items<'a>(node: &'a Node, key: &str) -> Result<&'a [Node], EnvironmentError> {
    match &node.value {
        Value::Sequence(items) => Ok(items),
        Value::Null => Ok(&[]),
        Value::String(_) => Ok(std::slice::from_ref(node)),
        _ => Err(shape(
            node.position.line,
            &format!("`{key}` is {}, not a list", node.describe()),
        )),
    }
}

/// A scalar as text, like a version that YAML read as a number.
fn scalar(node: &Node) -> Option<String> {
    match &node.value {
        Value::String(text) => Some(text.clone()),
        Value::Integer(number) => Some(number.to_string()),
        Value::Unsigned(number) => Some(number.to_string()),
        Value::Float(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn shape(line: u32, message: &str) -> EnvironmentError {
    EnvironmentError::Shape {
        line,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environments_give_their_channels_packages_and_pip_packages() {
        let text = "name: qc\nchannels:\n  - conda-forge\n  - bioconda\n  - nodefaults\n\
                    dependencies:\n  - bioconda::fastqc=0.12.1\n  - python>=3.9\n  - pip\n  \
                    - pip:\n      - yatiml==0.7\nprefix: /home/me/envs/qc\n";
        let environment = Environment::parse(text).expect("an environment");
        assert_eq!(environment.name.as_deref(), Some("qc"));
        assert_eq!(
            environment.channels,
            ["conda-forge", "bioconda", "nodefaults"]
        );
        let names: Vec<&str> = (environment.dependencies.iter())
            .map(|spec| spec.name.as_str())
            .collect();
        assert_eq!(names, ["fastqc", "python", "pip"]);
        assert_eq!(environment.pip, ["yatiml==0.7"]);
    }

    #[test]
    fn broken_environments_name_their_line() {
        let error = Environment::parse("dependencies:\n  - samtools=1.1\n  - bad name!\n");
        let error = error.expect_err("an error").to_string();
        assert!(error.starts_with("line 3:"), "{error}");
        let error = Environment::parse("- a\n- b\n")
            .expect_err("an error")
            .to_string();
        assert!(error.contains("no mapping"), "{error}");
        let error = Environment::parse("channels: {a: 1}\n").expect_err("an error");
        assert!(error.to_string().contains("not a list"), "{error}");
        let single = Environment::parse("channels: conda-forge\n").expect("an environment");
        assert_eq!(single.channels, ["conda-forge"]);
    }
}

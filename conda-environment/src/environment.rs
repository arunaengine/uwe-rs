//! Conda environment files, like `environment.yaml` of a Snakemake rule or of an nf-core
//! module: a name, the channels, the packages as match specs, and packages of `pip`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use saphyr::{LoadableYamlNode, MarkedYaml as Node, Scalar, YamlData as Value};

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
        let documents = Node::load_from_str(text).map_err(|error| EnvironmentError::Syntax {
            line: error.marker().line() as u32,
            message: error.info().to_owned(),
        })?;
        let Some(root) = documents
            .first()
            .filter(|root| matches!(root.data, Value::Mapping(_)))
        else {
            return Err(shape(
                1,
                "the file holds no mapping of `channels` and `dependencies`",
            ));
        };
        let mut environment = Environment {
            name: root.data.as_mapping_get("name").and_then(scalar),
            channels: Vec::new(),
            dependencies: Vec::new(),
            pip: Vec::new(),
        };
        if let Some(channels) = root.data.as_mapping_get("channels") {
            for channel in items(channels, "channels")? {
                let Some(name) = scalar(channel) else {
                    return Err(shape(
                        channel.span.start.line() as u32,
                        "a channel is no name",
                    ));
                };
                environment.channels.push(name);
            }
        }
        let Some(dependencies) = root.data.as_mapping_get("dependencies") else {
            return Ok(environment);
        };
        for dependency in items(dependencies, "dependencies")? {
            let line = dependency.span.start.line() as u32;
            if let Some(pip) = dependency.data.as_mapping_get("pip") {
                for package in items(pip, "pip")? {
                    let requirement = scalar(package).ok_or_else(|| {
                        shape(
                            package.span.start.line() as u32,
                            "a pip requirement is no text",
                        )
                    })?;
                    environment.pip.push(requirement);
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
fn items<'a, 'input>(
    node: &'a Node<'input>,
    key: &str,
) -> Result<&'a [Node<'input>], EnvironmentError> {
    match &node.data {
        Value::Sequence(items) => Ok(items),
        Value::Value(Scalar::Null) => Ok(&[]),
        Value::Value(Scalar::String(_)) => Ok(std::slice::from_ref(node)),
        _ => Err(shape(
            node.span.start.line() as u32,
            &format!(
                "`{key}` is {}, not a list",
                match node.data {
                    Value::Mapping(_) => "a mapping",
                    _ => "a scalar",
                }
            ),
        )),
    }
}

/// A scalar as text, like a version that YAML read as a number.
fn scalar(node: &Node) -> Option<String> {
    match &node.data {
        Value::Value(Scalar::String(text)) => Some(text.to_string()),
        Value::Value(Scalar::Integer(number)) => Some(number.to_string()),
        Value::Value(Scalar::FloatingPoint(number)) => Some(number.to_string()),
        Value::Value(Scalar::Boolean(flag)) => Some(flag.to_string()),
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
    fn parses_environment_dependencies() {
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
    fn reports_environment_lines() {
        let error = Environment::parse("dependencies:\n  - samtools=1.1\n  - b:c=1\n");
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
        assert!(
            Environment::parse("dependencies:\n  - pip:\n    - {invalid: requirement}\n").is_err()
        );
    }
}

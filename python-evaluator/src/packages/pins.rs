//! The version pins of Python packages in a workflow's own environment file, like
//! `yatiml==0.7` in an `environment.yaml` or a `requirements.txt` that lists snakemake. The
//! Python container installs a pinned package in that version (decision 19).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::value::Value;

/// A package name as PyPI compares names: in lower case, with `-` for runs of `-`, `_`
/// and `.`.
pub fn normalized(name: &str) -> String {
    let mut text = String::with_capacity(name.len());
    for part in name
        .to_lowercase()
        .split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
    {
        if !text.is_empty() {
            text.push('-');
        }
        text.push_str(part);
    }
    text
}

/// The name of the package of a requirement, like `yatiml` of `yatiml==0.7`.
pub fn name_of(requirement: &str) -> &str {
    let end = requirement
        .find(|c: char| "=<>!~;[ @".contains(c))
        .unwrap_or(requirement.len());
    requirement[..end].trim()
}

/// A requirement of pip as uv takes it; `None` for one that is no plain name and version,
/// like a URL.
fn pip_requirement(line: &str) -> Option<String> {
    let line = line.split('#').next().unwrap_or_default().trim();
    if line.is_empty() || line.starts_with('-') || line.contains("://") || line.contains(" @ ") {
        return None;
    }
    let name = name_of(line);
    (!name.is_empty() && !name.contains('/')).then(|| line.to_owned())
}

/// A conda dependency as a requirement of pip: `tabulate=0.8.10` as `tabulate==0.8.10`,
/// without its channel and build.
fn conda_requirement(dependency: &str) -> Option<String> {
    let dependency = dependency.rsplit("::").next().unwrap_or(dependency).trim();
    let name = name_of(dependency);
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let rest = dependency[name.len()..].trim();
    if rest.is_empty() {
        return Some(name.to_owned());
    }
    if let Some(version) = rest.strip_prefix('=')
        && !version.starts_with('=')
    {
        // `name=1.2` and `name=1.2=build`.
        let version = version.split('=').next().unwrap_or_default();
        return Some(format!("{name}=={version}"));
    }
    if rest.starts_with(['=', '<', '>', '!', '~']) {
        return Some(format!(
            "{name}{}",
            rest.split_whitespace().collect::<String>()
        ));
    }
    // `name 1.2`, as conda also writes a version.
    Some(format!(
        "{name}=={}",
        rest.split_whitespace().next().unwrap_or_default()
    ))
}

/// The requirements of an environment file: a conda `environment.yaml`, with its pip
/// section, or a pip `requirements.txt`. Only a file that lists snakemake is the workflow's
/// own environment; others give none.
pub fn requirements(file_name: &str, text: &str) -> Vec<String> {
    let requirements: Vec<String> = if file_name.ends_with(".yaml") || file_name.ends_with(".yml") {
        let Ok(document) = crate::modules::data::load_yaml(text) else {
            return Vec::new();
        };
        let Value::Dict(document) = document.plain() else {
            return Vec::new();
        };
        let dependencies = document.borrow().get("dependencies").cloned();
        let Some(Value::List(dependencies)) = dependencies else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for dependency in dependencies.borrow().iter() {
            match dependency.plain() {
                Value::Str(text) => found.extend(conda_requirement(text)),
                Value::Dict(section) => {
                    if let Some(Value::List(pip)) = section.borrow().get("pip").cloned() {
                        for requirement in pip.borrow().iter() {
                            if let Some(text) = requirement.as_str() {
                                found.extend(pip_requirement(text));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        found
    } else {
        text.lines().filter_map(pip_requirement).collect()
    };
    let names_snakemake = requirements
        .iter()
        .any(|requirement| normalized(name_of(requirement)) == "snakemake");
    if names_snakemake {
        requirements
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{normalized, requirements};

    #[test]
    fn conda_environments_give_their_pins() {
        let text = "channels:\n  - conda-forge\ndependencies:\n  - snakemake=6.15.3\n  - bioconda::pyfaidx>=0.5.8\n  - jq 1.6\n  - pip\n  - pip:\n      - yatiml==0.7\n      - git+https://example.org/x.git\n";
        assert_eq!(
            requirements("environment.yaml", text),
            [
                "snakemake==6.15.3",
                "pyfaidx>=0.5.8",
                "jq==1.6",
                "pip",
                "yatiml==0.7"
            ]
        );
        assert!(requirements("environment.yml", "dependencies:\n  - samtools=1.9\n").is_empty());
    }

    #[test]
    fn requirement_files_give_their_pins() {
        let text = "# tools\nsnakemake==9.0\nYaTiML==0.7  # the old loader\n-r other.txt\npandas\n";
        assert_eq!(
            requirements("requirements.txt", text),
            ["snakemake==9.0", "YaTiML==0.7", "pandas"]
        );
        assert_eq!(normalized("Ruamel_Yaml"), normalized("ruamel.yaml"));
    }
}

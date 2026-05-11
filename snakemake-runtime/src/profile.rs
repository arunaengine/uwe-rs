//! The workflow profile: settings in `profiles/default/profile.yaml` (or `config.yaml`) that
//! Snakemake reads by itself as default command-line options. The ones that change how the
//! workflow is planned are kept: config files and values, default resources, and resources
//! and threads per rule.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use python_evaluator::modules::data::load_yaml;
use python_evaluator::{Error, Interpreter, Value};

use crate::workflow::workflow_error;

#[derive(Clone, Default)]
pub struct Profile {
    pub configfiles: Vec<PathBuf>,
    /// `config:` as `key=value` texts, as `--config` takes them.
    pub config: Vec<(String, String)>,
    /// `default-resources:` as resource names and expressions.
    pub default_resources: Vec<(String, String)>,
    /// `set-resources:` as rule, resource and value.
    pub set_resources: Vec<(String, String, String)>,
    /// `set-threads:` as rule and value.
    pub set_threads: Vec<(String, String)>,
}

/// The folder of the workflow profile, as Snakemake looks for it: `profiles/default` in the
/// working directory, else next to the Snakefile.
pub fn find(workdir: &Path, snakefile: &Path) -> Option<PathBuf> {
    let relative = Path::new("profiles").join("default");
    [
        workdir.join("default"),
        workdir.join(&relative),
        snakefile
            .parent()
            .map(|folder| folder.join(&relative))
            .unwrap_or_default(),
    ]
    .into_iter()
    .find(|candidate| candidate.is_dir() && profile_file(candidate).is_some())
}

/// The settings file of a profile folder: `profile.yaml`, else `config.yaml`.
fn profile_file(folder: &Path) -> Option<PathBuf> {
    ["profile.yaml", "config.yaml"]
        .iter()
        .map(|name| folder.join(name))
        .find(|path| path.is_file())
}

/// A value as a command-line option gives it: text of a scalar, which becomes a path in the
/// profile folder when a file of that name is there.
fn option_text(
    interpreter: &mut Interpreter,
    folder: &Path,
    value: &Value,
) -> Result<String, Error> {
    let text = python_evaluator::host::expand_home(&interpreter.to_str(value)?);
    let in_folder = folder.join(&text);
    Ok(if in_folder.exists() {
        in_folder.to_string_lossy().into_owned()
    } else {
        text
    })
}

/// A value of a mapping as the profile parser writes it for `key=value`: texts that look like
/// numbers stay texts by their quotes.
fn value_text(interpreter: &mut Interpreter, value: &Value) -> Result<String, Error> {
    match value.plain() {
        Value::Str(text) if text.parse::<f64>().is_ok() => Ok(interpreter.repr(value)?),
        _ => interpreter.to_str(value),
    }
}

/// `key=value` pairs from a mapping, or from a list of such texts.
fn pairs(interpreter: &mut Interpreter, value: &Value) -> Result<Vec<(String, String)>, Error> {
    let mut result = Vec::new();
    if let Value::Dict(_) = value.plain() {
        for (key, item) in interpreter.mapping_items(value)? {
            result.push((interpreter.to_str(&key)?, value_text(interpreter, &item)?));
        }
        return Ok(result);
    }
    for item in interpreter.iterate(value)? {
        let text = interpreter.to_str(&item)?;
        let (key, value) = text.split_once('=').ok_or_else(|| {
            workflow_error(format!(
                "Invalid entry {text} in the profile: expected name=value"
            ))
        })?;
        result.push((key.trim().to_owned(), value.trim().to_owned()));
    }
    Ok(result)
}

impl Profile {
    /// Reads the settings of a profile folder.
    pub fn load(interpreter: &mut Interpreter, folder: &Path) -> Result<Profile, Error> {
        let mut profile = Profile::default();
        let Some(file) = profile_file(folder) else {
            return Ok(profile);
        };
        let text = std::fs::read_to_string(&file)
            .map_err(|error| Error::os_error(&error, &file.to_string_lossy()))?;
        let settings = load_yaml(&text)?;
        if !matches!(settings.plain(), Value::Dict(_)) {
            return Ok(profile);
        }
        for (key, value) in interpreter.mapping_items(&settings)? {
            if value.is_none() {
                continue;
            }
            match interpreter.to_str(&key)?.as_str() {
                "configfile" | "configfiles" => {
                    let files = match value.plain() {
                        Value::List(_) => interpreter.iterate(&value)?,
                        _ => vec![value.clone()],
                    };
                    for file in files {
                        profile
                            .configfiles
                            .push(option_text(interpreter, folder, &file)?.into());
                    }
                }
                "config" => profile.config = pairs(interpreter, &value)?,
                "default-resources" | "default-res" => {
                    profile.default_resources = pairs(interpreter, &value)?
                }
                "set-threads" => profile.set_threads = pairs(interpreter, &value)?,
                "set-resources" => {
                    if let Value::Dict(_) = value.plain() {
                        for (rule, resources) in interpreter.mapping_items(&value)? {
                            let rule = interpreter.to_str(&rule)?;
                            for (name, amount) in pairs(interpreter, &resources)? {
                                profile.set_resources.push((rule.clone(), name, amount));
                            }
                        }
                    } else {
                        for (target, amount) in pairs(interpreter, &value)? {
                            let (rule, name) = target.split_once(':').ok_or_else(|| {
                                workflow_error(format!(
                                    "Invalid entry {target}={amount} of set-resources in the profile: expected rule:resource=value"
                                ))
                            })?;
                            profile
                                .set_resources
                                .push((rule.to_owned(), name.to_owned(), amount));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workflow_profile_gives_config_and_resources() {
        let folder = tempfile::tempdir().expect("a folder");
        let profile_folder = folder.path().join("profiles/default");
        std::fs::create_dir_all(&profile_folder).expect("the profile folder");
        std::fs::write(profile_folder.join("extra.yaml"), "a: 1\n").expect("a config file");
        std::fs::write(
            profile_folder.join("config.yaml"),
            "configfile: extra.yaml\nconfig:\n  n: '2'\ndefault-resources:\n  mem_mb: 500\nset-resources:\n  align:\n    runtime: 30\nset-threads:\n  align: 4\ncores: 3\n",
        )
        .expect("the profile");
        let snakefile = folder.path().join("workflow/Snakefile");
        let found = find(folder.path(), &snakefile).expect("the profile is found");
        let mut interpreter = Interpreter::new(std::rc::Rc::new(python_evaluator::LocalHost::new(
            folder.path(),
        )));
        let profile = Profile::load(&mut interpreter, &found).expect("the profile loads");
        assert_eq!(profile.configfiles, [profile_folder.join("extra.yaml")]);
        assert_eq!(profile.config, [("n".to_owned(), "'2'".to_owned())]);
        assert_eq!(
            profile.default_resources,
            [("mem_mb".to_owned(), "500".to_owned())]
        );
        assert_eq!(
            profile.set_resources,
            [("align".to_owned(), "runtime".to_owned(), "30".to_owned())]
        );
        assert_eq!(profile.set_threads, [("align".to_owned(), "4".to_owned())]);
    }
}

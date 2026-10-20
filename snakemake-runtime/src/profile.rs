//! Loads workflow profile defaults that affect config, resources, threads, and job planning.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use python_evaluator::modules::data::load_yaml;
use python_evaluator::modules::render_yte;
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
    pub retries: Option<u32>,
    pub rerun_triggers: Option<Vec<String>>,
    pub storage_provider: Option<String>,
    pub storage_prefix: Option<String>,
    /// `cores:`, which caps the threads of each job, and run locally the threads of all.
    pub cores: Option<i64>,
    /// `jobs:`, the jobs that may run at once.
    pub jobs: Option<i64>,
    pub max_threads: Option<i64>,
    /// `resources:`, the global resources that running jobs share.
    pub resources: Vec<(String, String)>,
    /// `executor:`, which decides whether `jobs` stands for `cores`.
    pub executor: Option<String>,
    /// `all-temp:`, which marks every output of every rule `temp()`.
    pub all_temp: bool,
    /// `notemp:`, which keeps the outputs marked `temp()`.
    pub notemp: bool,
    /// `wrapper-prefix:`, where the wrappers that rules name by their path are.
    pub wrapper_prefix: Option<String>,
    /// `groups:` as rule and group, and `group-components:` as group and how many connected
    /// parts one job of the group takes.
    pub groups: Vec<(String, String)>,
    pub group_components: Vec<(String, usize)>,
    /// `prioritize:`, the targets whose jobs start first.
    pub prioritize: Vec<String>,
    /// `set-scatter:` as name and number of items.
    pub set_scatter: Vec<(String, i64)>,
    /// `consider-ancient:` as rule and its input items, by name or position.
    pub consider_ancient: Vec<(String, Vec<String>)>,
    /// `allowed-rules:`, the only rules that may make files, when given.
    pub allowed_rules: Option<Vec<String>>,
    /// `batch:` as rule, batch and number of batches.
    pub batch: Option<(String, usize, usize)>,
}

pub const RERUN_TRIGGERS: [&str; 5] = ["mtime", "params", "input", "software-env", "code"];

/// The executor plugins of Snakemake, whose settings start with their name and a hyphen.
const EXECUTOR_PLUGINS: &[&str] = &[
    "slurm",
    "slurm-jobstep",
    "lsf",
    "googlebatch",
    "kubernetes",
    "tes",
    "aws-batch",
    "azure-batch",
    "flux",
    "htcondor",
    "drmaa",
    "pcluster",
    "cluster-generic",
    "cluster-sync",
];

pub fn validate_triggers(triggers: &[String]) -> Result<(), Error> {
    if let Some(trigger) = triggers
        .iter()
        .find(|trigger| !RERUN_TRIGGERS.contains(&trigger.as_str()))
    {
        return Err(workflow_error(format!(
            "Invalid rerun trigger '{trigger}'; expected mtime, params, input, software-env or code"
        )));
    }
    Ok(())
}

/// The folder of the workflow profile, as Snakemake looks for it: `profiles/default` in the
/// working directory, else next to the Snakefile.
pub fn find(workdir: &Path, snakefile: &Path) -> Option<PathBuf> {
    find_named(workdir, snakefile, "default")
}

/// A workflow profile by name, as Snakemake looks for `--workflow-profile <name>`: the folder
/// of that name in the working directory, else in its `profiles` folder, else in the
/// `profiles` folder next to the Snakefile.
pub fn find_named(workdir: &Path, snakefile: &Path, name: &str) -> Option<PathBuf> {
    let relative = Path::new("profiles").join(name);
    [
        workdir.join(name),
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

/// A whole number of a setting, given as a number or as text, as the command line takes it.
fn count(value: &Value) -> Option<i64> {
    match value.plain() {
        Value::Str(text) => text.trim().parse().ok(),
        other => other.as_int(),
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

/// The words of a setting given as a list or as text with spaces, like `prioritize`.
fn words(interpreter: &mut Interpreter, value: &Value) -> Result<Vec<String>, Error> {
    if let Some(text) = value.as_str() {
        return Ok(text.split_whitespace().map(str::to_owned).collect());
    }
    let items = interpreter.iterate(value)?;
    items.iter().map(|item| interpreter.to_str(item)).collect()
}

/// `RULE=BATCH/BATCHES` of `batch`, as Snakemake's `parse_batch` takes it.
fn batch(text: &str) -> Result<(String, usize, usize), Error> {
    let message = "Invalid batch definition: batch entry has to be defined as RULE=BATCH/BATCHES (with integers BATCH <= BATCHES, BATCH >= 1).";
    let invalid = || workflow_error(message);
    let (rule, definition) = text.split_once('=').ok_or_else(invalid)?;
    let (number, total) = definition.split_once('/').ok_or_else(invalid)?;
    let number: usize = number.trim().parse().map_err(|_| invalid())?;
    let total: usize = total.trim().parse().map_err(|_| invalid())?;
    if number < 1 || number > total {
        return Err(invalid());
    }
    Ok((rule.trim().to_owned(), number, total))
}

impl Profile {
    /// The settings of profiles given at launch, like `--profile` and `--workflow-profile`: a
    /// folder or a settings file each. A later profile replaces the settings it has.
    pub fn given(interpreter: &mut Interpreter, paths: &[PathBuf]) -> Result<Profile, Error> {
        let mut merged = Profile::default();
        for path in paths {
            let (folder, file) = if path.is_file() {
                (
                    path.parent().unwrap_or(Path::new("")).to_path_buf(),
                    Some(path.clone()),
                )
            } else {
                (path.clone(), profile_file(path))
            };
            let Some(file) = file else {
                return Err(workflow_error(format!(
                    "The profile {} has no profile.yaml or config.yaml",
                    path.display()
                )));
            };
            let profile = Profile::read(interpreter, &folder, &file)?;
            merged.merge(profile);
        }
        Ok(merged)
    }

    /// Takes the settings that another profile has.
    fn merge(&mut self, other: Profile) {
        if !other.configfiles.is_empty() {
            self.configfiles = other.configfiles;
        }
        if !other.config.is_empty() {
            self.config = other.config;
        }
        if !other.default_resources.is_empty() {
            self.default_resources = other.default_resources;
        }
        if !other.set_resources.is_empty() {
            self.set_resources = other.set_resources;
        }
        if !other.set_threads.is_empty() {
            self.set_threads = other.set_threads;
        }
        if other.retries.is_some() {
            self.retries = other.retries;
        }
        if other.rerun_triggers.is_some() {
            self.rerun_triggers = other.rerun_triggers;
        }
        if other.storage_provider.is_some() {
            self.storage_provider = other.storage_provider;
        }
        if other.storage_prefix.is_some() {
            self.storage_prefix = other.storage_prefix;
        }
        if other.cores.is_some() {
            self.cores = other.cores;
        }
        if other.jobs.is_some() {
            self.jobs = other.jobs;
        }
        if other.max_threads.is_some() {
            self.max_threads = other.max_threads;
        }
        if !other.resources.is_empty() {
            self.resources = other.resources;
        }
        if other.executor.is_some() {
            self.executor = other.executor;
        }
        self.all_temp = self.all_temp || other.all_temp;
        self.notemp = self.notemp || other.notemp;
        if other.wrapper_prefix.is_some() {
            self.wrapper_prefix = other.wrapper_prefix;
        }
        if !other.groups.is_empty() {
            self.groups = other.groups;
        }
        if !other.group_components.is_empty() {
            self.group_components = other.group_components;
        }
        if !other.prioritize.is_empty() {
            self.prioritize = other.prioritize;
        }
        if !other.set_scatter.is_empty() {
            self.set_scatter = other.set_scatter;
        }
        if !other.consider_ancient.is_empty() {
            self.consider_ancient = other.consider_ancient;
        }
        if other.allowed_rules.is_some() {
            self.allowed_rules = other.allowed_rules;
        }
        if other.batch.is_some() {
            self.batch = other.batch;
        }
    }

    /// The global resources of the run as Snakemake's command line sets them: `_cores` and
    /// `_nodes`, then `resources:`. A local run without `cores` takes `jobs` as its cores.
    pub fn global_resources(&self) -> Vec<(String, Value)> {
        let local = (self.executor.as_deref()).is_none_or(|name| name == "local");
        let (cores, nodes) = match (self.cores, self.jobs) {
            (None, Some(jobs)) if local => (Some(jobs), None),
            given => given,
        };
        let mut resources = Vec::new();
        if let Some(cores) = cores {
            resources.push(("_cores".to_owned(), Value::Int(cores)));
        }
        if let Some(nodes) = nodes {
            resources.push(("_nodes".to_owned(), Value::Int(nodes)));
        }
        for (name, value) in &self.resources {
            resources.push((name.clone(), crate::config::parse_value(value)));
        }
        resources
    }

    /// Reads the settings of a profile folder.
    pub fn load(interpreter: &mut Interpreter, folder: &Path) -> Result<Profile, Error> {
        match profile_file(folder) {
            Some(file) => Profile::read(interpreter, folder, &file),
            None => Ok(Profile::default()),
        }
    }

    /// Reads a settings file of a profile, whose paths are relative to the folder.
    fn read(interpreter: &mut Interpreter, folder: &Path, file: &Path) -> Result<Profile, Error> {
        let mut profile = Profile::default();
        let text = std::fs::read_to_string(file)
            .map_err(|error| Error::os_error(&error, &file.to_string_lossy()))?;
        // Snakemake renders a profile that sets `__use_yte__: true` as a yte template.
        let settings = render_yte(interpreter, load_yaml(&text)?, None, true)?;
        if !matches!(settings.plain(), Value::Dict(_)) {
            return Err(workflow_error(
                "A profile must contain a mapping of settings",
            ));
        }
        for (key, value) in interpreter.mapping_items(&settings)? {
            if value.is_none() {
                continue;
            }
            let pending = [
                "wrapper-prefix",
                "groups",
                "group-components",
                "prioritize",
                "P",
                "set-scatter",
                "consider-ancient",
                "allowed-rules",
                "batch",
            ];
            if pending.contains(&interpreter.to_str(&key)?.as_str()) {
                return Err(Error::unsupported(format!(
                    "Snakemake profile setting '{}'",
                    interpreter.to_str(&key)?
                )));
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
                "default-storage-provider" => {
                    let provider = interpreter.to_str(&value)?;
                    crate::storage::check_provider(Some(&provider))?;
                    profile.storage_provider = Some(provider);
                }
                "default-storage-prefix" => {
                    profile.storage_prefix = Some(interpreter.to_str(&value)?)
                }
                "default-resources" | "default-res" => {
                    profile.default_resources = pairs(interpreter, &value)?
                }
                "set-threads" => profile.set_threads = pairs(interpreter, &value)?,
                "cores" => {
                    let message =
                        "Error parsing number of cores (--cores, -c): must be integer or 'all'.";
                    profile.cores = Some(match value.as_str() {
                        Some("all") => std::thread::available_parallelism()
                            .map_or(1, |count| count.get() as i64),
                        _ => count(&value).ok_or_else(|| workflow_error(message))?,
                    });
                }
                "jobs" => {
                    let message = "Error parsing number of jobs (--jobs, -j): must be integer.";
                    profile.jobs = Some(match value.as_str() {
                        Some("unlimited") => i64::MAX,
                        _ => count(&value).ok_or_else(|| workflow_error(message))?,
                    });
                }
                "max-threads" => {
                    let message = "Profile max-threads must be an integer";
                    profile.max_threads =
                        Some(count(&value).ok_or_else(|| workflow_error(message))?);
                }
                "resources" | "res" => profile.resources = pairs(interpreter, &value)?,
                "executor" => profile.executor = Some(interpreter.to_str(&value)?),
                "all-temp" => profile.all_temp = interpreter.truthy(&value)?,
                "notemp" | "nt" | "no-temp" => profile.notemp = interpreter.truthy(&value)?,
                "wrapper-prefix" => profile.wrapper_prefix = Some(interpreter.to_str(&value)?),
                "groups" => profile.groups = pairs(interpreter, &value)?,
                "group-components" => {
                    let message = "Invalid group components definition: entries have to be defined as GROUP=COMPONENTS pairs (with COMPONENTS being a positive integer)";
                    for (group, number) in pairs(interpreter, &value)? {
                        let number = (number.trim_matches(['\'', '"']).parse::<usize>().ok())
                            .filter(|number| *number > 0)
                            .ok_or_else(|| workflow_error(message))?;
                        profile.group_components.push((group, number));
                    }
                }
                "prioritize" | "P" => profile.prioritize = words(interpreter, &value)?,
                "allowed-rules" => profile.allowed_rules = Some(words(interpreter, &value)?),
                "set-scatter" => {
                    let message = "Invalid scatter definition: entries have to be defined as NAME=SCATTERITEMS pairs (with SCATTERITEMS being a positive integer).";
                    for (name, number) in pairs(interpreter, &value)? {
                        let number = (number.trim_matches(['\'', '"']).parse::<i64>().ok())
                            .filter(|number| *number >= 0)
                            .ok_or_else(|| workflow_error(message))?;
                        profile.set_scatter.push((name, number));
                    }
                }
                "consider-ancient" => {
                    for (rule, items) in pairs(interpreter, &value)? {
                        let items = items.trim_matches(['\'', '"']);
                        let items = items
                            .split(',')
                            .map(|item| item.trim().to_owned())
                            .collect();
                        profile.consider_ancient.push((rule, items));
                    }
                }
                "batch" => profile.batch = Some(batch(&interpreter.to_str(&value)?)?),
                "rerun-triggers" => {
                    let values = if let Some(text) = value.as_str() {
                        text.split_whitespace().map(str::to_owned).collect()
                    } else {
                        interpreter
                            .iterate(&value)?
                            .iter()
                            .map(|value| interpreter.to_str(value))
                            .collect::<Result<Vec<_>, _>>()?
                    };
                    validate_triggers(&values)?;
                    profile.rerun_triggers = Some(values);
                }
                "retries" | "restart-times" => {
                    profile.retries = Some(
                        value
                            .as_int()
                            .and_then(|number| u32::try_from(number).ok())
                            .ok_or_else(|| {
                                workflow_error(
                                    "Profile retries must be an integer >= 0 and <= 4294967295",
                                )
                            })?,
                    );
                }
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
                "quiet" | "printshellcmds" | "verbose" | "nocolor" | "show-failed-logs" => {}
                // How and where jobs run, not what they compute: iunctus chooses its own
                // executor, containers, environments and limits.
                "use-conda"
                | "use-singularity"
                | "use-apptainer"
                | "use-envmodules"
                | "software-deployment-method"
                | "sdm"
                | "conda-frontend"
                | "conda-prefix"
                | "conda-cleanup-pkgs"
                | "singularity-args"
                | "apptainer-args"
                | "singularity-prefix"
                | "apptainer-prefix"
                | "local-cores"
                | "cluster"
                | "cluster-status"
                | "cluster-cancel"
                | "cluster-generic-submit-cmd"
                | "cluster-generic-status-cmd"
                | "cluster-generic-cancel-cmd"
                | "jobscript"
                | "latency-wait"
                | "max-jobs-per-second"
                | "max-jobs-per-timespan"
                | "max-status-checks-per-second"
                | "scheduler"
                | "reason"
                | "keep-going"
                | "rerun-incomplete"
                | "keep-incomplete"
                | "nolock"
                | "jobname"
                | "jn"
                | "seconds-between-status-checks"
                | "shadow-prefix"
                | "max-inventory-time"
                | "wait-for-free-local-storage"
                | "local-storage-prefix"
                | "remote-job-local-storage-prefix"
                | "shared-fs-usage"
                | "keep-storage-local-copies"
                | "conda-base-path"
                | "conda-not-block-search-path-envvars"
                | "conda-cleanup-envs"
                | "container-cleanup-images"
                | "greediness"
                | "scheduler-greediness"
                | "scheduler-subsample"
                | "scheduler-solver-path"
                | "debug-dag"
                | "logger"
                | "runtime-profile"
                | "max-checksum-file-size"
                | "skip-script-cleanup"
                | "trust-io-cache"
                | "output-wait"
                | "envvars"
                | "precommand"
                | "preemptible-rules"
                | "preemptible-retries" => {}
                // The settings of Snakemake's executor plugins configure an executor that
                // iunctus replaces with its own.
                key if EXECUTOR_PLUGINS.iter().any(|plugin| {
                    key.strip_prefix(plugin)
                        .is_some_and(|rest| rest.starts_with('-'))
                }) => {}
                key => {
                    return Err(Error::unsupported(format!(
                        "Snakemake profile setting '{key}'"
                    )));
                }
            }
        }
        Ok(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_workflow_profile() {
        let folder = tempfile::tempdir().expect("a folder");
        let profile_folder = folder.path().join("profiles/default");
        std::fs::create_dir_all(&profile_folder).expect("the profile folder");
        std::fs::write(profile_folder.join("extra.yaml"), "a: 1\n").expect("a config file");
        std::fs::write(
            profile_folder.join("config.yaml"),
            "configfile: extra.yaml\nconfig:\n  n: '2'\ndefault-resources:\n  mem_mb: 500\nset-resources:\n  align:\n    runtime: 30\nset-threads:\n  align: 4\nquiet: true\n",
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

    #[test]
    fn overrides_profile_settings() {
        let folder = tempfile::tempdir().expect("a folder");
        let general = folder.path().join("general");
        std::fs::create_dir_all(&general).expect("the profile folder");
        std::fs::write(
            general.join("config.yaml"),
            "set-resources:\n  a:\n    test: one\nset-threads:\n  a: 2\n",
        )
        .expect("the general profile");
        let file = folder.path().join("workflow_profile.yaml");
        std::fs::write(&file, "set-resources:\n  a:\n    test: two\n").expect("a profile file");
        let mut interpreter = Interpreter::new(std::rc::Rc::new(python_evaluator::LocalHost::new(
            folder.path(),
        )));
        let profile =
            Profile::given(&mut interpreter, &[general, file]).expect("the profiles load");
        assert_eq!(
            profile.set_resources,
            [("a".to_owned(), "test".to_owned(), "two".to_owned())]
        );
        assert_eq!(profile.set_threads, [("a".to_owned(), "2".to_owned())]);
        let missing = Profile::given(&mut interpreter, &[folder.path().join("none")]);
        assert!(missing.is_err());
    }
}

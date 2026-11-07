//! Resolves configured workflow presets and verifies their installed local references.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};

use config_file::{ConfigFile, Table};
use serde_json::{Map, Value, json};
use task_model::Digest;
use task_runner::References;
use workflow_graph::Language;

use crate::Failure;
use crate::arguments::Arguments;
use crate::setup::EXECUTORS;

const MAX_INPUT_BYTES: u64 = 1 << 20;

/// Configured, pinned workflows and named installed reference releases.
pub struct Catalogue {
    workflows: BTreeMap<String, Workflow>,
    references: BTreeMap<String, Reference>,
}

struct Workflow {
    source: PathBuf,
    revision: Option<String>,
    digest: Option<Digest>,
    language: Option<Language>,
    profiles: Vec<String>,
    entry: Option<String>,
    executor: Option<String>,
    executors: Vec<String>,
    requires: Vec<String>,
    params: Option<PathBuf>,
    overrides: BTreeSet<String>,
    references: BTreeMap<String, String>,
}

struct Reference {
    path: PathBuf,
    digest: Digest,
    version: String,
    files: Vec<String>,
}

/// Keep the input file alive throughout description, checking or execution.
pub struct Prepared {
    pub inputs: tempfile::NamedTempFile,
    pub references: References,
}

impl Catalogue {
    pub fn load(named: Option<&Path>) -> Result<Self, Failure> {
        Self::from_config(ConfigFile::load(named).map_err(failed)?)
    }

    pub fn parse(text: &str, file: &Path) -> Result<Self, Failure> {
        Self::from_config(ConfigFile::parse(text, file).map_err(failed)?)
    }

    pub fn from_config(config: ConfigFile) -> Result<Self, Failure> {
        let mut workflows = BTreeMap::new();
        let mut references = BTreeMap::new();
        if let Some(section) = config.section("workflows").map_err(failed)? {
            for table in section.tables().map_err(failed)? {
                workflows.insert(
                    table.name()["workflows.".len()..].to_owned(),
                    workflow(&table)?,
                );
            }
        }
        if let Some(section) = config.section("references").map_err(failed)? {
            for table in section.tables().map_err(failed)? {
                table
                    .only(&["path", "digest", "version", "files"])
                    .map_err(failed)?;
                let reference = Reference {
                    path: located(&table, "path")?
                        .ok_or_else(|| failed(table.missing("path", "the installed reference")))?,
                    digest: required(&table, "digest")?.parse().map_err(failed)?,
                    version: required(&table, "version")?,
                    files: table.texts("files").map_err(failed)?.ok_or_else(|| {
                        failed(table.missing(
                            "files",
                            "required files relative to the installation, or [] for a single file",
                        ))
                    })?,
                };
                for file in &reference.files {
                    if file.is_empty()
                        || !Path::new(file)
                            .components()
                            .all(|part| matches!(part, Component::Normal(_)))
                    {
                        return Err(failed(
                            table.invalid("files", "must stay inside the reference directory"),
                        ));
                    }
                }
                references.insert(table.name()["references.".len()..].to_owned(), reference);
            }
        }
        for workflow in workflows.values() {
            for name in workflow.references.values() {
                if !references.contains_key(name) {
                    return Err(failed(format!("unknown reference `{name}`")));
                }
            }
        }
        Ok(Self {
            workflows,
            references,
        })
    }

    /// Lists configured selections without hashing installations or loading workflow code.
    pub fn list(&self) -> Value {
        json!({
            "version": 1,
            "capabilities": {
                "languages": ["nextflow", "snakemake", "cwl", "argo", "wdl"],
                "executors": EXECUTORS.iter().map(|name| (name.to_string(), json!(features(name)))).collect::<Map<_, _>>(),
                "nextflow": nextflow_runtime::capabilities()
            },
            "workflows": self.workflows.iter().map(|(name, workflow)| json!({
                "name": name, "source": workflow.source, "revision": workflow.revision,
                "language": workflow.language, "profiles": workflow.profiles,
                "references": workflow.references, "executor": workflow.executor,
                "executors": workflow.executors, "requires": workflow.requires
            })).collect::<Vec<_>>(),
            "references": self.references.iter().map(|(name, reference)| json!({
                "name": name, "path": reference.path, "version": reference.version,
                "digest": reference.digest, "files": reference.files
            })).collect::<Vec<_>>()
        })
    }

    /// Applies a preset once; normal input admission still checks the workflow's schema.
    /// Call outside Tokio, as reference verification uses the shared blocking runtime.
    pub fn prepare(&self, arguments: &mut Arguments) -> Result<Option<Prepared>, Failure> {
        let Some(name) = arguments.preset.as_deref() else {
            return Ok(None);
        };
        let workflow = self
            .workflows
            .get(name)
            .ok_or_else(|| failed(format!("unknown workflow preset `{name}`")))?;
        if let Some(executor) = arguments.executor.as_ref().or(workflow.executor.as_ref()) {
            if !workflow.executors.is_empty() && !workflow.executors.contains(executor) {
                return Err(failed(format!(
                    "preset `{name}` does not allow executor `{executor}`"
                )));
            }
            for feature in &workflow.requires {
                if !features(executor).contains(&feature.as_str()) {
                    return Err(failed(format!(
                        "preset `{name}` requires `{feature}`, unsupported by executor `{executor}`"
                    )));
                }
            }
        }
        if arguments.target.is_some()
            || arguments.revision.is_some()
            || arguments.latest
            || arguments.language.is_some()
            || arguments.profiles.is_some()
            || !arguments.targets.is_empty()
            || !arguments.configs.is_empty()
        {
            return Err(Failure::usage(
                "a preset supplies its source, revision, language, profiles, entry and workflow config; give only allowed input overrides",
            ));
        }
        let mut values = match &workflow.params {
            Some(path) => input_object(path)?,
            None => Map::new(),
        };
        let allowed = |key: &str| {
            if workflow.references.contains_key(key) || !workflow.overrides.contains(key) {
                Err(Failure::usage(format!(
                    "preset `{name}` does not allow overriding input `{key}`"
                )))
            } else {
                Ok(())
            }
        };
        if let Some(path) = &arguments.inputs {
            for (key, value) in input_object(path)? {
                allowed(&key)?;
                values.insert(key, value);
            }
        }
        for (key, _) in &arguments.params {
            allowed(key)?;
        }
        let runtime = workflow_graph::runtime().map_err(failed)?;
        let mut references = References::default();
        if let Some(digest) = &workflow.digest {
            let root = if workflow.source.is_dir() {
                workflow.source.as_path()
            } else {
                workflow
                    .source
                    .parent()
                    .ok_or_else(|| failed("workflow has no project directory"))?
            };
            runtime
                .block_on(references.register(root, digest.clone()))
                .map_err(failed)?;
        }
        let mut registered = BTreeSet::new();
        for (parameter, name) in &workflow.references {
            let reference = &self.references[name];
            let root = std::fs::canonicalize(&reference.path).map_err(failed)?;
            for file in &reference.files {
                let path = std::fs::canonicalize(root.join(file)).map_err(failed)?;
                if !path.starts_with(&root) || !path.is_file() {
                    return Err(failed(format!(
                        "reference `{name}` requires file `{file}` inside its installation"
                    )));
                }
            }
            if registered.insert(name) {
                runtime
                    .block_on(references.register(&root, reference.digest.clone()))
                    .map_err(failed)?;
            }
            values.insert(
                parameter.clone(),
                Value::String(root.to_string_lossy().into_owned()),
            );
        }
        let mut inputs = tempfile::Builder::new()
            .prefix("uwe-preset-")
            .suffix(".json")
            .tempfile()
            .map_err(failed)?;
        serde_json::to_writer(inputs.as_file(), &values).map_err(failed)?;
        inputs.as_file_mut().rewind().map_err(failed)?;
        arguments.target = Some(workflow.source.clone());
        arguments.revision = workflow.revision.clone();
        arguments.language = workflow.language;
        arguments.profiles = (!workflow.profiles.is_empty()).then(|| workflow.profiles.clone());
        arguments.targets = workflow.entry.iter().cloned().collect();
        arguments.executor = arguments
            .executor
            .clone()
            .or_else(|| workflow.executor.clone());
        arguments.inputs = Some(inputs.path().to_owned());
        arguments.preset = None;
        Ok(Some(Prepared { inputs, references }))
    }
}

fn workflow(table: &Table<'_>) -> Result<Workflow, Failure> {
    table
        .only(&[
            "source",
            "revision",
            "digest",
            "language",
            "profiles",
            "entry",
            "executor",
            "executors",
            "requires",
            "params",
            "overrides",
            "references",
        ])
        .map_err(failed)?;
    let source = required(table, "source")?;
    let local = located(table, "source")?.expect("required source");
    let remote = !local.exists() && crate::repository::Repository::parse(&source)?.is_some();
    let revision = table.text("revision").map_err(failed)?;
    let digest = table
        .text("digest")
        .map_err(failed)?
        .map(|value| value.parse::<Digest>().map_err(failed))
        .transpose()?;
    if remote {
        if revision
            .as_deref()
            .and_then(crate::revision::commit)
            .is_none()
            || digest.is_some()
        {
            return Err(failed(table.invalid(
                "revision",
                "requires a full repository commit and no local digest",
            )));
        }
    } else if digest.is_none() || revision.is_some() {
        return Err(failed(table.invalid(
            "digest",
            "requires the read-only project directory tree digest and no repository revision",
        )));
    }
    let language = table
        .text("language")
        .map_err(failed)?
        .map(|name| serde_json::from_value::<Language>(Value::String(name)).map_err(failed))
        .transpose()?;
    let executor = table.text("executor").map_err(failed)?;
    if executor
        .as_deref()
        .is_some_and(|name| !EXECUTORS.contains(&name))
    {
        return Err(failed(
            table.invalid("executor", "names an unknown executor"),
        ));
    }
    let executors = table
        .texts("executors")
        .map_err(failed)?
        .unwrap_or_default();
    let requires = table.texts("requires").map_err(failed)?.unwrap_or_default();
    if executors
        .iter()
        .any(|name| !EXECUTORS.contains(&name.as_str()))
        || requires
            .iter()
            .any(|name| !features("local").contains(&name.as_str()))
        || ((!executors.is_empty() || !requires.is_empty()) && executor.is_none())
    {
        return Err(failed(table.invalid("executor", "requires a default executor, known executors and capabilities (daemons, sidecars, offline)")));
    }
    let mut references = BTreeMap::new();
    if let Some(bindings) = table.table("references").map_err(failed)? {
        for key in bindings.keys() {
            references.insert(key.to_owned(), required(&bindings, key)?);
        }
    }
    let profiles = table.texts("profiles").map_err(failed)?.unwrap_or_default();
    let overrides = table
        .texts("overrides")
        .map_err(failed)?
        .unwrap_or_default();
    Ok(Workflow {
        source: if remote { source.into() } else { local },
        revision,
        digest,
        language,
        profiles,
        entry: table.text("entry").map_err(failed)?,
        executor,
        executors,
        requires,
        params: located(table, "params")?,
        overrides: overrides.into_iter().collect(),
        references,
    })
}

fn features(executor: &str) -> &'static [&'static str] {
    match executor {
        "local" => &["daemons", "sidecars", "offline"],
        "kubernetes" => &["daemons", "sidecars"],
        "slurm" | "htcondor" | "pbs" | "nomad" | "wr" => &["offline"],
        _ => &[],
    }
}

fn located(table: &Table<'_>, key: &str) -> Result<Option<PathBuf>, Failure> {
    table
        .text(key)
        .map_err(failed)?
        .map(|text| {
            if text.is_empty() {
                return Err(failed(table.invalid(key, "must name a nonempty path")));
            }
            let origin = table
                .value_origin(key)
                .expect("present value has an origin");
            std::path::absolute(origin.file.parent().unwrap_or(Path::new(".")).join(text))
                .map_err(failed)
        })
        .transpose()
}

fn required(table: &Table<'_>, key: &str) -> Result<String, Failure> {
    table
        .text(key)
        .map_err(failed)?
        .filter(|text| !text.is_empty())
        .ok_or_else(|| failed(table.missing(key, "a nonempty value")))
}

fn input_object(path: &Path) -> Result<Map<String, Value>, Failure> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(failed)?
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(failed("preset input JSON exceeds 1 MiB"));
    }
    match serde_json::from_slice(&bytes).map_err(failed)? {
        Value::Object(values) => Ok(values),
        _ => Err(failed("preset inputs must be a JSON object")),
    }
}

fn failed(error: impl std::fmt::Display) -> Failure {
    Failure::setup(format!("uwe: {error}"))
}

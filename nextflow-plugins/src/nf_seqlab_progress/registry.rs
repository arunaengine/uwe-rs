//! The source files, stages and process mappings that a pipeline registers with
//! `registerProgressInputs` and `registerProgressStages`, which the progress observer reads.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;

use nextflow_evaluator::value::Value;

use crate::nf_schema::{entries, get};

/// When a finished task completes its parent file for its stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Boundary {
    Always,
    Never,
    /// Only when the task's file is its parent file.
    Parent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stage {
    pub id: String,
    pub label: String,
    /// The source files of the stage, all registered files when it names none.
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mapping {
    pub process: String,
    pub stage: String,
    pub boundary: Boundary,
}

/// What the run registered so far.
#[derive(Clone, Debug, Default)]
pub(crate) struct Registry {
    pub inputs: Vec<(String, String)>,
    pub stages: Vec<Stage>,
    pub mappings: Vec<Mapping>,
}

thread_local! {
    /// The registrations of the run of this thread, which its observer reads.
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// Forgets the registrations of an earlier run of this thread.
#[expect(dead_code, reason = "the progress observer uses it")]
pub(crate) fn clear() {
    REGISTRY.with(|registry| *registry.borrow_mut() = Registry::default());
}

#[expect(dead_code, reason = "the progress observer uses it")]
pub(crate) fn snapshot() -> Registry {
    REGISTRY.with(|registry| registry.borrow().clone())
}

fn text(map: &Value, key: &str) -> Option<String> {
    let found = get(map, key).filter(|value| !matches!(value, Value::Null));
    found.map(|value| value.to_string())
}

fn items(value: &Value) -> Vec<Value> {
    match value {
        Value::List(list) => list.borrow().items.clone(),
        Value::Set(set) => set.borrow().items.iter().map(|key| key.0.clone()).collect(),
        _ => Vec::new(),
    }
}

/// `registerProgressInputs(inputs)`.
pub(crate) fn inputs(list: &Value) -> Result<(), String> {
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        for input in items(list) {
            let id = (text(&input, "file_id").or_else(|| text(&input, "id")))
                .ok_or("Each progress input requires file_id or id")?;
            let path =
                (text(&input, "path").or_else(|| text(&input, "variants"))).unwrap_or_default();
            match registry.inputs.iter().find(|(known, _)| *known == id) {
                Some((_, known)) if *known != path => {
                    return Err(format!(
                        "Source file '{id}' was registered with both '{known}' and '{path}'"
                    ));
                }
                Some(_) => {}
                None => registry.inputs.push((id, path)),
            }
        }
        Ok(())
    })
}

/// `registerProgressStages(stages, processes)`.
pub(crate) fn stages(stages: &Value, processes: &Value) -> Result<(), String> {
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        for stage in items(stages) {
            let id = text(&stage, "id").ok_or("Progress definition requires 'id'")?;
            let label = text(&stage, "label").ok_or("Progress definition requires 'label'")?;
            let files = match get(&stage, "file_ids") {
                None | Some(Value::Null) => {
                    (registry.inputs.iter()).map(|(id, _)| id.clone()).collect()
                }
                Some(value @ (Value::List(_) | Value::Set(_))) => {
                    let mut files = Vec::new();
                    for file in items(&value).iter().map(Value::to_string) {
                        if !registry.inputs.iter().any(|(known, _)| *known == file) {
                            return Err(format!("Unknown source file '{file}'"));
                        }
                        files.push(file);
                    }
                    files
                }
                Some(_) => return Err("Progress stage 'file_ids' must be a collection".to_owned()),
            };
            let stage = Stage { id, label, files };
            match registry.stages.iter().find(|known| known.id == stage.id) {
                Some(known) if *known != stage => {
                    return Err(format!(
                        "Stage '{}' already has a different definition",
                        stage.id
                    ));
                }
                Some(_) => {}
                None => registry.stages.push(stage),
            }
        }
        for mapping in items(processes) {
            let process =
                text(&mapping, "process").ok_or("Progress definition requires 'process'")?;
            let stage = text(&mapping, "stage").ok_or("Progress definition requires 'stage'")?;
            let boundary = match get(&mapping, "completion_boundary") {
                None | Some(Value::Null) | Some(Value::Boolean(false)) => Boundary::Never,
                Some(Value::Boolean(true)) => Boundary::Always,
                Some(other) => match other.to_string().as_str() {
                    "true" => Boundary::Always,
                    "false" => Boundary::Never,
                    "parent" => Boundary::Parent,
                    _ => {
                        return Err(
                            "completion_boundary must be true, false, or 'parent'".to_owned()
                        );
                    }
                },
            };
            if !registry.stages.iter().any(|known| known.id == stage) {
                return Err(format!(
                    "Cannot map process '{process}' to unknown stage '{stage}'"
                ));
            }
            let mapping = Mapping {
                process,
                stage,
                boundary,
            };
            match registry
                .mappings
                .iter()
                .find(|known| known.process == mapping.process)
            {
                Some(known) if *known != mapping => {
                    return Err(format!(
                        "Process '{}' already has a different mapping",
                        mapping.process
                    ));
                }
                Some(_) => {}
                None => registry.mappings.push(mapping),
            }
        }
        Ok(())
    })
}

/// `withProgressIdentity(meta)`: a copy with `file_id` and `parent_file_id`.
pub(crate) fn identity(meta: &Value) -> Result<Value, String> {
    let file = (text(meta, "file_id").or_else(|| text(meta, "id")))
        .ok_or("Progress metadata requires id or file_id")?;
    let parent =
        (text(meta, "parent_file_id").or_else(|| text(meta, "parent_id"))).unwrap_or(file.clone());
    let mut copied: Vec<(Value, Value)> = (entries(meta).into_iter())
        .map(|(key, value)| (Value::string(key), value))
        .collect();
    for (key, value) in [("file_id", file), ("parent_file_id", parent)] {
        match copied
            .iter_mut()
            .find(|(known, _)| known.to_string() == key)
        {
            Some(entry) => entry.1 = Value::string(value),
            None => copied.push((Value::string(key), Value::string(value))),
        }
    }
    Ok(Value::map(copied))
}

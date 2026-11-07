//! `paramsSummaryMap` and `paramsSummaryLog`: the params that differ from the defaults of the
//! parameters schema, by group, then the core options of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::regex::Pattern;
use nextflow_evaluator::value::{Key, Value};

use super::params::{groups, parameters_schema};
use super::settings::Settings;
use super::{entries, get, since, version};
use crate::Context;

/// `paramsSummaryMap`: for each group of the schema the params that differ from their
/// defaults, then the core options of the run.
pub(super) fn map(
    options: Option<&Value>,
    workflow: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let settings = Settings::read(context);
    let schema = parameters_schema(options, context)?;
    let rules = Rules {
        masks: masks(context, &settings)?,
        mask: settings.summary.mask.clone(),
        outdir: get(&context.params, "outdir").map_or("null".to_owned(), |v| v.to_string()),
        masking: since(context, [2, 7, 3]),
        modern: since(context, [3, 0, 0]),
    };
    let mut summary = Vec::new();
    for (title, properties) in groups(&schema.tree) {
        summary.push((title, rules.changed(&properties, &context.params)?));
    }
    hide(&mut summary, &settings.summary.hidden);
    let mut core = Vec::new();
    for name in ["revision", "runName", "containerEngine", "container"] {
        let value = match name {
            "containerEngine" => engine(context).map(Value::string),
            _ => get(workflow, name),
        };
        if let Some(value) = value.filter(Value::truth) {
            core.push((name.to_owned(), value));
        }
    }
    for name in ["launchDir", "workDir", "projectDir", "userName", "profile"] {
        let value = get(workflow, name).unwrap_or(Value::Null);
        let value = match name {
            "launchDir" | "workDir" | "projectDir" if !matches!(value, Value::Null) => {
                Value::string(rules.masked(rules.text(&value))?)
            }
            _ => value,
        };
        core.push((name.to_owned(), value));
    }
    let files = match get(workflow, "configFiles") {
        Some(Value::List(files)) => {
            let names: Vec<String> = (files.borrow().items.iter())
                .map(|file| rules.text(file))
                .collect();
            names.join(", ")
        }
        _ => String::new(),
    };
    core.push((
        "configFiles".to_owned(),
        Value::string(rules.masked(files)?),
    ));
    summary.push(("Core Nextflow options".to_owned(), core));
    Ok(Value::map(summary.into_iter().map(|(title, group)| {
        let group = group
            .into_iter()
            .map(|(name, value)| (Value::string(name), value));
        (Value::string(title), Value::map(group))
    })))
}

/// How the params of a summary are compared and shown.
struct Rules {
    masks: Vec<Pattern>,
    mask: String,
    /// The text of `params.outdir`, which defaults with `$params.outdir` start with.
    outdir: String,
    /// From 2.7.3 on: values go through the masks, and a null value with a default fails.
    masking: bool,
    /// From 3.0.0 on: paths as their URI, and a null value with a default is left out.
    modern: bool,
}

impl Rules {
    /// The params of `properties` whose values differ from their defaults, as texts; object
    /// params with a map value as maps of their own.
    fn changed(
        &self,
        properties: &[(String, Value)],
        values: &Value,
    ) -> Result<Vec<(String, Value)>, Error> {
        let mut changed = Vec::new();
        for (name, definition) in properties {
            let Some(value) = get(values, name) else {
                continue;
            };
            if let (Some(children), Value::Map(_)) = (get(definition, "properties"), &value) {
                let nested = self.changed(&entries(&children), &value)?;
                if !nested.is_empty() {
                    let nested = nested.into_iter().map(|(n, v)| (Value::string(n), v));
                    changed.push((name.clone(), Value::map(nested)));
                }
                continue;
            }
            let default = get(definition, "default").filter(|value| !matches!(value, Value::Null));
            if matches!(value, Value::Null) {
                match default {
                    Some(_) if !self.masking => changed.push((name.clone(), Value::Null)),
                    Some(_) if !self.modern => return Err(ambiguous()),
                    Some(Value::String(default)) if project(&default).is_some() => {
                        return Err(null_value());
                    }
                    _ => {}
                }
                continue;
            }
            let text = self.text(&value);
            let differs = match &default {
                Some(default) => !self.same(default, &text),
                None => !text.is_empty() && text != "false",
            };
            if differs {
                changed.push((name.clone(), Value::string(self.masked(text)?)));
            }
        }
        Ok(changed)
    }

    /// Whether a value equals its default; defaults with `$projectDir` or `$params.outdir`
    /// compare by the rest of their text.
    fn same(&self, default: &Value, text: &str) -> bool {
        let default = default.to_string();
        if let Some(rest) = project(&default) {
            return text.contains(&rest);
        }
        for marker in ["${params.outdir}", "$params.outdir"] {
            if default.contains(marker) {
                return text == format!("{}{}", self.outdir, default.replace(marker, ""));
            }
        }
        text == default
    }

    /// A value as text: a path as its URI from 3.0.0 on, else as its path.
    fn text(&self, value: &Value) -> String {
        match value {
            Value::Path(path) if self.modern => path.uri(),
            Value::Path(path) => path.text().to_owned(),
            other => other.to_string(),
        }
    }

    /// The text with each match of `maskSubpaths` replaced by `mask`, in list order.
    fn masked(&self, text: String) -> Result<String, Error> {
        let mut text = text;
        if self.masking {
            for pattern in &self.masks {
                text = pattern
                    .replace(&text, &self.mask, true)
                    .map_err(exception)?;
            }
        }
        Ok(text)
    }
}

/// The rest of a default with `$projectDir` or `${projectDir}`, if it has one.
fn project(default: &str) -> Option<String> {
    ["${projectDir}", "$projectDir"]
        .into_iter()
        .find(|marker| default.contains(marker))
        .map(|marker| default.replace(marker, ""))
}

fn masks(context: &Context, settings: &Settings) -> Result<Vec<Pattern>, Error> {
    if !since(context, [2, 7, 3]) {
        return Ok(Vec::new());
    }
    let patterns = settings.summary.patterns.iter();
    patterns
        .map(|pattern| Pattern::new(pattern).map_err(exception))
        .collect()
}

fn exception(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}

/// The failure of 2.7.3 and 2.8.0 for a null param with a default.
fn ambiguous() -> Error {
    exception(Exception::new(
        "GroovyRuntimeException",
        "Ambiguous method overloading for method \
         nextflow.validation.summary.SummaryCreator#maybeMask.\nCannot resolve which method \
         to invoke for [null] due to overlapping prototypes between:\n\t[interface \
         java.lang.CharSequence]\n\t[interface java.nio.file.Path]",
    ))
}

/// The failure of 3.0.0 for a null param whose default has `$projectDir`.
fn null_value() -> Error {
    exception(Exception::new(
        "NullPointerException",
        "Cannot invoke \"String.contains(java.lang.CharSequence)\" because \"value\" is null",
    ))
}

/// `summary.hideParams`: a plain name leaves the param out of every group; for `a.b`, every
/// group looks up `a`, which adds `a` as null to a group without it, and leaves out its `b`.
fn hide(summary: &mut [(String, Vec<(String, Value)>)], hidden: &[String]) {
    for entry in hidden {
        let parts: Vec<&str> = entry.split('.').collect();
        for (_, group) in summary.iter_mut() {
            let Some((first, rest)) = parts.split_first() else {
                continue;
            };
            if rest.is_empty() {
                group.retain(|(name, _)| name != first);
                continue;
            }
            match group.iter().find(|(name, _)| name == first) {
                Some((_, value)) => remove(value, rest),
                None => group.push(((*first).to_owned(), Value::Null)),
            }
        }
    }
}

fn remove(value: &Value, parts: &[&str]) {
    let Value::Map(map) = value else {
        return;
    };
    match parts {
        [last] => {
            map.borrow_mut()
                .entries
                .shift_remove(&Key(Value::string(*last)));
        }
        [first, rest @ ..] => {
            if let Some(inner) = get(value, first) {
                remove(&inner, rest);
            }
        }
        [] => {}
    }
}

/// `paramsSummaryLog`: the summary as text, one group after another.
pub(super) fn log(
    options: Option<&Value>,
    workflow: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let settings = Settings::read(context);
    let summary = map(options, workflow, context)?;
    let option = |name: &str, default: &str| {
        (options.and_then(|options| get(options, name)))
            .map_or(default.to_owned(), |v| v.to_string())
    };
    let before = option("beforeText", &settings.summary.before);
    let after = option("afterText", &settings.summary.after);
    let mut groups: Vec<(String, Vec<(String, Value)>)> = entries(&summary)
        .into_iter()
        .map(|(title, group)| (title, flattened(&group)))
        .collect();
    if let Some((_, core)) = groups.last_mut() {
        containers(core, context)?;
    }
    let width = groups
        .iter()
        .flat_map(|(_, group)| group)
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut output = before;
    for (title, params) in groups {
        if params.is_empty() {
            continue;
        }
        output.push_str(&format!("{title}\n"));
        for (name, value) in params {
            output.push_str(&format!("  {name:width$}: {value}\n"));
        }
        output.push('\n');
    }
    output.push_str("!! Only displaying parameters that differ from the pipeline defaults !!\n");
    output.push_str(&"-".repeat(54));
    output.push_str(&after);
    Ok(Value::string(output))
}

/// The containers in the log: shown per process up to 2.6.1; left out from 2.7.0 on, where
/// one container text fails the log; from 3.0.0 on a text stays and a map goes to the log file.
fn containers(core: &mut Vec<(String, Value)>, context: &Context) -> Result<(), Error> {
    if !version(context).is_none_or(|parts| parts.as_slice() >= [2, 7, 0].as_slice()) {
        return Ok(());
    }
    let mapped = |name: &String| name.starts_with("container.");
    if since(context, [3, 0, 0]) {
        let listed: Vec<String> = (core.iter().filter(|(name, _)| mapped(name)))
            .map(|(name, image)| format!("    {}: {image}", &name["container.".len()..]))
            .collect();
        if !listed.is_empty() {
            tracing::debug!("Containers specified in config:\n{}", listed.join("\n"));
        }
    } else if let Some((_, container)) = core.iter().find(|(name, _)| name == "container") {
        let message = format!(
            "Cannot cast object '{container}' with class '{}' to class 'java.util.Map'",
            container.java_class_name()
        );
        return Err(exception(Exception::new("ClassCastException", message)));
    }
    core.retain(|(name, _)| !mapped(name));
    Ok(())
}

/// The entries of a summary group, with maps inside as `name.key` entries.
fn flattened(group: &Value) -> Vec<(String, Value)> {
    let mut flat = Vec::new();
    for (name, value) in entries(group) {
        match value {
            Value::Map(_) => flat.extend(
                (flattened(&value).into_iter())
                    .map(|(key, value)| (format!("{name}.{key}"), value)),
            ),
            other => flat.push((name, other)),
        }
    }
    flat
}

/// The container engines of Nextflow, in the order it looks for the enabled one.
const ENGINES: [&str; 7] = [
    "docker",
    "podman",
    "sarus",
    "shifter",
    "singularity",
    "apptainer",
    "charliecloud",
];

/// The container engine that the config enables, which `workflow.containerEngine` gives in
/// Nextflow, else none.
fn engine(context: &Context) -> Option<&'static str> {
    ENGINES.into_iter().find(|engine| {
        (context.config.get(&[engine, "enabled"])).is_some_and(|enabled| enabled.truth())
    })
}

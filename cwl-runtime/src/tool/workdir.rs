//! `InitialWorkDirRequirement`: its listing, evaluated into files, directories and Dirents,
//! which are staged in the output directory, as cwltool's `_initialworkdir` does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Dirent, Listing, ListingEntry, Process, RequirementClass, Version};

use super::stage::{InPlace, Stager};
use crate::expression::{Evaluator, Variables};
use crate::files;
use crate::scope::Scope;
use crate::value::{Map, Value};

/// Evaluates `InitialWorkDirRequirement` and stages its entries in the output directory.
pub(super) fn initial_workdir(
    process: &Process,
    scope: &Scope,
    evaluator: &Evaluator,
    inputs: &Value,
    runtime: &Map,
    stager: &mut Stager,
) -> Result<(), String> {
    let Some((listing, _)) = scope.find(|class| match class {
        RequirementClass::InitialWorkDir { listing } => Some(listing),
        _ => None,
    }) else {
        return Ok(());
    };
    let docker_required = scope
        .find(|class| matches!(class, RequirementClass::Docker(_)).then_some(()))
        .is_some_and(|(_, required)| required);
    let in_place = scope
        .find(|class| match class {
            RequirementClass::InplaceUpdate { inplace_update } => Some(*inplace_update),
            _ => None,
        })
        .is_some_and(|(update, _)| update);
    let runtime = Value::Map(runtime.clone());
    let vars = Variables {
        inputs,
        this: &Value::Null,
        runtime: &runtime,
    };
    let mut entries: Vec<Value> = Vec::new();
    match listing {
        Listing::Expression(expression) => {
            match evaluator.evaluate(expression, vars).map_err(listed)? {
                Value::List(items) => {
                    for item in items {
                        match item {
                            Value::Null => {}
                            Value::List(nested) => entries.extend(nested),
                            other => entries.push(other),
                        }
                    }
                }
                Value::Null => {}
                other => {
                    return Err(format!(
                        "the InitialWorkDirRequirement listing must give a list, not {}",
                        other.to_json()
                    ));
                }
            }
        }
        Listing::Entries(items) => {
            for item in items {
                match item {
                    ListingEntry::Dirent(dirent) => {
                        entries.extend(dirent_entries(dirent, evaluator, vars)?);
                    }
                    ListingEntry::Expression(expression) => {
                        match evaluator.evaluate(expression, vars).map_err(listed)? {
                            Value::Null => {}
                            Value::List(items) => {
                                entries.extend(items.into_iter().filter(|item| !item.is_null()))
                            }
                            other if !other.is_truthy() => {}
                            other => entries.push(other),
                        }
                    }
                    ListingEntry::Object(node) => match Value::from_node(node) {
                        Value::List(items) => entries.extend(items),
                        other => entries.push(other),
                    },
                }
            }
        }
    }
    for entry in entries {
        let Value::Map(mut map) = entry else {
            return Err(format!(
                "an InitialWorkDirRequirement entry must be a file, a directory or a Dirent, not {}",
                entry.to_json()
            ));
        };
        // Dirents left by the expressions become files or renamed file objects.
        if map.contains_key("entry") {
            let entryname = map
                .get("entryname")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let writable = map.get("writable").cloned();
            let entry = map.shift_remove("entry").unwrap_or(Value::Null);
            map = match entry {
                Value::Map(mut object) if object.contains_key("class") => {
                    if let Some(name) = entryname {
                        object.insert("basename".into(), Value::String(name));
                    }
                    object
                }
                Value::Null => continue,
                other => {
                    let Some(name) = entryname else {
                        return Err("an InitialWorkDirRequirement entry has no entryname".into());
                    };
                    let contents = match other {
                        Value::String(text) => text,
                        other => other.to_python_json(true),
                    };
                    let mut file = Map::new();
                    file.insert("class".into(), Value::string("File"));
                    file.insert("basename".into(), Value::String(name));
                    file.insert("contents".into(), Value::String(contents));
                    file
                }
            };
            if let Some(writable) = writable {
                map.insert("writable".into(), writable);
            }
        }
        if !matches!(
            map.get("class").and_then(Value::as_str),
            Some("File" | "Directory")
        ) {
            return Err(format!(
                "an InitialWorkDirRequirement entry must be a file or a directory, not {}",
                Value::Map(map).to_json()
            ));
        }
        let mut value = Value::Map(map);
        files::normalize(&mut value)?;
        let Value::Map(map) = value else { continue };
        let basename = map
            .get("basename")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut basename = normalize_path(&basename);
        // An absolute name inside the output directory, like `$(runtime.outdir)/x`, is `x`.
        if let Some(inside) = basename.strip_prefix(&format!("{}/", stager.outdir)) {
            basename = inside.to_owned();
        }
        if basename.starts_with("../") || basename == ".." {
            return Err(format!(
                "the InitialWorkDir entry `{basename}` leaves the output directory"
            ));
        }
        if basename.starts_with('/') && process.version < Version::V1_2Dev4 {
            return Err(format!(
                "the InitialWorkDir entry `{basename}` is absolute, which needs cwlVersion v1.2"
            ));
        }
        if basename.starts_with('/') && !docker_required {
            return Err(format!(
                "the InitialWorkDir entry `{basename}` is absolute, which needs \
                 DockerRequirement under `requirements`"
            ));
        }
        let (folder, name) = match basename.rsplit_once('/') {
            Some(("", name)) => ("/".to_owned(), name.to_owned()),
            Some((folder, name)) => (folder.to_owned(), name.to_owned()),
            None => (String::new(), basename.clone()),
        };
        let mut map = map;
        map.insert("basename".into(), Value::String(name));
        let target = stager.stage(&map, &folder, true);
        if map.get("writable") == Some(&Value::Bool(true)) {
            stager.make_writable(&target);
            let location = map
                .get("location")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if in_place && !location.starts_with("_:") && !target.starts_with('/') {
                if map.get("class").and_then(Value::as_str) == Some("Directory") {
                    return Err(super::unsupported(
                        "in-place Directory updates are not supported",
                    ));
                }
                stager.in_place.push(InPlace {
                    path: target.clone(),
                    location: location.to_owned(),
                    directory: map.get("class").and_then(Value::as_str) == Some("Directory"),
                });
            }
            let mut entry = Value::Map(map);
            files::visit_files(&mut entry, &mut |inner| {
                if let Some(Value::String(location)) = inner.get("location") {
                    stager.writable.insert(location.clone());
                }
            });
        }
    }
    Ok(())
}

/// An error of an expression of the listing, with where it is.
fn listed(error: String) -> String {
    format!("the InitialWorkDir listing: {error}")
}

/// The entries of one Dirent: a file with literal contents, a renamed file object, or the
/// file objects of a list.
pub(super) fn dirent_entries(
    dirent: &Dirent,
    evaluator: &Evaluator,
    vars: Variables<'_>,
) -> Result<Vec<Value>, String> {
    let entry = evaluator
        .evaluate_untrimmed(&dirent.entry, vars)
        .map_err(listed)?;
    let entryname = match &dirent.entryname {
        Some(expression) => match evaluator.evaluate(expression, vars).map_err(listed)? {
            Value::String(name) => Some(name),
            other => {
                return Err(format!(
                    "`entryname` must be a string, not {}",
                    other.to_json()
                ));
            }
        },
        None => None,
    };
    let writable = Value::Bool(dirent.writable);
    match entry {
        Value::Null => Ok(Vec::new()),
        // A list of files and directories, which may be empty, as in cwltool.
        Value::List(items) if items.iter().all(Value::is_file_like) => {
            if entryname.is_some() {
                return Err("`entryname` cannot name a list of files".into());
            }
            Ok(items
                .into_iter()
                .map(|mut item| {
                    if let Value::Map(map) = &mut item {
                        map.insert("writable".into(), writable.clone());
                    }
                    item
                })
                .collect())
        }
        entry => {
            let mut dirent_map = Map::new();
            dirent_map.insert("entry".into(), entry);
            if let Some(name) = entryname {
                dirent_map.insert("entryname".into(), Value::String(name));
            }
            dirent_map.insert("writable".into(), writable);
            Ok(vec![Value::Map(dirent_map)])
        }
    }
}

/// A relative path without `.` segments and with `..` folded, as `os.path.normpath` does.
fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

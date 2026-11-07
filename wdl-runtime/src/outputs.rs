//! Finds the files the outputs of a WDL task need before it runs, and reads them after.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use wdl_analysis::Document;
use wdl_analysis::types::{Optional, Type};
use wdl_ast::AstToken;
use wdl_ast::v1::BoundDecl;
use workflow_graph::{Outcome, OutputFile};

use super::{Frame, STDERR, STDOUT, Settings, State, read_location, size_location};
use crate::document::declared;
use crate::expression::{self, Environment, Error, Stream};
use crate::value::Value;

/// Finds the files an output needs: the paths it names, its globs and the files it reads.
pub(super) fn plan_output(
    document: &Document,
    decl: &BoundDecl,
    frame: &mut Frame,
    outputs: &mut BTreeMap<String, Value>,
    patterns: &mut BTreeSet<String>,
) -> Result<(), Error> {
    let name = decl.name().text().to_owned();
    let ty = declared(document, &decl.ty()).map_err(Error::Failed)?;
    let mut planning = Planning {
        frame,
        found: BTreeSet::new(),
        outputs,
    };
    let value = expression::evaluate(&decl.expr(), &mut planning);
    let mut found = std::mem::take(&mut planning.found);
    match value {
        Ok(value) => {
            let mut value = value
                .coerce(&ty)
                .map_err(|error| Error::failed(format!("output `{name}`: {error}")))?;
            value.visit_paths(&mut |path, directory| {
                if !planning.frame.staged.contains_key(path.as_str()) {
                    found.insert(if directory {
                        format!("{path}/")
                    } else {
                        path.clone()
                    });
                }
            });
            planning.outputs.insert(name.clone(), value);
        }
        Err(Error::Unknown) if !found.is_empty() || !has_files(&ty) => {}
        Err(Error::Unknown) => {
            return Err(Error::Unsupported(format!(
                "output `{name}` names files that only the results of the task tell"
            )));
        }
        Err(error) => return Err(error),
    }
    patterns.extend(found);
    Ok(())
}

/// Whether values of a type can hold files or directories.
fn has_files(ty: &Type) -> bool {
    let text = ty.to_string();
    text.contains("File")
        || text.contains("Directory")
        || matches!(ty, Type::Object | Type::OptionalObject | Type::Union)
}

/// Reads the outputs of a task that ran, as the outputs object of its call.
pub fn finish(state: &State, outcome: &Outcome, settings: &Settings) -> Result<Value, String> {
    let code = i64::from(outcome.exit_code);
    if let Some(codes) = &state.codes
        && !codes.contains(&code)
    {
        return Err(format!("the command exited with code {code}"));
    }
    let made: BTreeMap<String, OutputFile> = (outcome.files.iter())
        .map(|file| (file.path.trim_end_matches('/').to_owned(), file.clone()))
        .collect();
    let mut names = state.names.clone();
    if let Some(Value::Object(task)) = names.get_mut("task") {
        task.insert("return_code".into(), Value::Int(code));
    }
    let mut results = Results {
        names,
        staged: &state.staged,
        made: &made,
        settings,
    };
    let mut outputs = IndexMap::new();
    if let Some(section) = state.definition.output() {
        for decl in section.declarations() {
            let name = decl.name().text().to_owned();
            let ty = declared(&state.document, &decl.ty())?;
            let value = expression::evaluate(&decl.expr(), &mut results)
                .map_err(|error| error.to_string())
                .and_then(|value| value.coerce(&ty));
            let value = match value {
                Ok(value) => results.locate(value, &ty),
                Err(_) if settings.simulated => Ok(placeholder(&ty)),
                Err(error) => Err(error),
            };
            let value = value.map_err(|error| format!("output `{name}`: {error}"))?;
            results.names.insert(name.clone(), value.clone());
            outputs.insert(name, value);
        }
    }
    let mut outputs = Value::Object(outputs);
    // Outputs that refer to other outputs read them at the paths the task sees.
    outputs.visit_paths(&mut |path, _| {
        if let Some(file) = made.get(path.as_str()) {
            *path = file.location.clone();
        } else if let Some(source) = state.staged.get(path.as_str()) {
            *path = source.clone();
        }
    });
    Ok(outputs)
}

/// A value of a type for a simulated run whose made-up files cannot be read.
fn placeholder(ty: &Type) -> Value {
    use wdl_analysis::types::PrimitiveType;
    match ty {
        _ if ty.is_optional() => Value::None,
        Type::Primitive(PrimitiveType::Boolean, _) => Value::Boolean(false),
        Type::Primitive(PrimitiveType::Integer, _) => Value::Int(0),
        Type::Primitive(PrimitiveType::Float, _) => Value::Float(0.0),
        Type::Primitive(_, _) => Value::String(String::new()),
        Type::Compound(wdl_analysis::types::CompoundType::Map(_), _) => Value::Map(Vec::new()),
        _ => Value::Array(Vec::new()),
    }
}

/// The names of a task while its outputs are planned: reads of files the task makes are
/// unknown, and give their paths as outputs.
struct Planning<'a> {
    frame: &'a mut Frame,
    found: BTreeSet<String>,
    /// The outputs planned before, which later outputs may refer to.
    outputs: &'a mut BTreeMap<String, Value>,
}

impl Environment for Planning<'_> {
    fn lookup(&self, name: &str) -> Option<Value> {
        self.outputs
            .get(name)
            .cloned()
            .or_else(|| self.frame.lookup(name))
    }

    fn read(&mut self, file: &str) -> Result<String, Error> {
        if self.frame.source(file).is_some()
            || self.frame.written.iter().any(|(path, _)| path == file)
        {
            return self.frame.read(file);
        }
        self.found.insert(file.to_owned());
        Err(Error::Unknown)
    }

    fn glob(&mut self, pattern: &str) -> Result<Vec<String>, Error> {
        self.found.insert(pattern.to_owned());
        Err(Error::Unknown)
    }

    fn stream(&mut self, stream: Stream) -> Result<Value, Error> {
        Ok(Value::File(
            match stream {
                Stream::Stdout => STDOUT,
                Stream::Stderr => STDERR,
            }
            .into(),
        ))
    }

    fn size(&mut self, file: &str) -> Result<u64, Error> {
        if self.frame.source(file).is_some() {
            return self.frame.size(file);
        }
        self.found.insert(file.to_owned());
        Err(Error::Unknown)
    }
}

/// The names of a task after it ran, with the files it made by their paths.
struct Results<'a> {
    names: BTreeMap<String, Value>,
    staged: &'a BTreeMap<String, String>,
    made: &'a BTreeMap<String, OutputFile>,
    settings: &'a Settings,
}

impl Results<'_> {
    fn location(&self, file: &str) -> Option<String> {
        let file = file.trim_end_matches('/');
        (self.made.get(file).map(|made| made.location.clone())).or_else(|| {
            self.staged
                .get(file)
                .or_else(|| self.staged.get(&format!("{file}/")))
                .cloned()
        })
    }

    /// Checks that the files of an output exist; a missing optional file is `None`.
    fn locate(&self, value: Value, ty: &Type) -> Result<Value, String> {
        if let Value::File(path) | Value::Directory(path) = &value
            && self.location(path).is_none()
            && !path.contains("://")
        {
            return match ty.is_optional() || self.settings.simulated {
                true => Ok(Value::None),
                false => Err(format!("the task did not make {path}")),
            };
        }
        let mut missing = None;
        let mut value = value;
        value.visit_paths(&mut |path, _| {
            if self.location(path).is_none() && !path.contains("://") {
                missing.get_or_insert(path.clone());
            }
        });
        match missing {
            Some(path) if !self.settings.simulated => Err(format!("the task did not make {path}")),
            _ => Ok(value),
        }
    }
}

impl Environment for Results<'_> {
    fn lookup(&self, name: &str) -> Option<Value> {
        self.names.get(name).cloned()
    }

    fn read(&mut self, file: &str) -> Result<String, Error> {
        let location = self
            .location(file)
            .ok_or_else(|| Error::failed(format!("the task did not make {file}")))?;
        read_location(&*self.settings.files, &location)
    }

    fn glob(&mut self, pattern: &str) -> Result<Vec<String>, Error> {
        Ok((self.made.iter())
            .filter(|(path, file)| {
                !file.directory && workflow_graph::simulator::glob_matches(pattern, path)
            })
            .map(|(path, _)| path.clone())
            .collect())
    }

    fn stream(&mut self, stream: Stream) -> Result<Value, Error> {
        Ok(Value::File(
            match stream {
                Stream::Stdout => STDOUT,
                Stream::Stderr => STDERR,
            }
            .into(),
        ))
    }

    fn size(&mut self, file: &str) -> Result<u64, Error> {
        if let Some(size) = self
            .made
            .get(file.trim_end_matches('/'))
            .and_then(|made| made.size)
        {
            return Ok(size);
        }
        let location = self
            .location(file)
            .ok_or_else(|| Error::failed(format!("the task did not make {file}")))?;
        size_location(&*self.settings.files, &location)
    }
}

//! The outputs a tool asks the executor for: the globs and secondary files of its output
//! bindings, with `/` for directories, and the files an abstract `Operation` is taken to make.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{OutputBinding, OutputParameter, SecondaryFile, Type, short_name};

use crate::expression::{Evaluator, Variables};
use crate::files;
use crate::inputs::has_code;
use crate::types::Types;
use crate::value::Value;

/// The patterns an output asks the executor for: its globs, with `/` for directories, and
/// the names of its secondary files.
#[allow(clippy::too_many_arguments)]
pub(super) fn patterns(
    types: &Types,
    value_type: &Type,
    binding: Option<&OutputBinding>,
    secondary: &[SecondaryFile],
    evaluator: &Evaluator,
    vars: Variables<'_>,
    outdir: &str,
    patterns: &mut Vec<String>,
) -> Result<(), String> {
    if let Some(binding) = binding {
        let directory = directory_type(types, value_type);
        for glob in globs(binding, evaluator, vars, outdir)? {
            if glob == "." {
                continue;
            }
            for entry in secondary {
                if !has_code(&entry.pattern) {
                    patterns.push(files::substitute(&glob, &entry.pattern.text));
                }
            }
            patterns.push(if directory { format!("{glob}/") } else { glob });
        }
    }
    if let Type::Record(record) = types.resolve(value_type)
        && binding.is_none()
    {
        for field in &record.fields {
            self::patterns(
                types,
                &field.value_type,
                field.output_binding.as_ref(),
                &field.secondary_files,
                evaluator,
                vars,
                outdir,
                patterns,
            )?;
        }
    }
    Ok(())
}

/// Whether an output, or a field of its record, has no binding, so that only
/// `cwl.output.json` can give it.
pub(super) fn unbound(types: &Types, value_type: &Type, binding: Option<&OutputBinding>) -> bool {
    if binding.is_some() {
        return false;
    }
    match types.resolve(value_type) {
        Type::Record(record) => record
            .fields
            .iter()
            .any(|field| unbound(types, &field.value_type, field.output_binding.as_ref())),
        Type::Stdout | Type::Stderr => false,
        _ => true,
    }
}

/// Whether a type takes files and whether it takes directories, in lists and unions too.
pub(super) fn classes(types: &Types, value_type: &Type) -> (bool, bool) {
    match types.resolve(value_type) {
        Type::File | Type::Stdout | Type::Stderr => (true, false),
        Type::Directory => (false, true),
        Type::Any => (true, true),
        Type::Array(array) => classes(types, &array.items),
        Type::Union(members) => members
            .iter()
            .fold((false, false), |(files, folders), member| {
                let (file, folder) = classes(types, member);
                (files || file, folders || folder)
            }),
        _ => (false, false),
    }
}

/// Whether the files an output collects are directories.
pub(super) fn directory_type(types: &Types, value_type: &Type) -> bool {
    match types.resolve(value_type) {
        Type::Directory => true,
        Type::Array(array) => directory_type(types, &array.items),
        Type::Union(members) => {
            let others: Vec<&Type> = members
                .iter()
                .filter(|member| !matches!(member, Type::Null))
                .collect();
            !others.is_empty() && others.iter().all(|member| directory_type(types, member))
        }
        _ => false,
    }
}

/// The glob patterns of an output binding, relative to the output directory; `.` stands
/// for the output directory itself.
pub(super) fn globs(
    binding: &OutputBinding,
    evaluator: &Evaluator,
    vars: Variables<'_>,
    outdir: &str,
) -> Result<Vec<String>, String> {
    let mut patterns = Vec::new();
    for glob in &binding.glob {
        let value = evaluator.evaluate(glob, vars)?;
        let values = match value {
            Value::Null => Vec::new(),
            Value::String(text) => vec![text],
            Value::List(items) => {
                let mut texts = Vec::new();
                for item in items {
                    match item {
                        Value::String(text) => texts.push(text),
                        other => {
                            return Err(format!(
                                "a glob must be a string, not {}",
                                other.summary()
                            ));
                        }
                    }
                }
                texts
            }
            other => return Err(format!("a glob must be a string, not {}", other.summary())),
        };
        for text in values {
            if text.is_empty() {
                continue;
            }
            let relative = if text == outdir {
                ".".to_owned()
            } else if let Some(rest) = text.strip_prefix(&format!("{outdir}/")) {
                rest.to_owned()
            } else if text.starts_with('/') {
                return Err(format!("the glob `{text}` must not start with `/`"));
            } else {
                text
            };
            let relative = relative
                .strip_prefix("./")
                .map(str::to_owned)
                .unwrap_or(relative);
            patterns.push(if relative.is_empty() {
                ".".to_owned()
            } else {
                relative
            });
        }
    }
    Ok(patterns)
}

/// The file an abstract `Operation` is taken to make for an output, named after it.
pub(super) fn operation_output(types: &Types, output: &OutputParameter) -> Option<String> {
    let name = short_name(&output.id);
    let mut value_type = types.resolve(&output.value_type);
    loop {
        match value_type {
            Type::Array(array) => value_type = types.resolve(&array.items),
            Type::Union(members) => {
                let member = members
                    .iter()
                    .find(|member| !matches!(member, Type::Null))?;
                value_type = types.resolve(member);
            }
            Type::File | Type::Stdout | Type::Stderr => return Some(name.to_owned()),
            Type::Directory => return Some(format!("{name}/")),
            _ => return None,
        }
    }
}

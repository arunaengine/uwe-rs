//! peppy's sample modifiers: remove, append, duplicate, imply and derive, and the merging of
//! samples of the same name and of the rows of subsample tables.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use indexmap::IndexMap;
use python_evaluator::pandas::DataFrame;
use python_evaluator::{Args, Error, Interpreter, Value};

use super::{Attributes, call, peppy_error, rows};

/// `str.format` with the attributes of a sample, keeping unknown fields as they are.
fn safe_format(
    interpreter: &mut Interpreter,
    template: &str,
    values: &Attributes,
) -> Result<String, Error> {
    let mut output = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            output.push_str(&rest[open..]);
            return Ok(output);
        };
        let field = &rest[open + 1..open + close];
        let name = field.split([':', '!']).next().unwrap_or_default();
        match values.get(name) {
            Some(value) => output.push_str(&interpreter.to_str(value)?),
            None => output.push_str(&rest[open..=open + close]),
        }
        rest = &rest[open + close + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

/// The names of the fields of a template.
fn field_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let field = &rest[open + 1..open + close];
        names.push(
            field
                .split([':', '!'])
                .next()
                .unwrap_or_default()
                .to_owned(),
        );
        rest = &rest[open + close + 1..];
    }
    names
}

/// A derived attribute: the template of its source key filled with the sample's attributes,
/// one value per item of list attributes, and globs expanded.
fn derive(
    interpreter: &mut Interpreter,
    sample: &Attributes,
    sources: &Value,
    attribute: &str,
) -> Result<Value, Error> {
    let key = sample.get(attribute).cloned().unwrap_or(Value::None);
    let template = match interpreter.get_item(sources, &key) {
        Ok(template) => template,
        Err(error) if error.is("KeyError") || error.is("TypeError") => {
            return Ok(Value::str(""));
        }
        Err(error) => return Err(error),
    };
    let template = interpreter.to_str(&template)?;
    let expanded = call(
        interpreter,
        "os.path",
        "expandvars",
        Args::new(vec![Value::str(template)]),
    )?;
    let template = interpreter.to_str(&expanded)?;
    let keys = field_names(&template);
    let mut lengths = Vec::new();
    for (name, value) in sample {
        if let Value::List(items) = value.plain()
            && keys.contains(name)
        {
            lengths.push(items.borrow().len());
        }
    }
    let mut patterns = Vec::new();
    if lengths.is_empty() {
        patterns.push(safe_format(interpreter, &template, sample)?);
    } else {
        if lengths.iter().any(|length| *length != lengths[0]) {
            return Err(peppy_error(
                "InvalidSampleTableFileException",
                format!(
                    "All attributes to format the sources ({template}) have to be the same length, got: {lengths:?}. Correct your sample_table"
                ),
            ));
        }
        for position in 0..lengths[0] {
            let mut values = sample.clone();
            for name in &keys {
                if let Some(Value::List(items)) = values.get(name).map(Value::plain) {
                    let item = items.borrow()[position].clone();
                    values.insert(name.clone(), item);
                }
            }
            patterns.push(safe_format(interpreter, &template, &values)?);
        }
    }
    let mut outputs = Vec::new();
    for pattern in patterns {
        if pattern.contains(['*', '[']) {
            let found = call(
                interpreter,
                "glob",
                "glob",
                Args::new(vec![Value::str(pattern.as_str())]),
            )?;
            let mut found = interpreter.iterate(&found)?;
            if !found.is_empty() {
                found.sort_by_key(|value| value.as_str().map(str::to_owned));
                outputs.extend(found);
                continue;
            }
        }
        outputs.push(Value::str(pattern));
    }
    Ok(if outputs.len() == 1 {
        outputs.remove(0)
    } else {
        Value::list(outputs)
    })
}

/// A mapping value of the config, like a sample modifier.
fn section(interpreter: &mut Interpreter, value: Option<&Value>, key: &str) -> Option<Value> {
    let value = value?;
    interpreter
        .get_item(value, &Value::str(key))
        .ok()
        .filter(|value| !value.is_none())
}

fn contains(interpreter: &mut Interpreter, container: &Value, value: &Value) -> bool {
    match container.plain() {
        Value::List(_) | Value::Tuple(_) | Value::Set(_) => interpreter
            .iterate(container)
            .map(|items| {
                items
                    .iter()
                    .any(|item| python_evaluator::value::key_equal(item, value))
            })
            .unwrap_or(false),
        _ => python_evaluator::value::key_equal(container, value),
    }
}

/// Applies the sample modifiers, then merges samples of the same name and subsample rows,
/// as `Project.modify_samples` does.
pub(super) fn modify(
    interpreter: &mut Interpreter,
    samples: &mut Vec<Attributes>,
    modifiers: Option<&Value>,
    index: &str,
    subsample_tables: &[Value],
) -> Result<(), Error> {
    if let Some(remove) = section(interpreter, modifiers, "remove") {
        let names = interpreter.iterate(&remove)?;
        for sample in samples.iter_mut() {
            for name in &names {
                sample.shift_remove(&interpreter.to_str(name)?);
            }
        }
    }
    if let Some(append) = section(interpreter, modifiers, "append") {
        let items = interpreter.mapping_items(&append)?;
        for sample in samples.iter_mut() {
            for (name, value) in &items {
                let name = interpreter.to_str(name)?;
                sample.entry(name).or_insert_with(|| value.clone());
            }
        }
    }
    if let Some(duplicate) = section(interpreter, modifiers, "duplicate") {
        let items = interpreter.mapping_items(&duplicate)?;
        for sample in samples.iter_mut() {
            for (from, to) in &items {
                let from = interpreter.to_str(from)?;
                if let Some(value) = sample.get(&from).cloned() {
                    sample.insert(interpreter.to_str(to)?, value);
                }
            }
        }
    }
    if let Some(imply) = section(interpreter, modifiers, "imply") {
        let implications = interpreter.iterate(&imply)?;
        for sample in samples.iter_mut() {
            for implication in &implications {
                let conditions = interpreter.get_item(implication, &Value::str("if"))?;
                let results = interpreter.get_item(implication, &Value::str("then"))?;
                let mut applies = true;
                for (name, wanted) in interpreter.mapping_items(&conditions)? {
                    let name = interpreter.to_str(&name)?;
                    match sample.get(&name) {
                        Some(value) if contains(interpreter, &wanted, value) => {}
                        _ => {
                            applies = false;
                            break;
                        }
                    }
                }
                if applies {
                    for (name, value) in interpreter.mapping_items(&results)? {
                        sample.insert(interpreter.to_str(&name)?, value);
                    }
                }
            }
        }
    }
    let derive_section = section(interpreter, modifiers, "derive");
    let (attributes, sources) = match &derive_section {
        Some(derive) => {
            let attributes = section(interpreter, Some(derive), "attributes")
                .map(|value| match value.plain() {
                    Value::List(_) => interpreter.iterate(&value),
                    _ => Ok(vec![value.clone()]),
                })
                .transpose()?
                .unwrap_or_default();
            let mut names = Vec::new();
            for attribute in attributes {
                names.push(interpreter.to_str(&attribute)?);
            }
            (names, section(interpreter, Some(derive), "sources"))
        }
        None => (Vec::new(), None),
    };
    let mut derived: Vec<Vec<String>> = vec![Vec::new(); samples.len()];
    let derive_all = |interpreter: &mut Interpreter,
                      samples: &mut Vec<Attributes>,
                      derived: &mut Vec<Vec<String>>,
                      names: &[String]|
     -> Result<(), Error> {
        let Some(sources) = &sources else {
            return Ok(());
        };
        derived.resize(samples.len(), Vec::new());
        for (sample, done) in samples.iter_mut().zip(derived.iter_mut()) {
            for name in names {
                if !sample.contains_key(name) || done.contains(name) {
                    continue;
                }
                let original = sample[name].clone();
                sample.insert(format!("_key_{name}"), original);
                let value = derive(interpreter, sample, sources, name)?;
                if interpreter.truthy(&value)? {
                    sample.insert(name.clone(), value);
                }
                done.push(name.clone());
            }
        }
        Ok(())
    };
    // Sample names first, as merging needs them.
    if attributes.iter().any(|name| name == "sample_name") {
        derive_all(
            interpreter,
            samples,
            &mut derived,
            &["sample_name".to_owned()],
        )?;
    }
    if samples.iter().any(|sample| !sample.contains_key(index)) {
        return Err(peppy_error(
            "InvalidSampleTableFileException",
            format!(
                "sample_table is missing '{index}' column; you must specify sample_tables in {index} or derive them"
            ),
        ));
    }
    merge_duplicates(interpreter, samples, index, !subsample_tables.is_empty())?;
    for table in subsample_tables {
        merge_subsamples(interpreter, samples, table, index)?;
    }
    derive_all(interpreter, samples, &mut derived, &attributes)?;
    Ok(())
}

/// Samples of the same name become one, whose attributes are lists of the values, or the
/// value when all are the same.
fn merge_duplicates(
    interpreter: &mut Interpreter,
    samples: &mut Vec<Attributes>,
    index: &str,
    has_subsamples: bool,
) -> Result<(), Error> {
    let mut names: Vec<String> = Vec::new();
    for sample in samples.iter() {
        names.push(interpreter.to_str(&sample[index])?);
    }
    let mut duplicated: Vec<String> = Vec::new();
    for (position, name) in names.iter().enumerate() {
        if names[..position].contains(name) && !duplicated.contains(name) {
            duplicated.push(name.clone());
        }
    }
    if duplicated.is_empty() {
        return Ok(());
    }
    if has_subsamples {
        return Err(peppy_error(
            "IllegalStateException",
            format!(
                "Duplicated sample names found and subsample_table is specified in the config; you may use either auto-merging or subsample_table-based merging. Duplicates: {duplicated:?}"
            ),
        ));
    }
    for name in duplicated {
        let (same, others): (Vec<Attributes>, Vec<Attributes>) = std::mem::take(samples)
            .into_iter()
            .partition(|sample| sample[index].as_str() == Some(name.as_str()));
        *samples = others;
        let mut merged = Attributes::new();
        for attribute in same[0].keys().filter(|key| !key.starts_with('_')) {
            let mut values = Vec::new();
            for sample in &same {
                let value = sample.get(attribute).cloned().unwrap_or(Value::str(""));
                match value.plain() {
                    Value::List(items) => values.extend(items.borrow().iter().cloned()),
                    _ => values.push(value),
                }
            }
            let first = values[0].clone();
            let all_same = values
                .iter()
                .all(|value| python_evaluator::value::key_equal(value, &first));
            merged.insert(
                attribute.clone(),
                if all_same { first } else { Value::list(values) },
            );
        }
        samples.push(merged);
    }
    Ok(())
}

/// The rows of a subsample table become list attributes of their sample.
fn merge_subsamples(
    interpreter: &mut Interpreter,
    samples: &mut [Attributes],
    table: &Value,
    index: &str,
) -> Result<(), Error> {
    let rows = rows(interpreter, table)?;
    let columns: Vec<String> = match table.as_native::<DataFrame>() {
        Some(frame) => {
            let mut names = Vec::new();
            for label in &frame.data().columns.values {
                names.push(interpreter.to_str(label)?);
            }
            names
        }
        None => Vec::new(),
    };
    if !columns.iter().any(|column| column == index) {
        return Err(Error::key_error(Value::str(format!(
            "Subannotation requires column '{index}'."
        ))));
    }
    for sample in samples.iter_mut() {
        let name = sample[index].clone();
        let mine: Vec<(usize, &Attributes)> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.get(index)
                    .is_some_and(|value| python_evaluator::value::key_equal(value, &name))
            })
            .collect();
        if mine.is_empty() {
            continue;
        }
        let mut merged: IndexMap<String, Vec<Value>> = columns
            .iter()
            .map(|column| (column.clone(), Vec::new()))
            .collect();
        for (position, row) in mine {
            let mut row = row.clone();
            row.entry("subsample_name".into())
                .or_insert_with(|| Value::str(position.to_string()));
            for (attribute, value) in row {
                if attribute == index || !interpreter.truthy(&value)? {
                    continue;
                }
                match merged.get_mut(&attribute) {
                    Some(values) => values.push(value),
                    None => {
                        let text = interpreter.to_str(&value)?;
                        merged.insert(attribute, vec![Value::str(text.trim_end())]);
                    }
                }
            }
        }
        merged.shift_remove(index);
        for (attribute, values) in merged {
            sample.insert(attribute, Value::list(values));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_unknown_fields() {
        let host = std::rc::Rc::new(python_evaluator::LocalHost::new(std::env::temp_dir()));
        let mut interpreter = Interpreter::new(host);
        let mut sample = Attributes::new();
        sample.insert("name".into(), Value::str("a"));
        let text =
            safe_format(&mut interpreter, "${HOME}/{name}_{read}.fq", &sample).expect("formats");
        assert_eq!(text, "${HOME}/a_{read}.fq");
        assert_eq!(field_names("{a}/{b:>3}"), ["a", "b"]);
    }
}

//! PEP projects: `pepfile:` loads a project as peppy 0.40 does, and `pep` in the Snakefile is
//! the project. peppy reads the project config, the sample table and the subsample tables as
//! texts, applies the sample modifiers (remove, append, duplicate, imply, derive), merges
//! samples of the same name and the rows of subsample tables, and gives the samples as a table
//! indexed by their names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod modifiers;

use indexmap::IndexMap;
use python_evaluator::pandas::DataFrame;
use python_evaluator::{Args, Error, Interpreter, Value};

/// The attributes of a sample, in the order of the table.
type Attributes = IndexMap<String, Value>;

fn peppy_error(class: &str, message: impl Into<String>) -> Error {
    Error::new(class, message.into())
}

/// Calls a function of a module, like `os.path.expandvars`.
fn call(
    interpreter: &mut Interpreter,
    module: &str,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    let module = interpreter.import_module(module)?;
    let function = interpreter.get_attribute(&module, name)?;
    interpreter.call(&function, args)
}

/// The rows of a table as attributes by column.
fn rows(interpreter: &mut Interpreter, table: &Value) -> Result<Vec<Attributes>, Error> {
    let Some(frame) = table.as_native::<DataFrame>() else {
        return Ok(Vec::new());
    };
    let data = frame.data();
    let mut names = Vec::new();
    for label in &data.columns.values {
        names.push(interpreter.to_str(label)?);
    }
    Ok((0..data.rows())
        .map(|row| {
            names
                .iter()
                .enumerate()
                .map(|(column, name)| (name.clone(), data.data[column][row].clone()))
                .collect()
        })
        .collect())
}

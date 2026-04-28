//! `pd.options`, `pd.errors` and `pd.api.types`: settings, the exceptions pandas raises,
//! and checks of types.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;

use super::{is_list_like, seq_values};
use crate::builtins::Record;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::modules::function;
use crate::value::{Dict, Native, Value};

/// `pd.options`: settings that code may read and set, like `mode.chained_assignment`.
struct Options(RefCell<Dict>);

impl Native for Options {
    fn type_name(&self) -> &str {
        "DictWrapper"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let mut settings = self.0.borrow_mut();
        if let Some(value) = settings.get(name) {
            return Ok(Some(value.clone()));
        }
        let nested = Value::native(Options(RefCell::new(Dict::default())));
        settings.insert(name, nested.clone());
        Ok(Some(nested))
    }

    fn set_attribute(&self, _: &mut Interpreter, name: &str, value: Value) -> Result<bool, Error> {
        self.0.borrow_mut().insert(name, value);
        Ok(true)
    }
}

pub fn options() -> Value {
    Value::native(Options(RefCell::new(Dict::default())))
}

/// `pd.errors` with the exceptions pandas raises while reading.
pub fn errors() -> Value {
    let mut entries = Dict::default();
    for (name, base) in [
        ("EmptyDataError", "ValueError"),
        ("ParserError", "ValueError"),
        ("IntCastingNaNError", "ValueError"),
        ("UndefinedVariableError", "NameError"),
        ("DtypeWarning", "Warning"),
        ("SettingWithCopyWarning", "Warning"),
        ("PerformanceWarning", "Warning"),
    ] {
        entries.insert(
            name,
            Value::Class(crate::types::library_exception(name, "pandas.errors", base)),
        );
    }
    Value::native(Record(RefCell::new(entries), "module"))
}

/// `pd.api.types` with the checks of types that workflows use.
pub fn api() -> Value {
    let mut types = Dict::default();
    types.insert(
        "is_list_like",
        function("is_list_like", |_, args| {
            Ok(Value::Bool(args.get(0).is_some_and(is_list_like)))
        }),
    );
    for (name, wanted) in [
        ("is_numeric_dtype", &["int64", "float64", "bool"][..]),
        ("is_integer_dtype", &["int64"][..]),
        ("is_float_dtype", &["float64"][..]),
        ("is_bool_dtype", &["bool"][..]),
        ("is_string_dtype", &["str", "object"][..]),
        ("is_object_dtype", &["object"][..]),
    ] {
        types.insert(
            name,
            Value::builtin(name, move |interpreter, args| {
                let value = args.get(0).cloned().unwrap_or(Value::None);
                let kind = match value.as_str() {
                    Some(text) => text.to_owned(),
                    None => {
                        let values = seq_values(interpreter, &value)?;
                        super::dtype_name(&values).to_owned()
                    }
                };
                Ok(Value::Bool(wanted.contains(&kind.as_str())))
            }),
        );
    }
    let mut api = Dict::default();
    api.insert(
        "types",
        Value::native(Record(RefCell::new(types), "module")),
    );
    Value::native(Record(RefCell::new(api), "module"))
}

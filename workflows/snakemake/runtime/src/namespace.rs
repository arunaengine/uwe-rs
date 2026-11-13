//! Provides the imported modules and names available in every Snakefile's workflow globals.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use python_evaluator::{Error, Interpreter, Scope, Value};

/// The modules that `snakemake.workflow` imports.
pub(crate) const MODULES: &[&str] = &[
    "os",
    "sys",
    "re",
    "copy",
    "hashlib",
    "platform",
    "subprocess",
    "tempfile",
    "threading",
    "tarfile",
    "snakemake",
];

/// Names that `snakemake.workflow` imports from modules: the name, its module, and its name
/// there.
pub(crate) const NAMES: &[(&str, &str, &str)] = &[
    ("Path", "pathlib", "Path"),
    ("OrderedDict", "collections", "OrderedDict"),
    ("namedtuple", "collections", "namedtuple"),
    ("datetime", "datetime", "datetime"),
    ("chain", "itertools", "chain"),
    ("filterfalse", "itertools", "filterfalse"),
    ("partial", "functools", "partial"),
    ("dataclass", "dataclasses", "dataclass"),
    ("field", "dataclasses", "field"),
    ("Callable", "typing", "Callable"),
    ("Optional", "typing", "Optional"),
    ("Union", "typing", "Union"),
    ("Dict", "typing", "Dict"),
    ("Iterable", "typing", "Iterable"),
    ("List", "typing", "List"),
    ("Set", "typing", "Set"),
    ("Mapping", "typing", "Mapping"),
    ("Iterator", "typing", "Iterator"),
    ("WorkflowError", "snakemake.exceptions", "WorkflowError"),
    ("logger", "snakemake.logging", "logger"),
];

/// Puts the modules and names of `snakemake.workflow` into the globals of a Snakefile.
pub fn install(interpreter: &mut Interpreter, globals: &Rc<Scope>) -> Result<(), Error> {
    for name in MODULES {
        let module = interpreter.import_module(name)?;
        globals.set(name, module);
    }
    for (name, module, attribute) in NAMES {
        let module = interpreter.import_module(module)?;
        if let Some(value) = interpreter.find_attribute(&module, attribute)? {
            globals.set(name, value);
        }
    }
    globals.set("ON_WINDOWS", Value::Bool(false));
    // The file of Snakemake's module `snakemake.workflow`, which is not on disk here.
    globals.set("__file__", Value::str("/snakemake/workflow.py"));
    Ok(())
}

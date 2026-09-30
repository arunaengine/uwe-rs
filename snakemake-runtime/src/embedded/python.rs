//! Python: the preamble of scripts, wrappers and notebooks, which makes the `snakemake` object
//! as Snakemake's `PythonScript.generate_preamble` does. Snakemake pickles the object; the
//! container of a task has no Snakemake to unpickle it with, so iunctus writes the object as
//! Python code that builds it with the stand-in package (`snakemake/iocontainers.py`).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::format::{bytes_repr, quote};
use python_evaluator::number::float_repr;

use super::data::{Data, Named};
use super::{Context, PACKAGE_FOLDER};

/// The first and the last line of a preamble, as Snakemake marks them.
pub const START: &str =
    "######## snakemake preamble start (automatically inserted, do not edit) ########";
pub const END: &str = "######## snakemake preamble end #########";

/// The name under which the preamble refers to the stand-in's `iocontainers` module.
const MODULE: &str = "__iunctus";

/// A float as a Python literal.
fn float(value: f64) -> String {
    if value.is_nan() {
        "float('nan')".into()
    } else if value.is_infinite() {
        if value > 0.0 {
            "float('inf')".into()
        } else {
            "-float('inf')".into()
        }
    } else {
        float_repr(value)
    }
}

fn joined(items: &[Data], types: &mut Vec<(usize, &'static str)>) -> String {
    items
        .iter()
        .map(|item| literal_with(item, types, None))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Python code that makes a value. A table becomes its stored form, and its kind is noted in
/// `types` by the position of the param it is, as Snakemake's `_params_types` holds it.
fn literal_with(
    data: &Data,
    types: &mut Vec<(usize, &'static str)>,
    param: Option<usize>,
) -> String {
    match data {
        Data::None => "None".into(),
        Data::Bool(true) => "True".into(),
        Data::Bool(false) => "False".into(),
        Data::Int(digits) => digits.clone(),
        Data::Float(value) => float(*value),
        Data::Complex(real, imag) => format!("complex({}, {})", float(*real), float(*imag)),
        Data::Str(text) => quote(text),
        Data::Bytes(bytes) => bytes_repr(bytes),
        Data::Path(path) => format!("__import__('pathlib').PosixPath({})", quote(path)),
        Data::List(items) => format!("[{}]", joined(items, types)),
        Data::Tuple(items) if items.len() == 1 => format!("({},)", joined(items, types)),
        Data::Tuple(items) => format!("({})", joined(items, types)),
        Data::Set(items) if items.is_empty() => "set()".into(),
        Data::Set(items) => format!("{{{}}}", joined(items, types)),
        Data::Dict(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    literal_with(key, types, None),
                    literal_with(value, types, None)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Data::Named(named) => named_list(named, types),
        Data::Table { kind, stored, .. } => {
            if let Some(position) = param {
                types.push((position, kind));
            }
            literal_with(stored, types, None)
        }
        Data::Scalar { kind, value } => format!(
            "__import__('numpy').{kind}({})",
            literal_with(value, types, None)
        ),
        Data::Opaque { repr, .. } => format!(
            "{MODULE}.Unavailable({})",
            quote(&format!(
                "{repr} is no plain data (a number, text, list, dict or path), so iunctus \
                 cannot hand it to the code of the job in its container"
            ))
        ),
    }
}

/// Python code that makes a value.
pub fn literal(data: &Data) -> String {
    literal_with(data, &mut Vec::new(), None)
}

/// The class of the stand-in for a kind of `Namedlist`.
fn class(kind: &str) -> &str {
    match kind {
        "InputFiles" | "OutputFiles" | "Params" | "Wildcards" | "Log" => kind,
        "Resources" | "ResourceList" => "ResourceList",
        _ => "Namedlist",
    }
}

/// Code that makes a `Namedlist` of a kind from the code of its values and its names.
fn named_code(named: &Named, values: &str) -> String {
    let names = named
        .names
        .iter()
        .map(|(name, start, end)| {
            let end = end.map_or_else(|| "None".to_owned(), |end| end.to_string());
            format!("({}, {start}, {end})", quote(name))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{MODULE}.named({MODULE}.{}, [{values}], [{names}])",
        class(&named.kind)
    )
}

fn named_list(named: &Named, types: &mut Vec<(usize, &'static str)>) -> String {
    named_code(named, &joined(&named.values, types))
}

/// The params, whose tables Snakemake keeps as plain data with their kinds.
fn params(named: &Named) -> (String, Vec<(usize, &'static str)>) {
    let mut types = Vec::new();
    let values = named
        .values
        .iter()
        .enumerate()
        .map(|(position, value)| literal_with(value, &mut types, Some(position)))
        .collect::<Vec<_>>()
        .join(", ");
    (named_code(named, &values), types)
}

/// Code that makes the object of a job under a name, with the kinds of its tables in params,
/// on one line. It needs the stand-in's `iocontainers` module as `__iunctus`.
pub fn object(context: &Context, name: &str) -> String {
    let (params, types) = params(&context.params);
    let mut code = format!(
        "{name} = {MODULE}.Snakemake({}, {}, {params}, {}, {}, {}, {}, {}, {}, {}, {}); ",
        literal(&Data::Named(context.input.clone())),
        literal(&Data::Named(context.output.clone())),
        literal(&Data::Named(context.wildcards.clone())),
        context.threads,
        literal(&Data::Named(context.resources.clone())),
        literal(&Data::Named(context.log.clone())),
        literal(&context.config),
        quote(&context.rule),
        context
            .bench_iteration
            .map_or_else(|| "None".to_owned(), |value| value.to_string()),
        quote(&context.scriptdir),
    );
    if !types.is_empty() {
        let entries = types
            .iter()
            .map(|(position, kind)| format!("{position}: {}", quote(kind)))
            .collect::<Vec<_>>()
            .join(", ");
        code.push_str(&format!("{name}._params_types = {{{entries}}}; "));
    }
    code
}

/// Code that puts the stand-in of the snakemake package first on the module path, and the
/// given folders last, and imports its `iocontainers` module as `__iunctus`.
pub fn module_path(searchpaths: &[String]) -> String {
    format!(
        "import sys; sys.path.insert(0, __import__('os').path.abspath({})); \
         sys.path.extend([{}]); import snakemake.iocontainers as {MODULE}; ",
        quote(PACKAGE_FOLDER),
        searchpaths
            .iter()
            .map(|path| quote(path))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// The code of the preamble: the stand-in on the module path, the `snakemake` object, and
/// what Snakemake's preamble adds, like `__file__` of the original script. `prologue` comes
/// first, as it may change the working directory, which the module path depends on.
pub fn code(context: &Context, searchpaths: &[String], prologue: &str, addendum: &str) -> String {
    let mut code = format!(
        "{prologue}{}{}",
        module_path(searchpaths),
        object(context, "snakemake")
    );
    code.push_str(
        "from snakemake.logging import logger; from snakemake.iocontainers import Snakemake; ",
    );
    if let Some(shell) = context.resource("shell_exec") {
        code.push_str(&format!(
            "from snakemake.shell import shell; shell.executable({}); ",
            literal(&shell)
        ));
    }
    code.push_str(addendum);
    code
}

/// Code that makes a table, an array or a series again from its stored form, as Snakemake
/// makes the tables in params again.
pub fn table(kind: &str, stored: &Data) -> String {
    let stored = literal(stored);
    match kind {
        "pd.DataFrame" => format!("__import__('pandas').DataFrame.from_dict({stored})"),
        "pd.Series" => format!("__import__('pandas').Series({stored})"),
        "np.ndarray" => format!("__import__('numpy').array({stored})"),
        _ => stored,
    }
}

/// The preamble of a Python script, as Snakemake writes it in front of the script: its start
/// line, the code in one line, and its end line, so that line numbers shift as in Snakemake.
pub fn preamble(context: &Context, searchpaths: &[String], file: &str) -> String {
    let addendum = format!("__real_file__ = __file__; __file__ = {};", quote(file));
    format!(
        "{START}\n{}\n{END}\n",
        code(context, searchpaths, "", &addendum)
    )
}

/// The preamble cell of a Python notebook, whose kernel first moves to the working directory
/// of the job.
pub fn cell(context: &Context, searchpaths: &[String], workdir: &str) -> String {
    format!(
        "{START}\n{}\n{END}\n",
        code(context, searchpaths, workdir, "")
    )
}

#[cfg(test)]
mod tests {
    use super::literal;
    use crate::embedded::data::{Data, Named};

    #[test]
    fn serializes_python_literals() {
        assert_eq!(literal(&Data::Float(f64::NAN)), "float('nan')");
        assert_eq!(literal(&Data::Float(1e16)), "1e+16");
        assert_eq!(literal(&Data::Tuple(vec![Data::int(1)])), "(1,)");
        assert_eq!(literal(&Data::Set(Vec::new())), "set()");
        let scalar = Data::Scalar {
            kind: "float64".into(),
            value: Box::new(Data::Float(3.9)),
        };
        assert_eq!(literal(&scalar), "__import__('numpy').float64(3.9)");
        let named = Named {
            kind: "Params".into(),
            values: vec![Data::str("x")],
            names: vec![("a".into(), 0, None)],
        };
        assert_eq!(
            literal(&Data::Named(named)),
            "__iunctus.named(__iunctus.Params, ['x'], [('a', 0, None)])"
        );
        let function = Data::Opaque {
            repr: "<function f>".into(),
            text: "<function f>".into(),
        };
        assert!(literal(&function).starts_with("__iunctus.Unavailable('<function f> is no plain"));
    }
}

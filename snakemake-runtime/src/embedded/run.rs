//! Builds container Python modules for run blocks with job arguments and captured globals.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use python_evaluator::format::quote;
use python_evaluator::{Interpreter, Scope};
use workflow_graph::Input;

use super::globals::Transfer;
use super::{Context, SCRIPTS_FOLDER, Setting, python};
use crate::namespace;

/// The parameters of the function that Snakemake makes of a `run:` block, in its order.
const PARAMETERS: &[&str] = &[
    "input",
    "output",
    "params",
    "wildcards",
    "threads",
    "resources",
    "log",
    "rule",
    "conda_env",
    "container_img",
    "singularity_args",
    "use_singularity",
    "env_modules",
    "bench_record",
    "jobid",
    "is_shell",
    "bench_iteration",
    "cleanup_scripts",
    "shadow_dir",
    "edit_notebook",
    "conda_base_path",
    "basedir",
    "sourcecache_path",
    "runtime_sourcecache_path",
    "runtime_paths",
];

/// The local that tells `shell()` that it runs in the function of a rule.
const MARKER: &str = "__is_snakemake_rule_func";

/// The functions of `snakemake.io` that every Snakefile sees, which the stand-in has.
const IO_NAMES: &[&str] = &[
    "expand",
    "collect",
    "temp",
    "temporary",
    "protected",
    "directory",
    "touch",
    "ancient",
    "unpack",
    "multiext",
    "glob_wildcards",
    "pipe",
    "service",
    "local",
    "ensure",
    "report",
    "repeat",
    "limit",
    "flag",
];

/// How the Snakefile set up `shell()`.
pub struct Shell<'a> {
    pub executable: Option<&'a str>,
    pub prefix: Option<&'a str>,
    pub suffix: &'a str,
}

/// A `run:` block of a job.
pub struct Block<'a> {
    pub rule: &'a str,
    /// The body, with the indentation it has in the Snakefile.
    pub text: &'a str,
    /// The globals of the Snakefile that defines the rule.
    pub globals: &'a Rc<Scope>,
    pub jobid: u64,
    /// The folder of that Snakefile.
    pub basedir: &'a Path,
    pub shell: Shell<'a>,
}

/// What a `run:` block adds to its task.
pub struct Parts {
    pub command: Vec<String>,
    pub inputs: Vec<Input>,
}

/// A name for Python code: characters that a name cannot have become `_`.
fn identifier(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The names that the module imports, as every Snakefile sees them.
fn header_names() -> HashSet<&'static str> {
    let mut names: HashSet<&str> = namespace::MODULES.iter().copied().collect();
    names.extend(namespace::NAMES.iter().map(|(name, _, _)| *name));
    names.extend(IO_NAMES);
    names.extend(["shell", "ON_WINDOWS"]);
    names
}

/// The imports of the names that every Snakefile sees.
fn header() -> String {
    let mut lines: Vec<String> = namespace::MODULES
        .iter()
        .map(|module| format!("import {module}"))
        .collect();
    for (name, module, attribute) in namespace::NAMES {
        lines.push(if name == attribute {
            format!("from {module} import {attribute}")
        } else {
            format!("from {module} import {attribute} as {name}")
        });
    }
    lines.push("from snakemake.shell import shell".into());
    lines.push(format!("from snakemake.io import {}", IO_NAMES.join(", ")));
    lines.push("ON_WINDOWS = False".into());
    lines.join("\n")
}

/// The body with an indentation, as the body of a function: a body on the line of `run:` gets
/// one. Lines of a text over several lines may have none, so the first line decides.
fn body(text: &str) -> String {
    let indented = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.starts_with([' ', '\t']));
    if indented {
        return text.to_owned();
    }
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("    {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The module of a `run:` block and the task parts that run it.
pub fn build(
    interpreter: &mut Interpreter,
    context: &Context,
    block: &Block<'_>,
    setting: &Setting,
    workdir: &Path,
) -> Result<Parts, String> {
    let mut skipped: HashSet<&'static str> = PARAMETERS.iter().copied().collect();
    skipped.insert("config");
    let mut transfer = Transfer::new(
        interpreter,
        Rc::clone(block.globals),
        workdir,
        skipped,
        header_names(),
    );
    // The block parses as the body of a function, with the indentation it has.
    transfer.code_uses(&format!("def __block():\n{}\n", body(block.text)));
    let function = format!("__rule_{}", identifier(block.rule));
    let mut shell = Vec::new();
    if let Some(executable) = block.shell.executable {
        shell.push(format!("shell.executable({})", quote(executable)));
    }
    if let Some(prefix) = block.shell.prefix {
        shell.push(format!("shell._process_prefix = {}", quote(prefix)));
    }
    if !block.shell.suffix.is_empty() {
        shell.push(format!(
            "shell._process_suffix = {}",
            quote(block.shell.suffix)
        ));
    }
    let arguments = format!(
        "__job.input, __job.output, __job.params, __job.wildcards, __job.threads, \
         __job.resources, __job.log, __job.rule, None, None, '', False, None, None, {}, False, \
         None, True, None, None, None, {}, None, None, None",
        block.jobid,
        quote(&block.basedir.to_string_lossy()),
    );
    let module = format!(
        "# The run: block of rule {rule}, as Snakemake runs it: a function with the values of the\n\
         # job as arguments, in a module with the globals of the Snakefile that it uses. Written\n\
         # by iunctus.\n\
         {path}\n\
         {header}\n\
         config = {config}\n\
         {shell}\n\
         {definitions}\n\
         def {function}({parameters}, {MARKER}=True):\n\
         {body}\n\n\
         {object}\n\
         {function}({arguments})\n",
        rule = block.rule,
        path = python::module_path(&transfer.searchpaths),
        header = header(),
        config = python::literal(&context.config),
        shell = shell.join("\n"),
        definitions = transfer.code.join("\n"),
        parameters = PARAMETERS.join(", "),
        body = body(block.text),
        object = python::object(context, "__job"),
    );
    let path = format!("{SCRIPTS_FOLDER}/{}.run.py", identifier(block.rule));
    let mut inputs = vec![super::literal(&path, &module), setting.package()?];
    inputs.append(&mut transfer.inputs);
    Ok(Parts {
        command: vec!["python3".into(), path],
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::body;

    #[test]
    fn preserves_body_indentation() {
        assert_eq!(
            body("        shell('x')\n        y = 1"),
            "        shell('x')\n        y = 1"
        );
        assert_eq!(body("shell('x')"), "    shell('x')");
        assert_eq!(
            body("        t = \"\"\"a\nb\"\"\""),
            "        t = \"\"\"a\nb\"\"\""
        );
    }
}

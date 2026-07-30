//! The context of embedded code: what the code of a job gets besides its files. Snakemake
//! gives scripts, wrappers and notebooks a `snakemake` object with the job's input, output,
//! params, wildcards, threads, resources, log, config and rule, through a preamble in each
//! language, and it runs `run:` blocks as functions with these names and the Snakefile's
//! globals. iunctus builds the same preambles while it plans the job, so that the code runs
//! unchanged in the task's container (decision 11); the design is in
//! docs/design/snakemake/embedded-code.md.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod bash;
pub mod data;
pub mod globals;
pub mod julia;
pub mod notebook;
pub mod python;
pub mod r;
pub mod run;
pub mod rust;
pub mod script;
pub mod wrapper;

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use python_evaluator::{Interpreter, Value};
use workflow_graph::Input;

use self::data::{Data, Named, from_value};

/// The folder of the work directory of a task where the stand-in of the snakemake package
/// is, which the Python preamble puts on the module path.
pub const PACKAGE_FOLDER: &str = ".snakemake/iunctus";

/// The folder of the work directory where the scripts with their preambles are, as Snakemake
/// writes them to `.snakemake/scripts`.
pub const SCRIPTS_FOLDER: &str = ".snakemake/scripts";

/// The modules of the stand-in of the snakemake package.
const PACKAGE: &[(&str, &str)] = &[
    ("__init__.py", include_str!("snakemake/__init__.py")),
    ("common.py", include_str!("snakemake/common.py")),
    ("exceptions.py", include_str!("snakemake/exceptions.py")),
    ("io.py", include_str!("snakemake/io.py")),
    ("iocontainers.py", include_str!("snakemake/iocontainers.py")),
    ("logging.py", include_str!("snakemake/logging.py")),
    ("script.py", include_str!("snakemake/script.py")),
    ("shell.py", include_str!("snakemake/shell.py")),
    ("utils.py", include_str!("snakemake/utils.py")),
];

/// A key of the stand-in's modules that stays the same between versions of iunctus with the
/// same modules: the FNV-1a hash of their names and texts.
fn package_key() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, content) in PACKAGE {
        for byte in name.bytes().chain([0]).chain(content.bytes()).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// What the code of jobs needs from the session: where the stand-in of the snakemake package
/// is on this machine, and whether the run is simulated.
pub struct Setting {
    scratch: PathBuf,
    package: OnceCell<PathBuf>,
    pub simulated: bool,
    /// The image of the Python container that the user gave (`-python-image`), which
    /// `run:` blocks without a container of their own run in.
    pub python_image: Option<String>,
}

impl Setting {
    /// `scratch` is a folder that the run may write to, like the scratch folder of the cache.
    pub fn new(scratch: &Path, simulated: bool) -> Setting {
        Setting {
            scratch: scratch.to_path_buf(),
            package: OnceCell::new(),
            simulated,
            python_image: None,
        }
    }

    /// The folder with the stand-in of the snakemake package, written once per run: the
    /// task sees it at `.snakemake/iunctus`, so that tasks share one folder rather than each
    /// carrying the modules.
    pub fn package(&self) -> Result<Input, String> {
        let folder = match self.package.get() {
            Some(folder) => folder.clone(),
            None => {
                let folder = write_package(&self.scratch).map_err(|error| {
                    format!("cannot write the stand-in of the snakemake package: {error}")
                })?;
                let _ = self.package.set(folder.clone());
                folder
            }
        };
        let source = folder.to_string_lossy().into_owned();
        Ok(Input {
            path: PACKAGE_FOLDER.to_owned(),
            source,
            directory: true,
            content: None,
            ..Default::default()
        })
    }
}

/// Writes the stand-in's modules into `<scratch>/snakemake/python-<key>/snakemake/`, unless
/// another run did. Each run writes a folder of its own and renames it into place, so that
/// runs at the same time never see half of it.
fn write_package(scratch: &Path) -> std::io::Result<PathBuf> {
    let folder = scratch
        .join("snakemake")
        .join(format!("python-{:016x}", package_key()));
    if folder.join("snakemake").join("__init__.py").is_file() {
        return Ok(folder);
    }
    let partial = scratch.join("snakemake").join(format!(
        ".python-{:016x}.{}",
        package_key(),
        std::process::id()
    ));
    let modules = partial.join("snakemake");
    std::fs::create_dir_all(&modules)?;
    for (name, content) in PACKAGE {
        std::fs::write(modules.join(name), content)?;
    }
    match std::fs::rename(&partial, &folder) {
        Ok(()) => Ok(folder),
        // Another run was faster.
        Err(_) if folder.join("snakemake").join("__init__.py").is_file() => {
            let _ = std::fs::remove_dir_all(&partial);
            Ok(folder)
        }
        Err(error) => Err(error),
    }
}

/// An input with literal content.
pub fn literal(path: &str, content: &str) -> Input {
    Input {
        path: path.to_owned(),
        source: String::new(),
        directory: false,
        content: Some(content.to_owned()),
        ..Default::default()
    }
}

/// The values of a job that its code gets.
#[derive(Clone, Debug)]
pub struct Context {
    pub input: Named,
    pub output: Named,
    pub params: Named,
    pub wildcards: Named,
    pub threads: i64,
    /// The resources with `_cores` and `_nodes`, in Snakemake's order.
    pub resources: Named,
    pub log: Named,
    pub config: Data,
    pub rule: String,
    /// The iteration of a benchmark; iunctus runs a job once.
    pub bench_iteration: Option<i64>,
    /// The folder of the script, or its web address.
    pub scriptdir: String,
}

/// The place of a resource in Snakemake's order of a job's resources: the temporary folder,
/// memory and disk (as the defaults give them), `_nodes`, the others, then `_cores`.
fn resource_rank(name: &str) -> u8 {
    match name {
        "tmpdir" => 0,
        "mem_mb" | "mem" | "mem_mib" => 1,
        "disk_mb" | "disk" | "disk_mib" => 2,
        "_nodes" => 3,
        "_cores" => 5,
        _ => 4,
    }
}

impl Context {
    /// The resources of a job in the order Snakemake gives them, from the threads and the
    /// evaluated resources.
    pub fn resources(
        interpreter: &mut Interpreter,
        threads: i64,
        evaluated: &[(String, Value)],
    ) -> Named {
        let mut entries: Vec<(String, Data)> = evaluated
            .iter()
            .map(|(name, value)| (name.clone(), from_value(interpreter, value)))
            .collect();
        let within = |name: &str| match name {
            "mem_mb" | "disk_mb" => 0,
            "mem" | "disk" => 1,
            _ => 2,
        };
        entries.push(("_nodes".into(), Data::int(1)));
        entries.push(("_cores".into(), Data::int(threads)));
        entries.sort_by_key(|(name, _)| (resource_rank(name), within(name)));
        let mut named = Named::new("Resources", Vec::new());
        for (name, value) in entries {
            named.values.push(value);
            named.names.push((name, named.values.len() - 1, None));
        }
        named
    }

    /// The resources without `_cores` and `_nodes`, which R, Julia and Rust get.
    pub fn plain_resources(&self) -> Vec<(String, Data)> {
        self.resources
            .items()
            .into_iter()
            .filter(|(name, _)| name != "_cores" && name != "_nodes")
            .collect()
    }

    pub fn resource(&self, name: &str) -> Option<Data> {
        self.resources.named(name)
    }
}

/// A text as Python's `json.dumps` writes it, with every character beyond ASCII escaped.
pub fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            c if (' '..='~').contains(&c) => quoted.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    quoted.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    quoted.push('"');
    quoted
}

/// The shebang line of a script with its line break, and the rest of the script, as
/// Snakemake's `strip_re` splits them with `^#![^\[].*?(\r\n|\n)`.
pub fn shebang(source: &str) -> (&str, &str) {
    if source.starts_with("#!")
        && !source[2..].starts_with('[')
        && let Some(end) = source.find('\n')
    {
        return source.split_at(end + 1);
    }
    ("", source)
}

#[cfg(test)]
mod tests {
    use super::{json_string, shebang};

    #[test]
    fn texts_are_written_as_python_json_writes_them() {
        assert_eq!(json_string("a\"b\\\n"), r#""a\"b\\\n""#);
        assert_eq!(json_string("é😀"), "\"\\u00e9\\ud83d\\ude00\"");
    }

    #[test]
    fn shebang_lines_are_split_off() {
        assert_eq!(shebang("#!/bin/bash\necho\n"), ("#!/bin/bash\n", "echo\n"));
        assert_eq!(shebang("#![allow(x)]\n"), ("", "#![allow(x)]\n"));
        assert_eq!(shebang("echo\n"), ("", "echo\n"));
    }
}

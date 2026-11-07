//! Records a shell command as a CWL CommandLineTool: parse it, run it once, write the tool.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

mod command;
mod probe;
mod tool;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use command::{name, parse};
pub use tool::{Argument, Binding, Input, Kind, Output, Tool};

/// What to record.
#[derive(Clone, Debug)]
pub struct Request {
    /// The command line, split into words. `|`, `>` and `2>` are shell operators.
    pub command: Vec<String>,
    /// The name of the tool file; without it, the command or its script names it.
    pub name: Option<String>,
    /// The container image of the `DockerRequirement` hint.
    pub image: Option<String>,
    /// The folder the command runs in, and where the tool file goes.
    pub folder: PathBuf,
}

/// A recorded tool and where it was written.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub path: PathBuf,
    pub tool: Tool,
    /// What the command wrote to standard output and standard error.
    pub log: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the command line is empty")]
    Empty,
    #[error("the name `{0}` must be a file name without a folder")]
    Name(String),
    #[error("{} exists; remove it or give another name", .0.display())]
    Exists(PathBuf),
    #[error("could not run `{program}`: {source}")]
    Start { program: String, source: io::Error },
    #[error("the command failed ({status}); nothing was written\n{log}")]
    Failed { status: String, log: String },
    #[error("could not read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("could not write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
}

/// Runs the command once in the folder, takes the files it writes as the outputs and writes
/// `<name>.cwl` there. A failed command writes nothing.
pub fn record(request: &Request) -> Result<Recorded, Error> {
    if request.command.is_empty() {
        return Err(Error::Empty);
    }
    let mut tool = parse(&request.command, &request.folder);
    tool.image = request.image.clone();
    let name = match &request.name {
        Some(name) => checked(name)?,
        None => name(&tool.base_command),
    };
    let path = request.folder.join(format!("{name}.cwl"));
    if path.exists() {
        return Err(Error::Exists(path));
    }
    let before = probe::snapshot(&request.folder)?;
    let log = probe::run(&request.command, &request.folder)?;
    let after = probe::snapshot(&request.folder)?;
    tool.outputs = (probe::changed(&before, &after).iter())
        .map(|found| Output::found(found, after[found].folder))
        .collect();
    tool.finish();
    fs::write(&path, tool.render()).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    Ok(Recorded { path, tool, log })
}

/// A name given by the user, without a `.cwl` ending.
fn checked(name: &str) -> Result<String, Error> {
    let name = name.strip_suffix(".cwl").unwrap_or(name);
    let plain = Path::new(name).file_name().is_some_and(|file| file == name);
    match plain && !name.starts_with('.') {
        true => Ok(name.to_owned()),
        false => Err(Error::Name(name.to_owned())),
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

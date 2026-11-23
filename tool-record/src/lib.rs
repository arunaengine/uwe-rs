//! Records a shell command as a CWL CommandLineTool: parse it, run it once, write the tool.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

mod command;
mod definition;
mod probe;
mod tool;

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tool_definition::{Source, SourceKind};

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
    /// Which files to write: the CWL tool, the tool definition, or both.
    pub format: Format,
}

/// The files a recording writes: `<name>.cwl`, `<name>.tool.json`, or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Cwl,
    Tool,
    Both,
}

/// A recorded tool and where it was written.
#[derive(Clone, Debug)]
pub struct Recorded {
    /// The CWL file, or the tool definition when no CWL file was written.
    pub path: PathBuf,
    /// The tool definition file, when one was written.
    pub definition: Option<PathBuf>,
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
/// `<name>.cwl`, `<name>.tool.json` or both there. A failed command writes nothing.
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
    let cwl = request.format != Format::Tool;
    let defined = request.format != Format::Cwl;
    let path = request.folder.join(format!("{name}.cwl"));
    let definition = request.folder.join(format!("{name}.tool.json"));
    for (wanted, file) in [(cwl, &path), (defined, &definition)] {
        if wanted && file.exists() {
            return Err(Error::Exists(file.clone()));
        }
    }
    let before = probe::snapshot(&request.folder)?;
    let log = probe::run(&request.command, &request.folder)?;
    let after = probe::snapshot(&request.folder)?;
    tool.outputs = (probe::changed(&before, &after).iter())
        .map(|found| Output::found(found, after[found].folder))
        .collect();
    tool.finish();
    let text = tool.render();
    if cwl {
        create(&path, &text)?;
    }
    if !defined {
        return Ok(Recorded {
            path,
            definition: None,
            tool,
            log,
        });
    }
    let mut described = tool.to_definition();
    described.id.clone_from(&name);
    described.title.clone_from(&name);
    // The CWL file beside the definition runs the tool.
    if cwl {
        let digest = Sha256::digest(text.as_bytes());
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        described.source = Some(Source {
            kind: SourceKind::Cwl,
            address: format!("{name}.cwl"),
            revision: format!("sha256:{hex}"),
        });
    }
    let json = serde_json::to_string_pretty(&described).unwrap_or_default();
    if let Err(error) = create(&definition, &format!("{json}\n")) {
        // The CWL file of this call goes too, so that no half of the pair stays.
        if cwl {
            let _ = fs::remove_file(&path);
        }
        return Err(error);
    }
    Ok(Recorded {
        path: if cwl { path } else { definition.clone() },
        definition: Some(definition),
        tool,
        log,
    })
}

/// Writes a new file. A file or link at its path, like one that the command made, stays as it
/// is, and a partly written file is removed.
fn create(file: &Path, text: &str) -> Result<(), Error> {
    let opened = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file);
    let mut opened = opened.map_err(|source| match source.kind() {
        io::ErrorKind::AlreadyExists => Error::Exists(file.to_owned()),
        _ => Error::Write {
            path: file.to_owned(),
            source,
        },
    })?;
    opened.write_all(text.as_bytes()).map_err(|source| {
        let _ = fs::remove_file(file);
        Error::Write {
            path: file.to_owned(),
            source,
        }
    })
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

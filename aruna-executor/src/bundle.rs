//! The input snapshot and result bundle produced inside the task container.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write;
use std::path::{Path, PathBuf};

use executor::{ExecutorError, Job, wrapper};

use crate::objects::MAX_BUNDLE;

pub const INPUT: &str = "/iunctus/input.tar";
pub const RESULT: &str = "/iunctus/result.tar";
pub const WORK: &str = "/iunctus/work";

/// Builds a bounded snapshot. The image needs `sh`, `tar`, and the wrapper's usual tools.
pub fn prepare(job: Job<'_>, directory: &Path) -> Result<PathBuf, ExecutorError> {
    let work = job.spec.workdir.as_deref().unwrap_or(WORK);
    if work != WORK && Path::new(work).starts_with("/iunctus") {
        return Err(ExecutorError::Unsupported(
            "Aruna reserves /iunctus for its wrapper".into(),
        ));
    }
    let folders = wrapper::Folders {
        work,
        control: "/iunctus/control",
        home: "/iunctus/home",
        outbox: "/iunctus/outbox",
        owner: "",
    };
    let path = directory.join("input.tar");
    let file = std::fs::File::create(&path)?;
    let mut archive = tar::Builder::new(Limited {
        file,
        remaining: MAX_BUNDLE,
    });
    archive.follow_symlinks(true);
    let mut start = format!(
        "#!/bin/sh\nset -eu\nmkdir -p /iunctus/control /iunctus/outbox /iunctus/home {}\ntar -xf {} -C /iunctus\n",
        wrapper::quote(work),
        INPUT
    );
    for input in &job.staging.inputs {
        if input.in_place {
            return Err(ExecutorError::Unsupported(
                "Aruna cannot modify a host input in place".into(),
            ));
        }
        let name = archive_path(&input.path)?;
        if input.directory {
            archive.append_dir_all(&name, &input.source)?;
        } else {
            archive.append_path_with_name(&input.source, &name)?;
        }
    }
    if job
        .staging
        .inputs
        .iter()
        .any(|input| input.path.starts_with('/'))
    {
        start.push_str("cp -R /iunctus/absolute/. /\n");
    }
    if work != WORK
        && job
            .staging
            .inputs
            .iter()
            .any(|input| !input.path.starts_with('/'))
    {
        start.push_str(&format!("cp -R /iunctus/work/. {}\n", wrapper::quote(work)));
    }
    for directory in &job.staging.directories {
        archive_path(directory)?;
        start.push_str(&format!(
            "mkdir -p {}\n",
            wrapper::quote(&inside(work, directory))
        ));
    }
    for input in &job.staging.inputs {
        let target = wrapper::quote(&inside(work, &input.path));
        if input.executable {
            start.push_str(&format!("chmod u+x {target}\n"));
        }
        if !input.copy {
            start.push_str(&format!("chmod -R a-w {target}\n"));
        }
    }
    if let Some(stdin) = &job.staging.stdin {
        archive.append_path_with_name(stdin, "control/stdin")?;
    }
    let script = wrapper::wrapper(job.spec, &folders);
    append(&mut archive, "wrapper.sh", script.as_bytes())?;
    start.push_str("set +e\nsh /iunctus/wrapper.sh\ncode=$?\nset -e\ntar -chf /iunctus/result.tar -C /iunctus control outbox\nexit \"$code\"\n");
    append(&mut archive, "start.sh", start.as_bytes())?;
    archive.into_inner()?.file.sync_all()?;
    Ok(path)
}

fn archive_path(path: &str) -> Result<PathBuf, ExecutorError> {
    if path.is_empty()
        || path.contains('\0')
        || path.split('/').any(|part| part == "..")
        || Path::new(path).starts_with("/iunctus")
    {
        return Err(ExecutorError::Unsupported(
            "Aruna input path escapes or overlaps its wrapper".into(),
        ));
    }
    Ok(match path.starts_with('/') {
        true => Path::new("absolute").join(path.trim_start_matches('/')),
        false => Path::new("work").join(path),
    })
}

fn inside(work: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.into()
    } else {
        format!("{work}/{path}")
    }
}

fn append<W: Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes)
}

struct Limited {
    file: std::fs::File,
    remaining: u64,
}

impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(std::io::Error::other(
                "Aruna input bundle exceeds the size limit",
            ));
        }
        let written = self.file.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

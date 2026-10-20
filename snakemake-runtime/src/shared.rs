//! Runs jobs in the one working directory that they all share, as Snakemake does, for
//! executors that see the folders of this machine at the same paths.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Component, Path, PathBuf};

use python_evaluator::Value;
use workflow_graph::{Input, Outcome, Task, Writable};

use crate::plan::Job;

/// What the driver keeps of a task in the shared directory until the task ends.
#[derive(Debug, Default)]
pub struct Placed {
    /// Each output as the task collects it, an absolute path, and as the job declared it.
    pub outputs: Vec<(String, String)>,
    /// Files written into the working directory for the task, like its scripts.
    pub written: Vec<PathBuf>,
}

/// Gets the working directory ready for a job, as Snakemake's `Job.prepare`: removes its old
/// outputs and logs, makes their folders, and notes where the task collects each output.
pub fn prepare(workdir: &Path, job: &Job, task: &Task, placed: &mut Placed) -> std::io::Result<()> {
    remove_outputs(workdir, job, true)?;
    let logs = texts(&job.log.values);
    let benchmark = job.benchmark.as_ref().and_then(Value::as_str);
    let mut files = texts(&job.output.values);
    files.extend(logs.iter().cloned().chain(benchmark.map(str::to_owned)));
    for file in files {
        if let Some(parent) = workdir.join(file).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    for declared in &task.outputs {
        let plain = declared.trim_end_matches('/');
        let mut collected = workdir.join(plain).to_string_lossy().into_owned();
        if declared.ends_with('/') {
            collected.push('/');
        }
        placed.outputs.push((collected, declared.clone()));
    }
    Ok(())
}

/// Makes a task run in the working directory itself: it sees the directory in place, gets
/// only the inputs from elsewhere, and collects its outputs where the job writes them.
pub fn place(workdir: &Path, task: &mut Task, placed: &mut Placed) -> std::io::Result<()> {
    for output in &mut task.outputs {
        let found = placed
            .outputs
            .iter()
            .find(|(_, declared)| declared == output);
        *output = match found {
            Some((collected, _)) => collected.clone(),
            None => workdir.join(&*output).to_string_lossy().into_owned(),
        };
    }
    let mut kept = Vec::new();
    for input in std::mem::take(&mut task.inputs) {
        let target = workdir.join(&input.path);
        let inside = normal(&target).starts_with(normal(workdir));
        // The job changes `update()` outputs in place; an input without a source is the
        // folder of an output, which the job gets anyway.
        if input.writable == Writable::Copy || (input.source.is_empty() && input.content.is_none())
        {
            continue;
        }
        if !inside || input.source.contains("://") {
            kept.push(input);
            continue;
        }
        if let Some(content) = &input.content {
            // An absolute path, since a job of a shadow directory starts there.
            let target = workdir.join(own_path(&input.path, task.id.0));
            rename(task, &input.path, &target.to_string_lossy());
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, content)?;
            placed.written.push(target);
            continue;
        }
        let source = Path::new(&input.source);
        let same = match (
            std::fs::canonicalize(source),
            std::fs::canonicalize(&target),
        ) {
            (Ok(source), Ok(target)) => source == target,
            _ => normal(source) == normal(&target),
        };
        if !same {
            copy_into(source, &target)?;
        }
    }
    let folder = workdir.to_string_lossy().into_owned();
    kept.push(Input {
        path: folder.clone(),
        source: folder.clone(),
        directory: true,
        writable: Writable::InPlace,
        ..Input::default()
    });
    task.inputs = kept;
    task.workdir = Some(folder);
    Ok(())
}

/// Takes back what a task left in the working directory once it ended: gives the outputs of
/// the outcome the paths the jobs declared, and removes the files written for it.
pub fn finish(placed: Placed, outcome: &mut Outcome) -> std::io::Result<()> {
    for file in &mut outcome.files {
        let path = file.path.trim_start_matches('/').trim_end_matches('/');
        let declared = placed
            .outputs
            .iter()
            .find(|(collected, _)| collected.trim_start_matches('/').trim_end_matches('/') == path);
        if let Some((_, declared)) = declared {
            file.path = declared.trim_end_matches('/').to_owned();
        }
    }
    for file in &placed.written {
        let _ = std::fs::remove_file(file);
    }
    Ok(())
}

/// Removes the outputs of a job that are there, except `update()` outputs, as Snakemake does
/// after a job failed; before a job runs, its logs go too.
pub fn remove_outputs(workdir: &Path, job: &Job, logs: bool) -> std::io::Result<()> {
    let logs = if logs { &job.log.values[..] } else { &[] };
    let outputs = (job.output.values.iter())
        .filter(|value| value.tag("update").is_none())
        .chain(logs)
        .filter_map(Value::as_str);
    for output in outputs {
        remove(&workdir.join(output.trim_end_matches('/')))?;
    }
    Ok(())
}

/// Removes a file, a link or a directory, if it is there.
pub fn remove(path: &Path) -> std::io::Result<()> {
    let result = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(_) => return Ok(()),
    };
    match result {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Makes an empty file for a `touch()` output that is not there, or a directory for one
/// flagged `directory()`, and gives one that is there the current time.
pub fn touch(path: &Path, directory: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if directory {
        std::fs::create_dir_all(path)?;
    }
    if path.is_dir() {
        return std::fs::File::open(path)?.set_modified(std::time::SystemTime::now());
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .set_modified(std::time::SystemTime::now())
}

fn texts(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// Copies a file or a directory to `target`, leaving files that already have that content.
fn copy_into(source: &Path, target: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_into(&entry.path(), &target.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if std::fs::read(target).ok() == Some(std::fs::read(source)?) {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, target).map(|_| ())
}

/// A path of its own for a file that the task writes below `.snakemake`, where tasks that run
/// at once would otherwise write the same file.
fn own_path(path: &str, task: u64) -> String {
    if !path.starts_with(".snakemake/") {
        return path.to_owned();
    }
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    format!("{folder}/iunctus-{task}-{name}")
}

/// Uses `new` for `old` in the command and the script of a task.
fn rename(task: &mut Task, old: &str, new: &str) {
    for word in &mut task.command {
        *word = word.replace(old, new);
    }
    if let Some(script) = &mut task.script {
        *script = script.replace(old, new);
    }
}

/// A path without `.` and with `..` taken back, without reading the disk.
fn normal(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_paths() {
        assert_eq!(
            own_path(".snakemake/scripts/x.py", 7),
            ".snakemake/scripts/iunctus-7-x.py"
        );
        assert_eq!(own_path("x.py", 7), "x.py");
    }
}

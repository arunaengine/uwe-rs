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
    pub shadows: Vec<Shadow>,
    /// Files written into the working directory for the task, like its scripts, which go
    /// once no task that runs needs them.
    pub written: Vec<PathBuf>,
}

/// The shadow directory of a job, with the outputs and logs that go back from it.
#[derive(Debug)]
pub struct Shadow {
    pub folder: PathBuf,
    pub outputs: Vec<String>,
    pub logs: Vec<String>,
}

/// Gets the working directory ready for a job, as Snakemake's `Job.prepare`: removes its old
/// outputs and logs, makes their folders and the shadow directory, and notes where the task
/// collects each output.
pub fn prepare(
    workdir: &Path,
    job: &Job,
    shadow: Option<&str>,
    task: &mut Task,
    placed: &mut Placed,
) -> std::io::Result<()> {
    remove_outputs(workdir, job, true)?;
    back_up(workdir, job)?;
    let logs = texts(&job.log.values);
    let benchmark = job.benchmark.as_ref().and_then(Value::as_str);
    let mut files = texts(&job.output.values);
    files.extend(logs.iter().cloned().chain(benchmark.map(str::to_owned)));
    for file in files {
        if let Some(parent) = workdir.join(file).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let folder = match shadow {
        Some(mode) if !task.command.is_empty() => {
            let folder = shadow_folder(workdir, task)?;
            populate(workdir, &folder, mode, job)?;
            let mut command = vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "cd \"$1\" || exit 1; shift; exec \"$@\"".to_owned(),
                "sh".to_owned(),
                folder.to_string_lossy().into_owned(),
            ];
            command.append(&mut task.command);
            task.command = command;
            let outputs = texts(&job.output.values);
            placed.shadows.push(Shadow {
                folder: folder.clone(),
                outputs,
                logs: logs.clone(),
            });
            Some(folder)
        }
        _ => None,
    };
    for declared in &task.outputs {
        let plain = declared.trim_end_matches('/');
        let base = match &folder {
            Some(folder) if Some(plain) != benchmark => folder,
            _ => workdir,
        };
        // Without `..`, which the collection of outputs refuses.
        let mut collected = normal(&base.join(plain)).to_string_lossy().into_owned();
        if declared.ends_with('/') {
            collected.push('/');
        }
        placed.outputs.push((collected, declared.clone()));
    }
    Ok(())
}

/// Makes a task run in the working directory itself: it sees the directory in place, gets
/// only the inputs from elsewhere, and collects its outputs where the job writes them.
pub fn place(
    workdir: &Path,
    task: &mut Task,
    placed: &mut Placed,
    run: &str,
) -> std::io::Result<()> {
    let mut folders: Vec<PathBuf> = Vec::new();
    for output in &mut task.outputs {
        let found = placed
            .outputs
            .iter()
            .find(|(_, declared)| declared == output);
        *output = match found {
            Some((collected, _)) => collected.clone(),
            None => normal(&workdir.join(&*output))
                .to_string_lossy()
                .into_owned(),
        };
        // The job writes an output outside the working directory in its folder, which it
        // gets as well, and no other folder there.
        let path = Path::new(output.trim_end_matches('/'));
        if path.starts_with(normal(workdir)) {
            continue;
        }
        let folder = path.parent().unwrap_or(path).to_path_buf();
        if folder.parent().is_none() {
            return Err(std::io::Error::other(format!(
                "the output {output} lies in the root folder, which iunctus does not give a job"
            )));
        }
        if !folders.iter().any(|other| folder.starts_with(other)) {
            folders.retain(|other| !other.starts_with(&folder));
            folders.push(folder);
        }
    }
    let mut kept = Vec::new();
    for input in std::mem::take(&mut task.inputs) {
        let target = workdir.join(&input.path);
        let inside = normal(&target).starts_with(normal(workdir));
        // An input without a source is the folder of an output, which the job gets anyway.
        // The inputs of a `copy-minimal` shadow count like others: the job reads copies.
        if input.source.is_empty() && input.content.is_none() {
            continue;
        }
        if !inside || input.source.contains("://") {
            kept.push(input);
            continue;
        }
        if let Some(content) = &input.content {
            // An absolute path, since a job of a shadow directory starts there.
            let target = workdir.join(own_path(&input.path, content));
            rename(task, &input.path, &target.to_string_lossy());
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            hold(workdir, &target, content, run)?;
            let path = target.to_string_lossy().into_owned();
            placed.written.push(target);
            kept.push(present(path, false));
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
        // The runner hashes a declared input where it is, for the cache key of the job. A job
        // that reads files it does not declare may get a stale stored result, as Snakemake's
        // own mtime checks may miss such a file; the README says so.
        kept.push(present(
            target.to_string_lossy().into_owned(),
            input.directory,
        ));
    }
    kept.extend(local_time());
    for folder in folders {
        let folder = folder.to_string_lossy().into_owned();
        kept.push(Input {
            path: folder.clone(),
            source: folder,
            directory: true,
            writable: Writable::Shared,
            ..Input::default()
        });
    }
    let folder = workdir.to_string_lossy().into_owned();
    kept.push(Input {
        path: folder.clone(),
        source: folder.clone(),
        directory: true,
        writable: Writable::Shared,
        ..Input::default()
    });
    task.inputs = kept;
    task.workdir = Some(folder);
    Ok(())
}

/// Writes a file of a task below the working directory and notes that this run uses it, by
/// a hard link of its own beside it, so that other runs in the folder keep it meanwhile.
fn hold(workdir: &Path, target: &Path, content: &str, run: &str) -> std::io::Result<()> {
    let _guard = guard(workdir)?;
    if std::fs::read(target).ok().as_deref() != Some(content.as_bytes()) {
        let temporary = suffixed(target, &format!("iunctus-{run}.part"));
        std::fs::write(&temporary, content)?;
        std::fs::rename(&temporary, target)?;
    }
    // Without hard links the file is this run's alone, as before.
    let _ = std::fs::hard_link(target, suffixed(target, &format!("iunctus-{run}")));
    Ok(())
}

/// Lets go of a file that `hold` wrote for this run; it goes once no other run holds it.
pub fn release(workdir: &Path, target: &Path, run: &str) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let _guard = guard(workdir)?;
    let _ = std::fs::remove_file(suffixed(target, &format!("iunctus-{run}")));
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.nlink() <= 1 => std::fs::remove_file(target),
        _ => Ok(()),
    }
}

/// An exclusive lock of the files that runs hold in the folder, while it is kept.
fn guard(workdir: &Path) -> std::io::Result<std::fs::File> {
    let folder = workdir.join(".snakemake");
    std::fs::create_dir_all(&folder)?;
    let file = (std::fs::OpenOptions::new().create(true).append(true))
        .open(folder.join("iunctus-written.lock"))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(file)
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

/// The time zone of this machine for the container, as Snakemake runs commands here: its
/// `/etc/localtime`, where the system has one.
pub fn local_time() -> Option<Input> {
    let source = std::fs::canonicalize("/etc/localtime").ok()?;
    Some(Input {
        path: "/etc/localtime".to_owned(),
        source: source.to_string_lossy().into_owned(),
        ..Input::default()
    })
}

/// Takes back what a task left in the working directory once it ended: moves the outputs of
/// a shadow directory to their place, only the logs after a failure, as Snakemake's
/// `unshadow_output`, and gives the outputs of the outcome the paths the jobs declared.
pub fn finish(placed: Placed, outcome: &mut Outcome, succeeded: bool) -> std::io::Result<()> {
    for shadow in &placed.shadows {
        let files = match succeeded {
            true => shadow
                .outputs
                .iter()
                .chain(&shadow.logs)
                .collect::<Vec<_>>(),
            false => shadow.logs.iter().collect(),
        };
        for file in files {
            unshadow(&shadow.folder, file)?;
        }
        let _ = std::fs::remove_dir_all(&shadow.folder);
    }
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
    Ok(())
}

/// Removes the outputs of a job that are there, except `update()` outputs, as Snakemake does
/// after a job failed; before a job runs, its logs go too.
pub fn remove_outputs(workdir: &Path, job: &Job, logs: bool) -> std::io::Result<()> {
    let logs = if logs { &job.log.values[..] } else { &[] };
    let outputs = (job.output.values.iter())
        .filter(|value| value.tag("update").is_none())
        .chain(logs);
    for value in outputs {
        let Some(output) = value.as_str() else {
            continue;
        };
        let path = workdir.join(output.trim_end_matches('/'));
        match value.tag("directory") {
            Some(_) => remove(&path)?,
            None => remove_output(&path, output)?,
        }
    }
    Ok(())
}

/// Copies the `update()` outputs of a job that are there to `.snakemake/backups`, as
/// Snakemake's `backup_output`, so that a failed job gives them back.
fn back_up(workdir: &Path, job: &Job) -> std::io::Result<()> {
    for output in updated(job) {
        let path = workdir.join(output.trim_end_matches('/'));
        let backup = backup_path(workdir, output);
        // A backup that is there already is of a job that was stopped before it ended: the
        // output may be half written, so it gets the content of the backup back first.
        if std::fs::symlink_metadata(&backup).is_ok() {
            eprintln!("Restored previous version of {output} (an earlier job was stopped).");
            remove(&path)?;
            copy_into(&backup, &path)?;
            continue;
        }
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        copy_into(&path, &backup)?;
    }
    Ok(())
}

/// Gives the `update()` outputs of a failed job their content from before the job, as
/// Snakemake's `restore_output`; one without a backup was not there, and goes.
pub fn restore_updates(workdir: &Path, job: &Job) -> std::io::Result<()> {
    for value in job
        .output
        .values
        .iter()
        .filter(|value| value.tag("update").is_some())
    {
        let Some(output) = value.as_str() else {
            continue;
        };
        let path = workdir.join(output.trim_end_matches('/'));
        let backup = backup_path(workdir, output);
        if std::fs::symlink_metadata(&backup).is_err() {
            match value.tag("directory") {
                Some(_) => remove(&path)?,
                None => remove_output(&path, output)?,
            }
            continue;
        }
        remove(&path)?;
        copy_into(&backup, &path)?;
        remove(&backup)?;
        eprintln!("Restored previous version of {output} (updating job failed).");
    }
    Ok(())
}

/// Removes the backups of the `update()` outputs of a job that ended well.
pub fn drop_backups(workdir: &Path, job: &Job) -> std::io::Result<()> {
    for output in updated(job) {
        remove(&backup_path(workdir, output))?;
    }
    Ok(())
}

fn updated(job: &Job) -> impl Iterator<Item = &str> {
    (job.output.values.iter())
        .filter(|value| value.tag("update").is_some())
        .filter_map(Value::as_str)
}

/// Where Snakemake keeps the backup of an output: below `.snakemake/backups`, an absolute
/// path without its root.
fn backup_path(workdir: &Path, output: &str) -> PathBuf {
    let output = output.trim_end_matches('/');
    let relative = Path::new(output)
        .strip_prefix("/")
        .unwrap_or(Path::new(output));
    workdir.join(".snakemake").join("backups").join(relative)
}

/// Removes an output that is not flagged `directory()`: a file, a link, or a folder only
/// when it is empty, as Snakemake's `remove` keeps a folder with files in it.
fn remove_output(path: &Path, shown: &str) -> std::io::Result<()> {
    let folder = std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir());
    if !folder {
        return remove(path);
    }
    match std::fs::remove_dir(path) {
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            eprintln!("Skipped removing non-empty directory {shown}");
            Ok(())
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The local inputs of a job that are not there, by Snakemake's `wait_for_files`, which
/// follows links. The files of `later` appear only while the task runs, like pipes.
pub fn missing_inputs(workdir: &Path, job: &Job, later: &[String]) -> Vec<String> {
    (job.input.values.iter())
        .filter(|value| value.tag(crate::storage::TAG).is_none())
        .filter_map(Value::as_str)
        .filter(|file| !later.iter().any(|other| other == file))
        .filter(|file| std::fs::metadata(workdir.join(file)).is_err())
        .map(str::to_owned)
        .collect()
}

/// Checks the outputs of a finished job as Snakemake's `check_and_touch_output`: gives those
/// that are a directory without `directory()` or the other way round. Otherwise each output
/// gets the current time, a real directory through its `.snakemake_timestamp`.
pub fn settle(workdir: &Path, job: &Job) -> std::io::Result<Vec<String>> {
    let outputs: Vec<(PathBuf, String, bool)> = (job.output.values.iter())
        .filter(|value| value.tag("service").is_none() && value.tag("pipe").is_none())
        .filter_map(|value| {
            let file = value.as_str()?;
            let path = workdir.join(file.trim_end_matches('/'));
            Some((path, file.to_owned(), value.tag("directory").is_some()))
        })
        .collect();
    let improper: Vec<String> = (outputs.iter())
        .filter(|(path, _, directory)| {
            std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir() != *directory)
        })
        .map(|(_, file, _)| file.clone())
        .collect();
    if !improper.is_empty() {
        return Ok(improper);
    }
    for (path, _, directory) in outputs {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if directory && !metadata.is_symlink() => {
                let stamp = path.join(".snakemake_timestamp");
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&stamp)?;
                touch_link(&stamp)?;
            }
            Ok(_) => touch_link(&path)?,
            Err(_) => {}
        }
    }
    Ok(Vec::new())
}

/// Gives a file, or a link itself, the current time, as Snakemake's `lutime`.
fn touch_link(path: &Path) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, CWD, Timestamps, UTIME_NOW, utimensat};
    let now = rustix::fs::Timespec {
        tv_sec: 0,
        tv_nsec: UTIME_NOW,
    };
    let times = Timestamps {
        last_access: now,
        last_modification: now,
    };
    utimensat(CWD, path, &times, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)
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

/// A shadow directory of its own below `.snakemake/shadow`, as Snakemake makes one.
fn shadow_folder(workdir: &Path, task: &Task) -> std::io::Result<PathBuf> {
    let parent = workdir.join(".snakemake").join("shadow");
    std::fs::create_dir_all(&parent)?;
    for number in 0.. {
        let folder = parent.join(format!("iunctus-{}-{number}", task.id.0));
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("a free shadow directory")
}

/// Fills a shadow directory as Snakemake's `Job.prepare` does for each depth.
fn populate(workdir: &Path, folder: &Path, mode: &str, job: &Job) -> std::io::Result<()> {
    match mode {
        "minimal" | "copy-minimal" => {
            let all = [&job.input.values, &job.output.values, &job.log.values];
            for file in all.into_iter().flat_map(|values| texts(values)) {
                if let Some(parent) = Path::new(&file).parent()
                    && !Path::new(&file).is_absolute()
                {
                    std::fs::create_dir_all(folder.join(parent))?;
                }
            }
            for input in texts(&job.input.values) {
                if Path::new(&input).is_absolute() {
                    continue;
                }
                let link = folder.join(&input);
                if mode == "copy-minimal" {
                    copy_into(&workdir.join(&input), &link)?;
                } else {
                    let base = link.parent().unwrap_or(folder);
                    let _ =
                        std::os::unix::fs::symlink(relative(&workdir.join(&input), base), &link);
                }
            }
        }
        "shallow" => {
            for entry in std::fs::read_dir(workdir)? {
                let entry = entry?;
                std::os::unix::fs::symlink(entry.path(), folder.join(entry.file_name()))?;
            }
        }
        _ => link_tree(workdir, workdir, folder)?,
    }
    Ok(())
}

/// The full shadow: the folders of the working directory, with links to each file.
fn link_tree(workdir: &Path, folder: &Path, shadow: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let source = entry.path();
        if source == workdir.join(".snakemake") {
            continue;
        }
        let target = shadow.join(source.strip_prefix(workdir).unwrap_or(&source));
        if std::fs::metadata(&source).is_ok_and(|metadata| metadata.is_dir()) {
            std::fs::create_dir_all(&target)?;
            link_tree(workdir, &source, shadow)?;
        } else {
            std::os::unix::fs::symlink(&source, &target)?;
        }
    }
    Ok(())
}

/// Moves an output of a shadow directory to its place; an absolute link becomes relative to
/// the shadow directory first, as in Snakemake.
fn unshadow(folder: &Path, file: &str) -> std::io::Result<()> {
    let shadowed = folder.join(file);
    let Some(real) = folder
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    else {
        return Ok(());
    };
    let real = real.join(file);
    if let Ok(target) = std::fs::read_link(&shadowed)
        && target.is_absolute()
    {
        std::fs::remove_file(&shadowed)?;
        std::os::unix::fs::symlink(relative(&target, folder), &shadowed)?;
    }
    if std::fs::symlink_metadata(&shadowed).is_err() {
        return Ok(());
    }
    if let (Ok(a), Ok(b)) = (
        std::fs::canonicalize(&shadowed),
        std::fs::canonicalize(&real),
    ) && a == b
    {
        return Ok(());
    }
    remove(&real)?;
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent)?;
    }
    move_path(&shadowed, &real)
}

/// Moves a file, link or folder as `shutil.move`: renames it, or copies it, links kept as
/// links, and removes it when the target is on another file system.
fn move_path(source: &Path, target: &Path) -> std::io::Result<()> {
    match std::fs::rename(source, target) {
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {}
        other => return other,
    }
    copy_tree(source, target)?;
    remove(source)
}

/// Copies a file, a link or a folder with its links as links and the times of its files.
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        return std::os::unix::fs::symlink(std::fs::read_link(source)?, target);
    }
    if metadata.is_dir() {
        std::fs::create_dir(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        return std::fs::set_permissions(target, metadata.permissions());
    }
    std::fs::copy(source, target)?;
    // The time is kept where the file may be written, as `shutil.copy2` keeps it.
    let modified = metadata.modified()?;
    let written = std::fs::File::options().write(true).open(target);
    let _ = written.and_then(|file| file.set_modified(modified));
    Ok(())
}

/// Lets the owner write a file, or the files and folders below a folder.
pub fn make_writable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_symlink() {
        return Ok(());
    }
    let mode = metadata.permissions().mode() | 0o200;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            make_writable(&entry?.path())?;
        }
    }
    Ok(())
}

/// Copies a file or a directory to `target`, leaving files that already have that content.
pub fn copy_into(source: &Path, target: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_into(&entry.path(), &target.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if same_content(source, target)? {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, target).map(|_| ())
}

/// Whether `target` is a file with the bytes of `source`, read piece by piece, so that a
/// large file never sits in memory.
fn same_content(source: &Path, target: &Path) -> std::io::Result<bool> {
    use std::io::BufRead;
    let size = std::fs::metadata(source)?.len();
    if std::fs::metadata(target).map_or(true, |metadata| metadata.len() != size) {
        return Ok(false);
    }
    let Ok(target) = std::fs::File::open(target) else {
        return Ok(false);
    };
    let mut left = std::io::BufReader::with_capacity(1 << 16, std::fs::File::open(source)?);
    let mut right = std::io::BufReader::with_capacity(1 << 16, target);
    loop {
        let (one, other) = (left.fill_buf()?, right.fill_buf()?);
        let length = one.len().min(other.len());
        if length == 0 {
            return Ok(one.is_empty() && other.is_empty());
        }
        if one[..length] != other[..length] {
            return Ok(false);
        }
        left.consume(length);
        right.consume(length);
    }
}

/// Copies a stored output into the working directory as the job left it: links stay links
/// where they lead into the working directory, and others become copies of what they name.
pub fn copy_restored(source: &Path, target: &Path, workdir: &Path) -> std::io::Result<()> {
    if let Ok(link) = std::fs::read_link(source) {
        let place = target.parent().unwrap_or(target).join(&link);
        if normal(&place).starts_with(normal(workdir)) {
            std::fs::create_dir_all(target.parent().unwrap_or(workdir))?;
            return std::os::unix::fs::symlink(link, target);
        }
        if std::fs::metadata(source).is_err() {
            return Ok(());
        }
    }
    if !source.is_dir() {
        return copy_into(source, target);
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        copy_restored(&entry.path(), &target.join(entry.file_name()), workdir)?;
    }
    Ok(())
}

/// A path of its own for a file that the task writes below `.snakemake`, where tasks that run
/// at once would otherwise write the same file.
fn own_path(path: &str, content: &str) -> String {
    if !path.starts_with(".snakemake/") {
        return path.to_owned();
    }
    use sha2::Digest;
    let digest = sha2::Sha256::digest(content.as_bytes());
    let short: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    // Named by the content, so that a job gets the same command in every run.
    format!("{folder}/iunctus-{short}-{name}")
}

/// An input that lies in the working directory already.
fn present(path: String, directory: bool) -> Input {
    Input {
        source: path.clone(),
        path,
        directory,
        ..Input::default()
    }
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

/// `os.path.relpath(target, base)` for absolute paths.
fn relative(target: &Path, base: &Path) -> PathBuf {
    let (target, base) = (normal(target), normal(base));
    let common = (target.components().zip(base.components()))
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in base.components().skip(common) {
        result.push("..");
    }
    for part in target.components().skip(common) {
        result.push(part);
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative(Path::new("/w/a/b.txt"), Path::new("/w/s/a")),
            PathBuf::from("../../a/b.txt")
        );
        assert_eq!(
            own_path(".snakemake/scripts/x.py", "a"),
            ".snakemake/scripts/iunctus-ca978112ca1b-x.py"
        );
        assert_eq!(own_path("x.py", "a"), "x.py");
    }

    /// The peak memory of this process, from `/proc/self/status`, in KiB.
    fn peak_memory() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    }

    /// Two large files of the same bytes are compared piece by piece, not read whole.
    #[test]
    fn compares_large_files() {
        let folder = tempfile::tempdir().expect("a folder");
        let (source, target) = (folder.path().join("a"), folder.path().join("b"));
        // Sparse files of 256 MiB, which take no disk space.
        for path in [&source, &target] {
            std::fs::File::create(path)
                .unwrap()
                .set_len(256 << 20)
                .unwrap();
        }
        let Some(before) = peak_memory() else {
            return;
        };
        copy_into(&source, &target).unwrap();
        let grown = peak_memory().unwrap() - before;
        assert!(grown < 128 << 10, "the peak grew by {grown} KiB");
        std::fs::write(&source, "same").unwrap();
        std::fs::write(&target, "sale").unwrap();
        copy_into(&source, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "same");
    }
}

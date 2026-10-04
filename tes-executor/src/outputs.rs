//! Declares TES FILE destinations and restores verified outputs into task scratch.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

use content_store::Store;
use executor::{ExecutorError, Finished, Job, manifest};

use crate::capture::{INDEX, RECORDS, TRANSFER};
use crate::client::{FileLog, FileType, Output, State, Status, Task};
use crate::staging::{
    ObjectStage, container_path, shell_command, storage_error, unsupported, wildcard,
};

pub(crate) fn declare(
    task: &mut Task,
    job: Job<'_>,
    work: &str,
    objects: &Store,
    prefix: &str,
    shell: bool,
) -> Result<(), ExecutorError> {
    if shell {
        for name in RECORDS {
            task.outputs.push(Output {
                path: format!("{}/{name}", super::backend::CONTROL),
                url: objects.url(&format!("{prefix}/control/{name}")),
                kind: FileType::File,
                path_prefix: None,
            });
        }
        task.outputs.push(Output {
            path: format!("{TRANSFER}/*"),
            url: format!("{}/", objects.url(&format!("{prefix}/files"))),
            kind: FileType::File,
            path_prefix: Some(format!("{TRANSFER}/")),
        });
    } else {
        for rule in &job.spec.outputs {
            if rule.pattern.ends_with('/')
                || rule.pattern.contains("**")
                || rule.pattern.contains(['{', '}'])
            {
                return Err(unsupported(
                    "TES directory outputs and shell glob extensions require a task shell",
                ));
            }
            let path = container_path(&rule.pattern, work)?;
            task.outputs.push(Output {
                url: match wildcard(&path) {
                    true => format!("{}/", objects.url(&format!("{prefix}/root"))),
                    false => objects.url(&format!("{prefix}/root{}", path)),
                },
                path_prefix: wildcard(&path).then(|| "/".into()),
                path,
                kind: FileType::File,
            });
        }
    }
    Ok(())
}

pub(crate) fn check_input(task: &Task, path: &str) -> Result<(), ExecutorError> {
    if task.inputs.iter().any(|input| overlaps(&input.path, path))
        || task.outputs.iter().any(|output| {
            if wildcard(&output.path) {
                let literal = output.path.split(['*', '?', '[']).next().unwrap_or("");
                let parent = literal
                    .rsplit_once('/')
                    .map(|(parent, _)| parent)
                    .unwrap_or("/");
                Path::new(parent).starts_with(path)
                    || Path::new(path)
                        .ancestors()
                        .filter_map(Path::to_str)
                        .any(|parent| matches(&output.path, parent))
            } else {
                overlaps(&output.path, path)
            }
        })
        || path == super::backend::WRAPPER
    {
        return Err(unsupported(
            "TES input paths overlap another input or output",
        ));
    }
    Ok(())
}

fn overlaps(left: &str, right: &str) -> bool {
    Path::new(left).starts_with(right) || Path::new(right).starts_with(left)
}

fn matches(pattern: &str, path: &str) -> bool {
    glob::Pattern::new(pattern).is_ok_and(|pattern| {
        pattern.matches_with(
            path,
            glob::MatchOptions {
                require_literal_separator: true,
                require_literal_leading_dot: true,
                ..glob::MatchOptions::new()
            },
        )
    })
}

impl ObjectStage {
    pub(crate) async fn collect(
        &self,
        job: Job<'_>,
        task: &Task,
        status: &Status,
        root: &Path,
    ) -> Result<Finished, ExecutorError> {
        let control = root.join(executor::CONTROL);
        tokio::fs::create_dir_all(&control).await?;
        let log = status.logs.last().and_then(|log| log.logs.last());
        let shell = shell_command(&job.spec.arguments);
        if status.state == State::ExecutorError || !shell {
            for (name, text) in [
                ("stdout", log.and_then(|log| log.stdout.as_deref())),
                ("stderr", log.and_then(|log| log.stderr.as_deref())),
            ] {
                tokio::fs::write(control.join(name), text.unwrap_or_default()).await?;
            }
        }
        if status.state == State::ExecutorError {
            let exit_code = log
                .and_then(|log| log.exit_code)
                .filter(|code| *code != 0)
                .unwrap_or(1);
            return Ok(Finished {
                exit_code: Some(exit_code),
                files: Vec::new(),
                root: root.into(),
                usage: None,
            });
        }
        let prefix = super::staging::attempt_prefix(job)?;
        let mut outputs: BTreeMap<String, (Option<u64>, Option<String>)> = BTreeMap::new();
        if let Some(log) = status.logs.last() {
            for file in &log.outputs {
                let (location, version) = self.file_location(task, file)?;
                let size = file
                    .size_bytes
                    .parse()
                    .map_err(|_| storage_error("TES output log has an invalid size"))?;
                outputs.insert(location, (Some(size), version));
            }
        }
        if outputs.is_empty() {
            for object in self
                .objects
                .objects(&format!("{prefix}/"))
                .await
                .map_err(storage_error)?
            {
                outputs.insert(object.location, (None, None));
            }
        }
        let mut finished = Finished {
            exit_code: log.and_then(|log| log.exit_code).or(Some(0)),
            files: Vec::new(),
            root: root.into(),
            usage: None,
        };
        let mut transferred = BTreeMap::new();
        for (location, (size, version)) in outputs {
            let relative = location
                .strip_prefix(&format!("{prefix}/"))
                .ok_or_else(|| storage_error("TES output is outside its attempt prefix"))?;
            let target = if shell {
                if let Some(name) = relative
                    .strip_prefix("control/")
                    .filter(|name| RECORDS.contains(name))
                {
                    control.join(name)
                } else if let Some(name) = relative.strip_prefix("files/").filter(|name| {
                    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
                }) {
                    let target = root.join("transfer").join(name);
                    transferred.insert(name.to_owned(), target.clone());
                    target
                } else {
                    return Err(storage_error("TES returned an undeclared capture object"));
                }
            } else {
                let path = relative
                    .strip_prefix("root/")
                    .ok_or_else(|| storage_error("TES output has no container path"))?;
                let path = format!("/{path}");
                if !task
                    .outputs
                    .iter()
                    .any(|output| output.path == path || matches(&output.path, &path))
                {
                    return Err(storage_error("TES returned an undeclared output"));
                }
                let path = container_path(&path, "/")?;
                let work = task.executors[0].workdir.as_deref().unwrap();
                let path = path
                    .strip_prefix(&format!("{work}/"))
                    .unwrap_or(&path)
                    .to_owned();
                finished.files.push(manifest::Collected {
                    path: path.clone(),
                    directory: false,
                });
                finished.local_path(&path)
            };
            let (digest, _) = self
                .objects
                .read_object(&location, version.as_deref(), &target, size)
                .await
                .map_err(storage_error)?;
            self.source
                .put_file_as(&target, &digest)
                .await
                .map_err(storage_error)?;
        }
        if shell {
            finished = Finished::from_control(root.into())?;
            if finished.exit_code.is_none() {
                return Err(storage_error("TES did not return the captured exit code"));
            }
            if finished.exit_code == Some(0) {
                let text = tokio::fs::read_to_string(control.join(INDEX)).await?;
                for line in text.lines() {
                    let (number, path) = line
                        .split_once(' ')
                        .ok_or_else(|| storage_error("TES capture index is invalid"))?;
                    let parsed = manifest::parse(&format!("file {path}"))?;
                    let path = &parsed[0].path;
                    let target = finished.local_path(path.trim_start_matches("./"));
                    let source = transferred
                        .remove(number)
                        .ok_or_else(|| storage_error("TES capture object is missing"))?;
                    tokio::fs::create_dir_all(target.parent().unwrap()).await?;
                    tokio::fs::rename(source, target).await?;
                }
                if !transferred.is_empty() {
                    return Err(storage_error(
                        "TES returned capture objects without layout records",
                    ));
                }
                for file in &finished.files {
                    if file.directory {
                        tokio::fs::create_dir_all(finished.local_path(&file.path)).await?;
                    } else if !finished.local_path(&file.path).is_file() {
                        return Err(storage_error("TES declared output is missing"));
                    }
                }
            }
        }
        Ok(finished)
    }

    fn file_location(
        &self,
        task: &Task,
        file: &FileLog,
    ) -> Result<(String, Option<String>), ExecutorError> {
        let address = file.url.split('?').next().unwrap();
        let output = task
            .outputs
            .iter()
            .find(|output| output.path == file.path || matches(&output.path, &file.path))
            .ok_or_else(|| storage_error("TES logged an undeclared output path"))?;
        let expected = match &output.path_prefix {
            Some(prefix) => format!(
                "{}{}",
                output.url,
                file.path
                    .strip_prefix(prefix)
                    .ok_or_else(|| storage_error("TES output path differs from path_prefix"))?
            ),
            None => output.url.clone(),
        };
        if address != expected {
            return Err(storage_error(
                "TES output URL differs from its declared destination",
            ));
        }
        let base = format!("{}/", self.objects.url("").trim_end_matches('/'));
        let location = address
            .strip_prefix(&base)
            .ok_or_else(|| storage_error("TES output is outside the staging store"))?
            .to_owned();
        if location
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(storage_error("TES output object path is not canonical"));
        }
        let url = reqwest::Url::parse(&file.url).map_err(storage_error)?;
        let mut version = None;
        for (key, value) in url.query_pairs() {
            if key != "versionId" || version.is_some() {
                return Err(unsupported(
                    "TES output URLs support only one versionId query parameter",
                ));
            }
            version = Some(value.into_owned());
        }
        Ok((location, version))
    }
}

//! Prepares an attempt for a node that shares no folders with this machine: uploads its inputs
//! to the staging root, writes the plan of the helper, and collects what the attempt sent back.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use content_store::{Store, hash_file, layout};
use task_model::Digest;

use crate::helper::{Input, Plan, Record, Request};
use crate::job::{CONTROL, ExecutorError, Finished, HOME, Job, MOUNTS, WORK};
use crate::process::Mount;

/// The staging root of a backend whose nodes share no folders with this machine.
#[derive(Clone, Debug)]
pub struct RemoteStage {
    /// The staging root, like `s3://bucket/prefix`, which this machine and the nodes reach.
    pub objects: Store,
    /// How long the presigned requests of a plan work. It must cover the time that an attempt
    /// waits in the queue and the time that it runs.
    pub expiry: Duration,
    /// How many output files an attempt may send back.
    pub max_outputs: usize,
}

/// The inputs of an attempt in the staging root, by their paths in the folder of the attempt.
#[derive(Debug, Default)]
pub struct Layout {
    pub files: Vec<Placed>,
    pub directories: Vec<String>,
    /// What the container sees besides the folder of the attempt: the inputs at absolute paths.
    pub mounts: Vec<Mount>,
}

/// One input file, whose content is at `cas/...` in the staging root.
#[derive(Debug)]
pub struct Placed {
    pub path: String,
    pub digest: Digest,
    pub size: u64,
    pub executable: bool,
}

/// A plan in the staging root, and the mounts that the container needs.
#[derive(Debug)]
pub struct Planned {
    /// Where the attempt sends its outputs back, below the staging root.
    pub prefix: String,
    /// Where the node reads the plan: a presigned URL or a `file://` URL.
    pub location: String,
    pub mounts: Vec<Mount>,
}

impl RemoteStage {
    /// Uploads the inputs of `job` and the `extra` files, like the wrapper, as the folder `root`
    /// of the attempt shows them, and writes the plan that sends back `records`. An `open` plan
    /// lets every user change what stage-in makes.
    pub async fn plan(
        &self,
        job: Job<'_>,
        root: &Path,
        extra: &[(&str, String)],
        records: &[&str],
        open: bool,
    ) -> Result<Planned, ExecutorError> {
        let prefix = crate::capture::attempt_prefix(job)?;
        let layout = self.upload(job, root, extra).await?;
        let mut plan = Plan {
            directories: layout.directories,
            open,
            ..Plan::default()
        };
        for file in layout.files {
            plan.inputs.push(Input {
                request: self.request(&layout::content(&file.digest), false).await?,
                path: file.path,
                sha256: file.digest.hex().to_owned(),
                size: file.size,
                executable: file.executable,
            });
        }
        for number in 1..=self.max_outputs {
            let location = format!("{prefix}/files/{number}");
            plan.files.push(self.request(&location, true).await?);
        }
        for name in records {
            let location = format!("{prefix}/control/{name}");
            plan.records.push(Record {
                name: (*name).to_owned(),
                request: self.request(&location, true).await?,
            });
        }
        let text = serde_json::to_vec(&plan).map_err(failed)?;
        let digest = self
            .objects
            .put_content(text.into())
            .await
            .map_err(failed)?;
        let location = self.request(&layout::content(&digest), false).await?.url;
        Ok(Planned {
            prefix,
            location,
            mounts: layout.mounts,
        })
    }

    /// Uploads the input files of `job` and the `extra` files to `cas/...` in the staging root,
    /// where they are not yet, and lays them out as `place` does with copies.
    pub async fn upload(
        &self,
        job: Job<'_>,
        root: &Path,
        extra: &[(&str, String)],
    ) -> Result<Layout, ExecutorError> {
        let staging = job.staging;
        let mut layout = Layout {
            directories: vec![WORK.into(), CONTROL.into(), HOME.into()],
            ..Layout::default()
        };
        let mut absolute = BTreeSet::new();
        let mut pending = Vec::new();
        for input in &staging.inputs {
            if input.in_place {
                return Err(ExecutorError::Unsupported(
                    "object-store staging cannot change an input in place".into(),
                ));
            }
            let folder = folder_path(&input.path)?;
            if input.path.starts_with('/') {
                absolute.insert(input.path.trim_end_matches('/').to_owned());
            }
            pending.push((input.source.clone(), folder, input.executable));
        }
        for directory in &staging.directories {
            layout.directories.push(folder_path(directory)?);
            if directory.starts_with('/') {
                absolute.insert(directory.trim_end_matches('/').to_owned());
            }
        }
        if let Some(stdin) = &staging.stdin {
            pending.push((stdin.clone(), format!("{CONTROL}/stdin"), false));
        }
        while let Some((source, path, executable)) = pending.pop() {
            let metadata = std::fs::metadata(&source).map_err(|_| {
                ExecutorError::Unsupported(format!(
                    "object-store staging cannot place {}, which is a link to nothing",
                    source.display()
                ))
            })?;
            if metadata.is_dir() {
                if std::fs::symlink_metadata(&source)?.is_symlink() {
                    return Err(ExecutorError::Unsupported(
                        "object-store staging cannot place a link to a directory".into(),
                    ));
                }
                let mut empty = true;
                for entry in std::fs::read_dir(&source)? {
                    let entry = entry?;
                    let name = entry.file_name().into_string().map_err(|_| {
                        ExecutorError::Unsupported("object-store paths must be UTF-8".into())
                    })?;
                    let mode = std::fs::metadata(entry.path()).map(|data| data.permissions());
                    let runs = mode.is_ok_and(|mode| mode.mode() & 0o111 != 0);
                    pending.push((entry.path(), format!("{path}/{name}"), runs));
                    empty = false;
                }
                if empty {
                    layout.directories.push(path);
                }
                continue;
            }
            let (digest, size) = hash_file(&source).await.map_err(failed)?;
            if !self.objects.has_content(&digest).await.map_err(failed)? {
                self.objects
                    .put_file_as(&source, &digest)
                    .await
                    .map_err(failed)?;
            }
            layout.files.push(Placed {
                path,
                digest,
                size,
                executable,
            });
        }
        for (path, text) in extra {
            let size = text.len() as u64;
            let digest = (self
                .objects
                .put_content(text.clone().into_bytes().into())
                .await)
                .map_err(failed)?;
            layout.files.push(Placed {
                path: (*path).to_owned(),
                digest,
                size,
                executable: false,
            });
        }
        let mut outer: Vec<String> = Vec::new();
        // In sorted order a folder comes right before the folders inside it.
        for path in absolute {
            if !outer
                .iter()
                .any(|folder| Path::new(&path).starts_with(folder))
            {
                outer.push(path);
            }
        }
        layout.mounts = (outer.into_iter())
            .map(|path| Mount {
                source: root.join(MOUNTS).join(path.trim_start_matches('/')),
                target: path,
                read_only: false,
            })
            .collect();
        Ok(layout)
    }

    /// Downloads what an attempt sent back to `prefix` into its folder `root`: the `records` in
    /// the control folder, and after a success its outputs where the wrapper found them.
    pub async fn collect(
        &self,
        prefix: &str,
        root: &Path,
        records: &[&str],
    ) -> Result<(), ExecutorError> {
        tokio::fs::create_dir_all(root.join(CONTROL)).await?;
        let objects = self.objects.objects(&format!("{prefix}/")).await;
        for object in objects.map_err(failed)? {
            let relative = object.location.strip_prefix(&format!("{prefix}/"));
            let Some(target) =
                relative.and_then(|relative| crate::capture::target(root, relative, records))
            else {
                continue;
            };
            (self
                .objects
                .read_object(&object.location, None, &target, None))
            .await
            .map_err(failed)?;
        }
        let finished = Finished::from_control(root.to_owned())?;
        if finished.exit_code == Some(0) {
            crate::capture::arrange(&finished).await?;
        }
        Ok(())
    }

    /// Downloads one record of an attempt into the control folder of `root`, if it is there.
    pub async fn fetch(
        &self,
        prefix: &str,
        name: &str,
        root: &Path,
    ) -> Result<bool, ExecutorError> {
        let location = format!("{prefix}/control/{name}");
        let target = root.join(CONTROL).join(name);
        match self
            .objects
            .read_object(&location, None, &target, None)
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.missing() => Ok(false),
            Err(error) => Err(failed(error)),
        }
    }

    /// A request that reads or writes `location`: presigned, or a `file://` URL for a staging
    /// root in a folder.
    async fn request(&self, location: &str, write: bool) -> Result<Request, ExecutorError> {
        if let Some(path) = self.objects.local_path(location) {
            let url = reqwest::Url::from_file_path(&path)
                .map_err(|()| failed(format!("{} is not an absolute path", path.display())))?;
            return Ok(Request {
                method: if write { "PUT" } else { "GET" }.into(),
                url: url.into(),
                headers: Vec::new(),
            });
        }
        let signed = match write {
            true => self.objects.presign_write(location, self.expiry).await,
            false => self.objects.presign_read(location, self.expiry).await,
        };
        let signed = signed.map_err(failed)?;
        Ok(Request {
            method: signed.method,
            url: signed.url,
            headers: signed.headers,
        })
    }
}

/// The path of an input in the folder of an attempt: in the work directory, or below `mounts`
/// for an absolute path.
fn folder_path(path: &str) -> Result<String, ExecutorError> {
    let parts: Vec<&str> = (path.split('/'))
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") || path.contains('\0') {
        return Err(ExecutorError::Unsupported(format!(
            "the input path {path:?} leaves its folder"
        )));
    }
    let base = if path.starts_with('/') { MOUNTS } else { WORK };
    Ok(format!("{base}/{}", parts.join("/")))
}

fn failed(error: impl std::fmt::Display) -> ExecutorError {
    std::io::Error::other(format!("object staging failed: {error}")).into()
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;

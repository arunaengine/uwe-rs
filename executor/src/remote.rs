//! Prepares an attempt for a node that shares no folders with this machine: uploads its inputs
//! to the staging root, writes the plan of the helper, and collects what the attempt sent back.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use content_store::{Store, hash_file, layout};
use task_model::Digest;

use crate::binary::Binary;
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
    /// Whether the nodes reach the staging root with credentials of their own, which the helper
    /// gets from [`crate::helper::STORAGE`], instead of with presigned requests.
    pub credentials: bool,
    /// Whether [`RemoteStage::clean`] removes the objects of an attempt after its collection.
    pub cleanup: bool,
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
    /// Where the plan is in the staging root, in `cas/`.
    pub plan: String,
    pub mounts: Vec<Mount>,
}

impl RemoteStage {
    /// Uploads the inputs of `job`, the `extra` texts, like the wrapper, and the local `files`
    /// as the folder `root` of the attempt shows them, and writes the plan that sends back
    /// `records`. An `open` plan lets every user change what stage-in makes.
    pub async fn plan(
        &self,
        job: Job<'_>,
        root: &Path,
        extra: &[(&str, String)],
        files: &[(&str, &Path)],
        records: &[&str],
        open: bool,
    ) -> Result<Planned, ExecutorError> {
        let prefix = crate::capture::attempt_prefix(job)?;
        let layout = self.upload(job, root, extra, files).await?;
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
        let plan = layout::content(&digest);
        let location = self.request(&plan, false).await?.url;
        Ok(Planned {
            prefix,
            location,
            plan,
            mounts: layout.mounts,
        })
    }

    /// Uploads the input files of `job`, the `extra` texts and the local `files` to `cas/...`
    /// in the staging root, where they are not yet, and lays them out as `place` does with
    /// copies. The paths of `extra` and `files` are in the folder of the attempt.
    pub async fn upload(
        &self,
        job: Job<'_>,
        root: &Path,
        extra: &[(&str, String)],
        files: &[(&str, &Path)],
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
        for (path, file) in files {
            let (digest, size) = remembered(file).await?;
            if !self.objects.has_content(&digest).await.map_err(failed)? {
                (self.objects.put_file_as(file, &digest))
                    .await
                    .map_err(failed)?;
            }
            layout.files.push(Placed {
                path: (*path).to_owned(),
                digest,
                size,
                executable: false,
            });
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

    /// Removes what an attempt sent back to `prefix` and its plan at `plan`, when the stage
    /// cleans up. The inputs in `cas/` stay, as other attempts may use them. A failure only
    /// leaves objects behind, so it is a warning.
    pub async fn clean(&self, prefix: &str, plan: &str) {
        if !self.cleanup {
            return;
        }
        let objects = self.objects.objects(&format!("{prefix}/")).await;
        let mut locations: Vec<String> = match objects {
            Ok(objects) => objects.into_iter().map(|object| object.location).collect(),
            Err(error) => return tracing::warn!(prefix, %error, "cannot list an attempt"),
        };
        locations.push(plan.to_owned());
        for location in locations {
            if let Err(error) = self.objects.remove(&location).await {
                tracing::warn!(location, %error, "cannot remove an object of an attempt");
            }
        }
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

    /// Where a job downloads `binary` with `curl` or `wget`: a presigned URL, or a `file://` URL
    /// for a staging root in a folder. The first call uploads the binary, where it is not yet.
    pub async fn binary(&self, binary: &Binary) -> Result<String, ExecutorError> {
        if !binary.uploaded.load(Ordering::Relaxed) {
            if !self
                .objects
                .has_content(&binary.digest)
                .await
                .map_err(failed)?
            {
                (self.objects.put_file_as(&binary.file, &binary.digest))
                    .await
                    .map_err(failed)?;
            }
            binary.uploaded.store(true, Ordering::Relaxed);
        }
        // The job has no helper yet to read an object address with credentials.
        let stage = RemoteStage {
            credentials: false,
            ..self.clone()
        };
        Ok(stage
            .request(&layout::content(&binary.digest), false)
            .await?
            .url)
    }

    /// A presigned upload, or a `file://` URL, of the record `name` of the attempt at `prefix`,
    /// which a job sends without the helper, like its log when the helper cannot run.
    pub async fn record(&self, prefix: &str, name: &str) -> Result<Request, ExecutorError> {
        let stage = RemoteStage {
            credentials: false,
            ..self.clone()
        };
        stage
            .request(&format!("{prefix}/control/{name}"), true)
            .await
    }

    /// A request that reads or writes `location`: presigned, the address of the object for
    /// nodes with credentials, or a `file://` URL for a staging root in a folder.
    async fn request(&self, location: &str, write: bool) -> Result<Request, ExecutorError> {
        let method = if write { "PUT" } else { "GET" };
        if let Some(path) = self.objects.local_path(location) {
            let url = reqwest::Url::from_file_path(&path)
                .map_err(|()| failed(format!("{} is not an absolute path", path.display())))?;
            return Ok(Request {
                method: method.into(),
                url: url.into(),
                headers: Vec::new(),
            });
        }
        if self.credentials {
            return Ok(Request {
                method: method.into(),
                url: self.objects.url(location),
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

/// The digest and size of a local file that attempts stage again and again, like an image
/// archive: hashed once while its size and time of change stay the same.
async fn remembered(file: &Path) -> Result<(Digest, u64), ExecutorError> {
    type Known = std::collections::BTreeMap<std::path::PathBuf, (u64, SystemTime, Digest)>;
    static KNOWN: std::sync::Mutex<Known> = std::sync::Mutex::new(Known::new());
    let known = || KNOWN.lock().unwrap_or_else(|poison| poison.into_inner());
    let metadata = tokio::fs::metadata(file).await?;
    let (size, modified) = (metadata.len(), metadata.modified()?);
    let same = |(was, then, _): &&(u64, SystemTime, Digest)| *was == size && *then == modified;
    if let Some((_, _, digest)) = known().get(file).filter(same) {
        return Ok((digest.clone(), size));
    }
    let (digest, hashed) = hash_file(file).await.map_err(failed)?;
    known().insert(file.to_owned(), (hashed, modified, digest.clone()));
    Ok((digest, hashed))
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

/// How often the results of an attempt are collected before it fails.
const COLLECT_TRIES: u32 = 4;

/// Runs `collect` again after a growing pause while it fails, as a staging root fails for a
/// moment now and then and the results of a long task have no other copy.
pub async fn patiently<T, F, R>(mut collect: F) -> Result<T, ExecutorError>
where
    F: FnMut() -> R,
    R: std::future::Future<Output = Result<T, ExecutorError>>,
{
    let mut pause = Duration::from_secs(1);
    for _ in 1..COLLECT_TRIES {
        match collect().await {
            Err(error) if !matches!(error, ExecutorError::Cancelled) => {
                tracing::debug!(%error, "collecting the results again");
                tokio::time::sleep(pause).await;
                pause *= 2;
            }
            result => return result,
        }
    }
    collect().await
}

fn failed(error: impl std::fmt::Display) -> ExecutorError {
    std::io::Error::other(format!("object staging failed: {error}")).into()
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;

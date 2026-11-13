//! Reads wrapper manifests and discovers confined outputs selected by a task's JSON manifest.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a task's JSON manifest names additional outputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub manifest: String,
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<String>,
    pub read_only: Vec<String>,
    pub aliases: Vec<String>,
}

impl Discovery {
    pub fn of(spec: &task_model::TaskSpec, staging: &crate::Staging) -> Option<Self> {
        Some(Self {
            manifest: spec.output_manifest.clone()?,
            workdir: spec.workdir.clone(),
            work: None,
            read_only: staging
                .inputs
                .iter()
                .filter(|input| !input.copy && !input.in_place)
                .flat_map(|input| {
                    [
                        input.path.clone(),
                        input.source.to_string_lossy().into_owned(),
                    ]
                })
                .collect(),
            aliases: staging
                .inputs
                .iter()
                .filter(|input| input.copy || input.in_place)
                .map(|input| input.path.clone())
                .chain(staging.directories.iter().cloned())
                .collect(),
        })
    }

    pub(crate) fn collect(&self, root: &Path, files: &mut Vec<Collected>) -> Result<(), String> {
        self.collect_with(root, files, &[])
    }

    pub(crate) fn collect_staged(
        &self,
        root: &Path,
        files: &mut Vec<Collected>,
        staging: &crate::Staging,
    ) -> Result<(), String> {
        let mut places = Vec::new();
        for input in staging.inputs.iter().filter(|input| input.in_place) {
            check_path(Path::new("/"), &input.source)?;
            places.push((self.resolve(root, &input.path)?.0, input.source.clone()));
        }
        self.collect_with(root, files, &places)
    }

    fn collect_with(
        &self,
        root: &Path,
        files: &mut Vec<Collected>,
        places: &[(String, PathBuf)],
    ) -> Result<(), String> {
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let root = root.as_path();
        self.check(root)?;
        let (_, manifest) = self.resolve(root, &self.manifest)?;
        let metadata = match std::fs::metadata(&manifest) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.is_file() || metadata.len() > 16 << 20 {
            return Err("the output JSON manifest must be a regular file of at most 16 MiB".into());
        }
        let work = self
            .work
            .as_ref()
            .map_or_else(|| root.join(crate::WORK), PathBuf::from);
        if !manifest
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(work.canonicalize().map_err(|error| error.to_string())?)
        {
            return Err("the output JSON manifest leaves the task folder".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&manifest)
            .map_err(|error| error.to_string())?
            .take((16 << 20) + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 16 << 20 {
            return Err("the output JSON manifest exceeds 16 MiB".into());
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid output JSON manifest: {error}"))?;
        let mut paths = BTreeSet::new();
        references(&value, 0, &mut paths)?;
        let mut count = 0;
        for path in paths {
            let (path, mut source) = self.resolve(root, &path)?;
            if self.read_only.iter().any(|input| {
                self.resolve(root, input)
                    .is_ok_and(|(input, _)| Path::new(&path).starts_with(input))
            }) {
                continue;
            }
            let captured = root.join(crate::CONTROL).join(crate::wrapper::ROOT);
            let place = places
                .iter()
                .filter(|(alias, _)| Path::new(&path).starts_with(alias))
                .max_by_key(|(alias, _)| alias.len())
                .filter(|_| !source.starts_with(&captured));
            if let Some((alias, base)) = place {
                source = base.join(
                    Path::new(&path)
                        .strip_prefix(alias)
                        .map_err(|error| error.to_string())?,
                );
            }
            if files.iter().any(|file| {
                file.path == path
                    || self
                        .resolve(root, &file.path)
                        .is_ok_and(|(other, _)| other == path)
            }) {
                continue;
            }
            let metadata = std::fs::metadata(&source)
                .map_err(|error| format!("cannot collect JSON output {path:?}: {error}"))?;
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(format!(
                    "JSON output {path:?} is not a regular file or directory"
                ));
            }
            let base = if let Some((_, base)) = place {
                base.clone()
            } else if source.starts_with(&captured) {
                captured.clone()
            } else if path.starts_with('/') && self.work.is_some() {
                PathBuf::from(
                    self.aliases
                        .iter()
                        .filter(|alias| Path::new(&path).starts_with(alias))
                        .max_by_key(|alias| alias.len())
                        .ok_or("an output has no staged alias")?,
                )
            } else if path.starts_with('/') {
                root.join(crate::job::MOUNTS)
            } else {
                self.work
                    .as_ref()
                    .map_or_else(|| root.join(crate::WORK), PathBuf::from)
            };
            check_path(
                if base.starts_with(root) {
                    root
                } else {
                    Path::new("/")
                },
                &base,
            )?;
            confine(&source, &base, 0, &mut count)?;
            if path.starts_with('/') || place.is_some() {
                let target = if path.starts_with('/') {
                    captured.join(path.trim_start_matches('/'))
                } else {
                    root.join(crate::WORK).join(&path)
                };
                if source != target {
                    copy_output(&source, &target, root, 0)?;
                }
            }
            files.push(Collected {
                path,
                directory: metadata.is_dir(),
            });
        }
        Ok(())
    }

    pub(crate) fn check(&self, root: &Path) -> Result<(), String> {
        for path in [
            crate::WORK,
            crate::job::MOUNTS,
            crate::CONTROL,
            "control/root",
            "control/manifest",
            "control/objects",
            "control/exit",
            "transfer",
        ] {
            check_path(root, &root.join(path))?;
        }
        if let Some(work) = &self.work {
            check_path(Path::new("/"), Path::new(work))?;
        }
        Ok(())
    }

    fn resolve(&self, root: &Path, path: &str) -> Result<(String, PathBuf), String> {
        let decoded;
        let path = if path.starts_with("file:") {
            decoded = reqwest::Url::parse(path)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .ok_or_else(|| format!("invalid local output URL {path:?}"))?;
            decoded.to_str().ok_or("an output path is not UTF-8")?
        } else {
            path
        };
        if path.is_empty()
            || path.contains(['\0', '\n', '\r'])
            || path.split('/').any(|part| part == "..")
            || path.contains("://")
        {
            return Err(format!("JSON output path {path:?} leaves the task folder"));
        }
        let work = self
            .work
            .as_ref()
            .map_or_else(|| root.join(crate::WORK), PathBuf::from);
        let relative = Path::new(path).strip_prefix(&work).ok().or_else(|| {
            self.workdir
                .as_ref()
                .and_then(|workdir| Path::new(path).strip_prefix(workdir).ok())
        });
        let path: PathBuf = relative
            .unwrap_or_else(|| Path::new(path))
            .components()
            .filter(|part| *part != std::path::Component::CurDir)
            .collect();
        if !path.is_absolute() {
            let path = if path.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                path
            };
            return Ok((path.to_string_lossy().into_owned(), work.join(path)));
        }
        let captured = root
            .join(crate::CONTROL)
            .join(crate::wrapper::ROOT)
            .join(path.strip_prefix("/").unwrap());
        if captured.exists() {
            return Ok((path.to_string_lossy().into_owned(), captured));
        }
        if self
            .aliases
            .iter()
            .chain(&self.read_only)
            .any(|alias| Path::new(alias).is_absolute() && path.starts_with(alias))
        {
            let source = if self.work.is_some() {
                path.clone()
            } else {
                root.join(crate::job::MOUNTS)
                    .join(path.strip_prefix("/").unwrap())
            };
            return Ok((path.to_string_lossy().into_owned(), source));
        }
        Err(format!("JSON output path {path:?} leaves the task folder"))
    }
}

fn references(
    value: &serde_json::Value,
    depth: usize,
    paths: &mut BTreeSet<String>,
) -> Result<(), String> {
    if depth > 64 {
        return Err("the output JSON manifest exceeds depth 64".into());
    }
    match value {
        serde_json::Value::Object(fields) => {
            if matches!(
                fields.get("class").and_then(|value| value.as_str()),
                Some("File" | "Directory")
            ) && let Some(path) = fields
                .get("path")
                .and_then(|value| value.as_str())
                .or_else(|| fields.get("location").and_then(|value| value.as_str()))
                .filter(|path| !path.starts_with("_:"))
            {
                paths.insert(path.to_owned());
                if paths.len() > 100_000 {
                    return Err("the output JSON manifest exceeds 100000 references".into());
                }
            }
            for value in fields.values() {
                references(value, depth + 1, paths)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                references(value, depth + 1, paths)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn confine(path: &Path, base: &Path, depth: usize, count: &mut usize) -> Result<(), String> {
    *count += 1;
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(format!(
            "JSON output {} is not a regular file or directory",
            path.display()
        ));
    }
    if depth > 64
        || *count > 100_000
        || !path
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(base.canonicalize().map_err(|error| error.to_string())?)
    {
        return Err(format!(
            "JSON output {} leaves the task folder or exceeds depth 64",
            path.display()
        ));
    }
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
            confine(
                &entry.map_err(|error| error.to_string())?.path(),
                base,
                depth + 1,
                count,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn check_path(root: &Path, path: &Path) -> Result<(), String> {
    let mut current = root.to_owned();
    for part in path
        .strip_prefix(root)
        .map_err(|_| "an output base leaves the attempt")?
        .components()
    {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err("an output base is not a plain path".into());
        }
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(data) if data.is_symlink() => {
                return Err(format!(
                    "output path {} is a symbolic link",
                    current.display()
                ));
            }
            Ok(_) => {
                if !current
                    .canonicalize()
                    .map_err(|error| error.to_string())?
                    .starts_with(root)
                {
                    return Err("an output base leaves the attempt".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn copy_output(source: &Path, target: &Path, root: &Path, depth: usize) -> Result<(), String> {
    check_path(root, target)?;
    if depth > 64 {
        return Err("an output directory exceeds depth 64".into());
    }
    if source.is_dir() {
        std::fs::create_dir_all(target).map_err(|error| error.to_string())?;
        for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            copy_output(
                &entry.path(),
                &target.join(entry.file_name()),
                root,
                depth + 1,
            )?;
        }
    } else {
        std::fs::create_dir_all(target.parent().ok_or("an output has no parent")?)
            .map_err(|error| error.to_string())?;
        std::fs::copy(source, target).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// A collected output: its path as the task named it (relative to the work directory, or
/// absolute), and whether it is a directory. The runner learns the size of a file when it
/// stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collected {
    pub path: String,
    pub directory: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("line {line} of the result manifest is not `file <path>` or `directory <path>`: {text}")]
pub struct ManifestError {
    pub line: usize,
    pub text: String,
}

/// Reads the manifest. A path that several patterns matched is listed once, in the order of
/// its first match.
pub fn parse(text: &str) -> Result<Vec<Collected>, ManifestError> {
    let mut files: Vec<Collected> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let parsed = match line.split_once(' ') {
            Some(("file", path)) => Some((false, path)),
            Some(("directory", path)) => Some((true, path)),
            _ => None,
        };
        let Some((directory, path)) = parsed.filter(|(_, path)| {
            !path.is_empty() && !path.contains('\0') && !path.split('/').any(|part| part == "..")
        }) else {
            return Err(ManifestError {
                line: index + 1,
                text: line.to_owned(),
            });
        };
        if !files.iter().any(|file| file.path == path) {
            files.push(Collected {
                path: path.to_owned(),
                directory,
            });
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    fn discovery() -> super::Discovery {
        super::Discovery {
            manifest: "cwl.output.json".into(),
            workdir: Some("/work".into()),
            work: None,
            read_only: vec!["input".into(), "/inputs/original".into()],
            aliases: vec!["/scratch/result".into()],
        }
    }

    #[test]
    fn discovers_nested_outputs() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        std::fs::create_dir_all(root.join("work/result")).unwrap();
        std::fs::write(root.join("work/.hidden"), "hidden").unwrap();
        std::fs::write(root.join("work/result/.child"), "child").unwrap();
        std::fs::write(root.join("work/unrelated"), "not selected").unwrap();
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{
            "nested": [{"class":"File","location":"file:///work/.hidden"},
                {"class":"File","path":".hidden"}],
            "directory":{"class":"Directory","path":"result"},
            "input":{"class":"File","path":"/inputs/original"},
            "relative":{"class":"File","path":"input"}
        }"#,
        )
        .unwrap();
        let mut files = super::parse("file cwl.output.json\n").unwrap();
        discovery().collect(root, &mut files).unwrap();
        assert_eq!(
            files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["cwl.output.json", ".hidden", "result"]
        );
        assert!(files[2].directory);
        assert!(root.join("work/result/.child").is_file());
    }

    #[test]
    fn confines_json_outputs() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        std::fs::create_dir_all(root.join("work")).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("work/link")).unwrap();
        for path in [
            "../secret",
            "/etc/passwd",
            "link",
            "https://example.com/file",
        ] {
            let json = serde_json::json!({"file":{"class":"File","path":path}});
            std::fs::write(root.join("work/cwl.output.json"), json.to_string()).unwrap();
            assert!(
                discovery().collect(root, &mut Vec::new()).is_err(),
                "{path}"
            );
        }
        std::fs::create_dir_all(root.join("mounts/scratch")).unwrap();
        std::fs::write(root.join("mounts/scratch/result"), "absolute").unwrap();
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{"file":{"class":"File","path":"/scratch/result"}}"#,
        )
        .unwrap();
        let mut files = Vec::new();
        discovery().collect(root, &mut files).unwrap();
        assert_eq!(files[0].path, "/scratch/result");
        assert_eq!(
            std::fs::read(root.join("control/root/scratch/result")).unwrap(),
            b"absolute"
        );
    }

    #[test]
    fn rejects_linked_bases() {
        let outside = tempfile::tempdir().unwrap();
        let json = r#"{"file":{"class":"File","path":"/passwd"}}"#;
        std::fs::write(outside.path().join("passwd"), "outside").unwrap();
        std::fs::write(outside.path().join("cwl.output.json"), json).unwrap();
        for base in ["work", "mounts", "control", "control/root"] {
            let folder = tempfile::tempdir().unwrap();
            let root = folder.path();
            std::fs::create_dir_all(root.join("work")).unwrap();
            std::fs::create_dir_all(root.join("control")).unwrap();
            std::fs::write(root.join("work/cwl.output.json"), json).unwrap();
            let target = root.join(base);
            if target.is_dir() {
                std::fs::remove_dir_all(&target).unwrap();
            }
            std::os::unix::fs::symlink(outside.path(), target).unwrap();
            let error = discovery().collect(root, &mut Vec::new()).unwrap_err();
            assert!(error.contains("symbolic link"), "{base}: {error}");
        }
    }

    #[test]
    fn rejects_linked_targets() {
        let outside = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        std::fs::create_dir_all(root.join("work")).unwrap();
        std::fs::create_dir_all(root.join("mounts/scratch")).unwrap();
        std::fs::create_dir_all(root.join("control/root/scratch")).unwrap();
        std::fs::write(root.join("mounts/scratch/result"), "safe").unwrap();
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{"file":{"class":"File","path":"/scratch/result"}}"#,
        )
        .unwrap();
        let target = outside.path().join("new");
        std::os::unix::fs::symlink(&target, root.join("control/root/scratch/result")).unwrap();
        assert!(
            discovery()
                .collect(root, &mut Vec::new())
                .unwrap_err()
                .contains("symbolic link")
        );
        assert!(!target.exists());
        std::fs::remove_file(root.join("control/root/scratch/result")).unwrap();
        std::fs::write(root.join("control/root/passwd"), "captured").unwrap();
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{"file":{"class":"File","path":"/passwd"}}"#,
        )
        .unwrap();
        let mut files = Vec::new();
        discovery().collect(root, &mut files).unwrap();
        assert_eq!(files[0].path, "/passwd");
    }

    #[test]
    fn collects_inplace_aliases() {
        let folder = tempfile::tempdir().unwrap();
        let approved = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let root = folder.path();
        std::fs::create_dir_all(root.join("work")).unwrap();
        std::fs::write(approved.path().join("new"), "output").unwrap();
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{"file":{"class":"File","path":"/data/new"}}"#,
        )
        .unwrap();
        let mut staging = crate::Staging {
            inputs: vec![crate::Staged {
                path: "/data".into(),
                source: approved.path().to_owned(),
                in_place: true,
                directory: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut discovery = discovery();
        discovery.aliases = vec!["/data".into()];
        let metadata = serde_json::to_string(&discovery).unwrap();
        assert!(!metadata.contains(approved.path().to_str().unwrap()));
        let mut files = Vec::new();
        discovery
            .collect_staged(root, &mut files, &staging)
            .unwrap();
        assert_eq!(files[0].path, "/data/new");
        assert_eq!(
            std::fs::read(root.join("control/root/data/new")).unwrap(),
            b"output"
        );
        std::os::unix::fs::symlink(outside.path(), approved.path().join("escape")).unwrap();
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{"file":{"class":"File","path":"/data/escape"}}"#,
        )
        .unwrap();
        assert!(
            discovery
                .collect_staged(root, &mut Vec::new(), &staging)
                .is_err()
        );
        assert!(!root.join("control/root/data/escape").exists());
        staging.inputs[0].path = "data".into();
        discovery.aliases = vec!["data".into()];
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{"file":{"class":"File","path":"data/new"}}"#,
        )
        .unwrap();
        let mut files = Vec::new();
        discovery
            .collect_staged(root, &mut files, &staging)
            .unwrap();
        assert_eq!(files[0].path, "data/new");
        assert_eq!(
            std::fs::read(root.join("work/data/new")).unwrap(),
            b"output"
        );
    }

    #[test]
    fn bounds_json_manifest() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        std::fs::create_dir_all(root.join("work")).unwrap();
        let deep = format!("{}0{}", "[".repeat(66), "]".repeat(66));
        for json in ["invalid".to_owned(), deep, " ".repeat((16 << 20) + 1)] {
            std::fs::write(root.join("work/cwl.output.json"), json).unwrap();
            assert!(discovery().collect(root, &mut Vec::new()).is_err());
        }
        let mut paths = std::collections::BTreeSet::new();
        for index in 0..100_000 {
            paths.insert(index.to_string());
        }
        let value = serde_json::json!({"class":"File","path":"overflow"});
        assert!(super::references(&value, 0, &mut paths).is_err());
    }

    #[test]
    fn skips_literal_locations() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        std::fs::create_dir_all(root.join("work")).unwrap();
        std::fs::write(root.join("work/.hidden"), "secondary").unwrap();
        std::fs::write(
            root.join("work/cwl.output.json"),
            r#"{
            "directory":{"class":"Directory","location":"_:literal","listing":[
                {"class":"File","location":"_:contents","contents":"inline",
                 "secondaryFiles":[{"class":"File","location":".hidden"}]}]}
        }"#,
        )
        .unwrap();
        let mut files = Vec::new();
        discovery().collect(root, &mut files).unwrap();
        assert_eq!(
            files,
            vec![super::Collected {
                path: ".hidden".into(),
                directory: false
            }]
        );
    }

    #[test]
    fn rejects_escaping_outputs() {
        for path in ["../secret", "a/../../secret", "/a/../secret", "bad\0path"] {
            assert!(super::parse(&format!("file {path}\n")).is_err());
        }
        let root = std::path::PathBuf::from("/scratch/task");
        let finished = crate::Finished {
            root: root.clone(),
            exit_code: Some(0),
            files: Vec::new(),
            usage: None,
            address: None,
        };
        let files = super::parse("file //tmp/result\ndirectory .\n").unwrap();
        assert_eq!(
            finished.local_path(&files[0].path),
            root.join("control/root/tmp/result")
        );
    }
}

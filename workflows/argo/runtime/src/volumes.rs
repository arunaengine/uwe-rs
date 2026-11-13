//! Shares files between task volume mounts and writes host paths back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use argo_parser::model::Template;
use argo_parser::{Node, Value as Yaml};
use workflow_graph::{Input, Outcome, Task};

use crate::objects::{KeyRef, config_map};
use crate::run::{Failure, Result, Run, scalar_text};
use crate::tags::{Variables, replace_node};

/// A mount whose files go into its volume when the task in `slot` of its node ends.
#[derive(Clone, Debug)]
pub(crate) struct Written {
    pub slot: usize,
    pub key: String,
    pub path: String,
    pub sub_path: String,
    /// The paths in the volume of the files the task got below the mount.
    pub given: Vec<String>,
    /// The folder the task got in a real run, which shows what it changed at its end.
    pub start: Option<PathBuf>,
}

/// Where the files of a mounted volume come from.
enum Source {
    /// Files that tasks write, shared under a key; a host path adds the files on the host.
    Shared { key: String, host: Option<Host> },
    ConfigMap {
        reference: KeyRef,
        items: Vec<(String, String)>,
    },
    Secret {
        reference: KeyRef,
        items: Vec<(String, String)>,
    },
}

/// A path of the machine that runs uwe, read as a `hostPath` volume.
struct Host {
    path: String,
    file: bool,
    create: bool,
}

/// `items` of a ConfigMap or Secret volume: keys and the paths of their files.
fn items(source: &Node) -> Vec<(String, String)> {
    match source.get("items").map(|items| &items.value) {
        Some(Yaml::Sequence(items)) => items
            .iter()
            .map(|item| {
                let text = |key: &str| item.get(key).map(scalar_text).unwrap_or_default();
                (text("key"), text("path"))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A mount path without `.` parts, which an empty parameter can leave, like `/mnt/input/.`.
fn clean(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| *part != ".").collect();
    parts.join("/")
}

/// A path below a mount path.
fn below(mount: &str, relative: &str) -> String {
    format!("{}/{relative}", mount.trim_end_matches('/'))
}

impl Run {
    /// The volume of this name: of the template, of the workflow, or a claim template.
    fn volume(&mut self, index: usize, template: &Template, name: &str) -> Result<Option<Source>> {
        if let Some(path) = self.options.policy.volumes.get(name) {
            let path = path
                .canonicalize()
                .map_err(|error| Failure::error(format!("volume {name}: {error}")))?;
            return Ok(Some(Source::Shared {
                key: format!("host:{}", path.display()),
                host: Some(Host {
                    file: path.is_file(),
                    path: path.to_string_lossy().into_owned(),
                    create: false,
                }),
            }));
        }
        let named = |volumes: &[Node]| {
            volumes
                .iter()
                .find(|volume| volume.get("name").and_then(Node::as_str) == Some(name))
                .cloned()
        };
        let volume = match named(&template.volumes) {
            Some(volume) => volume,
            None => match named(&self.spec.volumes) {
                Some(mut volume) => {
                    let variables = Variables::new(self.globals.clone());
                    let _ = replace_node(&mut volume, &variables, true);
                    volume
                }
                None => {
                    let claims = &self.spec.volume_claim_templates;
                    let claimed = claims.iter().any(|claim| {
                        claim
                            .get("metadata")
                            .and_then(|metadata| metadata.get("name"))
                            .and_then(Node::as_str)
                            == Some(name)
                    });
                    if !claimed {
                        return Ok(None);
                    }
                    self.claims.insert(name.to_owned());
                    return Ok(Some(Source::Shared {
                        key: format!("claim:{name}"),
                        host: None,
                    }));
                }
            },
        };
        let text = |source: &Node, key: &str| source.get(key).map(scalar_text).unwrap_or_default();
        if let Some(claim) = volume.get("persistentVolumeClaim") {
            let claim = text(claim, "claimName");
            if self.spec.volume_claim_templates.iter().any(|item| {
                item.get("metadata")
                    .and_then(|node| node.get("name"))
                    .and_then(Node::as_str)
                    == Some(claim.as_str())
            }) {
                self.claims.insert(claim.clone());
            }
            if !self.claims.contains(&claim) {
                if !self.options.assume_inputs {
                    return Err(Failure::unsupported(format!(
                        "volume {name}: existing claim {claim} requires an explicit host volume mapping"
                    )));
                }
                self.warn(format!(
                    "volume claim {claim} exists only in a cluster; the run starts it empty"
                ));
            }
            return Ok(Some(Source::Shared {
                key: format!("claim:{claim}"),
                host: None,
            }));
        }
        if let Some(host) = volume.get("hostPath") {
            let path = text(host, "path");
            if let Some(roots) = &self.options.policy.host_roots
                && (Path::new(&path)
                    .components()
                    .any(|part| part == Component::ParentDir)
                    || !roots
                        .iter()
                        .any(|root| confined(root, Path::new(&path)).is_ok()))
            {
                return Err(Failure::unsupported(format!(
                    "volume {name}: hostPath {path} is outside authorized roots"
                )));
            }
            let kind = text(host, "type");
            return Ok(Some(Source::Shared {
                key: format!("host:{path}"),
                host: Some(Host {
                    path,
                    file: kind.starts_with("File"),
                    create: kind.ends_with("OrCreate"),
                }),
            }));
        }
        if volume.get("emptyDir").is_some() {
            let pod = &self.nodes[index].name;
            return Ok(Some(Source::Shared {
                key: format!("pod:{pod}:{name}"),
                host: None,
            }));
        }
        if let Some(map) = volume.get("configMap") {
            let mut reference = KeyRef::of(map);
            reference.key.clear();
            return Ok(Some(Source::ConfigMap {
                reference,
                items: items(map),
            }));
        }
        if let Some(secret) = volume.get("secret") {
            let mut reference = KeyRef::of(secret);
            reference.name = text(secret, "secretName");
            return Ok(Some(Source::Secret {
                reference,
                items: items(secret),
            }));
        }
        if !self.options.assume_inputs {
            return Err(Failure::unsupported(format!(
                "volume {name}: this source requires an explicit host volume mapping"
            )));
        }
        self.warn(format!(
            "volume {name} is of a kind that needs a cluster; it acts like a claim of that name"
        ));
        Ok(Some(Source::Shared {
            key: format!("volume:{name}"),
            host: None,
        }))
    }

    /// Gives a task the volumes that `mounts` name: their files as inputs below the mount
    /// paths, and each mount that the task may write to as a directory output.
    pub(crate) fn mount_volumes(
        &mut self,
        index: usize,
        slot: usize,
        template: &Template,
        mounts: &[Node],
        task: &mut Task,
    ) -> Result<()> {
        for mount in mounts {
            let text = |key: &str| mount.get(key).map(scalar_text).unwrap_or_default();
            let (name, sub_path) = (text("name"), text("subPath"));
            let mut selected = PathBuf::new();
            for component in Path::new(&sub_path).components() {
                match component {
                    Component::Normal(part) => selected.push(part),
                    Component::CurDir => {}
                    _ => {
                        return Err(Failure::error(format!(
                            "volume {name} has invalid subPath {sub_path}"
                        )));
                    }
                }
            }
            let sub_path = selected.to_string_lossy().into_owned();
            let path = clean(&text("mountPath"));
            let read_only = matches!(
                mount.get("readOnly").map(|node| &node.value),
                Some(Yaml::Bool(true))
            );
            let Some(source) = self.volume(index, template, &name)? else {
                self.warn(format!(
                    "template {} mounts volume {name}, which neither the template nor the workflow declares; it is left out",
                    template.name
                ));
                continue;
            };
            let empty = |path: &str| Input {
                path: path.to_owned(),
                source: String::new(),
                directory: true,
                content: None,
                ..Default::default()
            };
            match source {
                Source::Shared { key, host } => {
                    let before = task.inputs.len();
                    // A mounted host file is read, not written.
                    let mut file = host.as_ref().is_some_and(|host| host.file);
                    let host_input = host.as_ref().is_some_and(|host| !host.path.is_empty());
                    let mut host_folder = None;
                    if let Some(host) = host.filter(|host| !host.path.is_empty()) {
                        let selected = if sub_path.is_empty() {
                            PathBuf::from(&host.path)
                        } else {
                            Path::new(&host.path).join(&sub_path)
                        };
                        if Path::new(&host.path).exists() {
                            confined(Path::new(&host.path), &selected).map_err(Failure::error)?;
                        }
                        let exists = selected.exists();
                        if !sub_path.is_empty() && exists {
                            file = selected.is_file();
                        }
                        if exists || self.options.assume_inputs {
                            host_folder = Some(selected);
                        } else if !host.create {
                            return Err(Failure::error(format!(
                                "template {} mounts the host path {}, which does not exist on this machine",
                                template.name,
                                selected.display()
                            )));
                        }
                    }
                    let prefix = match sub_path.trim_matches('/') {
                        "" => String::new(),
                        folder => format!("{folder}/"),
                    };
                    // A real task gets a writable volume as one folder, so it can remove files;
                    // its changes are committed on completion, to the host for a host path.
                    let whole = !self.options.assume_inputs && !read_only && !file;
                    if let Some(selected) = host_folder.as_ref().filter(|_| !whole) {
                        task.inputs.push(Input {
                            path: path.clone(),
                            source: selected.to_string_lossy().into_owned(),
                            directory: !file,
                            content: None,
                            ..Default::default()
                        });
                    }
                    // Volume files are writable copies; mounted host paths stay read-only.
                    let writable = match (read_only || host_input) && !whole {
                        true => workflow_graph::Writable::No,
                        false => workflow_graph::Writable::Copy,
                    };
                    let files = self.volumes.get(&key).cloned().unwrap_or_default();
                    let mut given = Vec::new();
                    let mut start = None;
                    if whole {
                        let mut found = Vec::new();
                        if let Some(selected) =
                            host_folder.as_ref().filter(|folder| folder.is_dir())
                        {
                            walk(selected, selected, &mut found).map_err(|error| {
                                Failure::error(format!(
                                    "cannot read the host path {}: {error}",
                                    selected.display()
                                ))
                            })?;
                            let outward = found.iter().find(|(inside, _)| {
                                crate::archives::resolved(selected, Path::new(inside)).is_none()
                            });
                            if let Some((inside, _)) = outward {
                                return Err(Failure::error(format!(
                                    "the host path {} holds the link {inside}, which leads out of it",
                                    selected.display()
                                )));
                            }
                        }
                        let mut below: BTreeMap<String, PathBuf> = found.into_iter().collect();
                        below.extend(files.iter().filter_map(|(relative, location)| {
                            let inside = relative.strip_prefix(&prefix)?;
                            Some((inside.to_owned(), location.clone()))
                        }));
                        let below: Vec<(String, PathBuf)> = below.into_iter().collect();
                        let folder = self.volume_copy(&below).map_err(Failure::error)?;
                        given = (below.into_iter())
                            .map(|(inside, _)| format!("{prefix}{inside}"))
                            .collect();
                        start = Some(folder.clone());
                        task.inputs.push(Input {
                            path: path.clone(),
                            source: folder.to_string_lossy().into_owned(),
                            directory: true,
                            writable,
                            ..Default::default()
                        });
                    }
                    for (relative, location) in files.into_iter().filter(|_| !whole) {
                        if file && relative == sub_path {
                            task.inputs.push(Input {
                                path: path.clone(),
                                source: location.to_string_lossy().into_owned(),
                                writable,
                                ..Default::default()
                            });
                            continue;
                        }
                        if let Some(inside) = relative.strip_prefix(&prefix) {
                            given.push(relative.clone());
                            task.inputs.push(Input {
                                executable: false,
                                path: below(&path, inside),
                                source: location.to_string_lossy().into_owned(),
                                directory: false,
                                content: None,
                                writable,
                            });
                        }
                    }
                    if task.inputs.len() == before {
                        task.inputs.push(empty(&path));
                    }
                    if !read_only && !file {
                        task.outputs
                            .push(format!("{}/", path.trim_end_matches('/')));
                        self.nodes[index].written.push(Written {
                            slot,
                            key,
                            path,
                            sub_path: prefix,
                            given,
                            start,
                        });
                    }
                }
                Source::ConfigMap { reference, items } => {
                    let found = config_map(
                        &self.library,
                        &self.document.path,
                        self.document.namespace(),
                        &reference.name,
                    );
                    let Some(entries) = found else {
                        if !reference.optional {
                            let message = format!(
                                "volume {name} of template {} reads ConfigMap {}, which none of the loaded documents (the workflow file, the configs and the repository) holds",
                                template.name, reference.name
                            );
                            if !self.options.assume_inputs {
                                return Err(Failure::error(message));
                            }
                            self.warn(format!("{message}; a simulated run mounts an empty folder"));
                        }
                        task.inputs.push(empty(&path));
                        continue;
                    };
                    let wanted: Vec<(String, String)> = if items.is_empty() {
                        entries
                            .iter()
                            .map(|(key, _)| (key.clone(), key.clone()))
                            .collect()
                    } else {
                        items
                    };
                    let before = task.inputs.len();
                    for (key, file) in wanted {
                        if let Some((_, value)) = entries.iter().find(|(known, _)| *known == key) {
                            task.inputs.push(Input {
                                path: below(&path, &file),
                                source: String::new(),
                                directory: false,
                                content: Some(value.clone()),
                                ..Default::default()
                            });
                        } else if !reference.optional {
                            let message = format!(
                                "volume {name} of template {} requires missing ConfigMap {} key {key}",
                                template.name, reference.name
                            );
                            if !self.options.assume_inputs {
                                return Err(Failure::error(message));
                            }
                            self.warn(message);
                        }
                    }
                    if task.inputs.len() == before {
                        task.inputs.push(empty(&path));
                    }
                }
                Source::Secret { reference, items } => {
                    if items.is_empty() {
                        let message = format!(
                            "volume {name} of template {} reads Secret {}",
                            template.name, reference.name
                        );
                        if !self.options.assume_inputs && !reference.optional {
                            return Err(Failure::error(format!(
                                "{message}; uwe has no secret store yet, so a real run cannot give it to a task"
                            )));
                        }
                        self.warn(format!(
                            "{message}, whose keys only a secret store knows; the task gets an empty folder"
                        ));
                        task.inputs.push(empty(&path));
                    }
                    for (key, file) in items {
                        let what = format!("file {file} of volume {name}");
                        let reference = KeyRef {
                            key,
                            ..reference.clone()
                        };
                        let value = self.secret(&what, &reference)?;
                        task.inputs.push(Input {
                            path: below(&path, &file),
                            source: String::new(),
                            directory: false,
                            content: Some(value),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// A new folder that holds the files of a volume, linked from the store where it can.
    fn volume_copy(&mut self, files: &[(String, PathBuf)]) -> std::result::Result<PathBuf, String> {
        let failed = |error: std::io::Error| format!("cannot copy a volume for a task: {error}");
        let folder = self.private_folder().map_err(failed)?;
        for (inside, location) in files {
            let target = folder.join(inside);
            confined(&folder, &target)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(failed)?;
            }
            if std::fs::symlink_metadata(location).is_ok_and(|meta| meta.is_symlink()) {
                let link = std::fs::read_link(location).map_err(failed)?;
                std::os::unix::fs::symlink(link, &target).map_err(failed)?;
            } else if std::fs::hard_link(location, &target).is_err() {
                std::fs::copy(location, &target).map_err(failed)?;
            }
        }
        Ok(folder)
    }

    /// Takes the files that the tasks of a node wrote into their volumes. In a real run, the
    /// files written into a host path go back to that path on this machine; a simulated run never
    /// writes there.
    pub(crate) fn keep_written(&mut self, index: usize) -> std::result::Result<(), String> {
        let written = self.nodes[index].written.clone();
        for mount in written {
            let Some(Some(outcome)) = self.nodes[index].outcomes.get(mount.slot).cloned() else {
                continue;
            };
            let Some(location) = directory_of(&outcome, &mount.path) else {
                continue;
            };
            let location = match &self.files {
                Some(files) => files.local(&location).map_err(|error| error.to_string())?,
                None => PathBuf::from(location),
            };
            let mut files = Vec::new();
            walk(&location, &location, &mut files).map_err(|error| error.to_string())?;
            let kept: std::collections::HashSet<String> = (files.iter())
                .map(|(relative, _)| format!("{}{relative}", mount.sub_path))
                .collect();
            // Only what the task changed goes on, as on a shared host path, where siblings
            // that run at the same time keep each other's changes.
            if let Some(start) = &mount.start {
                files.retain(|(relative, file)| !same(file, &start.join(relative)));
            }
            let host = mount
                .key
                .strip_prefix("host:")
                .filter(|_| !self.options.assume_inputs)
                .map(PathBuf::from);
            let selected = host.as_ref().map(|host| host.join(&mount.sub_path));
            if let Some(host) = &host {
                std::fs::create_dir_all(host).map_err(|error| error.to_string())?;
                confined(host, selected.as_ref().unwrap())?;
            }
            let volume = self.volumes.entry(mount.key.clone()).or_default();
            // The mount shows the task's view at its end: files it got and no longer has were
            // removed or renamed. Simulated tasks show only what they make.
            if let (Some(host), Some(selected)) = (&host, &selected) {
                let changed: Vec<(PathBuf, PathBuf)> = (files.iter())
                    .map(|(relative, file)| {
                        (
                            file.clone(),
                            host.join(format!("{}{relative}", mount.sub_path)),
                        )
                    })
                    .collect();
                write_back(&changed, selected)?;
            }
            if !self.options.assume_inputs {
                for relative in mount.given.iter().filter(|path| !kept.contains(*path)) {
                    volume.remove(relative);
                    // On a real host path, the task removed the file of the host.
                    if let (Some(host), Some(selected)) = (&host, &selected) {
                        let target = host.join(relative);
                        confined(selected, &target)?;
                        match std::fs::remove_file(&target) {
                            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                                return Err(format!(
                                    "cannot remove {} from the host path: {error}",
                                    target.display()
                                ));
                            }
                            _ => {}
                        }
                        // Folders that the task removed too go, if nothing else is in them.
                        let inside = relative.strip_prefix(&mount.sub_path).unwrap_or(relative);
                        let mut parent = Path::new(inside).parent();
                        while let Some(folder) =
                            parent.filter(|folder| !folder.as_os_str().is_empty())
                        {
                            if location.join(folder).exists() {
                                break;
                            }
                            let _ = std::fs::remove_dir(selected.join(folder));
                            parent = folder.parent();
                        }
                    }
                }
            }
            for (relative, file) in files {
                volume.insert(format!("{}{relative}", mount.sub_path), file);
            }
        }
        let outcomes = &self.nodes[index].outcomes;
        let pending: Vec<_> = self.nodes[index]
            .written
            .iter()
            .filter(|mount| !matches!(outcomes.get(mount.slot), Some(Some(_))))
            .cloned()
            .collect();
        self.nodes[index].written = pending;
        Ok(())
    }
}

/// Copies the files that a task wrote into a host path to that path, over files of the same
/// name. All copies are made beside their targets first and then renamed into place, so that
/// a failed copy leaves the host path as it was.
fn write_back(files: &[(PathBuf, PathBuf)], root: &Path) -> std::result::Result<(), String> {
    let failed = |target: &Path, error: std::io::Error| {
        format!(
            "cannot write {} back to the host path: {error}",
            target.display()
        )
    };
    let mut staged = Vec::new();
    let mut result = Ok(());
    for (index, (file, target)) in files.iter().enumerate() {
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        let copy = target.with_file_name(format!(".{name}.uwe-{}-{index}", std::process::id()));
        result = confined(root, target)
            .and_then(|()| stage_copy(file, &copy).map_err(|error| failed(target, error)));
        if result.is_err() {
            let _ = std::fs::remove_file(&copy);
            break;
        }
        staged.push((copy, target));
    }
    if result.is_ok() {
        for (copy, target) in &staged {
            result = std::fs::rename(copy, target).map_err(|error| failed(target, error));
            if result.is_err() {
                break;
            }
        }
    }
    for (copy, _) in &staged {
        let _ = std::fs::remove_file(copy);
    }
    result
}

/// Copies a file or a link to `copy`; a copied file stays writable for the next write back.
fn stage_copy(file: &Path, copy: &Path) -> std::io::Result<()> {
    if let Some(parent) = copy.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let metadata = std::fs::symlink_metadata(file)?;
    if metadata.is_symlink() {
        return std::os::unix::fs::symlink(std::fs::read_link(file)?, copy);
    }
    std::fs::copy(file, copy)?;
    let mode = metadata.permissions().mode() | 0o200;
    std::fs::set_permissions(copy, std::fs::Permissions::from_mode(mode))
}

/// Whether a task left a file or link as it got it.
fn same(file: &Path, start: &Path) -> bool {
    let (Ok(left), Ok(right)) = (
        std::fs::symlink_metadata(file),
        std::fs::symlink_metadata(start),
    ) else {
        return false;
    };
    if left.is_symlink() || right.is_symlink() {
        return left.is_symlink()
            && right.is_symlink()
            && std::fs::read_link(file).ok() == std::fs::read_link(start).ok();
    }
    use std::os::unix::fs::MetadataExt;
    if (left.dev(), left.ino()) == (right.dev(), right.ino()) {
        return true;
    }
    left.len() == right.len() && equal_bytes(file, start).unwrap_or(false)
}

fn equal_bytes(left: &Path, right: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let (mut left, mut right) = (
        std::io::BufReader::new(std::fs::File::open(left)?),
        std::io::BufReader::new(std::fs::File::open(right)?),
    );
    let (mut one, mut two) = (vec![0; 1 << 16], vec![0; 1 << 16]);
    loop {
        let count = left.read(&mut one)?;
        if count == 0 {
            return Ok(right.read(&mut two[..1])? == 0);
        }
        right.read_exact(&mut two[..count])?;
        if one[..count] != two[..count] {
            return Ok(false);
        }
    }
}

fn confined(root: &Path, target: &Path) -> std::result::Result<(), String> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut existing = target;
    while std::fs::symlink_metadata(existing).is_err() {
        existing = existing
            .parent()
            .ok_or_else(|| format!("invalid host path {}", target.display()))?;
    }
    let resolved = existing.canonicalize().map_err(|error| error.to_string())?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "host path {} escapes {}",
            target.display(),
            root.display()
        ));
    }
    Ok(())
}

/// The directory that a task made at a mount path.
fn directory_of(outcome: &Outcome, path: &str) -> Option<String> {
    let wanted = path.trim_matches('/');
    outcome
        .files
        .iter()
        .find(|file| file.path.trim_matches('/') == wanted && file.directory)
        .map(|file| file.location.clone())
}

/// The files and links below a directory, by their path relative to `root`; links are never
/// followed, so that a link to a folder is not read as one.
fn walk(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    paths.sort();
    for path in paths {
        if std::fs::symlink_metadata(&path)?.is_dir() {
            walk(root, &path, files)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push((relative.to_string_lossy().into_owned(), path));
        }
    }
    Ok(())
}

/// A map from the key of a volume to its files, by path in the volume.
pub(crate) type Volumes = BTreeMap<String, BTreeMap<String, PathBuf>>;

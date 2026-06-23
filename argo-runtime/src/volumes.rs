//! Volumes as shared files (decision 18). Volumes that several steps mount act as a shared file
//! system: the run keeps the files that tasks write into a volume, and gives them to the tasks
//! that mount it later. A volume is a claim (`persistentVolumeClaim` or `volumeClaimTemplates`),
//! a host path, or the `emptyDir` of one pod. In a real run, the files that tasks write into a
//! host path go back to that path on this machine. ConfigMaps become files with their text, and
//! Secrets placeholders in a simulated run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// A path of the machine that runs iunctus, read as a `hostPath` volume.
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
    fn volume(&mut self, index: usize, template: &Template, name: &str) -> Option<Source> {
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
                        return None;
                    }
                    self.claims.insert(name.to_owned());
                    return Some(Source::Shared {
                        key: format!("claim:{name}"),
                        host: None,
                    });
                }
            },
        };
        let text = |source: &Node, key: &str| source.get(key).map(scalar_text).unwrap_or_default();
        if let Some(claim) = volume.get("persistentVolumeClaim") {
            let claim = text(claim, "claimName");
            if !self.claims.contains(&claim) {
                self.warn(format!(
                    "volume claim {claim} exists only in a cluster; the run starts it empty"
                ));
            }
            return Some(Source::Shared {
                key: format!("claim:{claim}"),
                host: None,
            });
        }
        if let Some(host) = volume.get("hostPath") {
            let path = text(host, "path");
            let kind = text(host, "type");
            return Some(Source::Shared {
                key: format!("host:{path}"),
                host: Some(Host {
                    path,
                    file: kind.starts_with("File"),
                    create: kind.ends_with("OrCreate"),
                }),
            });
        }
        if volume.get("emptyDir").is_some() {
            let pod = &self.nodes[index].name;
            return Some(Source::Shared {
                key: format!("pod:{pod}:{name}"),
                host: None,
            });
        }
        if let Some(map) = volume.get("configMap") {
            let mut reference = KeyRef::of(map);
            reference.key.clear();
            return Some(Source::ConfigMap {
                reference,
                items: items(map),
            });
        }
        if let Some(secret) = volume.get("secret") {
            let mut reference = KeyRef::of(secret);
            reference.name = text(secret, "secretName");
            return Some(Source::Secret {
                reference,
                items: items(secret),
            });
        }
        self.warn(format!(
            "volume {name} is of a kind that needs a cluster; it acts like a claim of that name"
        ));
        Some(Source::Shared {
            key: format!("volume:{name}"),
            host: None,
        })
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
            let path = clean(&text("mountPath"));
            let read_only = matches!(
                mount.get("readOnly").map(|node| &node.value),
                Some(Yaml::Bool(true))
            );
            let Some(source) = self.volume(index, template, &name) else {
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
            };
            match source {
                Source::Shared { key, host } => {
                    let before = task.inputs.len();
                    // A mounted host file is read, not written.
                    let file = host.as_ref().is_some_and(|host| host.file);
                    if let Some(host) = host.filter(|host| !host.path.is_empty()) {
                        let exists = Path::new(&host.path).exists();
                        if exists || self.options.assume_inputs {
                            task.inputs.push(Input {
                                path: path.clone(),
                                source: host.path.clone(),
                                directory: !host.file,
                                content: None,
                            });
                        } else if !host.create {
                            return Err(Failure::error(format!(
                                "template {} mounts the host path {}, which does not exist on this machine",
                                template.name, host.path
                            )));
                        }
                    }
                    let prefix = match sub_path.trim_matches('/') {
                        "" => String::new(),
                        folder => format!("{folder}/"),
                    };
                    let files = self.volumes.get(&key).cloned().unwrap_or_default();
                    for (relative, location) in files {
                        if let Some(inside) = relative.strip_prefix(&prefix) {
                            task.inputs.push(Input {
                                path: below(&path, inside),
                                source: location.to_string_lossy().into_owned(),
                                directory: false,
                                content: None,
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
                        });
                    }
                }
                Source::ConfigMap { reference, items } => {
                    let found = config_map(&self.library, &self.document.path, &reference.name);
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
                    for (key, file) in wanted {
                        if let Some((_, value)) = entries.iter().find(|(known, _)| *known == key) {
                            task.inputs.push(Input {
                                path: below(&path, &file),
                                source: String::new(),
                                directory: false,
                                content: Some(value.clone()),
                            });
                        }
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
                                "{message}; iunctus has no secret store yet, so a real run cannot give it to a task"
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
                        });
                    }
                }
            }
        }
        Ok(())
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
            let mut files = Vec::new();
            walk(&location, &location, &mut files);
            let host = mount
                .key
                .strip_prefix("host:")
                .filter(|_| !self.options.assume_inputs)
                .map(PathBuf::from);
            let volume = self.volumes.entry(mount.key.clone()).or_default();
            for (relative, file) in files {
                let relative = format!("{}{relative}", mount.sub_path);
                if let Some(host) = &host {
                    write_back(&file, &host.join(&relative))?;
                }
                volume.insert(relative, file);
            }
        }
        Ok(())
    }
}

/// Copies a file that a task wrote into a host path to that path, over a file of the same name.
fn write_back(file: &Path, target: &Path) -> std::result::Result<(), String> {
    let failed = |error: std::io::Error| {
        format!(
            "cannot write {} back to the host path: {error}",
            target.display()
        )
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    std::fs::copy(file, target).map_err(failed)?;
    Ok(())
}

/// The directory that a task made at a mount path.
fn directory_of(outcome: &Outcome, path: &str) -> Option<PathBuf> {
    let wanted = path.trim_matches('/');
    outcome
        .files
        .iter()
        .find(|file| file.path.trim_matches('/') == wanted && file.directory)
        .map(|file| file.location.clone())
}

/// The files below a directory, by their path relative to `root`.
fn walk(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            walk(root, &path, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push((relative.to_string_lossy().into_owned(), path));
        }
    }
}

/// A map from the key of a volume to its files, by path in the volume.
pub(crate) type Volumes = BTreeMap<String, BTreeMap<String, PathBuf>>;

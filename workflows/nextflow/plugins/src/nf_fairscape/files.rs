//! The Datasets of nf-fairscape's crate: task outputs, published files, inputs and configs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use md5::{Digest as _, Md5};

use super::{Builder, DATASET, file_name, hex, refs, texts, time, uri};
use crate::observer::Task;
use crate::report_format::Json;

/// A file of the crate.
pub(super) struct Dataset {
    pub(super) id: String,
    pub(super) name: String,
    /// `produced`, `used` or `config`, for its description.
    pub(super) role: &'static str,
    pub(super) producers: Vec<String>,
    /// The key and value of its one locator, `contentUrl` or `localPath`.
    pub(super) locator: (&'static str, String),
    /// The local file, for its size and checksum.
    pub(super) local: Option<PathBuf>,
    pub(super) published: bool,
    pub(super) part_of: Option<String>,
}

impl Builder<'_> {
    /// Copies the main script and the config files into `workflow/` of the crate.
    pub(super) fn copy_workflow(&mut self) {
        if !self.settings.workflow {
            return;
        }
        let folder = self.folder.join("workflow");
        if let Err(error) = std::fs::create_dir_all(&folder) {
            tracing::warn!(
                "nf-fairscape: could not create '{}' to hold the workflow copy: {error}",
                folder.display()
            );
            return;
        }
        let files = std::iter::once(&self.start.script).chain(&self.start.config_files);
        for (place, source) in files.enumerate() {
            let name = file_name(source);
            match std::fs::copy(source, folder.join(&name)) {
                Ok(_) if place > 0 => self.configs.push(name),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        "nf-fairscape: could not copy '{source}' into the crate: {error}"
                    )
                }
            }
        }
    }

    /// The Datasets: task outputs in task order, then published files that no task made,
    /// then inputs, then the copied config files.
    pub(super) fn datasets(&self, tasks: &[Task], published: &[(String, String)]) -> Vec<Dataset> {
        let mut datasets: Vec<Dataset> = Vec::new();
        let position = |datasets: &[Dataset], id: &str| datasets.iter().position(|it| it.id == id);
        for task in tasks {
            let producer = self.task_id(task);
            for file in &task.outputs {
                let id = self.dataset_id(&file.path, &file.name);
                match position(&datasets, &id) {
                    Some(found) => datasets[found].producers.push(producer.clone()),
                    None => {
                        let mut dataset = self.file(id, &file.path, "produced");
                        dataset.producers.push(producer.clone());
                        datasets.push(dataset);
                    }
                }
            }
        }
        for (source, target) in published {
            let id = self.dataset_id(source, source);
            let place = match position(&datasets, &id) {
                Some(found) => found,
                None => {
                    datasets.push(self.file(id, source, "produced"));
                    datasets.len() - 1
                }
            };
            let dataset = &mut datasets[place];
            dataset.published = true;
            dataset.local = Some(PathBuf::from(target));
            dataset.locator = match Path::new(target).strip_prefix(self.folder) {
                Ok(inside) => ("contentUrl", inside.to_string_lossy().into_owned()),
                Err(_) => ("localPath", target.clone()),
            };
        }
        let mut inputs: Vec<(String, String)> = (tasks.iter())
            .flat_map(|task| &task.inputs)
            .map(|file| (file.path.clone(), file.name.clone()))
            .collect();
        if self.settings.param_inputs {
            inputs.extend(
                self.param_files()
                    .into_iter()
                    .map(|path| (path.clone(), path)),
            );
        }
        for (path, name) in inputs {
            let id = self.dataset_id(&path, &name);
            if position(&datasets, &id).is_none() {
                datasets.push(self.file(id, &path, "used"));
            }
        }
        for name in &self.configs {
            let crated = format!("workflow/{name}");
            let mut dataset = self.file(self.ark("dataset", name, &crated), &crated, "config");
            dataset.name = name.clone();
            dataset.locator = ("contentUrl", crated.clone());
            dataset.local = Some(self.folder.join(&crated));
            dataset.part_of = Some(self.workflow.clone());
            datasets.push(dataset);
        }
        datasets
    }

    /// A Dataset of a file at a path or address, with its locator.
    pub(super) fn file(&self, id: String, path: &str, role: &'static str) -> Dataset {
        let remote = path.contains("://");
        let locator = match (remote, Path::new(path).strip_prefix(self.folder)) {
            (true, _) => ("contentUrl", path.to_owned()),
            (false, Ok(inside)) => ("contentUrl", inside.to_string_lossy().into_owned()),
            (false, Err(_)) => ("localPath", path.to_owned()),
        };
        Dataset {
            id,
            name: file_name(path),
            role,
            producers: Vec::new(),
            locator,
            local: (!remote).then(|| PathBuf::from(path)),
            published: false,
            part_of: None,
        }
    }

    /// The ARK of the Dataset of a file, from its normalised path.
    pub(super) fn dataset_id(&self, path: &str, name: &str) -> String {
        self.ark("dataset", &file_name(name), &uri(path))
    }

    pub(super) fn dataset(&self, dataset: &Dataset) -> Json {
        let name = &dataset.name;
        let description = match dataset.role {
            "produced" => format!(
                "File '{name}' produced by the Nextflow workflow run '{}'",
                self.name
            ),
            "config" => format!(
                "Nextflow configuration file '{name}' for the run '{}'",
                self.name
            ),
            _ => format!(
                "File '{name}' used by the Nextflow workflow run '{}'",
                self.name
            ),
        };
        let format = match dataset.role {
            "config" => "nextflow-config".to_owned(),
            _ => media_type(name),
        };
        let mut entity = Json::Object(Vec::new())
            .with("@id", dataset.id.as_str())
            .with("@type", texts(&DATASET))
            .with("name", name.as_str())
            .with("author", self.author.as_str())
            .with("description", description)
            .with("datePublished", time(self.end.end))
            .with("keywords", texts(&self.settings.keywords))
            .with("format", format)
            .with("generatedBy", refs(&dataset.producers))
            .with(dataset.locator.0, dataset.locator.1.as_str());
        if let Some(local) = &dataset.local
            && let Ok(found) = std::fs::metadata(local)
        {
            match found.is_dir() {
                true if self.settings.sizes => {
                    entity.insert("contentSize", size(local).to_string())
                }
                true => {}
                false => entity.insert("contentSize", found.len().to_string()),
            }
            if self.settings.checksums
                && found.is_file()
                && let Ok(bytes) = std::fs::read(local)
            {
                entity.insert("md5", hex(&Md5::digest(&bytes)));
            }
        }
        if let Some(part) = &dataset.part_of {
            entity.insert("isPartOf", refs(std::slice::from_ref(part)));
        }
        entity
    }

    /// Params whose values name files, like `/data/in.csv`: local ones that exist, and
    /// remote ones as they are.
    pub(super) fn param_files(&self) -> Vec<String> {
        let values = self.start.params.as_object().into_iter().flatten();
        let texts = values.filter_map(|(_, value)| value.as_str());
        let files = texts.filter(|text| {
            let last = text.rsplit('/').next().unwrap_or_default();
            text.contains('/') && last.contains('.') && !last.ends_with('.')
        });
        let local = |text: &&str| text.contains("://") || Path::new(text).exists();
        files.filter(local).map(str::to_owned).collect()
    }
}

/// The media type of a file by its extension, else the extension, else `unknown`.
fn media_type(name: &str) -> String {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "unknown".into();
    };
    let known = match extension.to_lowercase().as_str() {
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "gz" => "application/gzip",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        other => return other.to_owned(),
    };
    known.to_owned()
}

/// The size of a file, or of every file below a folder, in bytes.
pub(super) fn size(path: &Path) -> u64 {
    match std::fs::symlink_metadata(path) {
        Ok(found) if found.is_dir() => (std::fs::read_dir(path).into_iter().flatten().flatten())
            .map(|entry| size(&entry.path()))
            .sum(),
        Ok(found) => found.len(),
        Err(_) => 0,
    }
}

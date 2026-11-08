//! Exports retained workflow sources, remote input identities and output schemas.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use input_schema::Property;
use run_database::{Database, RemoteFile, Run, Source, SourceKind};
use serde_json::{Value, json};

use crate::data::{Data, Placed, copy_hashed};
use crate::entities::one_or_many;
use crate::formal::formal_parameter;
use crate::ids::{encode, time};
use crate::{CrateError, Options};

/// The folder of the crate that holds the files of the workflow.
pub(crate) const WORKFLOW_FOLDER: &str = "workflow/";

/// The file of the crate that holds the JSON Schema of the outputs.
pub(crate) const OUTPUT_SCHEMA: &str = "outputs.schema.json";

/// What pins the run, gathered for the metadata.
#[derive(Default)]
pub(crate) struct Pinned {
    /// The files of the workflow in the crate, by their ids, like `workflow/main.nf`.
    pub workflow: Vec<(String, Source)>,
    /// The files of the workflow that the crate lacks, as they changed since the run.
    pub changed: Vec<Source>,
    /// The files that the command line named, placed in `data/`.
    pub named: Vec<(Placed, Source)>,
    /// The remote files that the run read, by address.
    pub remote: HashMap<String, RemoteFile>,
    /// The JSON Schema of the outputs of the workflow.
    pub output_schema: Option<Value>,
}

/// Copies the files of the workflow and of the command line of a run into the crate, from the
/// copies that `options` gives by digest, else from where they are now, as long as they are
/// as the run had them; and reads the remote files and the outputs of the run.
pub(crate) fn gather(
    database: &Database,
    run: &Run,
    directory: &Path,
    options: &Options,
    data: &mut Data,
) -> Result<Pinned, CrateError> {
    let mut pinned = Pinned {
        remote: (database.remote_files(&run.id)?.into_iter())
            .map(|file| (file.address.clone(), file))
            .collect(),
        output_schema: database.output_schema(&run.id)?,
        ..Pinned::default()
    };
    if let Some(schema) = &pinned.output_schema {
        let mut schema = schema.clone();
        crate::hidden::json(&mut schema, options);
        let text = serde_json::to_string_pretty(&run_database::public_json(&schema)?)?;
        std::fs::write(directory.join(OUTPUT_SCHEMA), text + "\n")?;
    }
    let folder = run.workflow_folder.as_deref().map(PathBuf::from);
    for source in database.sources(&run.id)? {
        let kept = options.sources.get(&source.digest).cloned();
        match source.kind {
            SourceKind::Workflow => {
                let Some(folder) = &folder else { continue };
                let from = kept.unwrap_or_else(|| folder.join(&source.path));
                let id = format!("{WORKFLOW_FOLDER}{}", source.path);
                if crate::hidden::holds_secret(&from, options).unwrap_or(false) {
                    pinned.changed.push(source);
                    continue;
                }
                match copy_checked(&from, &directory.join(&id), &source.digest)? {
                    true => {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(
                            directory.join(&id),
                            std::fs::Permissions::from_mode(if source.executable {
                                0o755
                            } else {
                                0o644
                            }),
                        )?;
                        pinned.workflow.push((encode(&id), source));
                    }
                    false => pinned.changed.push(source),
                }
            }
            SourceKind::Named => {
                let from = kept.unwrap_or_else(|| PathBuf::from(&source.path));
                if crate::hidden::holds_secret(&from, options).unwrap_or(false) {
                    continue;
                }
                if options.describe_data {
                    let size = Some(source.size);
                    let placed = data.describe(&source.path, false, Some(&source.digest), size)?;
                    if matches!(placed, Placed::Described { .. }) {
                        pinned.named.push((placed, source));
                    }
                    continue;
                }
                match data.place_checked(&from.to_string_lossy(), Some(&source.digest))? {
                    Placed::Copied { digest, .. } if digest.to_string() != source.digest => {}
                    placed @ Placed::Copied { .. } => pinned.named.push((placed, source)),
                    _ => {}
                }
            }
        }
    }
    Ok(pinned)
}

/// Copies a file to `target` when its content has the digest `expected`; false, and no copy,
/// when it has other content now or is gone.
pub(crate) fn copy_checked(from: &Path, target: &Path, expected: &str) -> Result<bool, CrateError> {
    let Ok(mut source) = File::open(from) else {
        return Ok(false);
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (digest, _) = copy_hashed(&mut source, target)?;
    if digest.to_string() == expected {
        return Ok(true);
    }
    std::fs::remove_file(target)?;
    Ok(false)
}

impl Pinned {
    /// The entities of what pins the run: the folder of the workflow with its files, the
    /// named files, the remote files that no task read, and the schema of the outputs with its
    /// formal parameters.
    pub(crate) fn entities(&self, run: &Run, workflow: &str, read: &[&str]) -> Vec<Value> {
        let mut entities = Vec::new();
        if !self.workflow.is_empty() {
            let mut folder = json!({
                "@id": WORKFLOW_FOLDER,
                "@type": "Dataset",
                "name": "The files of the workflow",
                "description": format!(
                    "The files of the workflow as run {} had them, from {}.",
                    run.id,
                    run.workflow_folder.as_deref().unwrap_or("its folder")
                ),
                "hasPart": one_or_many(
                    self.workflow.iter().map(|(id, _)| json!({"@id": id}))
                )
            });
            if !self.changed.is_empty() {
                let paths: Vec<&str> = self.changed.iter().map(|s| s.path.as_str()).collect();
                folder["description"] = json!(format!(
                    "{} It lacks these files, which changed since: {}.",
                    folder["description"].as_str().unwrap_or_default(),
                    paths.join(", ")
                ));
            }
            entities.push(folder);
        }
        for (id, source) in &self.workflow {
            entities.push(json!({
                "@id": id,
                "@type": "File",
                "name": source.path,
                "sha256": hex(&source.digest),
                "contentSize": source.size.to_string()
            }));
        }
        for (placed, source) in &self.named {
            let (Placed::Copied { id, .. } | Placed::Described { id, .. }) = placed else {
                continue;
            };
            let name = Path::new(&source.path).file_name().unwrap_or_default();
            let mut entity = json!({
                "@id": id,
                "@type": "File",
                "name": name.to_string_lossy(),
                "alternateName": source.path,
                "description": "A file that the command line of the run named.",
                "sha256": hex(&source.digest),
                "contentSize": source.size.to_string()
            });
            if let Placed::Described {
                url, digest, size, ..
            } = placed
            {
                crate::files::described(&mut entity, url, digest, *size, &source.path);
            }
            entities.push(entity);
        }
        for file in self
            .remote
            .values()
            .filter(|file| !read.contains(&file.address.as_str()))
        {
            let mut entity = json!({
                "@id": file.address,
                "@type": "File",
                "name": file.address.rsplit('/').next().unwrap_or(&file.address),
                "description": "A remote file that the workflow read while it planned its tasks."
            });
            if run_database::public_resource_id(&file.address) != file.address {
                entity["url"] = json!(file.address);
            }
            self.describe_remote(&mut entity, &file.address);
            entities.push(entity);
        }
        if let Some(schema) = &self.output_schema {
            entities.push(json!({
                "@id": OUTPUT_SCHEMA,
                "@type": "File",
                "name": "The JSON Schema of the outputs of the workflow",
                "encodingFormat": "application/schema+json",
                "about": {"@id": workflow}
            }));
            for property in input_schema::properties(schema) {
                entities.push(output_parameter(&property));
            }
        }
        entities
    }

    /// Adds the digest, size and time of reading of a remote file that the run read.
    pub(crate) fn describe_remote(&self, entity: &mut Value, address: &str) {
        if let Some(file) = self.remote.get(address) {
            entity["sha256"] = json!(hex(&file.digest));
            entity["contentSize"] = json!(file.size.to_string());
            entity["sdDatePublished"] = json!(time(file.fetched));
        }
    }

    /// The ids of the outputs of the schema.
    pub(crate) fn outputs(&self) -> Vec<String> {
        (self.output_schema.iter())
            .flat_map(input_schema::properties)
            .map(|property| output_id(property.name))
            .collect()
    }

    /// The ids of the parts of the crate that pin the run, for the root.
    pub(crate) fn parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if !self.workflow.is_empty() {
            parts.push(WORKFLOW_FOLDER.to_owned());
        }
        if self.output_schema.is_some() {
            parts.push(OUTPUT_SCHEMA.to_owned());
        }
        parts.extend(self.named.iter().filter_map(|(placed, _)| match placed {
            Placed::Copied { id, .. } => Some(id.clone()),
            _ => None,
        }));
        parts
    }

    /// The ids of the named files, in `data/` or described where the run left them.
    pub(crate) fn named_ids(&self) -> impl Iterator<Item = String> + '_ {
        self.named.iter().filter_map(|(placed, _)| match placed {
            Placed::Copied { id, .. } | Placed::Described { id, .. } => Some(id.clone()),
            _ => None,
        })
    }
}

/// The id of the formal parameter of an output.
pub(crate) fn output_id(name: &str) -> String {
    format!("#output/{}", encode(name))
}

/// The formal parameter of an output, as that of an input with its own id.
fn output_parameter(property: &Property) -> Value {
    let mut parameter = formal_parameter(property);
    parameter["@id"] = json!(output_id(property.name));
    parameter
}

/// The hexadecimal part of `sha256:<hex>`.
fn hex(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
}

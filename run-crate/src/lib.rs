//! Exports recorded runs as Workflow Run RO-Crates with task and file provenance.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod data;
mod entities;
mod files;
mod formal;
mod hidden;
mod ids;
mod image;
mod metadata;
mod pinned;
mod wir;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use run_database::{Database, DatabaseError, Run, TaskDetail};
use serde_json::json;

use crate::data::Data;
use crate::metadata::Record;
use crate::pinned::Pinned;

/// The file name of the metadata of every RO-Crate.
pub const METADATA: &str = "ro-crate-metadata.json";

#[derive(Debug, thiserror::Error)]
pub enum CrateError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("RO-Crate: {0}")]
    Io(#[from] std::io::Error),
    #[error("RO-Crate: {0}")]
    Json(#[from] serde_json::Error),
    #[error("RO-Crate: a directory of the run has an invalid manifest: {0}")]
    Tree(task_model::SpecError),
    #[error("the run database has no run `{0}`")]
    UnknownRun(String),
    #[error("RO-Crate: the workflow `{0}` is not a file")]
    Workflow(PathBuf),
    #[error("RO-Crate: the workflow `{0}` holds a secret value of the run, so it is not exported")]
    Secret(PathBuf),
    #[error(
        "RO-Crate: the crate would replace the workflow `{0}` itself; export to another folder"
    )]
    Overwrite(PathBuf),
}

/// What an export wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The files and directories copied into the crate.
    pub files: usize,
    /// Their size in bytes.
    pub bytes: u64,
    /// The tasks described.
    pub tasks: usize,
}

/// What an export needs besides the run.
#[derive(Clone, Default)]
pub struct Options {
    /// The main workflow file, which the crate holds a copy of: the target of the run, or the
    /// main file of a project directory, like `main.nf` or `workflow/Snakefile`.
    pub workflow: PathBuf,
    /// The license of the crate, as a URL like `https://spdx.org/licenses/CC-BY-4.0`.
    pub license: Option<String>,
    /// Copies of the workflow, command-line and task files as the run had them, by their
    /// digests; directories use `tree:<digest>` (plain digest keys remain accepted). The crate
    /// takes them over the files as they are now, which may have changed since the run.
    pub sources: HashMap<String, PathBuf>,
    /// Values that the crate must not show, like the secret values of the run: each becomes
    /// `[secret]`, and a file of the workflow or command line that holds one is not copied.
    pub secrets: Vec<String>,
    /// Folders of the server, like those of the run and its data: the crate names a path in
    /// one relative to it.
    pub folders: Vec<PathBuf>,
    /// Describes the files of the run where they are instead of copying them into `data/`:
    /// each keeps its name, size, SHA-256 and `file://` location as `contentUrl`, under an id
    /// `#data/<hex>`.
    pub describe_data: bool,
    /// Persistent addresses of described files, by location prefix: a location that starts
    /// with a key, like `/data/store/`, gets the value in place of that prefix, like
    /// `aruna://object/`, as its `contentUrl` and `@id`. The longest prefix wins.
    pub uri_map: BTreeMap<String, String>,
    /// Gives the persistent address of a location where `uri_map` gives none.
    pub uri_resolver: Option<UriResolver>,
}

/// A function of the host that gives the persistent address of a file of the run, like an
/// `aruna://` or `https://` address, from its location; `None` keeps the location.
#[derive(Clone)]
pub struct UriResolver(Arc<Resolve>);

/// The function of a [`UriResolver`].
type Resolve = dyn Fn(&str) -> Option<String> + Send + Sync;

impl UriResolver {
    pub fn new(resolve: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> UriResolver {
        UriResolver(Arc::new(resolve))
    }
}

impl std::fmt::Debug for UriResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("UriResolver")
    }
}

/// Shows the options without the secret values and without the persistent addresses, which
/// may hold credentials of the host.
impl std::fmt::Debug for Options {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Options")
            .field("workflow", &self.workflow)
            .field("license", &self.license)
            .field("sources", &self.sources.len())
            .field(
                "secrets",
                &format_args!("[{} redacted]", self.secrets.len()),
            )
            .field("folders", &self.folders)
            .field("describe_data", &self.describe_data)
            .field("uri_map", &self.uri_map.len())
            .field("uri_resolver", &self.uri_resolver)
            .finish()
    }
}

impl Options {
    /// The persistent address of a location, by `uri_map`, else by `uri_resolver`.
    pub fn persistent(&self, location: &str) -> Option<String> {
        let mapped = (self.uri_map.iter())
            .filter(|(prefix, _)| !prefix.is_empty() && location.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(prefix, uri)| format!("{uri}{}", &location[prefix.len()..]));
        mapped.or_else(|| {
            self.uri_resolver
                .as_ref()
                .and_then(|resolve| (resolve.0)(location))
        })
    }
}

/// Writes the crate of a run into a directory, creating it when needed.
pub fn export(
    database: &Database,
    run: &str,
    directory: &Path,
    options: &Options,
) -> Result<Summary, CrateError> {
    let Some(record) = database.run(run)? else {
        return Err(CrateError::UnknownRun(run.to_owned()));
    };
    let Some(name) = options.workflow.file_name() else {
        return Err(CrateError::Workflow(options.workflow.clone()));
    };
    let workflow = name.to_string_lossy().into_owned();
    let main = main_file(database, &record, options)?;
    if hidden::holds_secret(&main.0, options)? {
        return Err(CrateError::Secret(options.workflow.clone()));
    }
    std::fs::create_dir_all(directory)?;
    let target = directory.join(&workflow);
    if [&main.0, &options.workflow]
        .iter()
        .any(|source| same_file(source, &target))
    {
        return Err(CrateError::Overwrite(options.workflow.clone()));
    }
    if !pinned::copy_checked(&main.0, &target, main.1.as_deref())? {
        return Err(CrateError::Workflow(options.workflow.clone()));
    }
    use std::os::unix::fs::PermissionsExt;
    let permissions = match &main.1 {
        Some(_) => std::fs::Permissions::from_mode(if main.2 { 0o755 } else { 0o644 }),
        None => std::fs::metadata(&main.0)?.permissions(),
    };
    std::fs::set_permissions(&target, permissions)?;
    let mut tasks: Vec<TaskDetail> = Vec::new();
    for row in database.tasks(run)? {
        // A task that never started did not run, so the crate leaves it out.
        if row.attempts > 0
            && let Some(task) = database.task(run, row.id)?
        {
            tasks.push(task);
        }
    }
    let mut data = Data::new(directory);
    let mut placed = HashMap::new();
    for task in &tasks {
        for file in task.inputs.iter().chain(&task.outputs) {
            if !file.location.is_empty() && options.describe_data {
                let digest = file.digest.as_deref();
                let persistent = options.persistent(&file.location);
                let recorded = digest.and_then(|digest| digest.parse().ok()).zip(file.size);
                let described = match (persistent, recorded) {
                    // The host keeps the file, so the crate needs no local copy to describe it.
                    (Some(uri), Some((digest, size))) => data::persistent(
                        data::described(&file.location, file.directory, digest, size),
                        Some(uri),
                    ),
                    (persistent, _) => {
                        let found =
                            data.describe(&file.location, file.directory, digest, file.size)?;
                        data::persistent(found, persistent)
                    }
                };
                placed.insert(files::file_key(file), described);
                continue;
            }
            if !file.location.is_empty() {
                let mut copy = data::Placed::Absent;
                if let Some(kept) = file.digest.as_ref().and_then(|digest| {
                    if file.directory {
                        options
                            .sources
                            .get(&format!("tree:{digest}"))
                            .or_else(|| options.sources.get(digest))
                    } else {
                        options.sources.get(digest)
                    }
                }) {
                    copy = data.place_checked(&kept.to_string_lossy(), file.digest.as_deref())?;
                }
                if matches!(&copy, data::Placed::Copied { directory, .. } if *directory != file.directory)
                {
                    copy = data::Placed::Absent;
                }
                if copy == data::Placed::Absent {
                    copy = match file.digest.as_deref() {
                        Some(digest) => data.place_checked(&file.location, Some(digest))?,
                        None => data.place(&file.location)?,
                    };
                }
                if matches!(&copy, data::Placed::Copied { directory, .. } if *directory != file.directory)
                {
                    copy = data::Placed::Absent;
                }
                placed.insert(files::file_key(file), copy);
                continue;
            }
            // A literal input: its content is in the task description.
            let content = task.description.inputs.iter().find_map(|input| {
                (input.path == file.path)
                    .then_some(input.content.as_deref())
                    .flatten()
            });
            if let (Some(content), Some(digest)) = (content, &file.digest) {
                placed.insert(format!("content:{digest}"), data.place_content(content)?);
            }
        }
    }
    let pinned = pinned::gather(database, &record, directory, options, &mut data)?;
    let inputs = database.run_inputs(run)?;
    if let Some(inputs) = &inputs {
        let mut schema = inputs.schema.clone();
        hidden::json(&mut schema, options);
        let text = serde_json::to_string_pretty(&run_database::public_json(&schema)?)?;
        std::fs::write(directory.join(formal::INPUT_SCHEMA), text + "\n")?;
    }
    let published = ids::time(run_database::now());
    let version = match &record.revision {
        Some(revision) => Some(revision.clone()),
        None => std::fs::read(directory.join(&workflow))
            .ok()
            .map(|bytes| task_model::Digest::sha256(&bytes).to_string()),
    };
    let graph = Record {
        run: &record,
        workflow: &workflow,
        tasks: &tasks,
        placed: &placed,
        published,
        version,
        license: options.license.as_deref(),
        run_inputs: inputs.as_ref(),
        pinned: &pinned,
    }
    .graph();
    let mut graph = graph;
    wir::provenance(&mut graph, &record, options)?;
    let mut metadata = json!({
        "@context": [
            "https://w3id.org/ro/crate/1.1/context",
            "https://w3id.org/ro/terms/workflow-run/context"
        ],
        "@graph": graph
    });
    hidden::json(&mut metadata, options);
    let text = serde_json::to_string_pretty(&run_database::public_json(&metadata)?)?;
    std::fs::write(directory.join(METADATA), text + "\n")?;
    std::fs::write(
        directory.join(metadata::README),
        run_database::redact_public_text(&hidden::text(
            &readme(
                &record,
                &workflow,
                tasks.len(),
                inputs.is_some(),
                &pinned,
                options,
            ),
            options,
        )),
    )?;
    let (files, bytes) = data.totals();
    Ok(Summary {
        files,
        bytes,
        tasks: tasks.len(),
    })
}

/// Whether two paths name the same file, also through links.
fn same_file(first: &Path, second: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(first), std::fs::metadata(second)) {
        (Ok(first), Ok(second)) => (first.dev(), first.ino()) == (second.dev(), second.ino()),
        _ => false,
    }
}

/// The main file of the workflow as the run had it: the copy that the options give for its
/// recorded digest, else the file as it is now.
fn main_file(
    database: &Database,
    run: &Run,
    options: &Options,
) -> Result<(PathBuf, Option<String>, bool), CrateError> {
    let folder = run.workflow_folder.as_deref().map(Path::new);
    let recorded = (database.sources(&run.id)?.into_iter()).find(|source| {
        source.kind == run_database::SourceKind::Workflow
            && folder.is_some_and(|folder| {
                let path = folder.join(&source.path);
                path == options.workflow || path == Path::new(&run.workflow)
            })
    });
    let digest = recorded.as_ref().map(|source| source.digest.clone());
    let kept = digest
        .as_ref()
        .and_then(|digest| options.sources.get(digest).cloned());
    let executable = recorded.is_some_and(|source| source.executable);
    Ok((
        kept.unwrap_or_else(|| options.workflow.clone()),
        digest,
        executable,
    ))
}

/// A short text for people who open the crate folder.
fn readme(
    run: &Run,
    workflow: &str,
    tasks: usize,
    inputs: bool,
    pinned: &Pinned,
    options: &Options,
) -> String {
    let kind = if run.simulated {
        "Simulated run"
    } else {
        "Run"
    };
    let mut text = format!("# {kind} {} of {workflow}\n\n", run.id);
    if run.simulated {
        text.push_str(metadata::SIMULATED);
        text.push_str("\n\n");
    }
    let ended = run.ended.map(ids::time).unwrap_or_else(|| "not yet".into());
    text.push_str(&format!(
        "This folder is a Workflow Run RO-Crate of the Provenance Run Crate profile 0.5, written by \
         uwe {}.\n\n\
         - `ro-crate-metadata.json` describes the run: the workflow, its steps, and one action per \
         task with its command, container image, inputs, outputs, start and end.\n\
         - `{workflow}` is the workflow that ran.\n{}{}\n\
         The run {}: started {}, ended {ended}, {tasks} tasks.\n",
        env!("CARGO_PKG_VERSION"),
        [
            (inputs, format!(
                "- `{}` is the JSON Schema of the inputs of the workflow.\n",
                formal::INPUT_SCHEMA
            )),
            (pinned.output_schema.is_some(), format!(
                "- `{}` is the JSON Schema of the outputs of the workflow.\n",
                pinned::OUTPUT_SCHEMA
            )),
            (!pinned.workflow.is_empty(), format!(
                "- `{}` holds the files of the workflow as the run had them.\n",
                pinned::WORKFLOW_FOLDER
            )),
        ]
        .into_iter()
        .filter_map(|(shown, line)| shown.then_some(line))
        .collect::<String>(),
        match options.describe_data {
            true => "- The files of the run are not in this folder: the metadata gives each its \
                     name, size, SHA-256 digest and where the run left it.\n",
            false => "- `data/` holds each file of the run once, named by its SHA-256 digest; the \
                      metadata gives each file its name in the run.\n",
        },
        run.state,
        ids::time(run.started),
    ));
    text
}

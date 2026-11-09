//! `uwe lower` prints the wir of a CWL, WDL or Argo document, a Snakemake workflow or a
//! Nextflow pipeline, and compiles the workflow of every run into wir.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::{DriverError, Language};

use wir_runtime::SecretInputs;

use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, cache_directory, failure};

/// `uwe lower`: prints the wir document of a CWL, WDL or Argo file, with source locations from the
/// current folder, and names on standard error the ops that stay native ops.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let (target, language) = crate::target_and_language(arguments)?;
    let base = std::env::current_dir().map_err(|error| failure(format!("uwe: {error}")))?;
    let document = match language {
        Language::Wdl => wdl(&target, arguments, &Setup::load(arguments)?, &base)?.document,
        Language::Nextflow => {
            let setup = Setup::load(arguments)?;
            let _registry = setup.registry.enter().map_err(Failure::setup)?;
            nextflow(arguments, &setup, &target, &base, ("lower", None))?
        }
        Language::Snakemake => {
            let setup = Setup::load(arguments)?;
            let _registry = setup.registry.enter().map_err(Failure::setup)?;
            snakemake(&target, arguments, &setup, "lower", &base)?.document
        }
        Language::Argo => argo(&target, arguments, &base)?.document,
        _ => {
            lowered(
                &target,
                language,
                (arguments, &Setup::load(arguments)?),
                &base,
            )?
            .document
        }
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    println!("{text}");
    Ok(())
}

/// The wir document of the run `run` by its digest, or of the run a resume continues, with
/// what the run needs besides it. Old documents go, see [`pruned`].
pub fn document(
    target: &Path,
    language: Language,
    arguments: &Arguments,
    setup: &Setup,
    run: &str,
) -> Result<Compiled, Failure> {
    let folder = cache_directory().join("scratch").join("lowered");
    let secrets = secret_texts(&arguments.secrets);
    let mut private = Vec::new();
    let mut secret = SecretInputs::default();
    let (document, inputs) = match language {
        Language::Wdl => {
            let lowering = wdl(target, arguments, setup, &folder)?;
            let inputs = match converted(&lowering, arguments)? {
                // Values that hold a secret reach the driver in memory.
                Some(serde_json::Value::Object(values))
                    if holds_secret(&serde_json::to_vec(&values).unwrap_or_default(), &secrets) =>
                {
                    secret = SecretInputs(values);
                    None
                }
                Some(values) => {
                    let file = written(&folder, &values.to_string())?;
                    let path = file.to_path_buf();
                    private.push(file);
                    Some(path)
                }
                None => None,
            };
            (lowering.document, inputs)
        }
        Language::Snakemake => {
            // A config file of the inputs, like the one of an embedded run, is a value too.
            let inputs = match &arguments.inputs {
                Some(file) => {
                    let text = std::fs::read_to_string(file).map_err(|error| {
                        failure(format!("uwe: cannot read {}: {error}", file.display()))
                    })?;
                    // A file that holds a secret is read where it is, without a copy.
                    if holds_secret(text.as_bytes(), &secrets) {
                        Some(std::path::absolute(file).unwrap_or_else(|_| file.clone()))
                    } else {
                        let kept = written(&folder, &text)?;
                        let path = kept.to_path_buf();
                        private.push(kept);
                        Some(path)
                    }
                }
                None => None,
            };
            let arguments = &Arguments {
                inputs,
                ..arguments.clone()
            };
            // A resume plans in the launch folder of the run it continues.
            let earlier = match continued(arguments, target, language)? {
                Some((earlier, _)) => Some(first_launch(arguments, earlier.id)?),
                None => None,
            };
            let started = earlier.as_deref().unwrap_or(run);
            let mut document = snakemake(target, arguments, setup, started, &folder)?.document;
            let place = |text: &str| {
                if holds_secret(text.as_bytes(), &secrets) {
                    return Err(
                        "uwe: a config value holds a secret value, which no file may hold".into(),
                    );
                }
                let file = written(&folder, text).map_err(|error| error.message)?;
                let location = file.to_string_lossy().into_owned();
                private.push(file);
                Ok(location)
            };
            snakemake_compiler::place_values(&mut document, place).map_err(failure)?;
            (document, None)
        }
        Language::Nextflow => {
            let mut document = nextflow(arguments, setup, target, &folder, (run, None))?;
            // A compiled pipeline holds the values of its run; one native op holds none.
            if holds_secret(shown(&document)?.as_bytes(), &secrets) {
                let reason = Some("the values of the run hold a secret");
                document = nextflow(arguments, setup, target, &folder, (run, reason))?;
                secret = withheld(&mut document, &secrets);
            }
            (document, None)
        }
        Language::Argo => (argo(target, arguments, &folder)?.document, None),
        _ => (
            lowered(target, language, (arguments, setup), &folder)?.document,
            None,
        ),
    };
    let text = shown(&document)?;
    if holds_secret(text.as_bytes(), &secrets) {
        return Err(failure(
            "uwe: the wir document of the workflow would hold a secret value of the run",
        ));
    }
    let digest = task_model::Digest::sha256(text.as_bytes());
    let path = PathBuf::from(format!(
        "{}{}.wir.json",
        own(target, language),
        digest.hex()
    ));
    if let Some(recorded) = recorded(arguments, &folder)? {
        pruned(&folder, &recorded);
    }
    // A resume runs the document of the run it continues, so it writes no other.
    let (mut ran, resumes) = resumed(arguments, target, language, path.clone())?;
    let launch = match (language, resumes.clone()) {
        (Language::Snakemake, Some(earlier)) => Some(first_launch(arguments, earlier)?),
        (_, earlier) => earlier,
    };
    if ran != path && language == Language::Nextflow {
        ran = reprinted(&ran, &document, &own(target, language))?;
    }
    let compiled = Compiled {
        path: ran,
        inputs,
        private,
        secret,
        resumes,
        launch,
    };
    if compiled.path != path {
        return Ok(compiled);
    }
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|error| failure(format!("uwe: cannot write {}: {error}", path.display())))?;
    Ok(compiled)
}

/// The document `earlier` with the prints of the build of `compiled`, like those that show
/// `workflow.resume`: the emit ops that feed only `view` ops; a new file when they differ.
fn reprinted(earlier: &Path, compiled: &wir::Document, start: &str) -> Result<PathBuf, Failure> {
    let read = std::fs::read_to_string(earlier)
        .map_err(|error| failure(format!("uwe: cannot read {}: {error}", earlier.display())))?;
    let mut document = wir::parse(&read).map_err(|error| failure(format!("uwe: {error}")))?;
    for graph in &mut document.graphs {
        let Some(fresh) = compiled
            .graphs
            .iter()
            .find(|fresh| fresh.name == graph.name)
        else {
            continue;
        };
        let views: Vec<String> = (graph.ops.iter())
            .filter(|op| matches!(op.kind, wir::Kind::View(_)))
            .map(|op| op.path.clone())
            .collect();
        let printed = |path: &str| {
            let read = graph
                .edges
                .iter()
                .filter(|edge| edge.from.op.as_deref() == Some(path));
            let mut readers = read.map(|edge| edge.to.op.as_deref().unwrap_or_default());
            readers.all(|reader| views.iter().any(|view| view == reader))
        };
        let replaced: Vec<wir::Op> = (graph.ops.iter())
            .filter(|op| matches!(op.kind, wir::Kind::Emit(_)) && printed(&op.path))
            .filter_map(|op| fresh.ops.iter().find(|new| new.path == op.path))
            .filter(|new| matches!(new.kind, wir::Kind::Emit(_)))
            .cloned()
            .collect();
        for new in replaced {
            if let Some(op) = graph.ops.iter_mut().find(|op| op.path == new.path) {
                *op = new;
            }
        }
    }
    let text = shown(&document)?;
    if text == read {
        return Ok(earlier.to_owned());
    }
    let digest = task_model::Digest::sha256(text.as_bytes());
    let path = PathBuf::from(format!("{start}{}.wir.json", digest.hex()));
    std::fs::write(&path, text)
        .map_err(|error| failure(format!("uwe: cannot write {}: {error}", path.display())))?;
    Ok(path)
}

/// What a run runs of the workflow that it compiled.
pub struct Compiled {
    /// The wir document.
    pub path: PathBuf,
    /// The WDL inputs file of the entry graph.
    pub inputs: Option<PathBuf>,
    /// The files of the values, removed when the run ends.
    pub private: Vec<Values>,
    /// The values that hold a secret, which no file holds.
    pub secret: SecretInputs,
    /// The run that a resume continues.
    pub resumes: Option<String>,
    /// The run whose launch folder a resume starts in, as it runs the document of that run.
    pub launch: Option<String>,
}

/// The text of a wir document as uwe writes it.
fn shown(document: &wir::Document) -> Result<String, Failure> {
    serde_json::to_string_pretty(document).map_err(|error| failure(format!("uwe: {error}")))
}

/// The secret values that the host gave the run, also as JSON text writes them.
pub(crate) fn secret_texts(secrets: &config_file::SecretValues) -> Vec<String> {
    let mut texts = Vec::new();
    for value in secrets.names().filter_map(|name| secrets.get(name)) {
        let quoted = serde_json::Value::String(value.clone()).to_string();
        texts.push(quoted[1..quoted.len() - 1].to_owned());
        texts.push(value);
    }
    texts.dedup();
    texts
}

/// Whether `bytes` hold one of the texts of [`secret_texts`].
pub(crate) fn holds_secret(bytes: &[u8], secrets: &[String]) -> bool {
    (secrets.iter().filter(|secret| !secret.is_empty()))
        .any(|secret| (bytes.windows(secret.len())).any(|part| part == secret.as_bytes()))
}

/// Takes the defaults of the entry graph inputs that hold a secret out of `document`, as values
/// that reach the driver in memory.
fn withheld(document: &mut wir::Document, secrets: &[String]) -> SecretInputs {
    let mut held = SecretInputs::default();
    let entry = document.entry.clone();
    let graphs = document
        .graphs
        .iter_mut()
        .filter(|graph| graph.name == entry);
    for port in graphs.flat_map(|graph| graph.inputs.iter_mut()) {
        let text = port
            .default
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        if holds_secret(text.as_bytes(), secrets)
            && let Some(value) = port.default.take()
        {
            held.0.insert(port.name.clone(), value);
        }
    }
    held
}

/// The start of the paths of the compiled documents of the workflow `target`, by a digest of
/// its path and language.
fn own(target: &Path, language: Language) -> String {
    let target = std::path::absolute(target).unwrap_or_else(|_| target.to_owned());
    let identity = format!("{language:?} {}", target.display());
    let digest = task_model::Digest::sha256(identity.as_bytes());
    let digest = digest.hex();
    let folder = cache_directory().join("scratch").join("lowered");
    format!("{}/{}-", folder.display(), &digest[..16])
}

/// The document of the run that a resume continues, and its id, as compiling again may give
/// another, like a config that reads the time. A compile with the same values makes its files.
/// The earlier compiled run of the workflow that a resume continues, with its document.
fn continued(
    arguments: &Arguments,
    target: &Path,
    language: Language,
) -> Result<Option<(run_database::Run, String)>, Failure> {
    let Some(resume) = &arguments.resume else {
        return Ok(None);
    };
    let database = crate::runs::open(arguments)?;
    let broken = |error: run_database::DatabaseError| failure(format!("uwe: {error}"));
    let prefix = own(target, language);
    let real = arguments.executor.as_deref() != Some("simulator");
    let earlier = match resume {
        crate::arguments::Resume::Run(id) => database.run(id).map_err(broken)?,
        crate::arguments::Resume::Last => database.last_run_below(&prefix, real).map_err(broken)?,
    };
    let ran = (earlier.as_ref()).and_then(|earlier| earlier.document.clone());
    Ok(earlier.zip(ran.filter(|ran| ran.starts_with(&prefix))))
}

/// The run whose launch folder a resume of the compiled Snakemake run `earlier` starts in: a
/// resumed run started in the folder of the run it continued, back to the first.
fn first_launch(arguments: &Arguments, earlier: String) -> Result<String, Failure> {
    let database = crate::runs::open(arguments)?;
    let mut seen = std::collections::HashSet::new();
    let mut first = earlier;
    while seen.insert(first.clone()) {
        let run = database.run(&first);
        let run = run.map_err(|error| failure(format!("uwe: {error}")))?;
        match run.and_then(|run| run.resumes) {
            Some(resumes) => first = resumes,
            None => break,
        }
    }
    Ok(first)
}

fn resumed(
    arguments: &Arguments,
    target: &Path,
    language: Language,
    compiled: PathBuf,
) -> Result<(PathBuf, Option<String>), Failure> {
    // Without an earlier compiled run of this workflow, the run checks the resume itself.
    let Some((earlier, ran)) = continued(arguments, target, language)? else {
        return Ok((compiled, None));
    };
    let ran = PathBuf::from(ran);
    let text = std::fs::read_to_string(&ran).map_err(|error| {
        failure(format!(
            "uwe: run {} ran {}, which is gone ({error}); run the workflow without -resume",
            earlier.id,
            ran.display()
        ))
    })?;
    let document = wir::parse(&text).map_err(|error| failure(format!("uwe: {error}")))?;
    let values = cache_directory()
        .join("scratch")
        .join("lowered")
        .join("values");
    let locations: Vec<PathBuf> = (document.sources.iter())
        .map(|source| PathBuf::from(&source.location))
        .collect();
    let missing =
        (locations.iter()).find(|location| location.starts_with(&values) && !location.is_file());
    if missing.is_some() {
        return Err(failure(format!(
            "uwe: run {} ran with other values than this run gives; resume it with the same \
             values, or run without -resume",
            earlier.id
        )));
    }
    // A Snakefile compiles into the same jobs again, in the launch folder of that run, with
    // the options that pick them, like `-forcerun`, which its document does not have.
    if language == Language::Snakemake {
        return Ok((compiled, Some(earlier.id)));
    }
    // A config file that held a secret was read where it was, and is gone with its run.
    if (locations.iter()).any(|location| location.is_absolute() && !location.exists()) {
        return Ok((compiled, None));
    }
    Ok((ran, Some(earlier.id)))
}

/// Whether two compiled documents are of the same workflow, by the start of their names.
pub(crate) fn same_workflow(one: &str, other: &str) -> bool {
    let folder = cache_directory().join("scratch").join("lowered");
    let start = |path: &str| {
        let name = Path::new(path).strip_prefix(&folder).ok()?.to_str()?;
        name.split_once('-').map(|(start, _)| start.to_owned())
    };
    start(one).is_some_and(|first| start(other) == Some(first))
}

/// The file of the values of a run, which only the user reads. Runs with the same values
/// share it: each holds a shared lock on it while it runs, and the last to end removes it.
pub struct Values {
    path: PathBuf,
    file: std::fs::File,
}

impl std::ops::Deref for Values {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Values {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for Values {
    /// Removes the file when no other run holds it; a run that crashed holds nothing, and
    /// [`pruned`] removes what it left.
    fn drop(&mut self) {
        if self.file.try_lock().is_ok() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The file of the values `text` below `folder`, named by their digest so that compiling
/// again names it again, held by this run until it is dropped.
fn written(folder: &Path, text: &str) -> Result<Values, Failure> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    let values = folder.join("values");
    let shown = |error: std::io::Error| {
        failure(format!(
            "uwe: cannot write below {}: {error}",
            values.display()
        ))
    };
    (std::fs::DirBuilder::new().recursive(true).mode(0o700))
        .create(&values)
        .map_err(shown)?;
    std::fs::set_permissions(&values, std::fs::Permissions::from_mode(0o700)).map_err(shown)?;
    let digest = task_model::Digest::sha256(text.as_bytes());
    let path = values.join(format!("{}.json", digest.hex()));
    loop {
        // Not truncated: a run that holds the file reads the same bytes meanwhile.
        let mut file = (std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false))
        .mode(0o600)
        .open(&path)
        .map_err(shown)?;
        file.lock_shared().map_err(shown)?;
        // The last run of these values may have removed the file before the lock was taken.
        let held = file.metadata().map_err(shown)?;
        let named = std::fs::metadata(&path);
        if !named.is_ok_and(|named| (named.dev(), named.ino()) == (held.dev(), held.ino())) {
            continue;
        }
        std::io::Write::write_all(&mut file, text.as_bytes()).map_err(shown)?;
        file.set_len(text.len() as u64).map_err(shown)?;
        return Ok(Values { path, file });
    }
}

/// The file in the folder of compiled documents that names each run database whose runs
/// compile into it, one path per line, and as `kept <name>` the documents there before it.
const DATABASES: &str = "databases";

/// The documents that the runs of every database that compiles into `folder` ran, and those
/// there before the list of databases, after the list names the database of this run; none
/// when a database there cannot be read.
fn recorded(arguments: &Arguments, folder: &Path) -> Result<Option<Vec<String>>, Failure> {
    let broken = |error: run_database::DatabaseError| failure(format!("uwe: {error}"));
    let own = crate::runs::database_path(arguments);
    let own = std::path::absolute(&own).unwrap_or(own);
    let list = folder.join(DATABASES);
    let mut text = std::fs::read_to_string(&list).unwrap_or_default();
    if !text.lines().any(|line| Path::new(line) == own) {
        let line = format!("{}\n", own.display());
        let added = std::fs::create_dir_all(folder).and_then(|()| {
            let options = std::fs::File::options()
                .write(true)
                .create_new(true)
                .open(&list);
            let (mut file, added) = match options {
                // A run database that the list does not name may hold the documents there.
                Ok(file) => (file, before(folder)? + &line),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    (std::fs::File::options().append(true).open(&list)?, line)
                }
                Err(error) => return Err(error),
            };
            std::io::Write::write_all(&mut file, added.as_bytes())?;
            std::fs::read_to_string(&list)
        });
        text = added.map_err(|error| failure(format!("uwe: {}: {error}", list.display())))?;
    }
    let mut documents = crate::runs::open(arguments)?.documents().map_err(broken)?;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("kept ") {
            documents.push(folder.join(name).to_string_lossy().into_owned());
            continue;
        }
        let other = Path::new(line);
        // A database that is gone holds no runs to resume.
        if other == own || !other.exists() {
            continue;
        }
        let Ok(database) = run_database::Database::open(other) else {
            return Ok(None);
        };
        documents.extend(database.documents().map_err(broken)?);
    }
    Ok(Some(documents))
}

/// The lines `kept <name>` of the documents in `folder`.
fn before(folder: &Path) -> std::io::Result<String> {
    let mut kept = String::new();
    for entry in std::fs::read_dir(folder)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".wir.json") {
            kept.push_str(&format!("kept {name}\n"));
        }
    }
    Ok(kept)
}

/// Removes the documents below `folder` that no run wrote for a week and no `recorded` run
/// ran, and value files left by runs that ended without removing them.
fn pruned(folder: &Path, recorded: &[String]) {
    const WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
    for place in [folder.to_owned(), folder.join("values")] {
        let Ok(entries) = std::fs::read_dir(&place) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let old = (entry.metadata().and_then(|metadata| metadata.modified()))
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > WEEK);
            // A value file that a live run holds stays.
            let free = || std::fs::File::open(&path).is_ok_and(|file| file.try_lock().is_ok());
            // A document that a run ran stays for its resumes.
            let named = recorded.iter().any(|document| Path::new(document) == path);
            let listed = path == folder.join(DATABASES);
            if old && !named && !listed && path.is_file() && free() {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

fn lowered(
    target: &Path,
    language: Language,
    (arguments, setup): (&Arguments, &Setup),
    base: &Path,
) -> Result<cwl_lowering::Lowering, Failure> {
    if language != Language::Cwl {
        return Err(Failure::usage(
            "uwe: only CWL, WDL and Argo documents, Snakemake workflows and Nextflow pipelines compile \
             into wir yet",
        ));
    }
    let shown = target.display();
    // Documents at web addresses come through the storage providers, as a run reads them.
    let downloads = cache_directory().join("downloads");
    let remote = cwl_parser::source::Remote::new(&setup.storage, downloads);
    let document = cwl_parser::load_file(target, remote)
        .map_err(|error| failure(format!("uwe: {shown}: {error}")))?;
    let image = (arguments.default_image.as_deref()).unwrap_or(cwl_lowering::DEFAULT_IMAGE);
    let lowering = cwl_lowering::lower_with(&document, base, image)
        .map_err(|error| failure(format!("uwe: {shown}: {error}")))?;
    for kept in &lowering.kept {
        eprintln!(
            "uwe: op {} stays a native CWL op: {}",
            kept.path, kept.reason
        );
    }
    Ok(lowering)
}

/// The wir document of a Nextflow pipeline as the command line launches it, for its params and
/// config, or as one native op for the reason `native`; it names on standard error what keeps
/// it one native op.
fn nextflow(
    arguments: &Arguments,
    setup: &Setup,
    target: &Path,
    base: &Path,
    (run, native): (&str, Option<&str>),
) -> Result<wir::Document, Failure> {
    let launch = crate::launch(arguments, target.to_owned());
    // A feature that uwe does not run yet keeps its exit code, as in a run of the driver.
    let compiled = (crate::nextflow::compile(&launch, arguments, setup, base, (run, native)))
        .map_err(|error| DriverError {
            message: format!("uwe: {}: {}", target.display(), error.message),
            ..error
        })?;
    for kept in &compiled.kept {
        eprintln!(
            "uwe: {} keeps the pipeline a native Nextflow op: {}",
            kept.path, kept.reason
        );
    }
    Ok(compiled.document)
}

/// The wir document that a Snakemake workflow compiles into, planned for the targets, config
/// and profiles of the command line as a run plans it; it names on standard error what keeps
/// the workflow one native op.
fn snakemake(
    target: &Path,
    arguments: &Arguments,
    setup: &Setup,
    run: &str,
    base: &Path,
) -> Result<snakemake_compiler::Compiled, Failure> {
    let launch = crate::launch(arguments, target.to_owned());
    let cancel = workflow_graph::CancellationToken::default();
    let settings = crate::run::driver::snakemake(arguments, setup, &cancel);
    // The run plans its jobs in the folder that it starts in, as the run `run` does.
    let run = crate::run::driver::snakemake_options(arguments, setup, run);
    if let Some(folder) = &run.workdir {
        std::fs::create_dir_all(folder)
            .map_err(|error| failure(format!("uwe: cannot make {}: {error}", folder.display())))?;
    }
    let compiled = snakemake_compiler::compile(&launch, settings, run, base)
        .map_err(|error| failure(format!("uwe: {}: {error}", target.display())))?;
    for kept in &compiled.kept {
        eprintln!(
            "uwe: op {} stays a native Snakemake op: {}",
            kept.path, kept.reason
        );
    }
    Ok(compiled)
}

/// The lowering of the WDL workflow or task that `-entry` names, else the one a run picks.
fn wdl(
    target: &Path,
    arguments: &Arguments,
    setup: &Setup,
    base: &Path,
) -> Result<wdl_lowering::Lowering, Failure> {
    let entry = arguments.targets.first().map(String::as_str);
    let options = crate::run::driver::wdl_options(arguments, setup);
    let lowering = wdl_lowering::lower_with(target, entry, base, &options)
        .map_err(|error| failure(format!("uwe: {}: {error}", target.display())))?;
    for kept in &lowering.kept {
        eprintln!(
            "uwe: op {} stays a native WDL op: {}",
            kept.path, kept.reason
        );
    }
    Ok(lowering)
}

/// The wir document that an Argo workflow compiles into, with the template files of `-c`,
/// from the template that `-entry` names, else its entrypoint.
fn argo(
    target: &Path,
    arguments: &Arguments,
    base: &Path,
) -> Result<argo_compiler::Compiled, Failure> {
    let entry = arguments.targets.first().map(String::as_str);
    let compiled = argo_compiler::compile_with(target, &arguments.configs, entry, base)
        .map_err(|error| failure(format!("uwe: {}: {error}", target.display())))?;
    for kept in &compiled.kept {
        eprintln!(
            "uwe: op {} stays a native Argo op: {}",
            kept.path, kept.reason
        );
    }
    Ok(compiled)
}

/// The values of the entry graph from the WDL input file and the parameters of a run.
fn converted(
    lowering: &wdl_lowering::Lowering,
    arguments: &Arguments,
) -> Result<Option<serde_json::Value>, Failure> {
    let mut values = match &arguments.inputs {
        None if arguments.params.is_empty() => return Ok(None),
        None => serde_json::Value::Object(Default::default()),
        Some(file) => {
            let shown = file.display();
            let text = std::fs::read_to_string(file)
                .map_err(|error| failure(format!("uwe: cannot read {shown}: {error}")))?;
            let json = serde_json::from_str(&text)
                .map_err(|error| failure(format!("uwe: {shown}: {error}")))?;
            let absolute = std::path::absolute(file).unwrap_or(file.clone());
            (lowering.inputs(&json, absolute.parent().unwrap_or(Path::new("/"))))
                .map_err(|error| failure(format!("uwe: {shown}: {error}")))?
        }
    };
    let base = std::env::current_dir().map_err(|error| failure(format!("uwe: {error}")))?;
    let given = (lowering.parameters(&arguments.params, &base))
        .map_err(|error| failure(format!("uwe: {error}")))?;
    if let (serde_json::Value::Object(values), serde_json::Value::Object(given)) =
        (&mut values, given)
    {
        values.extend(given);
    }
    Ok(Some(values))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two live runs with the same values share one file, which the first to end leaves for
    /// the other, and the last removes.
    #[test]
    fn shares_value_files() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let first = written(folder.path(), "{\"word\": \"same\"}").expect("writes the values");
        let second = written(folder.path(), "{\"word\": \"same\"}").expect("writes them again");
        let path = first.to_path_buf();
        assert_eq!(path, second.to_path_buf());
        drop(first);
        let text = std::fs::read_to_string(&path).expect("the second run still reads them");
        assert_eq!(text, "{\"word\": \"same\"}");
        drop(second);
        assert!(!path.exists(), "the last run removes them");
    }
}

//! `uwe lower` prints the wir of a CWL, WDL or Argo document, a Snakemake workflow or a
//! Nextflow pipeline, and `-lowered` runs one through it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::Language;

use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, cache_directory, failure};

/// `uwe lower`: prints the wir document of a CWL, WDL or Argo file, with source locations from the
/// current folder, and names on standard error the ops that stay native ops.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let (target, language) = crate::target_and_language(arguments)?;
    let base = std::env::current_dir().map_err(|error| failure(format!("uwe: {error}")))?;
    let document = match language {
        Language::Wdl => wdl(&target, arguments, &base)?.document,
        Language::Nextflow => {
            let setup = Setup::load(arguments)?;
            let _registry = setup.registry.enter().map_err(Failure::setup)?;
            nextflow(arguments, &setup, &target, &base)?
        }
        Language::Snakemake => snakemake(&target, arguments, &base)?.document,
        Language::Argo => argo(&target, arguments, &base)?.document,
        _ => lowered(&target, language, &base)?.document,
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    println!("{text}");
    Ok(())
}

/// The wir document of a `-lowered` run below the scratch folder by its digest, the same when
/// compiled again; the WDL inputs file of its entry graph; and the files of its values, see
/// [`written`]. Documents that no run wrote for a week go.
pub fn document(
    target: &Path,
    language: Language,
    arguments: &Arguments,
    setup: &Setup,
) -> Result<(PathBuf, Option<PathBuf>, Vec<tempfile::TempPath>), Failure> {
    let folder = cache_directory().join("scratch").join("lowered");
    let mut private = Vec::new();
    let (document, inputs) = match language {
        Language::Wdl => {
            let lowering = wdl(target, arguments, &folder)?;
            let inputs = converted(&lowering, arguments, &folder)?;
            let path = inputs.as_ref().map(|inputs| inputs.to_path_buf());
            private.extend(inputs);
            (lowering.document, path)
        }
        Language::Snakemake => {
            // A config file of the inputs, like the one of an embedded run, is a value too.
            let inputs = match &arguments.inputs {
                Some(file) => {
                    let text = std::fs::read_to_string(file).map_err(|error| {
                        failure(format!("uwe: cannot read {}: {error}", file.display()))
                    })?;
                    let kept = written(&folder, &text)?;
                    let path = kept.to_path_buf();
                    private.push(kept);
                    Some(path)
                }
                None => None,
            };
            let arguments = &Arguments {
                inputs,
                ..arguments.clone()
            };
            let mut document = snakemake(target, arguments, &folder)?.document;
            let place = |text: &str| {
                let file = written(&folder, text).map_err(|error| error.message)?;
                let location = file.to_string_lossy().into_owned();
                private.push(file);
                Ok(location)
            };
            snakemake_compiler::place_values(&mut document, place).map_err(failure)?;
            (document, None)
        }
        Language::Nextflow => (nextflow(arguments, setup, target, &folder)?, None),
        Language::Argo => (argo(target, arguments, &folder)?.document, None),
        _ => (lowered(target, language, &folder)?.document, None),
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    let digest = task_model::Digest::sha256(text.as_bytes());
    let path = folder.join(format!("{}.wir.json", digest.hex()));
    pruned(&folder);
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|error| failure(format!("uwe: cannot write {}: {error}", path.display())))?;
    Ok((path, inputs, private))
}

/// The file of the values `text` below `folder`, named by their digest so that compiling
/// again names it again; only the user reads it, and it goes when it is dropped.
fn written(folder: &Path, text: &str) -> Result<tempfile::TempPath, Failure> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
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
    let mut file = (std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true))
    .mode(0o600)
    .open(&path)
    .map_err(shown)?;
    std::io::Write::write_all(&mut file, text.as_bytes()).map_err(shown)?;
    tempfile::TempPath::try_from_path(path).map_err(shown)
}

/// Removes the documents below `folder` that no run wrote for a week, and value files left
/// by runs that ended without removing them; a resume compiles a document again.
fn pruned(folder: &Path) {
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
            if old && path.is_file() {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

fn lowered(
    target: &Path,
    language: Language,
    base: &Path,
) -> Result<cwl_lowering::Lowering, Failure> {
    if language != Language::Cwl {
        return Err(Failure::usage(
            "uwe: only CWL, WDL and Argo documents, Snakemake workflows and Nextflow pipelines compile \
             into wir yet",
        ));
    }
    let shown = target.display();
    let document = cwl_parser::load_file(target, Default::default())
        .map_err(|error| failure(format!("uwe: {shown}: {error}")))?;
    let lowering = cwl_lowering::lower(&document, base)
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
/// config; it names on standard error what keeps it one native op.
fn nextflow(
    arguments: &Arguments,
    setup: &Setup,
    target: &Path,
    base: &Path,
) -> Result<wir::Document, Failure> {
    let launch = crate::launch(arguments, target.to_owned());
    let compiled = crate::nextflow::compile(&launch, arguments, setup, base)
        .map_err(|error| failure(format!("uwe: {}: {error}", target.display())))?;
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
    base: &Path,
) -> Result<snakemake_compiler::Compiled, Failure> {
    let setup = Setup::load(arguments)?;
    let _registry = setup.registry.enter().map_err(Failure::setup)?;
    let launch = crate::launch(arguments, target.to_owned());
    let cancel = workflow_graph::CancellationToken::default();
    let settings = crate::run::driver::snakemake(arguments, &setup, &cancel);
    let run = crate::run::driver::snakemake_options(arguments, &setup, "lower");
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
    base: &Path,
) -> Result<wdl_lowering::Lowering, Failure> {
    let entry = arguments.targets.first().map(String::as_str);
    let image = (arguments.default_image.as_deref()).unwrap_or(wdl_lowering::DEFAULT_IMAGE);
    let lowering = wdl_lowering::lower_with(target, entry, base, image)
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

/// The inputs file of the entry graph from the WDL input file and the parameters of a run,
/// below `folder`, which only the user reads.
fn converted(
    lowering: &wdl_lowering::Lowering,
    arguments: &Arguments,
    folder: &Path,
) -> Result<Option<tempfile::TempPath>, Failure> {
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
    written(folder, &values.to_string()).map(Some)
}

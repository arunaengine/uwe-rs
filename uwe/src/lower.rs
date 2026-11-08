//! `uwe lower` prints the wir of a CWL or WDL document, and `-lowered` runs one through it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::Language;

use crate::arguments::Arguments;
use crate::{Failure, cache_directory, failure};

/// `uwe lower`: prints the wir document of a CWL or WDL file, with source locations from the
/// current folder, and names on standard error the ops that stay native ops.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let (target, language) = crate::target_and_language(arguments)?;
    let base = std::env::current_dir().map_err(|error| failure(format!("uwe: {error}")))?;
    let document = match language {
        Language::Wdl => wdl(&target, arguments, &base)?.document,
        _ => lowered(&target, language, &base)?.document,
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    println!("{text}");
    Ok(())
}

/// The wir document that a run with `-lowered` runs instead of a CWL or WDL file, and for WDL
/// the inputs file of its entry graph. They are written below the scratch folder, named by
/// their digest, and the sources keep absolute locations.
pub fn document(
    target: &Path,
    language: Language,
    arguments: &Arguments,
) -> Result<(PathBuf, Option<PathBuf>), Failure> {
    let folder = cache_directory().join("scratch").join("lowered");
    let (document, inputs) = match language {
        Language::Wdl => {
            let lowering = wdl(target, arguments, &folder)?;
            let inputs = converted(&lowering, arguments, &folder)?;
            (lowering.document, inputs)
        }
        _ => (lowered(target, language, &folder)?.document, None),
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    let digest = task_model::Digest::sha256(text.as_bytes());
    let path = folder.join(format!("{}.wir.json", digest.hex()));
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|error| failure(format!("uwe: cannot write {}: {error}", path.display())))?;
    Ok((path, inputs))
}

fn lowered(
    target: &Path,
    language: Language,
    base: &Path,
) -> Result<cwl_lowering::Lowering, Failure> {
    if language != Language::Cwl {
        return Err(Failure::usage(
            "uwe: only CWL and WDL documents lower into wir yet",
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

/// The inputs file of the entry graph from the WDL input file of a run, below `folder`.
fn converted(
    lowering: &wdl_lowering::Lowering,
    arguments: &Arguments,
    folder: &Path,
) -> Result<Option<PathBuf>, Failure> {
    let Some(file) = &arguments.inputs else {
        return Ok(None);
    };
    let shown = file.display();
    let text = std::fs::read_to_string(file)
        .map_err(|error| failure(format!("uwe: cannot read {shown}: {error}")))?;
    let json =
        serde_json::from_str(&text).map_err(|error| failure(format!("uwe: {shown}: {error}")))?;
    let absolute = std::path::absolute(file).unwrap_or(file.clone());
    let values = (lowering.inputs(&json, absolute.parent().unwrap_or(Path::new("/"))))
        .map_err(|error| failure(format!("uwe: {shown}: {error}")))?;
    let text = values.to_string();
    let digest = task_model::Digest::sha256(text.as_bytes());
    let path = folder.join(format!("{}.inputs.json", digest.hex()));
    std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|error| failure(format!("uwe: cannot write {}: {error}", path.display())))?;
    Ok(Some(path))
}

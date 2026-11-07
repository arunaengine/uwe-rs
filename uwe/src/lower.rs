//! `uwe lower` prints the wir of a CWL document, and `-lowered` runs a CWL document through it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::Language;

use crate::arguments::Arguments;
use crate::{Failure, cache_directory, failure};

/// `uwe lower`: prints the wir document of a CWL file, with source locations from the current
/// folder, and names on standard error the ops that stay native CWL ops.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let (target, language) = crate::target_and_language(arguments)?;
    let base = std::env::current_dir().map_err(|error| failure(format!("uwe: {error}")))?;
    let lowering = lowered(&target, language, &base)?;
    let text = serde_json::to_string_pretty(&lowering.document)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    println!("{text}");
    Ok(())
}

/// The wir document that a run with `-lowered` runs instead of a CWL file. It is written below
/// the scratch folder, named by its digest, and its sources keep absolute locations.
pub fn document(target: &Path, language: Language) -> Result<PathBuf, Failure> {
    let folder = cache_directory().join("scratch").join("lowered");
    let lowering = lowered(target, language, &folder)?;
    let text = serde_json::to_string_pretty(&lowering.document)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    let digest = task_model::Digest::sha256(text.as_bytes());
    let path = folder.join(format!("{}.wir.json", digest.hex()));
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|error| failure(format!("uwe: cannot write {}: {error}", path.display())))?;
    Ok(path)
}

fn lowered(
    target: &Path,
    language: Language,
    base: &Path,
) -> Result<cwl_lowering::Lowering, Failure> {
    if language != Language::Cwl {
        return Err(Failure::usage("uwe: only CWL documents lower into wir yet"));
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

//! nf-bactopia's `bactopiaToolInputs`: the samples of a Bactopia results folder.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::{Level, log, param, record};
use crate::Context;
use crate::nf_schema::get;

/// The folders of a Bactopia results folder that are not samples.
const IGNORED: [&str; 6] = [
    ".nextflow",
    "bactopia-info",
    "bactopia-tools",
    "work",
    "bactopia-runs",
    "pipeline_info",
];

/// `bactopiaToolInputs()`: the samples of the Bactopia results folder `params.bactopia` that
/// have the files that `params.workflow.ext` asks for.
pub(super) fn samples(context: &mut Context) -> Result<Value, Error> {
    let Some(given) = param(context, "bactopia") else {
        log(Level::Error, "--bactopia is not set.");
        return Ok(Value::list(Vec::new()));
    };
    let folder = PathBuf::from(context.host.launch_dir()).join(&given);
    let Ok(listing) = std::fs::read_dir(&folder) else {
        log(
            Level::Error,
            format!("The Bactopia directory {given} (--bactopia) does not exist."),
        );
        return Ok(Value::list(Vec::new()));
    };
    let first_column = |name: &str| -> Option<Vec<String>> {
        let path = PathBuf::from(context.host.launch_dir()).join(param(context, name)?);
        let text = std::fs::read_to_string(path).ok()?;
        Some(
            text.lines()
                .map(|line| line.split('\t').next().unwrap_or("").trim().to_owned())
                .collect(),
        )
    };
    let (include, exclude) = (first_column("include"), first_column("exclude"));
    let wanted: Vec<String> = match get(&context.params, "workflow").and_then(|w| get(&w, "ext")) {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        _ => Vec::new(),
    };
    let mut names: Vec<String> = (listing.filter_map(Result::ok))
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !IGNORED.contains(&name.as_str()))
        .collect();
    names.sort();
    let (mut samples, mut excluded, mut skipped) = (Vec::new(), Vec::new(), 0);
    for name in names {
        if include
            .as_ref()
            .is_some_and(|include| !include.contains(&name))
            || exclude
                .as_ref()
                .is_some_and(|exclude| exclude.contains(&name))
        {
            skipped += 1;
            continue;
        }
        let main = folder.join(&name).join("main");
        if !main.join(format!("gather/{name}-meta.tsv")).is_file() {
            log(
                Level::Info,
                format!("{name} does not appear to be a Bactopia sample, skipping..."),
            );
            continue;
        }
        match tool_record(&main, &name, &wanted) {
            Some(record) => samples.push(record),
            None => excluded.push(name),
        }
    }
    if let Some(include) = &include {
        log(
            Level::Info,
            format!("Including {} samples for analysis", include.len()),
        );
    }
    if exclude.is_some() {
        log(
            Level::Info,
            format!("Excluding {skipped} samples from the analysis"),
        );
    }
    if !excluded.is_empty() {
        log(
            Level::Warn,
            format!(
                "{} samples were excluded due to missing files. They are:",
                excluded.len()
            ),
        );
        for name in &excluded {
            log(Level::Warn, format!("    {name}"));
        }
    }
    if samples.is_empty() {
        log(
            Level::Error,
            "No samples were found to process! Please verify the --bactopia path \nis correct and that it contains Bactopia results.",
        );
    } else {
        log(
            Level::Info,
            format!("Found {} samples to process", samples.len()),
        );
        log(
            Level::Info,
            "\nIf this looks wrong, now's your chance to back out (CTRL+C 3 times).",
        );
        log(Level::Info, "Sleeping for 5 seconds...\n");
        log(Level::Info, "-".repeat(68));
    }
    Ok(Value::list(samples))
}

/// The record of one sample of a results folder, `None` when a wanted file is missing.
fn tool_record(main: &Path, name: &str, wanted: &[String]) -> Option<Value> {
    let found = |paths: &[String]| -> Value {
        (paths.iter())
            .map(|path| main.join(path))
            .find(|path| path.is_file())
            .map_or(Value::Null, |path| Value::string(path.to_string_lossy()))
    };
    let annotated = |extension: &str, prokka: &str| -> Value {
        let mut paths = Vec::new();
        for (tool, ext) in [("bakta", extension), ("prokka", prokka)] {
            paths.push(format!("annotator/{tool}/{name}.{ext}.gz"));
            paths.push(format!("annotator/{tool}/{name}.{ext}"));
        }
        found(&paths)
    };
    let qc = |suffix: &str| found(&[format!("qc/{name}_{suffix}.fastq.gz")]);
    let ont = main.join("assembler/supplemental/ont.txt").is_file();
    let (r1, r2, se, lr) = (qc("R1"), qc("R2"), qc("SE"), qc("ONT"));
    let (runtype, single, reads_found) = if ont {
        ("ont", true, !matches!(lr, Value::Null))
    } else if !matches!(se, Value::Null) {
        ("illumina", true, true)
    } else {
        let both = !matches!(r1, Value::Null) && !matches!(r2, Value::Null);
        ("illumina", false, both)
    };
    let fna = found(&[
        format!("assembler/{name}.fna"),
        format!("assembler/{name}.fna.gz"),
    ]);
    let fields = [
        ("faa", annotated("faa", "faa")),
        ("fna_anno", annotated("fna", "fna")),
        ("gff", annotated("gff3", "gff")),
        ("gbk", annotated("gbff", "gbk")),
        (
            "blastdb",
            found(
                &["bakta", "prokka"].map(|tool| format!("annotator/{tool}/{name}-blastdb.tar.gz")),
            ),
        ),
    ];
    for kind in wanted {
        let present = match kind.as_str() {
            "fastq" => reads_found,
            "fna" => !matches!(fna, Value::Null),
            other => fields
                .iter()
                .any(|(key, value)| *key == other && !matches!(value, Value::Null)),
        };
        if !present {
            return None;
        }
    }
    let meta = record(vec![
        ("id", Value::string(name)),
        ("name", Value::string(name)),
        ("runtype", Value::string(runtype)),
        ("single_end", Value::Boolean(single)),
    ]);
    let tsv = found(&[format!("gather/{name}-meta.tsv")]);
    let mut entries = vec![
        ("meta", meta),
        ("tsv_meta", tsv),
        ("r1", r1),
        ("r2", r2),
        ("se", se),
        ("lr", lr),
        ("fna", fna),
    ];
    entries.extend(fields);
    Some(record(entries))
}

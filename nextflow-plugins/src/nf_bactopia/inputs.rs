//! nf-bactopia's sample records: from params, FOFN and accession files, and Bactopia results.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::{Level, log, param, record};
use crate::Context;
use crate::nf_schema::get;

const FOFN_COLUMNS: [&str; 9] = [
    "sample",
    "runtype",
    "genome_size",
    "species",
    "r1",
    "r2",
    "se",
    "ont",
    "assembly",
];
const ACCESSION_COLUMNS: [&str; 4] = ["accession", "runtype", "species", "genome_size"];

/// The files of a sample record, each a list of paths as text.
#[derive(Default)]
struct Reads {
    r1: Vec<String>,
    r2: Vec<String>,
    se: Vec<String>,
    lr: Vec<String>,
    assembly: Vec<String>,
}

impl Reads {
    fn record(self, meta: Value) -> Value {
        let list = |paths: Vec<String>| Value::list(paths.into_iter().map(Value::string).collect());
        record(vec![
            ("meta", meta),
            ("r1", list(self.r1)),
            ("r2", list(self.r2)),
            ("se", list(self.se)),
            ("lr", list(self.lr)),
            ("assembly", list(self.assembly)),
        ])
    }
}

/// `bactopiaInputs(runType)`: the samples of the run, or null for an unknown run type.
pub(super) fn bactopia(runtype: &str, context: &mut Context) -> Result<Value, Error> {
    let one = |value: Option<String>| value.into_iter().collect::<Vec<_>>();
    let sample = param(context, "sample").unwrap_or_default();
    let reads = match runtype {
        "paired-end" => Reads {
            r1: one(param(context, "r1")),
            r2: one(param(context, "r2")),
            ..Reads::default()
        },
        "single-end" => Reads {
            se: one(param(context, "se")),
            ..Reads::default()
        },
        "hybrid" | "short_polish" => Reads {
            r1: one(param(context, "r1")),
            r2: one(param(context, "r2")),
            lr: one(param(context, "ont")),
            ..Reads::default()
        },
        "assembly" => Reads {
            assembly: one(param(context, "assembly")),
            ..Reads::default()
        },
        "ont" => Reads {
            lr: one(param(context, "ont")),
            ..Reads::default()
        },
        "is_fofn" => return fofn(context),
        "is_accessions" => return accessions(context),
        "is_accession" => return Ok(Value::list(vec![accession(context)])),
        other => {
            log(
                Level::Error,
                format!(
                    "Invalid runtype '{other}' provided, please correct to continue. Expected: paired-end, single-end, hybrid, short_polish, assembly, or ont"
                ),
            );
            return Ok(Value::Null);
        }
    };
    let mut meta = vec![
        ("id", Value::string(sample.as_str())),
        ("name", Value::string(sample.as_str())),
        ("runtype", Value::string(runtype)),
        (
            "genome_size",
            get(&context.params, "genome_size").unwrap_or(Value::Null),
        ),
        (
            "species",
            get(&context.params, "species").unwrap_or(Value::Null),
        ),
    ];
    let single = match runtype {
        "single-end" | "short_polish" | "ont" => Some(true),
        "paired-end" | "hybrid" => Some(false),
        _ => None,
    };
    if let Some(single) = single {
        meta.push(("single_end", Value::Boolean(single)));
    }
    Ok(Value::list(vec![reads.record(record(meta))]))
}

/// The rows of a tab-separated file with a header, as column maps; `None` when columns miss.
fn table(path: &Path, required: &[&str], hint: &str) -> Option<Vec<Vec<(String, String)>>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()
        .unwrap_or_default()
        .split('\t')
        .map(|cell| cell.trim().to_owned())
        .collect();
    let missing: Vec<String> = (required.iter())
        .filter(|column| !header.iter().any(|name| name == *column))
        .map(|column| format!("'{column}'"))
        .collect();
    if !missing.is_empty() {
        log(
            Level::Error,
            format!(
                "Missing required column(s) {} in {}. Found columns: {}.\n\n{hint}",
                missing.join(", "),
                path.display(),
                header.join(", ")
            ),
        );
        return None;
    }
    let rows = lines
        .map(|line| {
            let cells = line.split('\t').map(|cell| cell.trim().to_owned());
            header.iter().cloned().zip(cells).collect::<Vec<_>>()
        })
        .filter(|row: &Vec<(String, String)>| row.first().is_some_and(|(_, cell)| !cell.is_empty()))
        .collect();
    Some(rows)
}

fn cell<'a>(row: &'a [(String, String)], name: &str) -> &'a str {
    row.iter()
        .find(|(column, _)| column == name)
        .map_or("", |(_, value)| value)
}

/// A file param relative to the launch directory.
fn local(context: &Context, name: &str) -> PathBuf {
    let launch = PathBuf::from(context.host.launch_dir());
    launch.join(param(context, name).unwrap_or_default())
}

/// The genome size and species of a row: the params' values when they are set.
fn sizes(context: &Context, row: &[(String, String)]) -> (Value, Value) {
    let set = |name: &str| param(context, name).filter(|value| value != "0");
    let genome = set("genome_size").map_or_else(
        || Value::string(cell(row, "genome_size")),
        |_| get(&context.params, "genome_size").unwrap_or(Value::Null),
    );
    let species = set("species").map_or_else(|| Value::string(cell(row, "species")), Value::string);
    (genome, species)
}

fn fofn(context: &mut Context) -> Result<Value, Error> {
    let path = local(context, "samples");
    let hint = "Please use 'bactopia prepare' to generate a properly formatted FOFN file.";
    let Some(rows) = table(&path, &FOFN_COLUMNS, hint) else {
        return Ok(Value::list(Vec::new()));
    };
    let mut samples = Vec::new();
    for row in rows {
        let sample = cell(&row, "sample");
        let runtype = cell(&row, "runtype");
        let split = |name: &str| -> Vec<String> {
            let text = cell(&row, name);
            match text.is_empty() {
                true => Vec::new(),
                false => text.split(',').map(|part| part.trim().to_owned()).collect(),
            }
        };
        let whole = |name: &str| -> Vec<String> {
            let text = cell(&row, name);
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text.to_owned()]
            }
        };
        let reads = match runtype {
            "paired-end" => Reads {
                r1: whole("r1"),
                r2: whole("r2"),
                ..Reads::default()
            },
            "single-end" => Reads {
                se: whole("se"),
                ..Reads::default()
            },
            "hybrid" | "short_polish" => Reads {
                r1: whole("r1"),
                r2: whole("r2"),
                lr: whole("ont"),
                ..Reads::default()
            },
            "ont" => Reads {
                lr: whole("ont"),
                ..Reads::default()
            },
            "assembly" => Reads {
                assembly: whole("assembly"),
                ..Reads::default()
            },
            "merge-pe" => Reads {
                r1: split("r1"),
                r2: split("r2"),
                ..Reads::default()
            },
            "hybrid-merge-pe" | "short_polish-merge-pe" => Reads {
                r1: split("r1"),
                r2: split("r2"),
                lr: whole("ont"),
                ..Reads::default()
            },
            "merge-se" => Reads {
                se: split("se"),
                ..Reads::default()
            },
            other => {
                log(
                    Level::Error,
                    format!(
                        "Invalid runtype {other} found, please correct to continue. Expected: single-end, paired-end, hybrid, short_polish, merge-pe, hybrid-merge-pe, short_polish-merge-pe, merge-se, or assembly"
                    ),
                );
                samples.push(Value::Null);
                continue;
            }
        };
        if sample.is_empty() {
            let shown: Vec<String> = row.iter().map(|(k, v)| format!("{k}:{v}")).collect();
            log(
                Level::Error,
                format!("Sample name cannot be null: [{}]", shown.join(", ")),
            );
            samples.push(Value::Null);
            continue;
        }
        let (genome, species) = sizes(context, &row);
        let mut meta = vec![
            ("id", Value::string(sample)),
            ("name", Value::string(sample)),
            ("runtype", Value::string(runtype)),
            ("genome_size", genome),
            ("species", species),
        ];
        let single = match runtype {
            "assembly" => None,
            "single-end" | "short_polish" | "ont" | "merge-se" | "short_polish-merge-pe" => {
                Some(true)
            }
            _ => Some(false),
        };
        if let Some(single) = single {
            meta.push(("single_end", Value::Boolean(single)));
        }
        samples.push(reads.record(record(meta)));
    }
    Ok(Value::list(samples))
}

/// The record of an accession, or null with an error for one of another kind.
fn accession_record(accession: &str, ont: bool, sizes: (Value, Value), single: bool) -> Value {
    let (runtype, id) = if accession.starts_with("GCF") || accession.starts_with("GCA") {
        let id = accession.split('.').next().unwrap_or(accession);
        ("assembly_accession", id)
    } else if ["DRX", "ERX", "SRX"]
        .iter()
        .any(|prefix| accession.starts_with(prefix))
    {
        (
            if ont {
                "sra_accession_ont"
            } else {
                "sra_accession"
            },
            accession,
        )
    } else {
        log(
            Level::Error,
            format!(
                "Invalid accession: {accession} is not an accepted accession type. Accessions must be Assembly (GCF_*, GCA*) or Experiment (DRX*, ERX*, SRX*) accessions. Please correct to continue.\n\nYou can use 'bactopia search' to convert BioProject, BioSample, or Run accessions into an Experiment accession."
            ),
        );
        return Value::Null;
    };
    let (genome, species) = sizes;
    let names = [
        ("id", Value::string(id)),
        ("name", Value::string(id)),
        ("runtype", Value::string(runtype)),
    ];
    let meta = match single {
        true => [("genome_size", genome), ("species", species)]
            .into_iter()
            .chain(names)
            .collect(),
        false => names
            .into_iter()
            .chain([("genome_size", genome), ("species", species)])
            .collect(),
    };
    Reads::default().record(record(meta))
}

fn accessions(context: &mut Context) -> Result<Value, Error> {
    let path = local(context, "accessions");
    let hint = "Please use 'bactopia search' to generate a properly formatted accessions file.";
    let Some(rows) = table(&path, &ACCESSION_COLUMNS, hint) else {
        return Ok(Value::list(Vec::new()));
    };
    let samples = rows.iter().map(|row| {
        let ont = cell(row, "runtype") == "ont";
        accession_record(cell(row, "accession"), ont, sizes(context, row), false)
    });
    Ok(Value::list(samples.collect()))
}

fn accession(context: &mut Context) -> Value {
    let Some(accession) = param(context, "accession") else {
        log(
            Level::Error,
            "Accession cannot be empty, please provide a valid accession to continue.",
        );
        return Value::Null;
    };
    let value = |name: &str| get(&context.params, name).unwrap_or(Value::Null);
    let sizes = (value("genome_size"), value("species"));
    accession_record(&accession, param(context, "ont").is_some(), sizes, true)
}

//! nf-versions 0.8.0: bash commands that print the version of a tool as a MultiQC YAML line.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: FUNCTIONS,
    call: Some(call),
    script: Some(include_str!("nf_versions.nf")),
    ..Plugin::new("nf-versions", Kind::Functions, State::Implemented)
};

/// The functions of the plugin; `nf_versions.nf` defines `collateVersions`.
const FUNCTIONS: &[&str] = &[
    "bambyVersion",
    "bcftoolsVersion",
    "bedtoolsVersion",
    "bwaVersion",
    "bwaMem2Version",
    "bwaMem3Version",
    "chelaeVersion",
    "dupblasterVersion",
    "ensemblVepVersion",
    "falcoVersion",
    "fastpVersion",
    "fastqcVersion",
    "fastqcRsVersion",
    "fgbioVersion",
    "fgumiVersion",
    "mosdepthVersion",
    "picardVersion",
    "revtagVersion",
    "rikerVersion",
    "sambambaVersion",
    "samtoolsVersion",
    "snpeffVersion",
    "snpsiftVersion",
    "somalierVersion",
    "splitcodeVersion",
    "unmuxVersion",
    "vartovcfVersion",
    "vcfannoVersion",
    "pyPackageVersion",
    "rLibraryVersion",
    "collateVersions",
];

/// Each tool function with the YAML key it prints and the command that gives the version. The
/// text becomes part of the task command and its hash, so it is the plugin's byte for byte.
const TOOLS: &[(&str, &str, &str)] = &[
    (
        "bambyVersion",
        "bamby",
        " bamby --version | sed -e 's/bamby //g' ",
    ),
    (
        "bcftoolsVersion",
        "bcftools",
        " bcftools --version | head -n1 | sed -e 's/bcftools //g' ",
    ),
    (
        "bedtoolsVersion",
        "bedtools",
        " bedtools --version | sed -e 's/bedtools v//g' ",
    ),
    ("bwaVersion", "bwa", " bwa 2>&1 | sed -n 's/^Version: //p' "),
    ("bwaMem2Version", "bwa-mem2", " bwa-mem2 version "),
    (
        "bwaMem3Version",
        "bwa-mem3",
        " bwa-mem3 version | head -n1 ",
    ),
    (
        "chelaeVersion",
        "chelae",
        " chelae --version | sed -e 's/chelae //g' ",
    ),
    (
        "dupblasterVersion",
        "dupblaster",
        " dupblaster --version | sed -e 's/dupblaster //g' ",
    ),
    (
        "ensemblVepVersion",
        "ensembl-vep",
        " vep --help | sed -n 's/^ *ensembl-vep *: *//p' ",
    ),
    (
        "falcoVersion",
        "falco",
        " falco --version | sed -e 's/falco //g' ",
    ),
    (
        "fastpVersion",
        "fastp",
        " fastp --version 2>&1 | sed -e 's/fastp //g' ",
    ),
    (
        "fastqcVersion",
        "fastqc",
        " fastqc --version | sed -e 's/FastQC v//g' ",
    ),
    (
        "fastqcRsVersion",
        "fastqc-rs",
        " fqc --version 2>&1 | sed -e 's/fastqc-rs //g' ",
    ),
    (
        "fgbioVersion",
        "fgbio",
        "\n        fgbio --version 2>&1             | tr -d '[:cntrl:]'             | sed -e 's/^.*Version: //' -e 's/\\[.*$//'\n    ",
    ),
    (
        "fgumiVersion",
        "fgumi",
        " fgumi --version | sed -e 's/fgumi //g' ",
    ),
    (
        "mosdepthVersion",
        "mosdepth",
        " mosdepth --version | sed -e 's/mosdepth //g' ",
    ),
    (
        "picardVersion",
        "picard",
        " picard ViewSam --version true 2>&1 | grep -v 'cannot change locale' | sed -e 's/Version://g' ",
    ),
    (
        "revtagVersion",
        "revtag",
        " revtag --version | sed -e 's/revtag //g' ",
    ),
    (
        "rikerVersion",
        "riker",
        " riker --version | sed -e 's/riker //g' ",
    ),
    (
        "sambambaVersion",
        "sambamba",
        " sambamba --version 2>&1 | sed -n 's/^sambamba //p' | head -n1 ",
    ),
    (
        "samtoolsVersion",
        "samtools",
        " samtools --version | head -n1 | sed -e 's/samtools //g' ",
    ),
    (
        "snpeffVersion",
        "snpeff",
        " snpEff -version | head -n1 | cut -f2 ",
    ),
    (
        "snpsiftVersion",
        "snpsift",
        " SnpSift 2>&1 | sed -n 's/^SnpSift version \\([^ ]*\\).*/\\1/p' ",
    ),
    (
        "somalierVersion",
        "somalier",
        " somalier 2>&1 | sed -n 's/^somalier version: //p' ",
    ),
    (
        "splitcodeVersion",
        "splitcode",
        " splitcode --version | sed -e 's/splitcode, version //g' | sed 's/\\.$//' ",
    ),
    (
        "unmuxVersion",
        "unmux",
        " unmux --version | sed -e 's/unmux //g' ",
    ),
    (
        "vartovcfVersion",
        "vartovcf",
        " vartovcf --version | sed -e 's/vartovcf //g' ",
    ),
    (
        "vcfannoVersion",
        "vcfanno",
        " vcfanno 2>&1 | sed -n 's/^vcfanno version \\([^ ]*\\).*/\\1/p' ",
    ),
];

fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    let command = match (name, arguments.as_slice()) {
        ("pyPackageVersion", [package]) => package_command(&package.to_string(), true)?,
        ("rLibraryVersion", [library]) => package_command(&library.to_string(), false)?,
        (name, []) => match TOOLS.iter().find(|(function, _, _)| *function == name) {
            Some((_, key, inside)) => format!("\n    echo '{key}: \"'$({inside})'\"'\n"),
            None => return Err(unsupported(name)),
        },
        (name, _) => return Err(unsupported(name)),
    };
    Ok(Value::string(command))
}

/// The command that prints the version of a Python package or an R library, whose name may
/// hold only letters, digits, `.`, `_` and `-`, as it goes into the command unquoted.
fn package_command(name: &str, python: bool) -> Result<String, Error> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if name.is_empty() || !name.chars().all(allowed) {
        let message = match python {
            true => format!(
                "Invalid Python package name '{name}': only letters, digits, hyphens, \
                 underscores, and dots are allowed (PEP 508)."
            ),
            false => format!(
                "Invalid R library name '{name}': only letters, digits, hyphens, underscores, \
                 and dots are allowed."
            ),
        };
        return Err(Error::Thrown {
            exception: Rc::new(Exception::illegal_argument(message)),
            span: Default::default(),
        });
    }
    let program = match python {
        true => {
            "python3 -c 'import sys; from importlib.metadata import version; \
                 print(version(sys.argv[1]))'"
        }
        false => {
            "Rscript -e 'cat(as.character(packageVersion(commandArgs(trailingOnly=TRUE)[1])))'"
        }
    };
    Ok(format!(
        "\necho '{name}: \"'$( {program} {name} 2>/dev/null )'\"'\n"
    ))
}

fn unsupported(name: &str) -> Error {
    Error::Unsupported {
        message: format!("the function `{name}` of the plugin `nf-versions` with these arguments"),
        span: Default::default(),
    }
}

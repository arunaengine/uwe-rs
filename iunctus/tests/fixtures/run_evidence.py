#!/usr/bin/env python3
# Records how Nextflow 26.04.6 fails on the nf-core pipelines that fail in
# run_test_workflows.txt and on the WorkflowHub workflows that fail in run_workflowhub.txt,
# into run_evidence.txt next to this script. The run test checks that every pipeline that fails
# there is listed here, so a pipeline may only fail when Nextflow fails on it too, or when
# iunctus leaves the feature out on purpose.
#
# Run it from the repository root: iunctus/tests/fixtures/run_evidence.py [pipeline...]
# With pipelines, it records only their cases and keeps the other records. It reads the
# pipelines of `just test-workflows` (NF_CORE_TEST_WORKFLOWS) and the workflows of `just
# workflowhub-test-workflows` (WORKFLOWHUB_TEST_WORKFLOWS), and needs `nextflow` 26.04.6 on
# the PATH. Each pipeline runs in a copy, from another folder, as the run test runs it:
# `nextflow run <pipeline> -profile test -stub-run --outdir <out>`. The machine has none of
# the tools of the pipelines, so a shell function stands in for each missing command: it
# prints `1.0`, which stubs take for a version, and succeeds. `SHOW=1` prints the whole output
# of each run.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HOME = Path.home() / ".cache/iunctus/test-workflows"
PIPELINES = Path(os.environ.get("NF_CORE_TEST_WORKFLOWS") or HOME / "nf-core-text") / "pipelines"
# The Nextflow workflows of WorkflowHub, which records name `workflowhub/<id>`.
WORKFLOWHUB = Path(os.environ.get("WORKFLOWHUB_TEST_WORKFLOWS") or HOME / "nextflow-workflowhub")
OUTPUT = Path(__file__).with_name("run_evidence.txt")
# The Java libraries that Nextflow releases before 24 shipped, which the `lib/` classes of older
# pipelines use, like `NfcoreSchema` with org.json and everit (decision 14).
JARS = Path.home() / ".nextflow/plugins/nf-validation-1.1.3/lib"
# Earlier Nextflow releases, for pipelines that name one: the launcher of the release runs the
# release of NXF_VER, which it downloads into a folder of its own below this one (NXF_HOME). Releases
# before 23.10 need Java 17 or earlier, from JAVA17_HOME, and releases before 21.10 Java 11,
# from JAVA11_HOME.
RELEASES = Path(os.environ.get("NXF_RELEASES") or "/tmp/nextflow-releases")
JAVA17 = os.environ.get("JAVA17_HOME")
JAVA11 = os.environ.get("JAVA11_HOME")
VERSION = "26.04.6"
TIMEOUT = 1200
JOBS = 4

# Each case says why the pipelines cannot run, in one plain sentence, and how Nextflow shows
# it: `expect` has patterns of the lines of the output to keep, each the first line after the
# one before that matches. The kind is `invalid` (the stub run of the test profile fails on its
# own, and the sentence says whether the pipeline, its stubs or its test profile are the
# cause) unless the case gives `kind`: `missing` (it needs something that is not public or not
# in its repository) or `unsupported (decision N)` (iunctus leaves the feature out on purpose,
# as decision N of docs/decisions.md says). A case may give `release` for the Nextflow release
# that the pipeline names, whose meaning iunctus keeps (decision 14), `parser: "v2"` for the
# strict parser (the legacy parser is the default, as iunctus accepts legacy syntax), `jars:
# True` to put the old Java libraries into `lib/`, `args` for more arguments of the run, where
# `{launch}` and `{pipeline}` name the folders of the run, `profile: False` to run a workflow
# that has no `test` profile without one, `config` for settings of an extra config file
# (`-c extra.config`), and `tools` for commands that print a given text, like a version that a
# stub reads. A case whose failure Nextflow does not reach, because it stops earlier at a tool
# that this machine lacks, gives `reproduce`: the files of a small script with the failing part
# of the pipeline, which Nextflow runs instead.
CASES = [
    {
        "pipelines": ["alleleexpression"],
        "why": "the test profile gives no `--input` samplesheet, which the pipeline requires, so "
        "the test run fails; runs with their own inputs are not affected",
        "expect": ["Input samplesheet not specified!"],
    },
    {
        "pipelines": ["ampliseq"],
        "why": "an output of CUTADAPT reads `outformat`, which only its script sets, so a stub "
        "run fails; a real run is not affected",
        "expect": ["No such variable: outformat", "Check script .*cutadapt/main.nf"],
    },
    {
        "pipelines": ["bacmodel"],
        "why": "the stub of BACMODEL_SUMMARY makes an empty versions.yml, which "
        "processVersionsFromYAML reads as null, so a stub run fails; a real run is not affected",
        "expect": [
            r"Cannot invoke method collectEntries\(\) on null object",
            "Check script .*utils_nfcore_pipeline/main.nf",
        ],
    },
    {
        "pipelines": ["daamicrobiome"],
        "why": "the test profile gives lists for params that the schema types as text, so the "
        "test run fails; runs with their own inputs are not affected",
        "expect": [
            "Validation of pipeline parameters failed!",
            r"--sim_p_da .*should be \[string\]",
        ],
    },
    {
        "pipelines": ["deepmodeloptim"],
        "why": "CUSTOM_MODIFY_MODEL_CONFIG emits `meta_updated`, which only its script sets, so "
        "a stub run fails; a real run is not affected",
        "expect": [
            "Error executing process > .*CUSTOM_MODIFY_MODEL_CONFIG",
            "Missing value declared as output parameter: meta_updated",
        ],
    },
    {
        "pipelines": ["denovotranscript"],
        "why": "the stub of FASTP makes an empty JSON report, which the pipeline parses, so a "
        "stub run fails; a real run is not affected",
        "expect": [
            "Text must not be null or empty",
            "Check script .*fastq_trim_fastp_fastqc/main.nf' at line: 15",
        ],
    },
    {
        "pipelines": ["drugresponseeval"],
        "kind": "missing",
        "why": "the test profile includes conf/test.config, which the repository does not have",
        "expect": ["Config file does not exist"],
    },
    {
        "pipelines": ["evexplorer"],
        "why": "main.nf gives PIPELINE_INITIALISATION 6 of its 8 inputs, which Nextflow 23.04.0, "
        "the release the pipeline names, rejects too",
        "release": "23.04.0",
        "expect": ["declares 8 input channels but 6 were given"],
    },
    {
        "pipelines": ["genomeassembler"],
        "why": "the stub of FASTPLONG makes an empty JSON report, which the pipeline parses, so "
        "a stub run fails; a real run is not affected",
        "expect": ["Unable to determine the current character", "Check script .*prepare/main.nf"],
    },
    {
        "pipelines": ["genomicrelatedness"],
        "why": "the test profile sets neither `--input` nor `--fasta`, which the schema "
        "requires, so the test run fails; runs with their own inputs are not affected",
        "expect": [
            "Validation of pipeline parameters failed!",
            r"Missing required parameter\(s\): input",
            r"Missing required parameter\(s\): fasta",
        ],
    },
    {
        "pipelines": ["gwas"],
        "kind": "missing",
        "why": "the test samplesheet samplesheet_gwas_test.csv is gone from "
        "nf-core/test-datasets",
        "expect": [
            "Validation of pipeline parameters failed!",
            "samplesheet_gwas_test.csv' does not exist",
        ],
    },
    {
        "pipelines": ["hgtseq"],
        "jars": True,
        "why": "the test samplesheet has no column `input1`, which the pipeline requires, so the "
        "test run fails; runs with their own inputs are not affected",
        "expect": ["a column named input1"],
    },
    {
        "pipelines": ["mag"],
        "why": "an output of UNTAR reads `output_dir`, which only its script sets, so a stub run "
        "fails; a real run is not affected",
        "expect": ["No such variable: output_dir", "Check script .*untar/main.nf"],
    },
    {
        "pipelines": ["magmap"],
        "why": "the stub of CHECK_DUPLICATES reads `outfile`, which only its script sets, so a "
        "stub run fails; a real run is not affected",
        "expect": ["Error executing process > .*CHECK_DUPLICATES", "No such variable: outfile"],
    },
    {
        "pipelines": ["marsseq"],
        "why": "the test profile reads the genome from the output folder of an earlier run, so "
        "the test run fails; runs with their own inputs are not affected",
        "expect": ["No such file or directory: <out>/references/mm10/GRCm39.fa"],
    },
    {
        "pipelines": ["mcmicro"],
        "why": "the stub of PRELUDE_MULTI_SUMMARY makes an empty error table, which the pipeline "
        "takes as a failed quality check, so a stub run fails; a real run is not affected",
        "expect": ["QC Error found"],
    },
    {
        "pipelines": ["meerpipe"],
        "kind": "missing",
        "why": "its config reads `PSRDB_URL`, the address of the pulsar database of its "
        "authors, from the environment",
        "expect": ["Unknown config attribute `params.PSRDB_URL`"],
    },
    {
        "pipelines": ["methylong"],
        "why": "the stub of PORECHOP gzips a file onto its own input, which gzip refuses, so a "
        "stub run fails; a real run is not affected",
        "expect": [
            "Error executing process > .*PORECHOP_PORECHOP",
            "gzip: .* already exists",
        ],
    },
    {
        "pipelines": ["mspepid"],
        "why": "the stub of COMETCONFIG reads `params_file`, which only its script sets, so a "
        "stub run fails; a real run is not affected",
        "expect": ["Error executing process > .*COMETCONFIG", "No such variable: params_file"],
    },
    {
        "pipelines": ["scdownstream"],
        "why": "the stub of CELLDEX_FETCHREFERENCE makes an empty versions.yml, which "
        "processVersionsFromYAML reads as null, so a stub run fails; a real run is not affected",
        "expect": [
            r"Cannot invoke method collectEntries\(\) on null object",
            "Check script .*utils_nfcore_pipeline/main.nf",
        ],
    },
    {
        "pipelines": ["variantcatalogue"],
        "jars": True,
        "why": "the test profile names assets/samplesheet.csv relative to the folder where the "
        "run starts, so the test run fails; runs with their own inputs are not affected",
        "expect": ["No such file or directory: <launch>/assets/samplesheet.csv"],
    },
    {
        "pipelines": ["callingcards"],
        "kind": "unsupported (decision 16)",
        "why": "SEQKIT_SPLIT2 has no stub, so it needs seqkit to split the reads into the files "
        "whose names the pipeline reads, and the mock executor only makes up one file",
        "expect": [
            "Error executing process > .*SEQKIT_SPLIT2",
            r"Missing output file\(s\) `\*\*/\*.gz`",
        ],
    },
    {
        "pipelines": ["chipseq", "pathogensurveillance"],
        "kind": "unsupported (decision 16)",
        "why": "SAMPLESHEET_CHECK has no stub, so it needs its script to write the samplesheet "
        "that the pipeline reads, and the mock executor only makes up an empty file",
        "tools": {"python3": "1.0", "Rscript": "1.0"},
        "expect": [
            "Error executing process > .*SAMPLESHEET_CHECK",
            r"Missing output file\(s\)",
        ],
    },
    {
        "pipelines": ["circrna"],
        "why": "the stub of MAJORITY_VOTE makes no `mirna.targets.tsv`, which the process "
        "declares, so a stub run fails; a real run is not affected (shown with a small script, as "
        "Nextflow first stops at SEQKIT_SPLIT, which has no stub and needs seqkit)",
        "reproduce": {
            "main.nf": """// The outputs and the stub of MAJORITY_VOTE of nf-core/circrna.
process MAJORITY_VOTE {
    input:
    val meta
    output:
    tuple val(meta), path("${meta.id}.majority.tsv"), emit: tsv
    tuple val(meta), path("${meta.id}.targets.tsv") , emit: targets
    script:
    'true'
    stub:
    "touch ${meta.id}.majority.tsv"
}

workflow {
    MAJORITY_VOTE([id: 'mirna'])
}
""",
        },
        "expect": [r"Missing output file\(s\) `mirna.targets.tsv`"],
    },
    {
        "pipelines": ["coproid"],
        "why": "the stub of SOURCEPREDICT makes `*.sourcepredict.csv`, not the "
        "`*.report.sourcepredict.csv` that the process declares, so a stub run fails; a real run "
        "is not affected (shown with a small script, as Nextflow first stops at DAMAGEPROFILER, "
        "which has no stub and needs damageprofiler)",
        "reproduce": {
            "main.nf": """// The output and the stub of SOURCEPREDICT of nf-core/coproid.
process SOURCEPREDICT {
    input:
    val meta
    output:
    tuple val(meta), path("*.report.sourcepredict.csv"), emit: report
    script:
    'true'
    stub:
    "touch ${meta.id}.sourcepredict.csv"
}

workflow {
    SOURCEPREDICT([id: 'samples_combined'])
}
""",
        },
        "expect": [r"Missing output file\(s\) `\*.report.sourcepredict.csv`"],
    },
    {
        "pipelines": ["dartseq"],
        "kind": "missing",
        "why": "the test profile and its samplesheet name folders on the computer of the author "
        "(`/work_ikmb/sukmb662`)",
        "args": ["-w", "{launch}/work"],
        "expect": [
            "The following invalid input values have been detected",
            r"Entry 1: Error for field 'fastq_2' \(/work_ikmb/",
        ],
    },
    {
        "pipelines": ["demultiplex", "rangeland", "smrnaseq", "spatialaxe", "spatialvi"],
        "kind": "unsupported (decision 16)",
        "why": "the UNTAR stub lists the files of the real archive with `tar -tf` and makes "
        "them, which the mock executor cannot do, and the pipeline then reads those files",
        "expect": [
            r"\[PROCESS .*UNTAR_FLOWCELL",
            r"\[PROCESS .*MULTIQCSAV",
        ],
    },
    {
        "pipelines": ["diseasemodulediscovery"],
        "kind": "unsupported (decision 16)",
        "parser": "v2",
        "why": "GRAPHTOOLPARSER and TOPOLOGY have no stubs, so they need their Python scripts to "
        "write the files that the pipeline reads, and the mock executor only makes up empty ones",
        "tools": {"python": "1.0"},
        "expect": [
            "Error executing process > .*GRAPHTOOLPARSER",
            r"Missing output file\(s\)",
        ],
    },
    {
        "pipelines": ["drop"],
        "why": "the stub of SAMTOOLS_IDXSTATS makes an empty file, and the pipeline reads the "
        "read counts of its first line, so a stub run fails; a real run is not affected",
        "tools": {"tabix": "Version: 1.21"},
        "expect": [r"Cannot invoke method getAt\(\) on null object"],
    },
    {
        "pipelines": ["epigenomesegmentation"],
        "why": "the stub of CUSTOM_FILTERCHROMSIZES makes `mm10.chrom.sizes_V2`, not the "
        "`mm10.chrom.sizes.V2` that the process declares, so a stub run fails; a real run is not "
        "affected",
        "expect": [r"Missing output file\(s\) `mm10.chrom.sizes.V2`"],
    },
    {
        "pipelines": ["nanostring"],
        "why": "the stub of CREATE_ANNOTATED_TABLES makes `mqc.tsv`, which the declared "
        "`*_mqc.tsv` does not match, so a stub run fails; a real run is not affected",
        "expect": [r"Missing output file\(s\) `\*_mqc.tsv`"],
    },
    {
        "pipelines": ["pairgenomealign"],
        "why": "the stub of LAST_SPLIT makes no `*.matrix.txt`, which the process declares, so a "
        "stub run fails; a real run is not affected",
        "expect": [r"Missing output file\(s\) `\*.matrix.txt`"],
    },
    {
        "pipelines": ["radseq"],
        "jars": True,
        "why": "workflows/radseq.nf includes subworkflows/local/fastq_fastp.nf, which the "
        "repository does not have",
        "args": ["--input", "{pipeline}/assets/samplesheet.csv"],
        "expect": ["fastq_fastp"],
    },
    {
        "pipelines": ["viralrecon"],
        "why": "the stubs of FREYJA_UPDATE and FASTP make no `usher_barcodes.feather` and an "
        "empty JSON report, which the pipeline needs, so a stub run fails; a real run is not "
        "affected",
        "expect": [r"Missing output file\(s\) `freyja_db/usher_barcodes.feather`"],
    },
    {
        "pipelines": ["crisprvar", "vipr"],
        "why": "the pipeline has no `test` profile",
        "expect": ["Unknown configuration profile: 'test'"],
    },
    {
        "pipelines": ["oncoanalyser"],
        "why": "the stub of CUSTOM_EXTRACTTARBALL makes an empty folder, and the pipeline checks "
        "that the reference files in it exist, so a stub run fails; a real run is not affected "
        "(shown with a small script, as Nextflow first downloads tens of gigabytes of reference "
        "data here)",
        "reproduce": {
            "main.nf": """process DECOMP_HMF_DATA {
    output:
    path 'hmf_pipeline_resources/'
    script:
    'true'
    stub:
    'mkdir -p hmf_pipeline_resources/'
}

// From subworkflows/local/prepare_reference/main.nf of nf-core/oncoanalyser.
def getRefdataFile(filepath, ref_data_path) {
    def data_path_noslash = ref_data_path.toString().replaceAll('/$', '')
    return file("${data_path_noslash}/${filepath}", checkIfExists: true)
}

workflow {
    DECOMP_HMF_DATA()
        .map { dir -> getRefdataFile('dna/copy_number/AmberGermlineSites.38.tsv.gz', dir) }
        .view()
}
""",
        },
        "expect": ["No such file or directory: .*AmberGermlineSites.38.tsv.gz"],
    },
    {
        "pipelines": ["phaseimpute"],
        "why": "the stub of BAMCHREXTRACT makes an empty list of contigs, which the pipeline "
        "checks against its panel, so a stub run fails; a real run is not affected",
        "release": "25.04.0",
        "expect": [r"Contig names: \[chr21\] absent from file"],
    },
    {
        "pipelines": ["rnadnavar"],
        "why": "the stub of STAR_ALIGN makes three files that `*d.out.bam` matches, and the "
        "`ext.prefix` of SAMTOOLS_SORT then calls `tokenize` on the list of their names, so a "
        "stub run fails; a real run is not affected (shown with a small script, as Nextflow first "
        "stops at TABIX_DBSNP, which needs bgzip)",
        "reproduce": {
            "main.nf": """// The outputs of the STAR_ALIGN stub of nf-core/rnadnavar, and SAMTOOLS_SORT with the
// ext.prefix of conf/modules/alignment/bam_align.config.
process STAR_ALIGN {
    output:
    path '*d.out.bam'
    script:
    'true'
    stub:
    'touch sXd.out.bam s.sortedByCoord.out.bam s.Aligned.sortedByCoord.out.bam'
}

process SAMTOOLS_SORT {
    input:
    path bam
    script:
    "echo ${task.ext.prefix}"
}

workflow {
    SAMTOOLS_SORT(STAR_ALIGN())
}
""",
            "nextflow.config": """process {
    withName: 'SAMTOOLS_SORT' {
        ext.prefix = { "s".concat('.').concat(bam.name.tokenize('.')[1]) }
    }
}
""",
        },
        "expect": [r"No signature of method: java.util.ArrayList.tokenize\(\)"],
    },
    {
        "pipelines": ["rnavar"],
        "release": "25.10.4",
        "why": "the SAMTOOLS_SORT of the transcriptome gets `[[:], []]` for its fasta and index, "
        "which does not match its input declaration, and Nextflow 25.10.4, the release the "
        "pipeline names, fails on it too",
        "tools": {"STAR": "versionGenome 2.7.4a"},
        "expect": [
            r"Error executing process > .*BAM_SORT_STATS_SAMTOOLS_TRANSCRIPTOME:SAMTOOLS_SORT",
            "Path value cannot be null",
        ],
    },
    {
        "pipelines": ["scrnaseq"],
        "why": "the stub of MTX_TO_H5AD makes an empty versions.yml, which "
        "processVersionsFromYAML reads as null, so a stub run fails; a real run is not affected "
        "(shown with a small script, as Nextflow first stops at STAR_ALIGN, which has no stub and "
        "needs STAR)",
        "reproduce": {
            "main.nf": """process MTX_TO_H5AD {
    output:
    path 'versions.yml', emit: versions
    script:
    'true'
    stub:
    'touch versions.yml'
}

// From subworkflows/nf-core/utils_nfcore_pipeline/main.nf of nf-core/scrnaseq.
def processVersionsFromYAML(yaml_file) {
    def yaml = new org.yaml.snakeyaml.Yaml()
    def versions = yaml.load(yaml_file).collectEntries { k, v -> [k.tokenize(':')[-1], v] }
    return yaml.dumpAsMap(versions).trim()
}

workflow {
    MTX_TO_H5AD().versions.map { processVersionsFromYAML(it) }.view()
}
""",
        },
        "expect": [r"Cannot invoke method collectEntries\(\) on null object"],
    },
]

# DSL1 pipelines, whose processes read `from` and write `into` channels. iunctus leaves DSL1
# out (decision 14), and Nextflow 26.04.6 does not run it either: each record shows how it
# fails on the pipeline.
DSL1 = {
    "cageseq": {},
    "clipseq": {"jars": True},
    "ddamsproteomics": {},
    "deepvariant": {},
    "denovohybrid": {},
    "diaproteomics": {"jars": True},
    # Without this, the run stops at `data/`, which the test workflows do not fetch.
    "dualrnaseq": {"args": ["--mapping_statistics", "false"]},
    "eager": {"jars": True},
    "exoseq": {"args": ["--reads", "{launch}/x_R{1,2}.fastq.gz", "--genome", "GRCh37"]},
    "imcyto": {},
    "kmermaid": {},
    "mnaseseq": {},
    "neutronstar": {"args": ["--id", "test", "--fastqs", "{launch}/x_R{1,2}.fastq.gz"]},
    # The test workflows do not fetch its `conf/*.yaml`, so another file of it stands in.
    "proteogenomicsdb": {
        "jars": True,
        "args": [
            argument
            for name in ["ensembl_downloader", "ensembl", "cosmic", "cbioportal", "protein_decoy"]
            for argument in [f"--{name}_config", "{pipeline}/conf/base.config"]
        ],
    },
    "proteomicslfq": {},
    "slamseq": {},
    "smartseq2": {},
    # The DSL1 workflows of WorkflowHub; those without a `test` profile run without one.
    "workflowhub/19": {"profile": False},
    "workflowhub/20": {"profile": False},
    "workflowhub/57": {},
    "workflowhub/104": {"profile": False},
    "workflowhub/105": {"profile": False},
    "workflowhub/106": {"profile": False},
    "workflowhub/119": {"profile": False},
    "workflowhub/156": {"profile": False},
    "workflowhub/244": {"profile": False},
    "workflowhub/416": {"profile": False},
    "workflowhub/435": {"profile": False},
    "workflowhub/436": {"profile": False},
    "workflowhub/965": {},
    "workflowhub/969": {},
    "workflowhub/971": {},
    "workflowhub/973": {},
    "workflowhub/980": {},
    "workflowhub/982": {},
    "workflowhub/983": {},
    "workflowhub/989": {},
    "workflowhub/992": {},
    "workflowhub/994": {},
    "workflowhub/1000": {},
    "workflowhub/1002": {},
    "workflowhub/1004": {},
    "workflowhub/1005": {},
    "workflowhub/1008": {},
    "workflowhub/1012": {},
    "workflowhub/1022": {},
    "workflowhub/2205": {},
}
CASES += [
    {
        "pipelines": [name],
        "kind": "unsupported (decision 14)",
        "why": "the pipeline is a DSL1 script, whose processes read `from` and write `into` "
        "channels, which iunctus leaves out and Nextflow 26.04.6 does not run either",
        "expect": [r"ERROR ~"],
        **extra,
    }
    for name, extra in DSL1.items()
]


# The Nextflow workflows of WorkflowHub (decision 4) that fail, named `workflowhub/<id>`. Those
# without a `test` profile run without one, as the run test runs them.
def workflowhub(numbers, why, expect, **extra):
    """A case of WorkflowHub workflows."""
    return {
        "pipelines": [f"workflowhub/{number}" for number in numbers],
        "why": why,
        "expect": expect,
        **extra,
    }


NO_INPUTS = (
    "the workflow has no `test` profile, and {}, so a run without its inputs fails; runs "
    "with their own inputs are not affected"
)
HELP = NO_INPUTS.format("without its inputs it prints its help and exits with code 1")
CASES += [
    workflowhub([140], NO_INPUTS.format("it reads a file named after a param that is null"),
                [r"No such file or directory: <launch>/null"], profile=False),
    workflowhub(
        [192],
        "WorkflowHub gives a web page as its main.nf, not the Nextflow script, so the run fails",
        ["Script compilation error", "<!DOCTYPE html>"],
        kind="missing",
        profile=False,
    ),
    workflowhub(
        [240],
        "the workflow includes its modules inside the workflow body, which Nextflow 21.10.3 of "
        "its time does not take, and WorkflowHub does not give its workflow/ folder, so the run "
        "fails",
        ["Missing process or function with name 'include'"],
        release="21.10.3",
        profile=False,
    ),
    workflowhub(
        [340],
        "the config sets `params.bam_folder` inside a `params` block, which Nextflow 22.04.5 of "
        "its time rejects, and the help text reads `param`, which is not defined, so the run "
        "fails",
        [r"Unknown config attribute `params.params.bam_folder`"],
        release="22.04.5",
        profile=False,
    ),
    workflowhub([393], HELP, ["I N D E X"], profile=False),
    workflowhub([402], NO_INPUTS.format("without `--output_dir_plots` a process input is null"),
                ["A process input channel evaluates to null"], profile=False),
    workflowhub(
        [417],
        "the test profile gives none of the input files, so the workflow stops with `missing "
        "some input data`; runs with their own inputs are not affected",
        ["missing some input data"],
    ),
    workflowhub(
        [418],
        "the test profile names test_data/ relative to the launch folder, where it is not, so "
        "the test run fails; runs from the folder of the workflow or with their own inputs are "
        "not affected",
        [r"No such file or directory: <launch>/test_data/test_input_paired.txt"],
    ),
    workflowhub(
        [419],
        "the test profile gives a reference file that does not exist, so the workflow stops, "
        "as Nextflow 22.10.8 of its time shows; runs with their own inputs are not affected",
        ["--reference points to a non existing file"],
        release="22.10.8",
    ),
    workflowhub([431], HELP, ["G E R M L I N E"], profile=False),
    workflowhub(
        [438],
        "the workflow includes BioNextflow, a git submodule that WorkflowHub and its repository "
        "do not give, and evaluates ../outdirs.nf from the launch folder, so the run fails",
        [r"dirs\.nf"],
        kind="missing",
        profile=False,
    ),
    workflowhub(
        [440],
        "config/standard.config has a closing brace too many, so the config does not parse, "
        "as Nextflow 20.07.1, which the workflow names, shows",
        ["Unable to parse config file"],
        release="20.07.1",
        profile=False,
    ),
    workflowhub(
        [444],
        "WorkflowHub gives no lib/ folder of this workflow, whose classes like WorkflowMain the "
        "script uses, and names no git repository, so the run fails",
        ["No such variable: WorkflowMain"],
        kind="missing",
        profile=False,
    ),
    workflowhub(
        [450],
        "modules/mapseq.nf holds the markers of a git merge conflict, so no release can parse "
        "it, and without inputs the run stops even earlier at a missing path",
        ["Missing `fromPath` parameter"],
        profile=False,
    ),
    workflowhub(
        [452],
        "the test profile gives no `--input`, which the schema requires, so the test run "
        "fails; runs with their own inputs are not affected",
        [r"Missing required parameter\(s\): input"],
    ),
    workflowhub(
        [453],
        "WorkflowHub names the shell script batchconvert as the main file, which runs the "
        "Nextflow scripts itself, and there is no main.nf, so a Nextflow run fails",
        ["Missing project main script"],
        kind="missing",
        profile=False,
    ),
    workflowhub([462], NO_INPUTS.format("without `--ena_genomes` it has no genomes to read"),
                ["Missing `fromPath` parameter"], profile=False),
    workflowhub(
        [480],
        NO_INPUTS.format("without `--repetitions` its range `1..params.repetitions` has no end"),
        ["Must specify a non-null value for the 'to' index in a Range"],
        profile=False,
    ),
    workflowhub(
        [556],
        "PLINK2_RELABELPVAR has no stub, and the stubs of the scoring steps make empty score "
        "files, which the workflow reads, so a stub run fails; a real run is not affected",
        [r"Error executing process > .*PLINK2_RELABELPVAR"],
    ),
    workflowhub([560], HELP, ["Please provide path to bam files"], profile=False),
    workflowhub(
        [654, 1330],
        NO_INPUTS.format("without `--genome` it stops with `Input genome is not specified!`"),
        ["Input genome is not specified!"],
        profile=False,
    ),
    workflowhub(
        [668],
        "the test profile names files below /lustre of the Sanger cluster, so the test run "
        "fails; runs with their own inputs are not affected",
        ["No such file or directory: /lustre/"],
        kind="missing",
        jars=True,
    ),
    workflowhub([691], HELP, ["Usage:  nextflow run main.nf --input"], profile=False),
    workflowhub(
        [813],
        "the test profile names busco/subset relative to the launch folder, where it is not, "
        "so the test run fails; runs with their own inputs are not affected",
        [r"No such file or directory: <launch>/busco/subset"],
    ),
    workflowhub([836], HELP, ["Parabricks-Genomics-nf"], profile=False),
    workflowhub(
        [837],
        NO_INPUTS.format("without `--main_sp_embl_file` it has no SP-EMBL file to read"),
        ["Missing `fromPath` parameter"],
        profile=False,
    ),
    workflowhub(
        [874],
        "the config loads the config file of the workflow that `--workflow` names, which the "
        "test run does not give, so it stops; runs that name a workflow are not affected",
        ["Could not load config/Not_Specified.config"],
    ),
    workflowhub(
        [884],
        NO_INPUTS.format("it requires `--samplesheet` or `--ena_raw_reads_study_accession`"),
        [r"Missing required parameter\(s\): ena_raw_reads_study_accession"],
        profile=False,
    ),
    workflowhub(
        [964],
        "an output of CUTADAPT reads `outformat`, which only its script sets, so a stub run "
        "fails; a real run is not affected",
        ["No such variable: outformat", "Check script .*cutadapt/main.nf"],
    ),
    workflowhub(
        [970],
        "SEQKIT_SPLIT2 has no stub, so a stub run gives it no split reads, which the workflow "
        "needs; a real run is not affected",
        [r"Missing output file\(s\) `\*\*/\*\.gz` expected by process .*SEQKIT_SPLIT2"],
    ),
    workflowhub(
        [974],
        "DAMAGEPROFILER has no stub and the stub of SOURCEPREDICT makes no report, so a stub run "
        "fails; a real run is not affected",
        [r"Error executing process > .*DAMAGEPROFILER"],
    ),
    workflowhub(
        [988],
        "the test samplesheet has no `input1` column, which the workflow requires, so the test "
        "run fails; runs with their own inputs are not affected",
        ["a column named input1 with either fastq or bam/cram input is mandatory"],
        jars=True,
    ),
    workflowhub(
        [1003],
        "the stub of CREATE_ANNOTATED_TABLES makes mqc.tsv, where its output needs files that "
        "end in `_mqc.tsv`, so a stub run fails; a real run is not affected",
        [r"Missing output file\(s\) `\*_mqc\.tsv`"],
    ),
    workflowhub(
        [1013],
        NO_INPUTS.format("it requires `--input` and `--database`"),
        ["Missing required parameter: --input"],
        profile=False,
        jars=True,
    ),
    workflowhub(
        [1015, 1069],
        "the test profile gives no `--input`, which the schema requires, so the test run "
        "fails; runs with their own inputs are not affected",
        ["Missing required parameter: --input"],
    ),
    workflowhub(
        [1021],
        "STAR_ALIGN has no stub, and the stubs of other steps make empty versions.yml files, "
        "which processVersionsFromYAML reads as null, so a stub run fails; a real run is not "
        "affected",
        [r"Missing output file\(s\) `\*d\.out\.bam` expected by process .*STAR_ALIGN"],
    ),
    workflowhub(
        [1023],
        "FORMAT_MATURE has no stub, and the stub of the Bowtie index makes no `.1.ebwt` file, "
        "whose name the workflow reads, so a stub run fails; a real run is not affected",
        [r"Missing output file\(s\) `\*_idx\.fa` expected by process .*FORMAT_MATURE"],
    ),
    workflowhub(
        [1027],
        "the stub of FREYJA_UPDATE makes usher_barcodes.csv, where its output needs "
        "usher_barcodes.feather, and the stub of FASTP makes an empty report, so a stub run "
        "fails; a real run is not affected",
        [r"Missing output file\(s\) `freyja_db/usher_barcodes\.feather`"],
    ),
    workflowhub(
        [1030],
        "the workflow reads the outputs of FASTP, which the test run does not call, so the test "
        "run fails; runs that trim their reads are not affected",
        ["Access to 'FASTP.out' is undefined"],
    ),
    workflowhub(
        [1100],
        "the stub of FASTP makes an empty JSON report, whose text the workflow parses, so a stub "
        "run fails; a real run is not affected",
        ["Text must not be null or empty"],
    ),
    workflowhub(
        [1106],
        "the test YAML gives no `sample.name`, which the workflow changes with `replace`, so "
        "the test run fails; runs with their own inputs are not affected",
        [r"Cannot invoke method replace\(\) on null object"],
    ),
    workflowhub(
        [1160, 1769],
        NO_INPUTS.format("it reads params_ccrcc.csv from the launch folder, where it is not"),
        [r"No such file or directory: <launch>/params_ccrcc.csv"],
        profile=False,
    ),
    workflowhub(
        [1222],
        "the config sets `params.nci_project` inside a `process` block, which Nextflow 24.10.1 "
        "of its time rejects, so the run fails",
        [r"Unknown config attribute `process.params.nci_project`"],
        release="24.10.1",
        profile=False,
    ),
    workflowhub(
        [1226],
        "the stub of BAMCHREXTRACT makes an empty list of contigs, which the workflow checks "
        "against its panel, so a stub run fails; a real run is not affected",
        [r"Contig names: \[chr21, chr22\] absent from file"],
        release="25.04.0",
    ),
    workflowhub([1263], HELP, ["O N T - B A C P A C K"], profile=False),
    workflowhub(
        [27],
        "WorkflowHub gives only virify.nf of this workflow, whose includes need its "
        "nextflow/modules folder, so the run fails, as Nextflow 20.04.1 of its time shows",
        ["Can't find a matching module file for include: ./nextflow/modules/pprmeta"],
        kind="missing",
        release="20.04.1",
        profile=False,
    ),
    workflowhub(
        [967],
        "the test genome that the test profile names is no longer in nf-core/test-datasets, so "
        "the test run fails, as Nextflow 21.04.0 of its time shows; runs with their own inputs "
        "are not affected",
        ["No such file: https://raw.githubusercontent.com/nf-core/test-datasets/bactmap/genome"],
        release="21.04.0",
        jars=True,
    ),
    workflowhub(
        [996],
        "the test profile names the genome R64-1-1, which the config of the pipeline does not "
        "have, so it stops, as Nextflow 21.04.0 of its time shows; runs with their own inputs "
        "are not affected",
        ["Genome 'R64-1-1' not found in any config files"],
        release="21.04.0",
        jars=True,
    ),
    workflowhub(
        [1019],
        "the SAMTOOLS_SORT of the transcriptome gets `[[:], []]` for its fasta and index, which "
        "does not match its input declaration, and Nextflow 25.10.4, the release the pipeline "
        "names, fails on it too",
        [
            r"Error executing process > .*BAM_SORT_STATS_SAMTOOLS_TRANSCRIPTOME:SAMTOOLS_SORT",
            "Path value cannot be null",
        ],
        release="25.10.4",
        tools={"STAR": "versionGenome 2.7.4a"},
    ),
    workflowhub(
        [1250],
        "the stub of UNTAR makes an empty folder, where the workflow looks for its Landsat "
        "data, so a stub run fails; a real run is not affected",
        [r"Error executing process > .*UNTAR_WVDB"],
    ),
    workflowhub(
        [1306],
        "the processes have no stubs, so a stub run gives them no real outputs, and the "
        "workflow parses a JSON file that one of them makes; a real run is not affected",
        [r"Error executing process > 'fullWf:"],
    ),
    workflowhub(
        [1320, 1321],
        "the test profile reads TreeValTinyData/, test data that the repository does not hold "
        "and that users download first, so the test run fails",
        [r"No such file or directory: <pipeline>/TreeValTinyData/genomic_data/pacbio"],
        kind="missing",
        jars=True,
    ),
    workflowhub(
        [1360],
        "the stub of PORECHOP_PORECHOP runs `gzip` on a file whose `.gz` file it made before, "
        "so a stub run fails; a real run is not affected",
        [r"Error executing process > .*PORECHOP_PORECHOP", "already exists"],
    ),
    workflowhub(
        [1757],
        "PRINSEQ_READS has no stub, and the stubs of other steps make files without the "
        "extensions that CAT_CAT reads from their names, so a stub run fails; a real run is not "
        "affected",
        [r"Missing output file\(s\) `\*_good_out\*` expected by process .*PRINSEQ_READS"],
    ),
    workflowhub(
        [1763],
        "the RO-Crate that WorkflowHub gives leaves out assets/.pathogensurveillance_output.json, "
        "which the workflow reads, so the run fails",
        [r"No such file or directory: <pipeline>/assets/.pathogensurveillance_output.json"],
        kind="missing",
    ),
    workflowhub(
        [1782],
        "the schema requires `--ncbi_user_email`, `--ncbi_api_key` and `--taxdb`, an NCBI "
        "account and a database that the test profile does not give, so the test run fails",
        [r"Missing required parameter\(s\): ncbi_user_email, ncbi_api_key, taxdb"],
        kind="missing",
    ),
    workflowhub(
        [1795],
        "WorkflowHub gives no lib/ folder of this workflow, whose class Utils the workflow uses, "
        "so the run fails",
        ["No such variable: Utils"],
        kind="missing",
    ),
    workflowhub(
        [1841],
        "conf/module.config adds text to a closure, as in "
        "`{ params.out_dir } + \"/registered_stacks/\"`, which Groovy cannot do, so the config "
        "fails, as Nextflow 25.04.0 of its time shows",
        ["plus"],
        release="25.04.0",
    ),
]


def stand_ins(directory, case):
    """A file for BASH_ENV that stands in for every missing command, and a folder of the
    commands of the case."""
    environment = directory / "tools.sh"
    environment.write_text("command_not_found_handle() { echo 1.0; return 0; }\n")
    folder = directory / "bin"
    folder.mkdir()
    for name, text in case.get("tools", {}).items():
        tool = folder / name
        tool.write_text(f"#!/bin/sh\ncat <<'END'\n{text}\nEND\n")
        tool.chmod(0o755)
    return environment, folder


def command(case, pipeline):
    """The command of a case as the record shows it."""
    words = []
    if "release" in case:
        words.append(f"NXF_VER={case['release']}")
    elif case.get("parser", "v1") == "v1":
        words.append("NXF_SYNTAX_PARSER=v1")
    if "reproduce" in case:
        words += ["nextflow", "run", f"<reproduction of {pipeline}>/main.nf", "-stub-run"]
    else:
        main = main_script(pipeline)
        shown = pipeline if main is None else f"{pipeline}/{main}"
        words += ["nextflow", "run", shown]
        if case.get("profile", True):
            words += ["-profile", "test"]
        words += stub_run(case) + ["--outdir", "<out>"]
        words += [
            argument.replace("{launch}", "<launch>").replace("{pipeline}", "<pipeline>")
            for argument in case.get("args", [])
        ]
    text = " ".join(words)
    notes = []
    if "config" in case:
        text += " -c extra.config"
        notes.append("extra.config: " + "; ".join(case["config"].strip().splitlines()))
    if case.get("jars"):
        notes.append("with the Java libraries of nf-validation 1.1.3 in lib/")
    for name, output in case.get("tools", {}).items():
        notes.append(f"with a `{name}` that prints {output!r}")
    if notes:
        text += "  # " + ", ".join(notes)
    return text


def source(pipeline):
    """The folder of a pipeline or of a WorkflowHub workflow."""
    if pipeline.startswith("workflowhub/"):
        return WORKFLOWHUB / pipeline.split("/", 1)[1]
    return PIPELINES / pipeline


def main_script(pipeline):
    """The main script of a WorkflowHub workflow that runs no project folder, as the run test
    picks it: the script that its RO-Crate names, when the folder has no main.nf and names no
    `manifest.mainScript`; `None` for the others."""
    folder = source(pipeline)
    metadata = folder / "ro-crate-metadata.json"
    if not pipeline.startswith("workflowhub/") or not metadata.exists():
        return None
    config = folder / "nextflow.config"
    if (folder / "main.nf").exists() or (config.exists() and "mainScript" in config.read_text()):
        return None
    graph = json.loads(metadata.read_text())["@graph"]
    root = next(entity for entity in graph if entity.get("@id") == "./")
    main = root.get("mainEntity", {}).get("@id", "main.nf").removeprefix("./")
    return main if main.endswith(".nf") else None


def stub_run(case):
    """`-stub-run`, which releases before 20.11 do not have; they run the scripts."""
    release = tuple(map(int, case.get("release", VERSION).split(".")[:2]))
    return ["-stub-run"] if release >= (20, 11) else []


def launcher(version):
    """The launcher of a Nextflow release, which the older releases need, as the launcher of
    26.04 needs Java 17; downloaded when missing."""
    path = RELEASES / f"nextflow-{version}"
    if not path.exists():
        RELEASES.mkdir(parents=True, exist_ok=True)
        url = f"https://github.com/nextflow-io/nextflow/releases/download/v{version}/nextflow"
        subprocess.run(["curl", "-sfL", "-o", str(path), url], check=True)
        path.chmod(0o755)
    return path


def execute(case):
    """Runs a case; gives the output with the paths of the run replaced by names like
    `<pipeline>`."""
    pipeline = case["pipelines"][0]
    with tempfile.TemporaryDirectory(prefix="run-evidence-") as temporary:
        temporary = Path(temporary)
        copy = temporary / Path(pipeline).name
        launch = temporary / "launch"
        out = temporary / "out"
        if "reproduce" in case:
            copy.mkdir()
            for name, text in case["reproduce"].items():
                (copy / name).write_text(text)
            launch = copy
            arguments = ["nextflow", "run", str(copy / "main.nf"), "-stub-run"]
        else:
            shutil.copytree(
                source(pipeline),
                copy,
                symlinks=True,
                ignore=shutil.ignore_patterns("work", ".nextflow", ".nextflow.log*"),
            )
            if case.get("jars"):
                (copy / "lib").mkdir(exist_ok=True)
                for jar in JARS.glob("*.jar"):
                    shutil.copy(jar, copy / "lib")
            launch.mkdir()
            extra = [
                argument.replace("{launch}", str(launch)).replace("{pipeline}", str(copy))
                for argument in case.get("args", [])
            ]
            main = main_script(pipeline)
            arguments = ["nextflow", "run", str(copy if main is None else copy / main)]
            if case.get("profile", True):
                arguments += ["-profile", "test"]
            arguments += stub_run(case) + ["--outdir", str(out)] + extra
            if "config" in case:
                config = temporary / "extra.config"
                config.write_text(case["config"])
                arguments += ["-c", str(config)]
        environment_file, folder = stand_ins(temporary, case)
        environment = dict(os.environ)
        if "release" in case:
            environment["NXF_VER"] = case["release"]
            environment["NXF_HOME"] = str(RELEASES / f"home-{case['release']}")
            arguments[0] = str(launcher(case["release"]))
            version = tuple(map(int, case["release"].split(".")[:2]))
            java = JAVA11 if version < (21, 10) else JAVA17 if version < (23, 10) else None
            if java:
                environment["JAVA_HOME"] = java
                environment["NXF_JAVA_HOME"] = java
        environment["NXF_SYNTAX_PARSER"] = case.get("parser", "v1")
        environment["BASH_ENV"] = str(environment_file)
        environment["PATH"] = f"{folder}:{environment['PATH']}"
        result = subprocess.run(
            ["nice", "-n", "10", "ionice", "-c3", "timeout", str(TIMEOUT)]
            + arguments
            + ["-ansi-log", "false"],
            cwd=launch,
            env=environment,
            capture_output=True,
            text=True,
            errors="replace",
        )
        text = result.stdout + result.stderr
        # Some errors show only in the log, like the cause of "Execution aborted due to an
        # unexpected error".
        log = launch / ".nextflow.log"
        if log.exists():
            text += "\n.nextflow.log:\n" + log.read_text(errors="replace")
        text = re.sub(r"\x1b\[[0-9;]*m", "", text)
        # Nextflow writes some messages without a line break before the next one.
        text = re.sub(r"(?<=\S)(\[(PROCESS|ERROR|WARN|PIPELINE|WORKDIR|SUCCESS|FAILED)\b)", r"\n\1", text)
        text = re.sub(r"(?<=\S)(Staging foreign file: )", r"\n\1", text)
        text = text.replace(str(launch / "work"), "<work>")
        text = text.replace(str(launch), "<launch>")
        text = text.replace(str(out), "<out>")
        text = text.replace(str(copy), "<pipeline>")
        text = text.replace(str(temporary), "<tmp>")
        text = re.sub(r"<work>/[0-9a-f]{2}/[0-9a-f]{30}", "<task>", text)
        text = re.sub(r"\[(PROCESS )?[0-9a-f]{2}/[0-9a-f]{6}\]", r"[\1<task>]", text)
        # The names and addresses of the classes that Groovy makes for a script.
        text = re.sub(r"_nf_script_[0-9a-f]+", "_nf_script_<hash>", text)
        text = re.sub(r"(closure\d+)@[0-9a-f]+", r"\1@<address>", text)
        return text


def run(case):
    """Runs a case; gives the command and the lines of evidence."""
    pipeline = case["pipelines"][0]
    text = execute(case)
    if os.environ.get("SHOW"):
        print(f"==== {pipeline}\n{text}", flush=True)
    lines = [line.rstrip() for line in text.splitlines() if line.strip()]
    kept = []
    start = 0
    for expected in case["expect"]:
        pattern = re.compile(expected)
        found = next(
            (index for index in range(start, len(lines)) if pattern.search(lines[index])), None
        )
        if found is None:
            tail = "\n".join(lines[-40:])
            raise SystemExit(f"{pipeline}: no line matches {expected!r}:\n{tail}")
        kept.append(lines[found])
        start = found + 1
    return command(case, pipeline), kept


def block(case, evidence):
    """The record of a case."""
    shown, lines = evidence
    kind = case.get("kind", "invalid")
    text = "\n".join(case["pipelines"])
    text += f"\n= {kind}: {case['why']}\n$ {shown}\n"
    text += "".join(f"> {line}\n" for line in lines)
    return text


def main():
    wanted = set(sys.argv[1:])
    cases = [case for case in CASES if not wanted or wanted & set(case["pipelines"])]
    missing = wanted - {pipeline for case in cases for pipeline in case["pipelines"]}
    if missing:
        raise SystemExit(f"no case names {', '.join(sorted(missing))}")
    blocks = {}
    if wanted and OUTPUT.exists():
        for text in OUTPUT.read_text().split("\n\n")[1:]:
            if text.strip():
                blocks[text.split("\n", 1)[0]] = text.strip("\n") + "\n"
    failed = []
    with concurrent.futures.ThreadPoolExecutor(JOBS) as pool:
        futures = [(case, pool.submit(run, case)) for case in cases]
        for case, future in futures:
            try:
                blocks[case["pipelines"][0]] = block(case, future.result())
            except SystemExit as error:
                failed.append(str(error))
    header = (
        "# Why each nf-core pipeline that fails in run_test_workflows.txt, and each WorkflowHub\n"
        "# workflow that fails in run_workflowhub.txt, cannot run, and how Nextflow\n"
        f"# {VERSION} fails on it, recorded by run_evidence.py. Each block gives the\n"
        "# pipelines, then `=`, the kind (`invalid`, `missing` or `unsupported (decision N)`)\n"
        "# and why in one plain sentence, then what Nextflow was asked (`$`, with `NXF_VER` for\n"
        "# the earlier release that a pipeline names) and the lines of its output that show it\n"
        "# (`>`). Tools that the pipelines call are stood in by a shell function that prints\n"
        "# `1.0`.\n"
    )
    records = "\n".join(blocks[name] for name in sorted(blocks))
    OUTPUT.write_text(header + "\n" + records)
    if failed:
        raise SystemExit("\n\n".join(failed))


if __name__ == "__main__":
    main()

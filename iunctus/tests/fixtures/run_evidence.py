#!/usr/bin/env python3
# Records how Nextflow 26.04.6 fails on the nf-core pipelines that fail in
# run_test_workflows.txt, into run_evidence.txt next to this script. The run test checks that
# every pipeline that fails there is listed here, so a pipeline may only fail when Nextflow
# fails on it too, or when iunctus leaves the feature out on purpose.
#
# Run it from the repository root: iunctus/tests/fixtures/run_evidence.py [pipeline...]
# With pipelines, it records only their cases and keeps the other records. It reads the
# pipelines of `just test-workflows` (NF_CORE_TEST_WORKFLOWS) and needs `nextflow` 26.04.6 on
# the PATH. Each pipeline runs in a copy, from another folder, as the run test runs it:
# `nextflow run <pipeline> -profile test -stub-run --outdir <out>`. The machine has none of
# the tools of the pipelines, so a shell function stands in for each missing command: it
# prints `1.0`, which stubs take for a version, and succeeds. `SHOW=1` prints the whole output
# of each run.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import concurrent.futures
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HOME = Path.home() / ".cache/iunctus/test-workflows"
PIPELINES = Path(os.environ.get("NF_CORE_TEST_WORKFLOWS") or HOME / "nf-core-text") / "pipelines"
OUTPUT = Path(__file__).with_name("run_evidence.txt")
# The Java libraries that Nextflow releases before 24 shipped, which the `lib/` classes of older
# pipelines use, like `NfcoreSchema` with org.json and everit (decision 14).
JARS = Path.home() / ".nextflow/plugins/nf-validation-1.1.3/lib"
VERSION = "26.04.6"
TIMEOUT = 1200
JOBS = 4

# Each case says why the pipelines cannot run, in one plain sentence, and how Nextflow shows
# it: `expect` has patterns of the lines of the output to keep, each the first line after the
# one before that matches. The kind is `invalid` (the pipeline itself is broken) unless the case
# gives `kind`: `missing` (it needs something that is not public or not in its repository) or
# `unsupported (decision N)` (iunctus leaves the feature out on purpose, as decision N of
# docs/decisions.md says). A case may give `parser: "v2"` for the strict parser (the legacy
# parser is the default, as iunctus accepts legacy syntax), `jars: True` to put the old Java
# libraries into `lib/`, `args` for more arguments of the run, where `{launch}` and
# `{pipeline}` name the folders of the run, `config` for settings of an extra config file
# (`-c extra.config`), and `tools` for commands that print a given text, like a version that a
# stub reads. A case whose failure Nextflow does not reach, because it stops earlier at a tool
# that this machine lacks, gives `reproduce`: the files of a small script with the failing part
# of the pipeline, which Nextflow runs instead.
CASES = [
    {
        "pipelines": ["alleleexpression"],
        "why": "the test profile gives no `--input` samplesheet, which the pipeline requires",
        "expect": ["Input samplesheet not specified!"],
    },
    {
        "pipelines": ["ampliseq"],
        "why": "an output of CUTADAPT reads `outformat`, which only its script sets, so a stub "
        "run fails",
        "expect": ["No such variable: outformat", "Check script .*cutadapt/main.nf"],
    },
    {
        "pipelines": ["bacmodel"],
        "why": "the BACMODEL_SUMMARY stub makes an empty versions.yml, which "
        "processVersionsFromYAML reads as null",
        "expect": [
            r"Cannot invoke method collectEntries\(\) on null object",
            "Check script .*utils_nfcore_pipeline/main.nf",
        ],
    },
    {
        "pipelines": ["cutandrun", "ssds"],
        "why": "an output of its TRIMGALORE ends with `emit: html optional true`, without a "
        "comma, which Nextflow reads as calling `optional` on null",
        "expect": [r"Cannot invoke method optional\(\) on null object"],
    },
    {
        "pipelines": ["daamicrobiome"],
        "why": "the test profile gives lists for params that the schema types as text",
        "expect": [
            "Validation of pipeline parameters failed!",
            r"--sim_p_da .*should be \[string\]",
        ],
    },
    {
        "pipelines": ["deepmodeloptim"],
        "why": "CUSTOM_MODIFY_MODEL_CONFIG emits `meta_updated`, which only its script sets, so "
        "a stub run fails",
        "expect": [
            "Error executing process > .*CUSTOM_MODIFY_MODEL_CONFIG",
            "Missing value declared as output parameter: meta_updated",
        ],
    },
    {
        "pipelines": ["denovotranscript"],
        "why": "the FASTP stub makes an empty JSON report, which the pipeline parses",
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
        "why": "main.nf gives PIPELINE_INITIALISATION 6 of its 8 inputs",
        "expect": ["declares 8 input channels but 6 were given"],
    },
    {
        "pipelines": ["genomeassembler"],
        "why": "the FASTPLONG stub makes an empty JSON report, which the pipeline parses",
        "expect": ["Unable to determine the current character", "Check script .*prepare/main.nf"],
    },
    {
        "pipelines": ["genomicrelatedness"],
        "why": "the test profile sets neither `--input` nor `--fasta`, which the schema requires",
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
        "why": "the test samplesheet has no column `input1`, which the pipeline requires",
        "expect": ["a column named input1"],
    },
    {
        "pipelines": ["mag"],
        "why": "an output of UNTAR reads `output_dir`, which only its script sets, so a stub "
        "run fails",
        "expect": ["No such variable: output_dir", "Check script .*untar/main.nf"],
    },
    {
        "pipelines": ["magmap"],
        "why": "the CHECK_DUPLICATES stub reads `outfile`, which only its script sets",
        "expect": ["Error executing process > .*CHECK_DUPLICATES", "No such variable: outfile"],
    },
    {
        "pipelines": ["marsseq"],
        "why": "the test profile reads the genome from the output folder of an earlier run",
        "expect": ["No such file or directory: <out>/references/mm10/GRCm39.fa"],
    },
    {
        "pipelines": ["mcmicro"],
        "why": "the PRELUDE_MULTI_SUMMARY stub makes an empty error table, which the pipeline "
        "takes as a failed quality check",
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
        "why": "the PORECHOP stub gzips a file onto its own input, which gzip refuses",
        "expect": [
            "Error executing process > .*PORECHOP_PORECHOP",
            "gzip: .* already exists",
        ],
    },
    {
        "pipelines": ["mspepid"],
        "why": "the COMETCONFIG stub reads `params_file`, which only its script sets",
        "expect": ["Error executing process > .*COMETCONFIG", "No such variable: params_file"],
    },
    {
        "pipelines": ["references"],
        "why": "main.nf uses the `publish:` section of the workflow outputs of Nextflow 24.10, "
        "which Nextflow 26.04.6 no longer accepts",
        "expect": ["Invalid workflow publish statement"],
    },
    {
        "pipelines": ["scdownstream"],
        "why": "the CELLDEX_FETCHREFERENCE stub makes an empty versions.yml, which "
        "processVersionsFromYAML reads as null",
        "expect": [
            r"Cannot invoke method collectEntries\(\) on null object",
            "Check script .*utils_nfcore_pipeline/main.nf",
        ],
    },
    {
        "pipelines": ["variantcatalogue"],
        "jars": True,
        "why": "the test profile names assets/samplesheet.csv relative to the folder where the "
        "run starts, so it only runs from the folder of the pipeline",
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
        "why": "the MAJORITY_VOTE stub makes no `mirna.targets.tsv`, which the process declares "
        "(shown with a small script, as Nextflow first stops at SEQKIT_SPLIT, which has no "
        "stub and needs seqkit)",
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
        "why": "the SOURCEPREDICT stub makes `*.sourcepredict.csv`, not the "
        "`*.report.sourcepredict.csv` that the process declares (shown with a small script, as "
        "Nextflow first stops at DAMAGEPROFILER, which has no stub and needs damageprofiler)",
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
        "why": "the SAMTOOLS_IDXSTATS stub makes an empty file, and the pipeline reads the read "
        "counts of its first line",
        "tools": {"tabix": "Version: 1.21"},
        "expect": [r"Cannot invoke method getAt\(\) on null object"],
    },
    {
        "pipelines": ["epigenomesegmentation"],
        "why": "the CUSTOM_FILTERCHROMSIZES stub makes `mm10.chrom.sizes_V2`, not the "
        "`mm10.chrom.sizes.V2` that the process declares",
        "expect": [r"Missing output file\(s\) `mm10.chrom.sizes.V2`"],
    },
    {
        "pipelines": ["nanostring"],
        "why": "the CREATE_ANNOTATED_TABLES stub makes `mqc.tsv`, which the declared "
        "`*_mqc.tsv` does not match",
        "expect": [r"Missing output file\(s\) `\*_mqc.tsv`"],
    },
    {
        "pipelines": ["pairgenomealign"],
        "why": "the LAST_SPLIT stub makes no `*.matrix.txt`, which the process declares",
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
        "why": "the FREYJA_UPDATE stub makes no `usher_barcodes.feather`, which the process "
        "declares",
        "expect": [r"Missing output file\(s\) `freyja_db/usher_barcodes.feather`"],
    },
    {
        "pipelines": ["crisprvar", "vipr"],
        "why": "the pipeline has no `test` profile",
        "expect": ["Unknown configuration profile: 'test'"],
    },
    {
        "pipelines": ["oncoanalyser"],
        "why": "the CUSTOM_EXTRACTTARBALL stub makes an empty folder, and the pipeline checks "
        "that the reference files in it exist (shown with a small script, as Nextflow first "
        "downloads tens of gigabytes of reference data here)",
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
        "why": "the pipeline reads the result of `subscribe`, which Nextflow 26.04.6 gives as "
        "null; iunctus gives the channel, as Nextflow up to 25.10 did, and then fails as those "
        "did, because the BAMCHREXTRACT stub makes an empty list of contigs",
        "expect": [r"Cannot invoke method toList\(\) on null object"],
    },
    {
        "pipelines": ["rnadnavar"],
        "why": "the STAR_ALIGN stub makes three files that `*d.out.bam` matches, and the "
        "`ext.prefix` of SAMTOOLS_SORT then calls `tokenize` on the list of their names (shown "
        "with a small script, as Nextflow first stops at TABIX_DBSNP, which needs bgzip)",
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
        "parser": "v2",
        "config": "timeline.overwrite = true\nreport.overwrite = true\ntrace.overwrite = true\ndag.overwrite = true\n",
        "why": "SAMTOOLS_SORT gets a tuple `[[:], []]`, which does not match its input "
        "declaration",
        "tools": {"STAR": "versionGenome 2.7.4a"},
        "expect": [
            "Execution aborted due to an unexpected error",
            r"error \[nextflow.exception.ProcessUnrecoverableException\]: Input tuple does not "
            "match tuple declaration",
        ],
    },
    {
        "pipelines": ["scrnaseq"],
        "why": "the MTX_TO_H5AD stub makes an empty versions.yml, which "
        "processVersionsFromYAML reads as null (shown with a small script, as Nextflow first "
        "stops at STAR_ALIGN, which has no stub and needs STAR)",
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
    if case.get("parser", "v1") == "v1":
        words.append("NXF_SYNTAX_PARSER=v1")
    if "reproduce" in case:
        words += ["nextflow", "run", f"<reproduction of {pipeline}>/main.nf", "-stub-run"]
    else:
        words += ["nextflow", "run", pipeline, "-profile", "test", "-stub-run"]
        words += ["--outdir", "<out>"]
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


def execute(case):
    """Runs a case; gives the output with the paths of the run replaced by names like
    `<pipeline>`."""
    pipeline = case["pipelines"][0]
    with tempfile.TemporaryDirectory(prefix="run-evidence-") as temporary:
        temporary = Path(temporary)
        copy = temporary / pipeline
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
                PIPELINES / pipeline,
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
            arguments = ["nextflow", "run", str(copy), "-profile", "test", "-stub-run"]
            arguments += ["--outdir", str(out)] + extra
            if "config" in case:
                config = temporary / "extra.config"
                config.write_text(case["config"])
                arguments += ["-c", str(config)]
        environment_file, folder = stand_ins(temporary, case)
        environment = dict(os.environ)
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
        text = re.sub(r"\[[0-9a-f]{2}/[0-9a-f]{6}\]", "[<task>]", text)
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
        "# Why each nf-core pipeline that fails in run_test_workflows.txt cannot run, and how\n"
        f"# Nextflow {VERSION} fails on it, recorded by run_evidence.py. Each block gives the\n"
        "# pipelines, then `=`, the kind (`invalid`, `missing` or `unsupported (decision N)`)\n"
        "# and why in one plain sentence, then what Nextflow was asked (`$`) and the lines of\n"
        "# its output that show it (`>`). Tools that the pipelines call are stood in by a\n"
        "# shell function that prints `1.0`.\n"
    )
    records = "\n".join(blocks[name] for name in sorted(blocks))
    OUTPUT.write_text(header + "\n" + records)
    if failed:
        raise SystemExit("\n\n".join(failed))


if __name__ == "__main__":
    main()

#!/usr/bin/env -S uv run -q --with cwltool==3.3.20260925135507 python3
# Records how cwltool fails on the CWL documents that fail in graph_test_workflows.txt, into
# graph_evidence.txt next to this script. The graph test-workflows test checks that every
# document that fails there is listed here, so a document may only fail when cwltool fails on it
# too.
#
# Run it from the repository root: cwl-runtime/tests/fixtures/graph_evidence.py
# It reads the test workflows of `just cwl-test-workflows` (CWL_TEST_WORKFLOWS) and the data
# files of `just cwl-conformance-data` (CWL_CONFORMANCE_DATA), and needs Node.js for cwltool's
# JavaScript.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HOME = Path.home() / ".cache/iunctus/test-workflows"
TEST_WORKFLOWS = Path(os.environ.get("CWL_TEST_WORKFLOWS") or HOME / "cwl")
DATA = Path(os.environ.get("CWL_CONFORMANCE_DATA") or HOME / "cwl-conformance-data")
OUTPUT = Path(__file__).with_name("graph_evidence.txt")
# cwltool wraps its messages to the width of the terminal.
os.environ["COLUMNS"] = "1000"

GENOME = "realworld/collections/github.com__genome__analysis-workflows/definitions/"
PGAP = "realworld/collections/github.com__ncbi__pgap/"
UNLOCK = "realworld/sources/gitlab.com__m-unlock__cwl/tools/"
WUR = "realworld/sources/git.wur.nl__unlock__cwl/cwl/"
DTGEO = "realworld/sources/gitlab.com__dtgeo__metadata__cwl-ro-crate/"


def file(path):
    return {"class": "File", "path": path}


def directory(path):
    return {"class": "Directory", "path": path}


# Each case says why the documents fail, and how cwltool shows it: `run` runs cwltool on a
# document with a job (a path in the test workflows, or values whose files are made empty);
# `collect` loads a tool with cwltool and collects one output from an empty output directory,
# as after a run that made nothing; `evaluate` evaluates one expression of a document with
# cwltool's evaluator, for steps of workflows that cwltool will not run.
CASES = [
    {
        "why": "the tool names `inputs.in2`, which it does not declare, in its arguments",
        "run": "conformance/v1.2/tests/fail-unspecified-input.cwl",
        "job": {"in": "hello"},
        "documents": [
            "conformance/v1.0/v1.0/v1.0/fail-unspecified-input.cwl",
            "conformance/v1.1/tests/fail-unspecified-input.cwl",
            "conformance/v1.2/tests/fail-unspecified-input.cwl",
        ],
    },
    {
        "why": "a step input that the tool does not declare reaches it as `inputs.in2` "
        "(should_fail conformance test fail-unconnected)",
        "run": "conformance/v1.2/tests/fail-unconnected.cwl",
        "job": "conformance/v1.2/tests/empty.json",
        "documents": [
            "conformance/v1.0/v1.0/v1.0/fail-unconnected.cwl",
            "conformance/v1.1/tests/fail-unconnected.cwl",
            "conformance/v1.2/tests/fail-unconnected.cwl",
        ],
    },
    {
        "why": "the glob is an absolute path (should_fail conformance test)",
        "collect": "conformance/v1.2/tests/glob-path-error.cwl",
        "output": "OUTPUT",
        "job": {},
        "documents": [
            "conformance/v1.1/tests/glob-path-error.cwl",
            "conformance/v1.2/tests/glob-path-error.cwl",
        ],
    },
    {
        "why": "the time limit is negative (should_fail conformance test timelimit_invalid)",
        "run": "conformance/v1.2/tests/timelimit2.cwl",
        "job": "conformance/v1.2/tests/empty.json",
        "documents": [
            "conformance/v1.1/tests/timelimit2.cwl",
            "conformance/v1.2/tests/timelimit2.cwl",
        ],
    },
    {
        "why": "`when` gives a number (should_fail conformance test conditionals_non_boolean_fail)",
        "run": "conformance/v1.2/tests/conditionals/cond-wf-012.cwl",
        "job": "conformance/v1.2/tests/conditionals/val.1.job.yaml",
        "documents": ["conformance/v1.2/tests/conditionals/cond-wf-012.cwl"],
    },
    {
        "why": "`when` gives a number (should_fail conformance test "
        "conditionals_non_boolean_fail_nojs)",
        "run": "conformance/v1.2/tests/conditionals/cond-wf-012_nojs.cwl",
        "job": "conformance/v1.2/tests/empty.json",
        "documents": ["conformance/v1.2/tests/conditionals/cond-wf-012_nojs.cwl"],
    },
    {
        "why": "an absolute InitialWorkDir name without DockerRequirement (should_fail "
        "conformance test)",
        "run": "conformance/v1.2/tests/iwd/iwd-container-entryname2.cwl",
        "job": "conformance/v1.2/tests/loadContents/input.yml",
        "documents": ["conformance/v1.2/tests/iwd/iwd-container-entryname2.cwl"],
    },
    {
        "why": "an absolute InitialWorkDir name with DockerRequirement only as a hint "
        "(should_fail conformance test)",
        "run": "conformance/v1.2/tests/iwd/iwd-container-entryname3.cwl",
        "job": "conformance/v1.2/tests/loadContents/input.yml",
        "documents": ["conformance/v1.2/tests/iwd/iwd-container-entryname3.cwl"],
    },
    {
        "why": "an InitialWorkDir name leaves the output directory (should_fail conformance test)",
        "run": "conformance/v1.2/tests/iwd/iwd-container-entryname4.cwl",
        "job": "conformance/v1.2/tests/loadContents/input.yml",
        "documents": ["conformance/v1.2/tests/iwd/iwd-container-entryname4.cwl"],
    },
    {
        "why": "the output reads `null.something` (should_fail conformance test params_broken_null)",
        "run": "conformance/v1.2/tests/params_broken_null.cwl",
        "job": "conformance/v1.2/tests/empty.json",
        "documents": ["conformance/v1.2/tests/params_broken_null.cwl"],
    },
    {
        "why": "the output reads `length` of a number (should_fail conformance test "
        "params_broken_length_of_non_list)",
        "run": "conformance/v1.2/tests/params_broken_length_of_non_list.cwl",
        "job": "conformance/v1.2/tests/empty.json",
        "documents": ["conformance/v1.2/tests/params_broken_length_of_non_list.cwl"],
    },
    {
        "why": "the script that the varscan tool writes holds `${HOME}`, which runs as "
        "JavaScript under the InlineJavascriptRequirement of the enclosing "
        "subworkflows/varscan_pre_and_post_processing.cwl",
        "evaluate": GENOME + "tools/varscan_somatic.cwl",
        "field": ["requirements", "InitialWorkDirRequirement", "listing", 0, "entry"],
        "javascript": True,
        "documents": [
            GENOME + "pipelines/" + name
            for name in [
                "aml_trio_cle.cwl",
                "aml_trio_cle_gathered.cwl",
                "detect_variants.cwl",
                "detect_variants_nonhuman.cwl",
                "detect_variants_wgs.cwl",
                "detect_variants_wgs_nonhuman.cwl",
                "somatic_exome.cwl",
                "somatic_exome_cle.cwl",
                "somatic_exome_cle_gathered.cwl",
                "somatic_exome_gathered.cwl",
                "somatic_exome_nonhuman.cwl",
                "somatic_wgs.cwl",
                "somatic_wgs_nonhuman.cwl",
            ]
        ]
        + [GENOME + "subworkflows/varscan_pre_and_post_processing.cwl"],
    },
    {
        "why": "the output glob calls `inputs.prefix.equals`, which JavaScript strings do not have",
        "collect": GENOME + "tools/bam_readcount.cwl",
        "output": "indel_bam_readcount_tsv",
        "job": {
            "vcf": file("calls.vcf"),
            "sample": "S1",
            "reference_fasta": "reference.fa",
            "bam": {**file("reads.bam"), "secondaryFiles": [file("reads.bam.bai")]},
        },
        "documents": [
            GENOME + "tools/bam_readcount.cwl",
            GENOME + "subworkflows/bam_readcount.cwl",
            GENOME + "subworkflows/pvacseq.cwl",
            GENOME + "subworkflows/vcf_eval_cle_gold.cwl",
            GENOME + "subworkflows/vcf_eval_concordance.cwl",
        ],
    },
    {
        "why": "the step gives a float (`$(self.contamination)` of a float field) to the "
        "`string?` input `contamination_fraction` of gatk_haplotype_caller.cwl",
        "run": GENOME + "tools/gatk_haplotype_caller.cwl",
        "job": {
            "reference": "reference.fa",
            "intervals": ["chr1"],
            "gvcf_gq_bands": ["10"],
            "emit_reference_confidence": "GVCF",
            "bam": file("reads.bam"),
            "contamination_fraction": 0.02,
        },
        "documents": [
            GENOME + "pipelines/downsample_and_recall.cwl",
            GENOME + "pipelines/gathered_downsample_and_recall.cwl",
        ],
    },
    {
        "why": "the step default of `filtering_frequency` is the string \"0.05\", but the tool "
        "takes a double",
        "run": GENOME + "tools/annotsv_filter.cwl",
        "job": {"annotsv_tsv": file("annotsv.tsv"), "filtering_frequency": "0.05"},
        "documents": [
            GENOME + "pipelines/germline_wgs.cwl",
            GENOME + "subworkflows/merge_svs.cwl",
            GENOME + "subworkflows/single_sample_sv_callers.cwl",
        ],
    },
    {
        "why": "the default of the int input `chim_junction_overhang_min` is the string \"12\", "
        "and the steps that run the tool do not set it",
        "run": GENOME + "tools/star_align_fusion.cwl",
        "job": {
            "star_genome_dir": "genome",
            "outsam_attrrg_line": ["ID:1"],
            "fastq": [file("reads_1.fastq")],
            "fastq2": [file("reads_2.fastq")],
        },
        "documents": [
            GENOME + "pipelines/immuno.cwl",
            GENOME + "pipelines/rnaseq_star_fusion.cwl",
            GENOME + "pipelines/rnaseq_star_fusion_with_xenosplit.cwl",
        ],
    },
    {
        "why": "vt_decompose.cwl needs the `.tbi` index of its VCF, but the output it gets "
        "(combined_vcf of germline_combine_variants.cwl) declares none",
        "run": GENOME + "tools/vt_decompose.cwl",
        "job": {"vcf": file("combined.vcf.gz")},
        "documents": [
            GENOME + "pipelines/tumor_only_detect_variants.cwl",
            GENOME + "pipelines/tumor_only_exome.cwl",
            GENOME + "pipelines/tumor_only_wgs.cwl",
        ],
    },
    {
        "why": "the output reads `outputs.ofull_file`, but there is no `outputs` in CWL "
        "expressions",
        "collect": PGAP + "progs/cache_kmer.cwl",
        "output": "out_kmer_file_list",
        "job": {"kmer_cache_path": directory("cache")},
        "documents": [
            PGAP + "progs/cache_kmer.cwl",
            PGAP + "task_types/tt_kmer_cache_retrieve.cwl",
            PGAP + "task_types/tt_kmer_cache_store.cwl",
        ],
    },
    {
        "why": "the InitialWorkDir script reads `inputs.kmer_dir.listing`, but a CWL v1.2 tool "
        "without LoadListingRequirement gets no listing",
        "run": PGAP + "progs/kmer_files2sqlite.cwl",
        "job": {"kmer_dir": directory("kmers")},
        "documents": [PGAP + "bacterial_kmer/wf_bacterial_kmer.cwl"],
    },
    {
        "why": "the ResourceRequirement reads `inputs.readsFA`, which the tool does not declare",
        "run": "realworld/collections/github.com__common-workflow-library__bio-cwl-tools/"
        "minimap2/minimap2_sam.cwl",
        "job": {"target": file("target.fa"), "query": file("query.fa")},
        "documents": [
            "realworld/collections/github.com__common-workflow-library__bio-cwl-tools/"
            "minimap2/minimap2_sam.cwl",
        ],
    },
    {
        "why": "the ResourceRequirement reads `inputs.readsFA`, which the tool does not declare",
        "run": "realworld/sources/github.com__arvados__bh20-seq-resource/workflows/tools/"
        "minimap2/minimap2_sam.cwl",
        "job": {"target": file("target.fa"), "query": file("query.fa")},
        "documents": [
            "realworld/sources/github.com__arvados__bh20-seq-resource/workflows/tools/"
            "minimap2/minimap2_sam.cwl",
        ],
    },
    {
        "why": "an InitialWorkDir entry is the text `inputs.genome_folder` without `$()` and "
        "without entryname",
        "run": "realworld/collections/github.com__common-workflow-library__bio-cwl-tools/"
        "bismark/bismark_prepare_genome.cwl",
        "job": {"genome_folder": directory("genome")},
        "documents": [
            "realworld/collections/github.com__common-workflow-library__bio-cwl-tools/"
            "bismark/bismark_prepare_genome.cwl",
        ],
    },
    {
        "why": "with the default `base_file_name`, the arguments read `inputs.input_bam`, which "
        "the tool does not declare (it declares `input_bams`)",
        "evaluate": "realworld/collections/github.com__kids-first__kf-alignment-workflow/dev/"
        "pilot-run/tools/dev/picard_markduplicates_optname.cwl",
        "field": ["arguments", 0, "valueFrom"],
        "inputs": {"input_bams": [file("reads.bam")], "base_file_name": "default"},
        "documents": [
            "realworld/collections/github.com__kids-first__kf-alignment-workflow/dev/"
            "pilot-run/tools/dev/picard_markduplicates_optname.cwl",
        ],
    },
    {
        "why": "the library function `default_output_filename` reads `inputs.filelist`, an "
        "input that is commented out, so it gives null, which the arguments split",
        "run": UNLOCK + "bowtie2/bowtie2_align.cwl",
        "job": {"forward_reads": file("reads_1.fastq")},
        "documents": [UNLOCK + "bowtie2/bowtie2_align.cwl"],
    },
    {
        "why": "the library function `default_output_filename` reads `inputs.filelist`, an "
        "input that is commented out, so it gives null, which the arguments split",
        "run": WUR + "bowtie2/bowtie2_align.cwl",
        "job": {"forward_reads": file("reads_1.fastq")},
        "documents": [WUR + "bowtie2/bowtie2_align.cwl"],
    },
    {
        "why": "the arguments read `inputs.inputFile`, which the tool does not declare",
        "run": UNLOCK + "sapp/eggnog.cwl",
        "job": {"input_fasta": file("proteins.fasta")},
        "documents": [UNLOCK + "sapp/eggnog.cwl"],
    },
    {
        "why": "the glob is `$(inputs.output_dir)`, a Directory, not a string",
        "collect": UNLOCK + "longqc/longqc.cwl",
        "output": "longqc_out",
        "job": {"sampleqc": file("reads.fastq"), "output_dir": directory("out")},
        "documents": [UNLOCK + "longqc/longqc.cwl"],
    },
    {
        "why": "the glob is `$(inputs.output_dir)`, a Directory, not a string",
        "collect": WUR + "longqc/longqc.cwl",
        "output": "longqc_out",
        "job": {"sampleqc": file("reads.fastq"), "output_dir": directory("out")},
        "documents": [WUR + "longqc/longqc.cwl"],
    },
    {
        "why": "`when` reads `inputs.useMLStep`, which is no input of the step (cwltool runs no "
        "workflow with an Operation, so this is its evaluator)",
        "evaluate": DTGEO + "DTC76/WF7601/WF7601.cwl",
        "field": ["steps", "ST760103", "when"],
        "inputs": {"PretrainedMLModel": file("model.pt")},
        "documents": [
            DTGEO + "DTC76/WF7601/WF7601.cwl",
            "realworld/workflowhub/1231/DTC76/WF7601/WF7601.cwl",
            "realworld/workflowhub/1728/DTC76/WF7601/WF7601.cwl",
            "realworld/workflowhub/1786/DTC76/WF7601/WF7601.cwl",
            "realworld/workflowhub/1997/WF7601.cwl",
        ],
    },
    {
        "why": "`when` reads `inputs.useMLStep`, which is no input of the step (cwltool runs no "
        "workflow with an Operation, so this is its evaluator)",
        "evaluate": DTGEO + "DTC76/WF7602/WF7602.cwl",
        "field": ["steps", "ST760103", "when"],
        "inputs": {"PretrainedMLModel": file("model.pt")},
        "documents": [
            DTGEO + "DTC76/WF7602/WF7602.cwl",
            "realworld/workflowhub/1231/DTC76/WF7602/WF7602.cwl",
            "realworld/workflowhub/1728/DTC76/WF7602/WF7602.cwl",
            "realworld/workflowhub/1786/DTC76/WF7602/WF7602.cwl",
            "realworld/workflowhub/1998/WF7602.cwl",
        ],
    },
    {
        "why": "`when` reads `inputs.Run_User_Model`, which is no input of the step (cwltool "
        "runs no workflow with an Operation, so this is its evaluator)",
        "evaluate": DTGEO + "DTC75/WF7501/WF7501.cwl",
        "field": ["steps", "ST750106", "when"],
        "inputs": {"UserModel": file("model.dat")},
        "documents": [
            DTGEO + "DTC75/WF7501/WF7501.cwl",
            "realworld/workflowhub/1231/DTC75/WF7501/WF7501.cwl",
            "realworld/workflowhub/1333/WF7501.cwl",
            "realworld/workflowhub/1728/DTC75/WF7501/WF7501.cwl",
            "realworld/workflowhub/1786/DTC75/WF7501/WF7501.cwl",
        ],
    },
    {
        "why": "the source of the step is the output SOURCE_PARAMETERS of a subworkflow that "
        "has no outputSource, so `self` is null, and `valueFrom` is JavaScript without "
        "InlineJavascriptRequirement (cwltool runs no workflow with an Operation, so this "
        "is its evaluator)",
        "evaluate": DTGEO + "DTC75/WF7502/WF7502.cwl",
        "field": ["steps", "ST750207", "in", "DAL", "valueFrom"],
        "inputs": {},
        "documents": [
            DTGEO + "DTC75/WF7502/WF7502.cwl",
            "realworld/workflowhub/1231/DTC75/WF7502/WF7502.cwl",
            "realworld/workflowhub/1332/WF7502.cwl",
            "realworld/workflowhub/1728/DTC75/WF7502/WF7502.cwl",
            "realworld/workflowhub/1786/DTC75/WF7502/WF7502.cwl",
        ],
    },
]


def make_files(value, folder):
    """Makes the files and folders a job names, empty, below `folder`."""
    if isinstance(value, list):
        for item in value:
            make_files(item, folder)
    elif isinstance(value, dict):
        if value.get("class") == "File" and "path" in value:
            target = folder / value["path"]
            target.parent.mkdir(parents=True, exist_ok=True)
            target.touch()
        elif value.get("class") == "Directory" and "path" in value:
            (folder / value["path"]).mkdir(parents=True, exist_ok=True)
        for item in value.values():
            make_files(item, folder)


def located(value, work):
    """A job with its files made below `work` and named by `file:` locations, as cwltool's job
    loader gives them."""
    make_files(value, work)
    if isinstance(value, list):
        return [located(item, work) for item in value]
    if not isinstance(value, dict):
        return value
    value = {key: located(item, work) for key, item in value.items()}
    if value.get("class") in ("File", "Directory") and "path" in value:
        value["location"] = (work / value.pop("path")).as_uri()
    return value


def link_tree(source, target):
    """Links every file below `source` into the same place below `target`."""
    for root, folders, names in os.walk(source):
        folders[:] = [name for name in folders if name != ".git"]
        place = target / Path(root).relative_to(source)
        place.mkdir(parents=True, exist_ok=True)
        for name in names:
            link = place / name
            if not link.exists():
                link.symlink_to(Path(root) / name)


# Lines of Python tracebacks, of the JavaScript that cwltool shows, and of Node.js stacks.
NOISE = re.compile(
    r'(INFO|WARNING|DEBUG)\b|Traceback|File "|[~^ ]+$|raise |\.\.\.<|\)$|\d{2,3} |at |'
    r"\(node:|stdout was|stderr was|evalmachine|\(function|\{$|\}$|\"\w+\": |"
    r"Expecting value|script was:|\{\}$"
)


def clean(text, work):
    """cwltool's messages without colours, log lines, traces and local paths, from the first
    error on."""
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    for path, name in [(str(work) + "/merged/", ""), (str(work), "<tmp>"), (str(TEST_WORKFLOWS), "<test-workflows>")]:
        text = text.replace(path, name)
    text = text.replace("merged/conformance/", "conformance/")
    text = re.sub(r"(\.\./)*\.\.<tmp>", "<tmp>", text)
    # Lines after the first carry the place in the document again; the first keeps it.
    lines = [re.sub(r"\s+", " ", line.strip()) for line in text.splitlines()]
    lines = [lines[0]] + [re.sub(r"^\S+\.cwl:\d+:\d+: ", "", line) for line in lines[1:]]
    lines = [line for line in lines if line and not NOISE.match(line)]
    first = next((index for index, line in enumerate(lines) if "ERROR" in line), 0)
    return lines[first:]


def document(path, work):
    """The path cwltool reads a document of the test workflows from: conformance documents from
    a tree that also holds the data files of their jobs."""
    if path.startswith("conformance/"):
        merged = work / "merged"
        if not merged.exists():
            link_tree(TEST_WORKFLOWS / "conformance", merged / "conformance")
            link_tree(DATA / "conformance", merged / "conformance")
        return merged / path
    return TEST_WORKFLOWS / path


def job_file(case, work):
    job = case["job"]
    if isinstance(job, str):
        return document(job, work)
    make_files(job, work)
    path = work / "job.json"
    path.write_text(json.dumps(job))
    return path


def run(case, work):
    job = job_file(case, work)
    command = ["cwltool", "--no-container", "--outdir", str(work / "out")]
    command += [str(document(case["run"], work)), str(job)]
    shown = ["cwltool --no-container", case["run"], "<job>"]
    # Paths in messages are relative to the working folder: the test workflows, or for
    # conformance documents the folder with their data files.
    folder = work if case["run"].startswith("conformance/") else TEST_WORKFLOWS
    result = subprocess.run(command, cwd=folder, capture_output=True, text=True, timeout=600)
    return shown, clean(result.stderr + result.stdout, work)


def loaded(path):
    from cwltool.context import LoadingContext
    from cwltool.load_tool import load_tool
    from cwltool.workflow import default_make_tool

    context = LoadingContext()
    context.construct_tool_object = default_make_tool
    return load_tool(str(TEST_WORKFLOWS / path), context)


def collect(case, work):
    """Collects one output of a tool from an empty output directory, as cwltool does after a
    run of the tool."""
    from cwltool.context import RuntimeContext
    from cwltool.stdfsaccess import StdFsAccess

    job = located(case["job"], work)
    tool = loaded(case["collect"])
    outdir = work / "outdir"
    outdir.mkdir()
    context = RuntimeContext({"outdir": str(outdir), "tmpdir": str(work / "tmp")})
    shown = ["cwltool collect_output", case["collect"], "output", case["output"]]
    try:
        builder = tool._init_job(job, context)
        builder.outdir = str(outdir)
        schema = next(
            output
            for output in tool.tool["outputs"]
            if output["id"].rsplit("#", 1)[-1].rsplit("/", 1)[-1] == case["output"]
        )
        value = tool.collect_output(schema, builder, str(outdir), StdFsAccess(""))
        return shown, [f"no error: {json.dumps(value)}"]
    except Exception as error:
        return shown, clean(f"{type(error).__name__}: {error}", work)


def evaluate(case, work):
    """Evaluates one expression of a document with cwltool's evaluator, with the requirements
    of the document, or with InlineJavascriptRequirement when the enclosing workflow gives it."""
    from cwl_utils.expression import do_eval

    tool = loaded(case["evaluate"])
    node = tool.tool
    for key in case["field"]:
        if isinstance(key, int) or isinstance(node, dict):
            node = node[key]
        else:
            node = next(
                item
                for item in node
                if item.get("class") == key
                or str(item.get("id", "")).rsplit("#", 1)[-1].rsplit("/", 1)[-1] == key
            )
    requirements = list(tool.tool.get("requirements", []))
    if case.get("javascript"):
        requirements.append({"class": "InlineJavascriptRequirement"})
    inputs = located(case.get("inputs", {}), work)
    shown = ["cwltool do_eval", case["evaluate"], "/".join(map(str, case["field"]))]
    try:
        value = do_eval(
            node, inputs, requirements, str(work), str(work), {}, strip_whitespace=False
        )
        return shown, [f"no error: {json.dumps(value)}"]
    except Exception as error:
        return shown, clean(f"{type(error).__name__}: {error}", work)


def main():
    if shutil.which("node") is None and shutil.which("nodejs") is None:
        sys.exit("cwltool needs Node.js for JavaScript")
    os.chdir(TEST_WORKFLOWS)
    lines = [
        "# How cwltool fails on each CWL document that fails in graph_test_workflows.txt, recorded by",
        "# graph_evidence.py with cwltool 3.3.20260925135507. Each block gives the documents, the",
        "# reason in plain words, what cwltool was asked, and the first lines of its error.",
    ]
    for case in CASES:
        with tempfile.TemporaryDirectory() as folder:
            work = Path(folder)
            if "run" in case:
                shown, output = run(case, work)
            elif "collect" in case:
                shown, output = collect(case, work)
            else:
                shown, output = evaluate(case, work)
        lines.append("")
        lines.extend(case["documents"])
        lines.append(f"= {case['why']}")
        lines.append("$ " + " ".join(shown))
        lines.extend("> " + line for line in output[:8])
        print(f"{case['documents'][0]}: {output[-1] if output else 'no output'}", file=sys.stderr)
    OUTPUT.write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()

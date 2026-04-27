#!/usr/bin/env -S uv run -q --with cwltool==3.3.20260925135507 python3
# Records how cwltool fails on the CWL documents that fail in graph_corpus.txt, into
# graph_evidence.txt next to this script. The graph corpus test checks that every document that
# fails there is listed here, so a document may only fail when cwltool fails on it too.
#
# Run it from the repository root: cwl-runtime/tests/fixtures/graph_evidence.py
# It reads the corpus of `just cwl-corpus` (CWL_CORPUS) and the data files of
# `just cwl-conformance-data` (CWL_CONFORMANCE_DATA), and needs Node.js for cwltool's JavaScript.
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

HOME = Path.home() / ".cache/iunctus/corpus"
CORPUS = Path(os.environ.get("CWL_CORPUS") or HOME / "cwl")
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
# document with a job (a path in the corpus, or values whose files are made empty);
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
    for path, name in [(str(work) + "/merged/", ""), (str(work), "<tmp>"), (str(CORPUS), "<corpus>")]:
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
    """The path cwltool reads a corpus document from: conformance documents from a tree that
    also holds the data files of their jobs."""
    if path.startswith("conformance/"):
        merged = work / "merged"
        if not merged.exists():
            link_tree(CORPUS / "conformance", merged / "conformance")
            link_tree(DATA / "conformance", merged / "conformance")
        return merged / path
    return CORPUS / path


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
    # Paths in messages are relative to the working folder: the corpus, or for conformance
    # documents the folder with their data files.
    folder = work if case["run"].startswith("conformance/") else CORPUS
    result = subprocess.run(command, cwd=folder, capture_output=True, text=True, timeout=600)
    return shown, clean(result.stderr + result.stdout, work)


def loaded(path):
    from cwltool.context import LoadingContext
    from cwltool.load_tool import load_tool
    from cwltool.workflow import default_make_tool

    context = LoadingContext()
    context.construct_tool_object = default_make_tool
    return load_tool(str(CORPUS / path), context)


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
    os.chdir(CORPUS)
    lines = [
        "# How cwltool fails on each CWL document that fails in graph_corpus.txt, recorded by",
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

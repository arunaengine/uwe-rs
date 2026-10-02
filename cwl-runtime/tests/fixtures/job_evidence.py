#!/usr/bin/env -S uv run -q --with cwltool==3.3.20260925135507 python3
"""Refresh cwltool input-check evidence for failing example jobs."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

from ruamel.yaml import YAML

HOME = Path.home() / ".cache/iunctus/test-workflows"
TEST_WORKFLOWS = Path(os.environ.get("CWL_TEST_WORKFLOWS") or HOME / "cwl")
FIXTURES = Path(__file__).parent
# cwltool wraps its messages to the width of the terminal.
os.environ["COLUMNS"] = "1000"

PGAP = "realworld/collections/github.com__ncbi__pgap/"
MAW = "maw-inputs-mutiple_ara.yaml"

# The kind and the reason of pairs whose check by cwltool does not give it, by document or
# job file name. `missing` pairs are checked by a run of cwltool on the job as it is, with a
# stand-in container command.
REASONS = {
    PGAP + "progs/unit_tests/test_kmer_files2sqlite/test.cwl": (
        "invalid",
        "the InitialWorkDir script reads `inputs.kmer_dir.listing`, but a CWL v1.2 tool "
        "without LoadListingRequirement gets no listing",
    ),
    MAW: (
        "missing",
        "the job names data files on the author's computer (/Users/mahnoorzulfiqar/...) and "
        "the scripts Workflow_Python_Script_all.py and Workflow_R_Script_all.r, which the "
        "repository does not have",
    ),
}


def clean(text, work):
    """cwltool's messages without colours, file objects, local paths and places."""
    text = re.sub(r"\x1b\[[0-9;]*m", "", text).replace(str(work), "<tmp>")
    text = text.replace(str(TEST_WORKFLOWS) + "/", "")
    lines = [re.sub(r"\s+", " ", line.strip()) for line in text.splitlines()]
    lines = [re.sub(r"^\S+\.(cwl|ya?ml|json):\d+:\d+: ", "", line) for line in lines]
    noise = re.compile(
        r'(INFO|WARNING|DEBUG)\b|"|\{$|\}$|Traceback|File "|[~^ ]+$|\d{2,3} |at |\(node:|'
        r"Expecting value|script was:|stdout was|stderr was|evalmachine|\(function"
    )
    return [line for line in lines if line and not noise.match(line)]


def stubbed(value, work):
    """The job with every file and directory it names replaced by an empty one below `work`,
    with the same name; other fields, like `format` and `secondaryFiles`, stay."""
    if isinstance(value, list):
        return [stubbed(item, work) for item in value]
    if not isinstance(value, dict):
        return value
    value = {key: stubbed(item, work) for key, item in value.items()}
    kind = value.get("class")
    name = value.get("location") or value.get("path")
    if kind in ("File", "Directory") and isinstance(name, str):
        value.pop("path", None)
        # One stand-in per path of the job, in a folder per folder of the job.
        folder = work / "data" / re.sub(r"[^A-Za-z0-9._-]", "_", str(Path(name).parent))
        target = folder / Path(name).name
        target.parent.mkdir(parents=True, exist_ok=True)
        if kind == "File":
            target.touch()
        else:
            target.mkdir(exist_ok=True)
        value["location"] = target.as_uri()
    return value


def secondary_stubs(tool, values):
    """Makes empty files for the secondary files that the inputs of a tool declare, beside the
    files of the job, as real data has them."""
    for parameter in tool.tool.get("inputs", []):
        name = parameter["id"].rsplit("#", 1)[-1].rsplit("/", 1)[-1]
        patterns = parameter.get("secondaryFiles") or []
        value = values.get(name)
        files = value if isinstance(value, list) else [value]
        for file in files:
            if not (isinstance(file, dict) and file.get("class") == "File"):
                continue
            primary = Path(file["location"].removeprefix("file://"))
            for entry in patterns:
                pattern = entry.get("pattern") if isinstance(entry, dict) else entry
                if not isinstance(pattern, str) or "$" in pattern:
                    continue
                stem = primary.name
                pattern = pattern.rstrip("?")
                while pattern.startswith("^"):
                    stem, pattern = stem.rsplit(".", 1)[0] if "." in stem else stem, pattern[1:]
                (primary.parent / (stem + pattern)).touch()


def check(document, job):
    """cwltool's verdict on a job for a document: its error, or `accepted`."""
    from cwltool.context import LoadingContext, RuntimeContext
    from cwltool.load_tool import load_tool
    from cwltool.workflow import default_make_tool

    context = LoadingContext()
    context.construct_tool_object = default_make_tool
    # As `cwltool --validate --enable-dev` of the loader test, development versions load.
    context.enable_dev = True
    with tempfile.TemporaryDirectory() as folder:
        work = Path(folder)
        text = (TEST_WORKFLOWS / job).read_text()
        values = YAML(typ="safe", pure=True).load(text) or {}
        values = stubbed(values, work)
        try:
            tool = load_tool(str(TEST_WORKFLOWS / document), context)
            secondary_stubs(tool, values)
            runtime = RuntimeContext({"outdir": str(work / "out"), "tmpdir": str(work / "tmp")})
            # As for the process the command line runs, secondary files are looked for.
            runtime.toplevel = True
            tool._init_job(values, runtime)
        except Exception as error:
            return ["job check: " + line for line in clean(f"{type(error).__name__}: {error}", work)[:5]]
        # cwltool accepts the job: then it fails, if at all, when it prepares or runs the tasks.
        path = work / "job.json"
        path.write_text(json.dumps(values))
        return run(document, str(path), work)


# cwltool without containers stops at a DockerRequirement under `requirements`; a stand-in
# container command that does nothing lets it stage the inputs, where missing files show.
NO_CONTAINER = ["--no-container"]
STAND_IN = ["--user-space-docker-cmd", "true"]


def run(document, job, work, flags=NO_CONTAINER):
    """The first lines of the error of cwltool on a document and a job."""
    command = ["cwltool", "--enable-dev", *flags, "--outdir", str(work / "out"), document, job]
    result = subprocess.run(command, capture_output=True, text=True, timeout=600)
    # From the first error on, the errors and the exceptions, without the lines of the
    # tracebacks between them.
    lines = clean(result.stderr, work)
    first = next((index for index, line in enumerate(lines) if "ERROR" in line), len(lines))
    errors = re.compile(r"ERROR|\w+(Error|Exception): ")
    lines = [line for line in lines[first:] if errors.search(line)]
    return ["run: " + line for line in lines[:5]] or ["run: no error"]


def untemplated(value):
    """Whether the job names a file or directory without a location, path or contents, as a
    template that was never filled in does."""
    if isinstance(value, list):
        return any(untemplated(item) for item in value)
    if not isinstance(value, dict):
        return False
    if value.get("class") in ("File", "Directory"):
        if not any(value.get(key) for key in ("location", "path", "contents", "listing")):
            return True
    return any(untemplated(item) for item in value.values())


def names(words):
    """`a`, `a` and `b`, or `a`, `b` and `c`."""
    words = [f"`{word}`" for word in dict.fromkeys(words)]
    return words[0] if len(words) == 1 else ", ".join(words[:-1]) + " and " + words[-1]


def reason(job, verdict):
    """The kind and the reason of a pair, from cwltool's check of the job."""
    text = "\n".join(verdict)
    formats = re.findall(r"Expected value of '(\w+)' to have format", text)
    if formats:
        what = "without a format" if "has no 'format' defined" in text else "of another format"
        return ("invalid", f"the job gives the input {names(formats)} a file {what}, but the "
                "input asks for a format")
    fields = re.findall(r"the '(\w+)' field is not valid because", text)
    if fields:
        inputs = "input" if len(set(fields)) == 1 else "inputs"
        return ("invalid", f"the job gives the {inputs} {names(fields)} a value of the wrong type")
    values = YAML(typ="safe", pure=True).load((TEST_WORKFLOWS / job).read_text()) or {}
    if "KeyError: 'location'" in text and untemplated(values):
        return ("invalid", "the job is a template whose files have no path")
    return ("unknown", "no reason is known; add one to REASONS in job_evidence.py")


def main():
    os.chdir(TEST_WORKFLOWS)
    failing = []
    for line in (FIXTURES / "job_test_workflows.txt").read_text().splitlines():
        words = line.split(" ", 2)
        if line.startswith("#") or len(words) < 3 or words[2].startswith("ok "):
            continue
        failing.append((words[0], words[1]))
    lines = [
        "# Why each example job file that fails in job_test_workflows.txt cannot run, and how",
        "# cwltool rejects it, recorded by job_evidence.py with cwltool 3.3.20260925135507: the",
        "# document and the job, then `=`, the kind (`invalid`, `missing` or `unsupported",
        "# (decision N)`) and why in one plain sentence, then the first lines of cwltool's error",
        "# (`>`) when it checks the job against the document, or, when it accepts the job, when it",
        "# runs it without containers. A document that fails for any job has the record of",
        "# graph_evidence.txt.",
    ]
    # Documents that fail for any job, with their records in graph_evidence.txt.
    broken = {}
    for block in (FIXTURES / "graph_evidence.txt").read_text().split("\n\n"):
        rows = [row for row in block.splitlines() if row and not row.startswith("#")]
        for row in rows:
            if not row.startswith(("=", "$", ">")):
                broken[row] = [row for row in rows if row.startswith(("=", "$", ">"))]
    for document, job in failing:
        lines.append("")
        lines.append(f"{document} {job}")
        if document in broken:
            lines.extend(broken[document])
            continue
        given = REASONS.get(document) or REASONS.get(Path(job).name)
        if given and given[0] == "missing":
            with tempfile.TemporaryDirectory() as folder:
                verdict = run(document, job, Path(folder), STAND_IN)
        else:
            verdict = check(document, job)
        kind, why = given or reason(job, verdict)
        lines.append(f"= {kind}: {why}")
        lines.extend("> " + line for line in verdict)
    (FIXTURES / "job_evidence.txt").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()

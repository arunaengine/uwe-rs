#!/usr/bin/env -S uv run -q --with cwltool==3.3.20260925135507 python3
# Records how cwltool rejects the example job files of job_corpus.txt that fail there, into
# job_evidence.txt next to this script. The job corpus test checks that every pair that fails
# there is listed here.
#
# For each failing pair, cwltool loads the document and checks the job against its inputs
# (`Process._init_job`), the check that `cwltool <document> <job>` makes before it runs
# anything. The files that the job names are replaced by empty files of the same name, since
# the corpus holds no data, so that cwltool reports what is wrong with the job itself.
#
# Run it from the repository root: cwl-runtime/tests/fixtures/job_evidence.py
# It reads the corpus of `just cwl-corpus` (CWL_CORPUS).
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

from ruamel.yaml import YAML

HOME = Path.home() / ".cache/iunctus/corpus"
CORPUS = Path(os.environ.get("CWL_CORPUS") or HOME / "cwl")
FIXTURES = Path(__file__).parent
# cwltool wraps its messages to the width of the terminal.
os.environ["COLUMNS"] = "1000"

# Why a pair fails in iunctus although cwltool accepts its job: the reason to record instead.
NOTES = {
    "maw-inputs-mutiple_ara.yaml": "the job fits the document; the step metfrag scatters over "
    "`peaks_and_parameters`, an `Any[]?` output that only the `cwl.output.json` of the R tool "
    "gives, so the mock executor, which runs no tool, gives null, and a real run is needed",
}


def clean(text, work):
    """cwltool's messages without colours, file objects, local paths and places."""
    text = re.sub(r"\x1b\[[0-9;]*m", "", text).replace(str(work), "<tmp>")
    text = text.replace(str(CORPUS) + "/", "")
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
    with tempfile.TemporaryDirectory() as folder:
        work = Path(folder)
        text = (CORPUS / job).read_text()
        values = YAML(typ="safe", pure=True).load(text) or {}
        values = stubbed(values, work)
        try:
            tool = load_tool(str(CORPUS / document), context)
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
        command = ["cwltool", "--no-container", "--outdir", str(work / "out"), document, str(path)]
        result = subprocess.run(command, capture_output=True, text=True, timeout=600)
        lines = clean(result.stderr, work)
        first = next((index for index, line in enumerate(lines) if "ERROR" in line), len(lines))
        return ["run: " + line for line in lines[first : first + 5]] or ["run: no error"]


def main():
    os.chdir(CORPUS)
    failing = []
    for line in (FIXTURES / "job_corpus.txt").read_text().splitlines():
        words = line.split(" ", 2)
        if line.startswith("#") or len(words) < 3 or words[2].startswith("ok "):
            continue
        failing.append((words[0], words[1]))
    lines = [
        "# How cwltool rejects each example job file that fails in job_corpus.txt, recorded by",
        "# job_evidence.py with cwltool 3.3.20260925135507: the document and the job, then the",
        "# first lines of cwltool's error when it checks the job against the document, or, when",
        "# it accepts the job, when it runs it without containers; `=` gives a reason.",
    ]
    # Documents that fail for any job, with the evidence in graph_evidence.txt.
    broken = {
        line
        for line in (FIXTURES / "graph_evidence.txt").read_text().splitlines()
        if line and not line.startswith(("#", "=", "$", ">"))
    }
    for document, job in failing:
        lines.append("")
        lines.append(f"{document} {job}")
        if document in broken:
            lines.append("= the document fails for any job, as graph_evidence.txt shows")
            continue
        verdict = check(document, job)
        note = NOTES.get(Path(job).name)
        if note:
            lines.append(f"= {note}")
        lines.extend("> " + line for line in verdict)
    (FIXTURES / "job_evidence.txt").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()

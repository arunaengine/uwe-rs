# Writes run_corpus_oracle.txt: what real Snakemake 9.27.0 does with each workflow of the
# Snakemake corpus and each launch that launches.py considers for it. Each run is
# `snakemake --dry-run` in a copy of the workflow folder under the temporary directory. Run it
# from the repository root after `just snakemake-corpus-data`, then run launches.py:
#
#     uv run -q --with snakemake==9.27.0 --with pyyaml --with pandas --with numpy --with peppy \
#         --with biopython --with psutil --with requests --with openpyxl --with pyarrow \
#         --with ruamel.yaml --with snakemake-storage-plugin-http \
#         --with snakemake-storage-plugin-s3 python snakemake-runtime/tests/fixtures/oracle.py
#
# Snakemake is only the reference here; iunctus never runs it (decision 1).
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

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from launches import ORACLE, candidates  # noqa: E402

# Snakemake prints this once the Snakefile is evaluated and it starts to plan the jobs.
PLANNING = "Building DAG of jobs..."
TIMEOUT = 300


def command(folder, copy, fields):
    """The working directory and the arguments of `snakemake` for a launch in a copy."""
    values = dict(fields)
    workdir = os.path.join(copy, values.get("directory", "."))
    arguments = [sys.executable, "-m", "snakemake", "--dry-run", "--cores", "1", "--nocolor"]
    if "snakefile" in values:
        arguments += ["--snakefile", os.path.join(copy, values["snakefile"])]
    configfiles = [os.path.join(copy, value) for key, value in fields if key == "configfile"]
    if configfiles:
        arguments += ["--configfile", *configfiles]
    config = [value for key, value in fields if key == "config"]
    if config:
        arguments += ["--config", *config]
    targets = [value for key, value in fields if key == "target"]
    if targets:
        arguments += ["--", *targets]
    return workdir, arguments


def first_error(output, copy):
    """The first line that names an error, with the line after it, without the copy's path."""
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    for index, line in enumerate(lines):
        if re.search(r"(Error|Exception)\b", line) and not line.startswith("Traceback"):
            text = " ".join(lines[index:index + 2])
            return text.replace(copy + "/", "").replace(copy, ".")[:300]
    return (lines[-1] if lines else "").replace(copy, ".")[:300]


def run(name, folder, via, fields):
    """One line of the oracle file."""
    with tempfile.TemporaryDirectory(prefix="iunctus-oracle-") as scratch:
        copy = os.path.join(scratch, "workflow")
        shutil.copytree(folder, copy, symlinks=True)
        workdir, arguments = command(folder, copy, fields)
        os.makedirs(workdir, exist_ok=True)
        try:
            result = subprocess.run(
                arguments, cwd=workdir, capture_output=True, text=True, timeout=TIMEOUT,
                env={**os.environ, "PYTHONPATH": ""},
            )
            output = result.stdout + result.stderr
            code = result.returncode
        except subprocess.TimeoutExpired:
            return f"{name}\t{via}\t124\tfailed\ttimeout"
        stage = "evaluated" if PLANNING in output else "failed"
        message = "" if code == 0 else first_error(output, copy)
        return f"{name}\t{via}\t{code}\t{stage}\t{message}".rstrip("\t")


def failing(lines):
    """The lines of the workflows whose first launch Snakemake did not evaluate."""
    first = {}
    for line in lines:
        name, _, _, stage, *_ = line.split("\t")
        first.setdefault(name, stage)
    return [line for line in lines if first[line.split("\t")[0]] != "evaluated"]


def main():
    jobs = [(name, folder, via, fields) for name, folder, launches in candidates()
            for via, fields in launches]
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        lines = list(pool.map(lambda job: run(*job), jobs))
    with open(ORACLE, "w", encoding="utf-8") as output:
        output.write("# What `snakemake --dry-run` of Snakemake 9.27.0 does with the workflows of the corpus\n")
        output.write("# that it cannot evaluate with their preferred launch, for each launch launches.py\n")
        output.write("# considers; written by oracle.py. Workflows not listed evaluate with their preferred\n")
        output.write("# launch. Columns, separated by tabs: the workflow, the launch (plain, ci, catalog or\n")
        output.write("# tests.py), the exit code, `evaluated` when Snakemake evaluated the Snakefile and began\n")
        output.write("# to plan the jobs, else `failed`, and the first error line.\n")
        for line in failing(lines):
            output.write(line + "\n")
    print(f"{len(lines)} runs", file=sys.stderr)


if __name__ == "__main__":
    main()

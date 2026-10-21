"""Records real Snakemake dry-run outcomes and job counts for the test workflows."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from launches import ORACLE, TEST_WORKFLOWS, candidates  # noqa: E402

# Snakemake prints this once the Snakefile is evaluated and it starts to plan the jobs.
PLANNING = "Building DAG of jobs..."
TIMEOUT = 300


def command(folder, copy, fields):
    """The working directory and the arguments of `snakemake` for a launch in a copy."""
    values = dict(fields)
    workdir = os.path.join(copy, values.get("directory", "."))
    # The `snakemake` command, as users run it: unlike `python -m snakemake`, it does not put
    # the working directory on the Python path.
    snakemake = os.path.join(os.path.dirname(sys.executable), "snakemake")
    arguments = [snakemake, "--dry-run", "--printshellcmds", "--cores", "1", "--nocolor"]
    if "profile" in values:
        arguments += ["--profile", os.path.join(copy, values["profile"])]
    if "workflow-profile" in values:
        arguments += ["--workflow-profile", os.path.join(copy, values["workflow-profile"])]
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
            # Paths of installed packages differ between machines.
            text = re.sub(r" \(/[^)]*site-packages/[^)]*\)", "", text)
            return text.replace(copy + "/", "").replace(copy, ".")[:300]
    return (lines[-1] if lines else "").replace(copy, ".")[:300]


def run(name, folder, via, fields):
    """One line of the oracle file."""
    with tempfile.TemporaryDirectory(prefix="uwe-oracle-") as scratch:
        copy = os.path.join(scratch, "workflow")
        shutil.copytree(folder, copy, symlinks=True)
        workdir, arguments = command(folder, copy, fields)
        os.makedirs(workdir, exist_ok=True)
        try:
            result = subprocess.run(
                arguments, cwd=workdir, capture_output=True, text=True, timeout=TIMEOUT,
                env={**os.environ, "PYTHONPATH": "", "LC_CTYPE": "C.UTF-8"},
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


def record_jobs(resume=False):
    """Runs each recorded successful workflow once, using the harness launch fixture."""
    fixtures = os.path.dirname(os.path.abspath(__file__))
    launches = {}
    for line in open(os.path.join(fixtures, "run_test_workflows_launches.txt"), encoding="utf-8"):
        if line.startswith("#") or not line.strip():
            continue
        name, via, *fields = line.rstrip("\n").split("\t")
        launches[name] = (via, [tuple(field.split("=", 1)) for field in fields if "=" in field])
    path = os.path.join(fixtures, "run_test_workflows_jobs.txt")
    completed = set()
    if resume:
        completed = {line.split("\t")[0] for line in open(path, encoding="utf-8") if not line.startswith("#")}
    folders = {}
    for group, directory in (("catalog", "catalog"), ("tests", "snakemake-tests/tests")):
        base = os.path.join(TEST_WORKFLOWS, directory)
        folders.update({f"{group}/{name.replace(' ', '_')}": os.path.join(base, name) for name in os.listdir(base)})
    with open(os.path.join(fixtures, "run_test_workflows.txt"), encoding="utf-8") as outcomes:
        lines = outcomes.read().splitlines()
    with open(path, "a" if resume else "w", encoding="utf-8") as recorded:
        if not resume:
            recorded.write("# Recorded serially with nice -n 19 uv run --offline --with snakemake==9.27.0\n")
            recorded.write("# python snakemake-runtime/tests/fixtures/oracle.py --jobs; LC_CTYPE=C.UTF-8.\n")
            recorded.write("# Also passed --with for pyyaml, pandas, numpy, peppy, biopython, psutil, requests,\n")
            recorded.write("# openpyxl, pyarrow, ruamel.yaml, snakemake-storage-plugin-http,\n")
            recorded.write("# snakemake-storage-plugin-s3 and snakemake-executor-plugin-slurm.\n")
            recorded.write("# Uses run_test_workflows_launches.txt targets, config, profiles and working directory.\n")
            recorded.write("# Tab-separated workflow, launch, exit code, JSON per-rule counts (including total), error.\n")
        for line in lines:
            if line.startswith("#") or " ok " not in line:
                continue
            name = line.split(" ", 1)[0]
            if name in completed:
                continue
            folder = folders[name]
            via, fields = launches.get(name, ("plain", []))
            with tempfile.TemporaryDirectory(prefix="uwe-oracle-") as scratch:
                copy = os.path.join(scratch, "workflow")
                shutil.copytree(folder, copy, symlinks=True)
                workdir, arguments = command(folder, copy, fields)
                try:
                    result = subprocess.run(
                        arguments, cwd=workdir, capture_output=True, text=True, timeout=TIMEOUT,
                        env={**os.environ, "PYTHONPATH": "", "LC_CTYPE": "C.UTF-8"},
                    )
                    output = result.stdout + result.stderr
                    code = result.returncode
                except subprocess.TimeoutExpired as error:
                    output = (error.stdout or b"").decode() + (error.stderr or b"").decode()
                    code = 124
                logs = os.environ.get("SNAKEMAKE_ORACLE_LOGS")
                if logs:
                    os.makedirs(logs, exist_ok=True)
                    with open(os.path.join(logs, name.replace("/", "__") + ".txt"), "w", encoding="utf-8") as log:
                        log.write(output)
                counts = job_counts(output)
                message = first_error(output, copy) if code else ""
                if code == 0 and counts is None:
                    raise RuntimeError(f"{name}: successful dry run without a job table")
                recorded.write(f"{name}\t{via}\t{code}\t{json.dumps(counts, sort_keys=True)}\t{message}".rstrip("\t") + "\n")
                recorded.flush()
                print(f"{name}: {code} {counts}", flush=True)


def job_counts(output):
    """Reads the first job statistics table, or an explicitly empty dry run."""
    counts = {}
    table = False
    for line in output.splitlines():
        if re.fullmatch(r"\s*Job\s+stats:\s*", line, re.IGNORECASE):
            table = True
            continue
        if not table:
            continue
        match = re.fullmatch(r"\s*(\S+)\s+(\d+)\s*", line)
        if match:
            counts[match[1]] = int(match[2])
            if match[1] == "total":
                return counts
    if "Nothing to be done" in output:
        return {"total": 0}
    return None


def main():
    if sys.argv[1:] in (["--jobs"], ["--jobs", "--resume"]):
        record_jobs("--resume" in sys.argv)
        return
    only = None
    if len(sys.argv) == 3 and sys.argv[1] == "--only":
        only = {line.strip() for line in open(sys.argv[2], encoding="utf-8") if line.strip()}
    jobs = [(name, folder, via, fields) for name, folder, launches in candidates()
            for via, fields in launches if only is None or name in only]
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        lines = list(pool.map(lambda job: run(*job), jobs))
    if only is None:
        kept = failing(lines)
    else:
        old = [line.rstrip("\n") for line in open(ORACLE, encoding="utf-8")
               if not line.startswith("#") and line.split("\t")[0] not in only]
        kept = sorted(old + lines, key=lambda line: line.split("\t")[0])
    with open(ORACLE, "w", encoding="utf-8") as output:
        output.write("# What `snakemake --dry-run --printshellcmds` of Snakemake 9.27.0 does with the Snakemake\n")
        output.write("# test workflows that it cannot evaluate with their preferred launch, and with those that\n")
        output.write("# iunctus cannot run, for each launch launches.py considers; written by oracle.py.\n")
        output.write("# Workflows not listed evaluate with their preferred launch. Columns, separated by tabs:\n")
        output.write("# the workflow, the launch (plain, ci, catalog or tests.py), the exit code, `evaluated`\n")
        output.write("# when Snakemake evaluated the Snakefile and began to plan the jobs, else `failed`, and\n")
        output.write("# the first error line.\n")
        for line in kept:
            output.write(line + "\n")
    print(f"{len(lines)} runs", file=sys.stderr)


if __name__ == "__main__":
    main()

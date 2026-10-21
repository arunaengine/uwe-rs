#!/usr/bin/env python3
"""Checks Snakemake expected results on SLURM with Apptainer."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import os
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import real_runs_snakemake as local  # noqa: E402

IUNCTUS = os.environ.get("IUNCTUS") or "iunctus"
OUTPUT = Path(os.environ.get("RECORD") or Path(__file__).with_name("real_runs_slurm_snakemake.txt"))
IMAGE = "python:3-slim"
ALLOWED = {"python:3-slim", "docker.io/python:3-slim", "debian:stable-slim",
           "docker.io/debian:stable-slim"}
PARALLEL = int(os.environ.get("PARALLEL") or 2)
TIMEOUT = 900
# Why a test fails on this cluster alone, where the reason is known.
KNOWN = {
    "test05": "a rule asks for a GPU (`gpu=1`), which Snakemake's SLURM plugin asks SLURM for "
              "too, and the nodes of this cluster have none",
}
# Python packages that IMAGE does not have.
PACKAGES = re.compile(r"^\s*(?:import|from)\s+(pandas|numpy|yaml|pysam|matplotlib|scipy)\b",
                      re.M)


def skipped(source, fields):
    """Why a test does not run here, or None."""
    if not (source / "expected-results").is_dir():
        return "no expected-results"
    text = local.sources(source)
    for pattern, reason in local.NEEDS:
        if pattern.search(text):
            return reason
    other = sorted({image.removeprefix("docker://") for image in local.IMAGES.findall(text)}
                   - ALLOWED)
    if other:
        return f"names images that this run does not pull: {', '.join(other)}"
    found = PACKAGES.search(text)
    if found:
        return f"needs the Python package {found.group(1)}, which {IMAGE} lacks"
    if any(field.split("=")[0] in ("directory", "snakefile", "workflow-profile")
           for field in fields):
        return "runs from another folder than its own"
    return None


def command(fields):
    """The command of iunctus for the fields of a launch."""
    words = [IUNCTUS, "run", "Snakefile", "-lang", "snakemake", "-executor", "slurm",
             "-no-cache", "-default-image", IMAGE, "-python-image", IMAGE, "-max-pending", "3"]
    for field in fields:
        key, _, value = field.partition("=")
        if key == "target":
            words.append(value)
        elif key == "configfile":
            words += ["-configfile", value]
        elif key == "config":
            words += ["--config", value]
        elif key == "profile":
            words += ["-profile", value]
    return words


def run_stopped(command, cwd):
    """Runs iunctus; the exit code and its standard output and error. After TIMEOUT it stops
    iunctus with SIGTERM, so that it cancels its SLURM jobs, and the code is `timeout`."""
    process = subprocess.Popen(command, cwd=cwd, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True)
    try:
        stdout, stderr = process.communicate(timeout=TIMEOUT)
        return process.returncode, stdout, stderr
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.communicate(timeout=120)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
        return "timeout", "", "timeout"


def native_records():
    """How each test ran in Snakemake and iunctus on a machine with Docker, by name."""
    found = {}
    record = Path(__file__).with_name("real_runs_snakemake.txt")
    for line in record.read_text().splitlines() if record.exists() else []:
        name, _, rest = line.partition("\t")
        found[name] = rest
    return found


NATIVE = native_records()


def compare(name, fields, tests, work):
    """The record line of one test."""
    source = tests / name
    reason = skipped(source, fields)
    if reason:
        return f"skipped: {reason}"
    place = work / name
    local.remove(place)
    subprocess.run(["cp", "-a", str(source), str(place)], check=True)
    # The tests live in Snakemake's git repository, whose files go to the jobs.
    for words in (["git", "init", "-q"], ["git", "add", "-A"]):
        subprocess.run(words, cwd=place, capture_output=True, check=True)
    code, _, stderr = run_stopped(command(fields), place)
    errors = stderr.splitlines()
    if "shouldfail" in fields:
        return "same: fails as the test expects" if code != 0 else \
            "differs: iunctus succeeds where the test expects a failure"
    native = NATIVE.get(name, "")
    if code != 0:
        if native.startswith("same: both fail"):
            return "same: fails, as Snakemake does"
        return f"iunctus fails (exit {code}): {local.error_line(errors)}"
    wrong = local.expected(source, place)
    local.remove(place)
    if wrong and native == f"same: both differ from expected-results in {wrong[0]}":
        return f"same: {wrong[0]} is not as in expected-results, also in Snakemake"
    if wrong:
        return f"differs: {wrong[0]} is not as in expected-results"
    return "same"


def main():
    tests = Path(sys.argv[1])
    wanted = set(sys.argv[2:])
    local.LAUNCHES = Path(os.environ.get("LAUNCHES")
                          or Path(__file__).with_name("run_test_workflows_launches.txt"))
    launched = local.launches()
    names = [folder.name for folder in sorted(tests.iterdir())
             if (folder / "Snakefile").is_file() and (not wanted or folder.name in wanted)]
    work = Path(tempfile.mkdtemp(prefix="iunctus-slurm-snakemake-", dir=os.getcwd()))
    with ThreadPoolExecutor(PARALLEL) as pool:
        results = pool.map(lambda name: compare(name, launched.get(name, []), tests, work),
                           names)
        records = dict(zip(names, results))
    for name, record in records.items():
        reason = KNOWN.get(name) or local.KNOWN.get(name)
        if reason and not record.startswith(("same", "skipped")):
            records[name] = f"{record} (known: {reason})"
    counts = {}
    for record in records.values():
        kind = record.split(":")[0].split(" (")[0]
        counts[kind] = counts.get(kind, 0) + 1
    summary = ", ".join(f"{count} {kind}" for kind, count in sorted(counts.items()))
    header = ("# The tests of Snakemake run for real on a SLURM cluster with Apptainer, checked "
              "against their expected-results; written by real_runs_slurm_snakemake.py. A tab "
              "ends the name.\n"
              f"# {len(records)} tests: {summary}.\n")
    OUTPUT.write_text(header + "".join(f"{name}\t{record}\n"
                                       for name, record in sorted(records.items())))
    print(OUTPUT.read_text(), end="")


if __name__ == "__main__":
    main()

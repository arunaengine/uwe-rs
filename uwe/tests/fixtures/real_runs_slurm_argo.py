#!/usr/bin/env python3
"""Checks Argo workflow outcomes on SLURM with Apptainer."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import os
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

IUNCTUS = os.environ.get("IUNCTUS") or "iunctus"
LOCAL = Path(__file__).with_name("real_runs_argo.txt")
OUTPUT = Path(os.environ.get("RECORD") or Path(__file__).with_name("real_runs_slurm_argo.txt"))
IMAGES = {"argoproj/argosay:v2", "alpine:3.23", "alpine:latest", "alpine", "python:alpine3.23"}
PARALLEL = int(os.environ.get("PARALLEL") or 4)
TIMEOUT = 900
# Why a workflow fails on this cluster alone, where the reason is known.
KNOWN = {
    "argo-workflows/test/e2e/testdata/pending-workflow.yaml":
        "it asks for more memory than any node has, on purpose, so that its pod stays pending "
        "in Argo's tests; sbatch refuses such a job",
}


def expected():
    """Whether each workflow that ran the same way here should succeed, by path."""
    found = {}
    for line in LOCAL.read_text().splitlines():
        name, _, record = line.partition("\t")
        if record.startswith("same"):
            found[name] = record == "same: succeeds"
    return found


def images(path):
    """The images that a workflow names."""
    text = path.read_text(errors="replace")
    return set(re.findall(r"^\s*image:\s*[\"']?([^\s\"']+)", text, re.M))


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


def compare(name, succeeds, tests, work):
    """The record line of one workflow."""
    folder = Path(tempfile.mkdtemp(dir=work))
    command = [IUNCTUS, "run", str(tests / name), "-lang", "argo", "-executor", "slurm",
               "-no-network", "-no-cache", "-max-pending", "3",
               "-output-dir", str(folder / "out")]
    code, _, stderr = run_stopped(command, folder)
    lines = [line for line in stderr.splitlines() if line.startswith("iunctus: ")] or [stderr]
    if succeeds:
        if code == 0:
            return "same: succeeds"
        return f"iunctus fails (exit {code}): {(lines[0] if lines else '')[:160]}"
    if code != 0:
        return "same: fails as Argo's tests expect"
    return "differs: succeeds where Argo's tests expect a failure"


def main():
    tests = Path(sys.argv[1])
    wanted = sys.argv[2:]
    known = expected()
    if wanted:
        names = [name for name in wanted if name in known]
    else:
        names = [name for name in sorted(known)
                 if (tests / name).is_file() and images(tests / name)
                 and images(tests / name) <= IMAGES]
    work = Path(tempfile.mkdtemp(prefix="iunctus-slurm-argo-", dir=os.getcwd()))
    with ThreadPoolExecutor(PARALLEL) as pool:
        records = dict(zip(names, pool.map(
            lambda name: compare(name, known[name], tests, work), names)))
    for name, record in records.items():
        if name in KNOWN and not record.startswith("same"):
            records[name] = f"{record} (known: {KNOWN[name]})"
    counts = {}
    for record in records.values():
        kind = record.split(":")[0].split(" (")[0]
        counts[kind] = counts.get(kind, 0) + 1
    summary = ", ".join(f"{count} {kind}" for kind, count in sorted(counts.items()))
    header = ("# Argo workflows run for real on a SLURM cluster with Apptainer, against what "
              "Argo's tests expect; written by real_runs_slurm_argo.py. A tab ends the path.\n"
              f"# {len(records)} workflows: {summary}.\n")
    OUTPUT.write_text(header + "".join(f"{name}\t{record}\n"
                                       for name, record in sorted(records.items())))
    print(OUTPUT.read_text(), end="")


if __name__ == "__main__":
    main()

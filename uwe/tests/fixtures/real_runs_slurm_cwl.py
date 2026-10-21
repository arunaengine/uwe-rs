#!/usr/bin/env python3
"""Checks CWL conformance outputs on SLURM with Apptainer."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import json
import os
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import yaml

UWE = os.environ.get("UWE") or "uwe"
OUTPUT = Path(os.environ.get("RECORD") or Path(__file__).with_name("real_runs_slurm_cwl.txt"))
IMAGE = "debian:stable-slim"
# The images that tests may name; others are left out to keep the downloads small.
ALLOWED = {"debian:stable-slim", "docker.io/debian:stable-slim", "debian:stable",
           "python:3-slim", "docker.io/python:3-slim"}
PARALLEL = int(os.environ.get("PARALLEL") or 4)
TIMEOUT = 900


def named_images(tool):
    """The images that a tool and the documents it runs name, and whether one needs the
    network."""
    images, network, seen = set(), False, set()
    pending = [Path(tool.split("#")[0])]
    while pending:
        path = pending.pop()
        if path in seen or not path.is_file():
            continue
        seen.add(path)
        text = path.read_text(errors="replace")
        network |= "NetworkAccess" in text or "networkAccess" in text
        for image in re.findall(r"dockerPull\"?\s*:\s*[\"']?([^\"'\s,}]+)", text):
            images.add(image)
        for other in re.findall(r"(?:run|\$import|\$include)\"?\s*:\s*[\"']?([^\"'\s,}#]+)", text):
            pending.append(path.parent / other)
    return images, network


def matches(expected, actual, where="output"):
    """None when the actual value meets the expected one, else the first place where not."""
    if expected == "Any":
        return None
    if isinstance(expected, dict):
        if not isinstance(actual, dict):
            return f"{where}: {json.dumps(actual)[:80]} instead of an object"
        for key, want in expected.items():
            if key == "location":
                got = str(actual.get("location", "")).rstrip("/").split("/")[-1]
                if want != "Any" and got != str(want).rstrip("/").split("/")[-1]:
                    return f"{where}.location: {got} instead of {want}"
                continue
            if key == "path" or (key == "listing" and "listing" not in actual):
                continue
            if key not in actual:
                return f"{where}.{key}: missing"
            if key in ("listing", "secondaryFiles") and isinstance(want, list):
                name = lambda item: json.dumps(item.get("basename") or item.get("location"))
                want = sorted(want, key=name)
                got = sorted(actual[key], key=name)
            else:
                got = actual[key]
            found = matches(want, got, f"{where}.{key}")
            if found:
                return found
        return None
    if isinstance(expected, list):
        if not isinstance(actual, list) or len(actual) != len(expected):
            return f"{where}: {json.dumps(actual)[:80]} instead of {len(expected)} items"
        for index, (want, got) in enumerate(zip(expected, actual)):
            found = matches(want, got, f"{where}[{index}]")
            if found:
                return found
        return None
    if expected != actual:
        return f"{where}: {json.dumps(actual)[:80]} instead of {json.dumps(expected)[:80]}"
    return None


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


def compare(test, suite, work):
    """The record line of one test."""
    tool = str(suite / test["tool"])
    job = str(suite / test["job"]) if test.get("job") else None
    images, network = named_images(tool)
    if network:
        return "skipped: needs the network"
    other = sorted(image for image in images if image not in ALLOWED)
    if other:
        return f"skipped: names images that this run does not pull: {', '.join(other)}"
    folder = Path(tempfile.mkdtemp(dir=work))
    command = [UWE, "run", tool, "-lang", "cwl", "-executor", "slurm", "-no-cache",
               "-default-image", IMAGE, "-output-dir", str(folder / "out")]
    command += ["-inputs", job] if job else []
    code, stdout, stderr = run_stopped(command, folder)
    if code == "timeout":
        return "iunctus fails: timeout"
    errors = [line for line in stderr.splitlines() if line.strip()]
    try:
        # The summary of the run follows the output object.
        output, _ = json.JSONDecoder().raw_decode(stdout[stdout.index("{"):])
    except ValueError:
        output = None
    ok = code == 0 and output is not None
    if test.get("should_fail"):
        return "same: fails as the test expects" if not ok else "differs: runs, but should fail"
    if not ok:
        return f"iunctus fails (exit {code}): {(errors[-1] if errors else '')[:160]}"
    found = matches(test.get("output", {}), output)
    return f"differs: {found}" if found else "same"


def main():
    suite = Path(sys.argv[1]) / "v1.2"
    wanted = sys.argv[2:]
    tests = yaml.safe_load((suite / "conformance_tests.yaml").read_text())
    chosen = [test for test in tests if "tool" in test
              and (not wanted or (test.get("id") or test.get("label")) in wanted)]
    work = Path(tempfile.mkdtemp(prefix="uwe-slurm-cwl-", dir=os.getcwd()))
    with ThreadPoolExecutor(PARALLEL) as pool:
        records = dict(zip((test.get("id") or test.get("label") for test in chosen),
                           pool.map(lambda test: compare(test, suite, work), chosen)))
    counts = {}
    for record in records.values():
        kind = record.split(":")[0].split(" (")[0]
        counts[kind] = counts.get(kind, 0) + 1
    summary = ", ".join(f"{count} {kind}" for kind, count in sorted(counts.items()))
    header = ("# CWL conformance tests (v1.2) run for real on a SLURM cluster with Apptainer, "
              "compared with the output each test expects; written by real_runs_slurm_cwl.py.\n"
              f"# {len(records)} tests: {summary}.\n")
    OUTPUT.write_text(header + "".join(f"{name} {record}\n"
                                       for name, record in sorted(records.items())))
    print(OUTPUT.read_text(), end="")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Compares real CWL conformance outputs with cwltool results."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

HOME = Path.home()
TESTS = Path(os.environ.get("CWL_TEST_WORKFLOWS") or HOME / ".cache/uwe/test-workflows/cwl")
DATA = Path(os.environ.get("CWL_CONFORMANCE_DATA")
            or HOME / ".cache/uwe/test-workflows/cwl-conformance-data")
UWE = Path(os.environ.get("UWE") or "target/release/uwe").resolve()
OUTPUT = Path(__file__).with_name("real_runs_cwl.txt")
IMAGE = "debian:stable-slim"
TIMEOUT = 300

# A docker that refuses to pull, so that no run fetches an image.
DOCKER = """#!/bin/bash
if [ "$1" = pull ]; then
    echo "iunctus real runs: no pulls, the image must be on this machine" >&2
    exit 1
fi
exec /usr/bin/docker "$@"
"""

# The keys of a file or directory that the comparison looks at; places differ by design.
FILE_KEYS = ("class", "basename", "checksum", "size", "contents", "secondaryFiles", "listing")


def local_images():
    """The images on this machine, by every name they have."""
    text = subprocess.run(["docker", "image", "ls", "--format", "{{.Repository}}:{{.Tag}}"],
                          capture_output=True, text=True, check=True).stdout
    names = set()
    for name in text.split():
        names.add(name)
        names.add(name.removeprefix("docker.io/"))
        names.add(name.removeprefix("docker.io/library/"))
    return names


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


def normal(value):
    """An output value with only what both tools must agree on."""
    if isinstance(value, dict):
        if value.get("class") in ("File", "Directory"):
            kept = {key: normal(value[key]) for key in FILE_KEYS if key in value}
            if isinstance(kept.get("listing"), list):
                kept["listing"] = sorted(kept["listing"], key=lambda entry: json.dumps(entry))
            if isinstance(kept.get("secondaryFiles"), list):
                kept["secondaryFiles"] = sorted(kept["secondaryFiles"],
                                                key=lambda entry: json.dumps(entry))
            return kept
        return {key: normal(item) for key, item in value.items()}
    if isinstance(value, list):
        return [normal(item) for item in value]
    return value


def mask(value, expected):
    """An output value without what the test's expected output leaves open: values that it
    gives as `Any`, and the names of files that it does not name, like random `stdout` names."""
    if expected == "Any":
        return "Any"
    if isinstance(value, dict) and isinstance(expected, dict):
        masked = {key: mask(item, expected.get(key)) for key, item in value.items()}
        # A test that names no basename leaves the name open, as for `stdout` files.
        if value.get("class") in ("File", "Directory") and "basename" not in expected:
            masked["basename"] = "Any"
        return masked
    if isinstance(value, list) and isinstance(expected, list) and len(value) == len(expected):
        return [mask(item, want) for item, want in zip(value, expected)]
    return value


def difference(ours, theirs, where="output"):
    """The first place where two output values differ, or None."""
    if isinstance(ours, dict) and isinstance(theirs, dict):
        for key in sorted(set(ours) | set(theirs)):
            # A key that only one side has counts only for the keys of files and directories.
            if (key not in ours or key not in theirs) and key in ("listing", "contents"):
                continue
            if key not in ours:
                return f"{where}.{key}: missing"
            if key not in theirs:
                return f"{where}.{key}: extra"
            found = difference(ours[key], theirs[key], f"{where}.{key}")
            if found:
                return found
        return None
    if isinstance(ours, list) and isinstance(theirs, list):
        if len(ours) != len(theirs):
            return f"{where}: {len(ours)} items instead of {len(theirs)}"
        for index, (a, b) in enumerate(zip(ours, theirs)):
            found = difference(a, b, f"{where}[{index}]")
            if found:
                return found
        return None
    if ours != theirs:
        return f"{where}: {json.dumps(ours)[:80]} instead of {json.dumps(theirs)[:80]}"
    return None


def run(command, folder, environment):
    """Runs a command without network; the exit code, the output object or None, and the
    last line of the error."""
    words = ["unshare", "--user", "--net", "--map-current-user", "--"] + command
    try:
        done = subprocess.run(words, cwd=folder, capture_output=True, text=True,
                              env=environment, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        return "timeout", None, "timeout"
    errors = [line for line in done.stderr.splitlines() if line.strip()]
    try:
        start = done.stdout.index("{")
        # The summary of the run follows the output object.
        output, _ = json.JSONDecoder().raw_decode(done.stdout[start:])
    except ValueError:
        output = None
    return done.returncode, output, (errors[-1] if errors else "")[:160]


def compare(test, suite, work, environment, images):
    """The record line of one test."""
    tool = str(suite / test["tool"])
    job = str(suite / test["job"]) if test.get("job") else None
    needed, network = named_images(tool)
    if network:
        return "skipped: needs the network"
    missing = sorted(image for image in needed
                     if image not in images and image.removeprefix("docker.io/") not in images)
    if missing:
        return f"skipped: images not on this machine: {', '.join(missing)}"
    folder = Path(tempfile.mkdtemp(dir=work))
    native = ["cwltool", "--quiet", "--default-container", IMAGE, "--outdir",
              str(folder / "native"), tool] + ([job] if job else [])
    ours = [str(UWE), "run", tool, "-lang", "cwl", "-executor", "local", "-default-image",
            IMAGE, "-store", str(work / "store"), "-output-dir", str(folder / "uwe")]
    ours += ["-inputs", job] if job else []
    native_code, native_output, native_error = run(native, folder, environment)
    our_code, our_output, our_error = run(ours, folder, environment)
    shutil.rmtree(folder, ignore_errors=True)
    native_ok = native_code == 0 and native_output is not None
    our_ok = our_code == 0 and our_output is not None
    if not native_ok and not our_ok:
        return "same: both fail"
    # A test that expects a failure is met by a failure, whatever the other tool does.
    if test.get("should_fail") and not our_ok:
        return "same: fails as the test expects"
    if not our_ok:
        return f"iunctus fails (exit {our_code}): {our_error}"
    if not native_ok:
        return f"cwltool fails (exit {native_code}): {native_error}"
    expected = test.get("output")
    found = difference(mask(normal(our_output), expected), mask(normal(native_output), expected))
    return f"differs: {found}" if found else "same"


def main():
    wanted = set(sys.argv[1:])
    work = Path(tempfile.mkdtemp(prefix="uwe-real-cwl-"))
    try:
        suite_root = work / "conformance"
        shutil.copytree(TESTS / "conformance", suite_root, symlinks=True)
        shutil.copytree(DATA / "conformance", suite_root, symlinks=True, dirs_exist_ok=True)
        suite = suite_root / "v1.2"
        bin_folder = work / "bin"
        bin_folder.mkdir()
        (bin_folder / "docker").write_text(DOCKER)
        (bin_folder / "docker").chmod(0o755)
        environment = dict(os.environ, PATH=f"{bin_folder}:{os.environ['PATH']}")
        images = local_images()
        tests = yaml.safe_load((suite / "conformance_tests.yaml").read_text())
        records = {}
        for test in tests:
            name = test.get("id") or test.get("label")
            if not name or "tool" not in test or (wanted and name not in wanted):
                continue
            records[name] = compare(test, suite, work, environment, images)
            print(f"{name} {records[name]}", flush=True)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if wanted and OUTPUT.exists():
        for line in OUTPUT.read_text().splitlines():
            if line and not line.startswith("#"):
                name, _, rest = line.partition(" ")
                records.setdefault(name, rest)
    counts = {}
    for record in records.values():
        kind = record.split(":")[0].split(" (")[0]
        counts[kind] = counts.get(kind, 0) + 1
    summary = ", ".join(f"{count} {kind}" for kind, count in sorted(counts.items()))
    header = ("# The CWL conformance tests (v1.2) run for real by iunctus and by cwltool, compared "
              "by their output objects; written by real_runs_cwl.py.\n"
              f"# {len(records)} tests: {summary}.\n")
    OUTPUT.write_text(header + "".join(f"{name} {record}\n"
                                       for name, record in sorted(records.items())))


if __name__ == "__main__":
    main()

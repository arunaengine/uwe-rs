#!/usr/bin/env python3
"""Compares real Argo workflow outcomes with the reference test expectations."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# The helper next to this script; Python keeps no compiled copy of it in the repository.
sys.dont_write_bytecode = True
from docker_no_pull import NoPull  # noqa: E402

import yaml

TESTS = Path(os.environ.get("ARGO_TEST_WORKFLOWS")
             or Path.home() / ".cache/uwe/test-workflows/argo")
REPOSITORY = Path(__file__).resolve().parents[3]
OUTCOMES = REPOSITORY / "argo-runtime/tests/fixtures/test-workflows/outcomes.txt"
UWE = Path(os.environ.get("UWE") or "target/release/uwe").resolve()
OUTPUT = Path(__file__).with_name("real_runs_argo.txt")
TIMEOUT = 300
# The start of the record of a workflow whose images are not all on this machine.
ABSENT = "skipped: images not on this machine"
# Workflows that Argo's end-to-end tests expect to fail.
FAILURES = "argo-workflows/test/e2e/expectedfailures/"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
# Servers that Argo's end-to-end tests start next to the cluster, which a run here does not
# have, by a text that names them in the workflow.
SERVERS = [
    ("minio:9000", "needs the MinIO server of Argo's tests"),
    ("httpbin:", "needs the httpbin server of Argo's tests"),
    ("plugin:", "needs an executor plugin server"),
]


def local_images():
    """The images on this machine, by every name they have."""
    text = subprocess.run(["docker", "image", "ls", "--format", "{{.Repository}}:{{.Tag}}"],
                          capture_output=True, text=True, check=True).stdout
    names = set()
    for name in text.split():
        names.update({name, name.removeprefix("docker.io/"),
                      name.removeprefix("docker.io/library/")})
        if name.endswith(":latest"):
            names.add(name.removesuffix(":latest").removeprefix("docker.io/library/"))
    return names


def runnable():
    """The workflows that run with the simulator, by path."""
    found = []
    for line in OUTCOMES.read_text().splitlines():
        path, _, outcome = line.partition("\t")
        if outcome.startswith("ok "):
            found.append(path)
    return found


def images(value):
    """Every `image` that a document names, at any depth."""
    if isinstance(value, dict):
        for key, item in value.items():
            if key == "image" and isinstance(item, str):
                yield item
            else:
                yield from images(item)
    elif isinstance(value, list):
        for item in value:
            yield from images(item)


def skipped(path, local):
    """Why a workflow cannot run here without pulls, or None."""
    try:
        documents = [document for document in yaml.safe_load_all(path.read_text())
                     if document]
    except yaml.YAMLError:
        return "not YAML that PyYAML reads"
    text = path.read_text(errors="replace")
    for marker, reason in SERVERS:
        if marker in text:
            return reason
    if re.search(r"^\s*http:\s*$", text, re.M) or re.search(r"\burl:\s*['\"]?https?://", text):
        return "calls a web address"
    named = {image for document in documents for image in images(document)}
    if any("{{" in image for image in named):
        return "names its image by a parameter"
    missing = sorted(named - local)
    if missing:
        return f"images not on this machine: {', '.join(missing)}"
    return None


def expected_failures():
    """The workflows that Argo's tests expect to fail, by their real paths: the files of
    `expectedfailures`, some of which link to examples."""
    folder = TESTS / FAILURES
    return {path.resolve() for path in folder.rglob("*.yaml")}


def probable_cause(path):
    """Why a workflow probably fails in uwe, from what it holds; None when nothing
    explains it."""
    text = path.read_text(errors="replace")
    if re.search(r"exit[ (\"',\]]+[1-9]|sys\.exit\(\s*[1-9]|exit_code|\bfail", text):
        return "a step fails on purpose, so Argo fails the workflow too"
    return None


def stop_containers(work):
    """Kills the containers of uwe that still run on folders of this script."""
    names = subprocess.run(["docker", "ps", "--filter", "name=uwe-", "--format",
                            "{{.Names}}"], capture_output=True, text=True).stdout.split()
    for name in names:
        mounts = subprocess.run(["docker", "inspect", "--format",
                                 "{{range .Mounts}}{{.Source}} {{end}}", name],
                                capture_output=True, text=True).stdout.split()
        if any(source.startswith(str(work)) for source in mounts):
            subprocess.run(["docker", "kill", name], capture_output=True)


def last_error(text):
    """The first line of an output that says `uwe:` or names an error, else the last."""
    lines = [ANSI.sub("", line).strip() for line in text.splitlines() if line.strip()]
    for line in lines:
        if line.startswith("uwe: ") or "error" in line.lower():
            return line[:160]
    return (lines[-1] if lines else "")[:160]


def compare(name, work, environment, local):
    """The record line of one workflow."""
    # The second and later workflows of a file are named `<file>#<n>`; this script runs whole
    # files.
    if "#" in name:
        return "skipped: one of several workflows in a file"
    path = TESTS / name
    reason = skipped(path, local)
    if reason:
        return f"skipped: {reason}"
    lint = subprocess.run(["argo", "lint", "--offline", str(path)], capture_output=True,
                          text=True, cwd=path.parent)
    folder = Path(tempfile.mkdtemp(dir=work))
    command = ["unshare", "--user", "--net", "--map-current-user", "--", str(UWE), "run",
               str(path), "-lang", "argo", "-executor", "local", "-no-network",
               "-max-pending", "3",
               "-store", str(work / "store"), "-output-dir", str(folder / "out")]
    process = subprocess.Popen(command, cwd=folder, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, env=environment)
    try:
        _, stderr = process.communicate(timeout=TIMEOUT)
        code, error, text = process.returncode, last_error(stderr), stderr
    except subprocess.TimeoutExpired:
        # uwe stops its containers on SIGTERM; any left are stopped here.
        process.terminate()
        try:
            process.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
        stop_containers(work)
        code, error, text = "timeout", "timeout", ""
    shutil.rmtree(folder, ignore_errors=True)
    if re.search(r"cannot read (https?|s3|gs)://", text):
        return "skipped: reads a remote file"
    if "no secret store yet" in text:
        return "skipped: reads a Kubernetes Secret, which uwe has no store for yet"
    if "is required, but has no value" in text:
        return "skipped: needs a value for a parameter, which a run of Argo needs too"
    if re.search(r"couldn't find (cluster )?workflow template", text):
        return "skipped: calls a WorkflowTemplate that is not next to it, for argo lint too"
    if "real runs here pull no images" in text:
        return "skipped: needs an image that is not on this machine (found when it ran)"
    should_fail = name.startswith(FAILURES) or path.resolve() in expected_failures()
    # Offline, argo lint knows only the templates of the file itself; a reference to another
    # file is no error of the workflow.
    lint_text = ANSI.sub("", lint.stdout + lint.stderr)
    errors = [line for line in lint_text.splitlines() if "✖ in " in line]
    if errors and all(re.search(r"couldn't find (cluster )?workflow template", line)
                      for line in errors):
        lint = subprocess.CompletedProcess(lint.args, 0)
    if lint.returncode != 0:
        if code != 0:
            return "same: both fail (argo lint rejects it)"
        return f"argo fails: argo lint: {last_error(lint.stdout + lint.stderr)}"
    if should_fail:
        if code != 0:
            return "same: fails as Argo's tests expect"
        return "differs: succeeds where Argo's tests expect a failure"
    if code != 0:
        cause = probable_cause(path)
        note = f" (probable cause: {cause})" if cause else ""
        return f"uwe fails (exit {code}): {error}{note}"
    return "same: succeeds"


def main():
    wanted = set(sys.argv[1:])
    work = Path(tempfile.mkdtemp(prefix="uwe-real-argo-"))
    records = {}
    try:
        local = local_images()
        with NoPull(work) as socket:
            environment = dict(os.environ, XDG_CACHE_HOME=str(work / "cache"),
                               DOCKER_HOST=f"unix://{socket}")
            for name in runnable():
                if wanted and name not in wanted:
                    continue
                records[name] = compare(name, work, environment, local)
                print(f"{name}\t{records[name]}", flush=True)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if wanted and OUTPUT.exists():
        for line in OUTPUT.read_text().splitlines():
            if line and not line.startswith("#"):
                name, _, rest = line.partition("\t")
                records.setdefault(name, rest)
        # The workflows that the record only counts are skipped again, without running.
        local = local_images()
        for name in runnable():
            if name not in records and skipped(TESTS / name, local):
                records[name] = f"skipped: {skipped(TESTS / name, local)}"
    counts = {}
    for record in records.values():
        kind = record.split(":")[0].split(" (")[0]
        counts[kind] = counts.get(kind, 0) + 1
    summary = ", ".join(f"{count} {kind}" for kind, count in sorted(counts.items()))
    # The workflows whose images are not on this machine are only counted.
    absent = sorted(name for name, record in records.items() if record.startswith(ABSENT))
    header = ("# Argo workflows run for real by uwe, against what Argo's tests expect (argo "
              "lint, expected failures); written by real_runs_argo.py. A tab ends the path.\n"
              f"# {len(records)} workflows: {summary}.\n"
              f"# {len(absent)} of the skipped ones name images that are not on this machine; "
              "they are not listed.\n")
    OUTPUT.write_text(header + "".join(f"{name}\t{record}\n"
                                       for name, record in sorted(records.items())
                                       if not record.startswith(ABSENT)))


if __name__ == "__main__":
    main()

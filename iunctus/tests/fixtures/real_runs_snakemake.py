#!/usr/bin/env python3
# Runs the tests of Snakemake (the `tests/` folder of its repository, from the Snakemake test
# workflows) for real, with iunctus and with Snakemake, and compares the files that each run
# made, changed or removed in the folder of the test, by name and SHA-256 digest. It also checks
# both runs against the `expected-results` folder of the test. It records the comparison in
# real_runs_snakemake.txt next to this script.
#
# Run it from the repository root: iunctus/tests/fixtures/real_runs_snakemake.py [test...]
# With tests, it runs only them and keeps the other records. It needs Docker, Snakemake 9.27.0
# (SNAKEMAKE, else `snakemake` on the PATH), the release binary of iunctus
# (target/release/iunctus, or IUNCTUS) and the Snakemake test workflows (SNAKEMAKE_TESTS).
#
# Each test runs the way run_test_workflows_launches.txt says (targets, config files and
# values), with 3 jobs at once, as Snakemake's own tests do, in a git repository of its files,
# as the tests are in Snakemake's repository. Snakemake gets the CPUs of the machine as cores,
# as iunctus sees them, where Snakemake's tests give 3. Snakemake runs the jobs on this machine,
# iunctus in containers of IMAGE, which has Bash, Python with pandas, and the usual tools. Both
# run in the same folder, one after the other, so that paths in outputs agree.
#
# The runs use no network: both tools run in a network namespace of their own, and iunctus
# gives its containers none (`-no-network`). Tests that read
# remote files, need Conda environments, wrappers or modules, or name an image that is not on
# this machine, are left out with the reason. iunctus talks to Docker through a socket that
# refuses pulls (docker_no_pull.py), since the Docker daemon has network.
# When the runs differ, Snakemake runs once more, and files that differ between its two runs
# count by name only. Snakemake's own files (`.snakemake`, the `.snakemake_timestamp` of
# directory outputs, Python's `__pycache__`) are left out of the comparison.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import hashlib
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

# The helper next to this script; Python keeps no compiled copy of it in the repository.
sys.dont_write_bytecode = True
from docker_no_pull import NoPull  # noqa: E402

HOME = Path.home() / ".cache/iunctus/test-workflows/snakemake"
TESTS = Path(os.environ.get("SNAKEMAKE_TESTS") or HOME / "snakemake-tests") / "tests"
REPOSITORY = Path(__file__).resolve().parents[3]
LAUNCHES = REPOSITORY / "snakemake-runtime/tests/fixtures/run_test_workflows_launches.txt"
IUNCTUS = Path(os.environ.get("IUNCTUS") or "target/release/iunctus").resolve()
SNAKEMAKE = os.environ.get("SNAKEMAKE") or "snakemake"
OUTPUT = Path(__file__).with_name("real_runs_snakemake.txt")
IMAGE = "pandas/pandas:pip-all"
TIMEOUT = 300
JOBS = "3"

# What the runs themselves leave in the folder, which the comparison leaves out.
OWN = (".snakemake", "expected-results", ".git")
# The file that Snakemake leaves in each directory output to note when it was made, which
# iunctus does not write.
TIMESTAMP = ".snakemake_timestamp"
# Where Python keeps the compiled modules that a Snakefile imports on this machine.
PYCACHE = "__pycache__"
# Directives that need what this machine does not have without the network.
NEEDS = [
    (re.compile(r"^\s*conda\s*:", re.M), "needs a Conda environment"),
    (re.compile(r"^\s*wrapper\s*:", re.M), "needs a Snakemake wrapper from the network"),
    (re.compile(r"^\s*envmodules\s*:", re.M), "needs environment modules"),
    (re.compile(r"storage[.:(]|https?://|\b(github|gitlab)\("), "reads remote files"),
]
# Why a test differs or fails in iunctus alone, where the reason is known and by design or
# not done yet: the record names it after the result. Each reason is one plain sentence.
LINKS = ("an output is a symbolic link, which iunctus stores as the file that it points to, "
         "so the link itself is not delivered")
UNDECLARED = ("its jobs share files that they do not declare, which only a shared working "
              "directory gives; iunctus runs each job in a folder of its own")
KNOWN = {
    "profile": "4000 jobs of one container each take longer than the time limit of this script",
    "test_access_patterns": "a job reads where its input links point; iunctus gives inputs as "
                            "links into its store",
    "test_all_temp": "it needs --all-temp, an option of Snakemake's test suite",
    "test_checkpoint_rerun": LINKS,
    "test_delete_output": LINKS,
    "test_directory": LINKS,
    "test_github_issue3687": LINKS,
    "test_github_issue413": LINKS,
    "test_github_issue806": UNDECLARED,
    "test_group_parallel": UNDECLARED,
    "test_module_workflow_snakefile_usage": "a job reads a file of the workflow at its absolute "
                                            "path on this machine, which its container does not "
                                            "see",
    "test_priority": UNDECLARED,
    "test_protected_symlink_output": LINKS,
    "test_service_jobs": "a service job runs as long as the jobs that use it, which iunctus "
                         "does not do yet, so the run waits",
    "test_shadow": LINKS,
    "test_shadow_copy": UNDECLARED,
    "test_symlink_temp": LINKS,
    "test_symlink_time_handling": LINKS,
    "test_template_engine": "iunctus does not render the templates of template_engine rules yet",
}
IMAGES = re.compile(r"^\s*(?:container|singularity)\s*:\s*[\"']([^\"']+)[\"']", re.M)


def local_images():
    """The images on this machine, by every name they have."""
    text = subprocess.run(["docker", "image", "ls", "--format", "{{.Repository}}:{{.Tag}}"],
                          capture_output=True, text=True, check=True).stdout
    names = set()
    for name in text.split():
        names.update({name, name.removeprefix("docker.io/"),
                      name.removeprefix("docker.io/library/")})
    return names


def launches():
    """How each test runs: its fields from run_test_workflows_launches.txt, by test name."""
    found = {}
    for line in LAUNCHES.read_text().splitlines():
        if line.startswith("tests/"):
            name, _, *fields = line.split("\t")
            found[name.removeprefix("tests/")] = fields
    return found


def sources(folder):
    """The text of the Snakefiles and rule files of a test."""
    texts = []
    for path in folder.rglob("*"):
        if path.is_file() and (path.name.startswith("Snakefile") or path.suffix == ".smk"):
            texts.append(path.read_text(errors="replace"))
    return "\n".join(texts)


def skipped(folder, fields, images):
    """Why a test cannot run here without the network, or None."""
    text = sources(folder)
    for pattern, reason in NEEDS:
        if pattern.search(text):
            return reason
    missing = sorted({image.removeprefix("docker://") for image in IMAGES.findall(text)}
                     - images)
    if missing:
        return f"images not on this machine: {', '.join(missing)}"
    if any(field.split("=")[0] in ("directory", "snakefile", "workflow-profile")
           for field in fields):
        return "runs from another folder than its own"
    if IMAGE not in images:
        return f"image not on this machine: {IMAGE}"
    return None


def commands(fields, work):
    """The commands of Snakemake and iunctus for the fields of a launch."""
    # Snakemake gets the CPUs of the machine as cores, which iunctus gives to
    # `workflow.cores` and to the threads of jobs.
    native = [SNAKEMAKE, "--cores", str(os.cpu_count()), "--jobs", JOBS, "--snakefile",
              "Snakefile"]
    ours = [str(IUNCTUS), "run", "Snakefile", "-lang", "snakemake", "-executor", "local",
            "-no-network",
            "-default-image", IMAGE, "-python-image", IMAGE, "-max-pending", JOBS,
            "-store", str(work / "store")]
    # Snakemake's tests give lists of config files and values, which its command line takes
    # after one option each; a second option would replace the first.
    configfiles, configs = [], []
    for field in fields:
        key, _, value = field.partition("=")
        if key == "target":
            native.append(value)
            ours.append(value)
        elif key == "configfile":
            configfiles.append(value)
            ours += ["-configfile", value]
        elif key == "config":
            configs.append(value)
            ours += ["--config", value]
        elif key == "profile":
            native += ["--profile", value]
            ours += ["-profile", value]
    native += ["--configfile"] + configfiles if configfiles else []
    native += ["--config"] + configs if configs else []
    return native, ours


def state(folder):
    """Each file below a folder by its path, with its digest, or where a link points."""
    files = {}
    subprocess.run(["chmod", "-R", "u+rX", str(folder)], capture_output=True)
    for path in sorted(folder.rglob("*")):
        name = str(path.relative_to(folder))
        if name.split("/")[0] in OWN or path.name == TIMESTAMP or PYCACHE in name.split("/"):
            continue
        if path.is_symlink():
            files[name] = "link to " + os.readlink(path)
        elif path.is_file():
            digest = hashlib.sha256()
            try:
                with open(path, "rb") as file:
                    for part in iter(lambda: file.read(1 << 20), b""):
                        digest.update(part)
                files[name] = digest.hexdigest()
            except OSError as error:
                files[name] = f"unreadable: {error}"
        elif path.is_dir() and not any(path.iterdir()):
            files[name] = "empty folder"
    return files


def remove(folder):
    """Removes a folder, also when a test took away the rights to change it."""
    subprocess.run(["chmod", "-R", "u+rwX", str(folder)], capture_output=True)
    shutil.rmtree(folder, ignore_errors=True)


def changes(before, after):
    """What a run made, changed or removed: each path with its new state, None if removed."""
    found = {name: value for name, value in after.items() if before.get(name) != value}
    found.update({name: None for name in before if name not in after})
    return found


def run(command, source, place, environment):
    """Runs a command without network in a fresh copy of the test at `place`; the exit code,
    what it changed, and a line of its errors."""
    remove(place)
    shutil.copytree(source, place, symlinks=True)
    # The tests live in Snakemake's git repository, whose files its source deployment sends
    # to jobs; iunctus sends them the same way.
    for words in (["git", "init", "-q"], ["git", "add", "-A"]):
        subprocess.run(words, cwd=place, capture_output=True, check=True)
    before = state(place)
    words = ["unshare", "--user", "--net", "--map-current-user", "--"] + command
    # A session of its own, so that a stop reaches the jobs that the tool started too, and
    # errors in a file, since jobs that outlive the tool, like readers of a pipe, would keep a
    # pipe open.
    with tempfile.TemporaryFile("w+") as errors_file:
        process = subprocess.Popen(words, cwd=place, stdout=subprocess.DEVNULL,
                                   stderr=errors_file, text=True, env=environment,
                                   start_new_session=True)
        try:
            code = process.wait(timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            # Ctrl-C lets the tool stop its jobs and containers.
            os.killpg(process.pid, signal.SIGINT)
            try:
                process.wait(timeout=60)
            except subprocess.TimeoutExpired:
                pass
            code = "timeout"
        # Jobs that outlive the tool end with it.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        errors_file.seek(0)
        errors = ["timeout"] if code == "timeout" else errors_file.read().splitlines()
    # A run that needed an image that is not on this machine says so on its own line.
    pulls = [line for line in errors if "real runs here pull no images" in line]
    return code, changes(before, state(place)), (pulls or [error_line(errors)])[0][:200]


def error_line(lines):
    """The line of an error output that says the most: the first that names an error."""
    lines = [line.strip() for line in lines if line.strip()]
    for number, line in enumerate(lines):
        if line.startswith("iunctus: ") or re.search(r"Error\b|error:|Exception", line):
            # A line that only names the kind of error goes on in the next one.
            if line.endswith(":") and number + 1 < len(lines):
                line = f"{line} {lines[number + 1]}"
            return line[:160]
    return (lines[-1] if lines else "")[:160]


def expected(source, place):
    """The files of `expected-results` that the folder does not hold with the same content."""
    wanted = source / "expected-results"
    if not wanted.is_dir():
        return None
    wrong = []
    for path in sorted(wanted.rglob("*")):
        # Placeholders that keep a folder in git are not results, as in Snakemake's tests, and
        # Snakemake's own time stamps are left out, as in the comparison of the runs.
        if path.is_file() and path.name not in (".gitkeep", ".gitignore", TIMESTAMP):
            name = str(path.relative_to(wanted))
            made = place / name
            if not made.is_file() or made.read_bytes() != path.read_bytes():
                wrong.append(name)
    return wrong


def first_difference(native, ours, varying):
    """The first path whose state differs between two runs, or None."""
    for name in sorted(set(native) | set(ours)):
        if native.get(name) == ours.get(name):
            continue
        if name in varying and native.get(name) is not None and ours.get(name) is not None:
            continue
        if name not in ours:
            return f"{name}: only Snakemake changed it"
        if name not in native:
            return f"{name}: only iunctus changed it"
        if ours[name] is None:
            return f"{name}: iunctus removed it"
        if native[name] is None:
            return f"{name}: Snakemake removed it"
        return f"{name}: other content"
    return None


def compare(name, fields, work, environment, images):
    """The record line of one test."""
    source = TESTS / name
    reason = skipped(source, fields, images)
    if reason:
        return f"skipped: {reason}"
    place = work / "test" / name
    native, ours = commands(fields, work)
    native_code, native_changes, native_error = run(native, source, place, environment)
    native_expected = expected(source, place)
    our_code, our_changes, our_error = run(ours, source, place, environment)
    our_expected = expected(source, place)
    if "real runs here pull no images" in our_error:
        return "skipped: needs an image that is not on this machine (found when it ran)"
    should_fail = "shouldfail" in fields
    if native_code != 0 and our_code != 0:
        return "same: fails as the test expects" if should_fail else "same: both fail"
    if our_code != 0:
        return f"iunctus fails (exit {our_code}): {our_error}"
    if native_code != 0:
        if should_fail:
            return "differs: iunctus succeeds where the test expects a failure"
        return f"snakemake fails (exit {native_code}): {native_error}"
    found = first_difference(native_changes, our_changes, set())
    if found:
        # Files that differ between two runs of Snakemake count by name only.
        _, again, _ = run(native, source, place, environment)
        varying = {path for path in native_changes if again.get(path) != native_changes[path]}
        found = first_difference(native_changes, our_changes, varying)
    remove(place)
    if found:
        return f"differs: {found}"
    if our_expected:
        if native_expected == our_expected:
            return f"same: both differ from expected-results in {our_expected[0]}"
        return f"differs: {our_expected[0]} is not as in expected-results"
    return f"same ({len(native_changes)} files)"


def main():
    wanted = set(sys.argv[1:])
    work = Path(tempfile.mkdtemp(prefix="iunctus-real-snakemake-"))
    records = {}
    try:
        images = local_images()
        launched = launches()
        no_pull = NoPull(work)
        socket = no_pull.__enter__()
        environment = dict(os.environ, XDG_CACHE_HOME=str(work / "cache"),
                           DOCKER_HOST=f"unix://{socket}")
        for folder in sorted(TESTS.iterdir()):
            name = folder.name
            if not (folder / "Snakefile").is_file() or (wanted and name not in wanted):
                continue
            record = compare(name, launched.get(name, []), work, environment, images)
            if name in KNOWN and not record.startswith(("same", "skipped")):
                record += f" (known: {KNOWN[name]})"
            records[name] = record
            print(f"{name}\t{record}", flush=True)
        no_pull.__exit__(None, None, None)
    finally:
        remove(work)
    if wanted and OUTPUT.exists():
        for line in OUTPUT.read_text().splitlines():
            if line and not line.startswith("#"):
                name, _, rest = line.partition("\t")
                records.setdefault(name, rest)
    counts = {}
    for record in records.values():
        kind = record.split(":")[0].split(" (")[0]
        counts[kind] = counts.get(kind, 0) + 1
    summary = ", ".join(f"{count} {kind}" for kind, count in sorted(counts.items()))
    header = ("# The tests of Snakemake run for real by iunctus and by Snakemake 9.27.0, compared "
              "by the files each run made; written by real_runs_snakemake.py. A tab ends the "
              "name.\n"
              f"# {len(records)} tests: {summary}.\n")
    OUTPUT.write_text(header + "".join(f"{name}\t{record}\n"
                                       for name, record in sorted(records.items())))


if __name__ == "__main__":
    main()

#!/usr/bin/env -S uv run -q --with cwltool==3.3.20260925135507 python3
"""Refresh cwltool rejection evidence for CWL workflow fixtures."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import json
import os
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from ruamel.yaml import YAML

HOME = Path.home() / ".cache/iunctus/test-workflows"
TEST_WORKFLOWS = Path(os.environ.get("CWL_TEST_WORKFLOWS") or HOME / "cwl")
FIXTURES = Path(__file__).parent
OUTPUT = FIXTURES / "rejected.txt"
COMMAND = ["cwltool", "--validate", "--skip-schemas", "--enable-dev"]
SUITES = [
    "conformance/v1.0/v1.0/conformance_test_v1.0.yaml",
    "conformance/v1.1/conformance_tests.yaml",
    "conformance/v1.2/conformance_tests.yaml",
]
# cwltool wraps its messages to the width of the terminal.
os.environ["COLUMNS"] = "1000"


# The reasons of documents whose error from cwltool does not say it.
REASONS = {
    "realworld/workflowhub/258/workflow.cwl": (
        "invalid",
        "the step `append_ligand` runs biobb_adapters/append_ligand.cwl, an empty file",
    ),
}


def identifier(text):
    """An identifier without the document it belongs to, as `step/output`."""
    return text.rsplit("#", 1)[-1].removeprefix("main/")


def short_type(text):
    """A type as cwltool writes it in JSON, in the short form of CWL, like `File[]?`."""
    try:
        value = json.loads(text)
    except ValueError:
        return text

    def render(value):
        if isinstance(value, str):
            return identifier(value)
        if isinstance(value, list):
            others = [render(item) for item in value if item != "null"]
            optional = "?" if "null" in value else ""
            single = others[0] if len(others) == 1 else "(" + " or ".join(others) + ")"
            return single + optional
        if isinstance(value, dict) and value.get("type") == "array":
            return render(value.get("items")) + "[]"
        if isinstance(value, dict):
            return str(value.get("type", "record"))
        return str(value)

    return render(value)


def differences():
    """The documents of differences.txt."""
    return [
        line.split(" ", 1)[0]
        for line in (FIXTURES / "differences.txt").read_text().splitlines()
        if line and not line.startswith("#")
    ]


def documents():
    """The documents of the records in rejected.txt, without those of differences.txt."""
    known = set(differences())
    found = []
    for line in OUTPUT.read_text().splitlines():
        if line and not line.startswith(("#", "=", "$", ">")):
            name = line.split(" ", 1)[0]
            if name not in known and name not in found:
                found.append(name)
    return found


def should_fail():
    """The conformance tools that only `should_fail` tests use."""
    uses = {}
    for suite in SUITES:
        path = TEST_WORKFLOWS / suite
        for test in YAML(typ="safe", pure=True).load(path.read_text()) or []:
            if not isinstance(test, dict) or "tool" not in test:
                continue
            tool = os.path.normpath(os.path.join(os.path.dirname(suite), test["tool"]))
            uses.setdefault(tool.split("#", 1)[0], []).append(bool(test.get("should_fail")))
    return {tool for tool, failing in uses.items() if all(failing)}


def error(document):
    """cwltool's error for a document, without colours, places and local paths, or `None`
    when cwltool accepts the document."""
    result = subprocess.run(
        [*COMMAND, document], capture_output=True, text=True, timeout=600, cwd=TEST_WORKFLOWS
    )
    if result.returncode == 0:
        return None
    # `--validate` writes its messages to standard output.
    text = re.sub(r"\x1b\[[0-9;]*m", "", result.stdout + result.stderr)
    text = text.replace(TEST_WORKFLOWS.as_uri() + "/", "").replace(str(TEST_WORKFLOWS) + "/", "")
    lines = text.splitlines()
    first = next((index for index, line in enumerate(lines) if line.startswith("ERROR")), 0)
    cleaned = []
    for line in lines[first:]:
        line = re.sub(r"^\S+\.(cwl|ya?ml|json):\d+:\d+:", "", line)
        line = re.sub(r"\s+", " ", line).strip()
        if line and not line.startswith(("Traceback", "File \"", "raise ", "~", "^")):
            cleaned.append(line)
    return cleaned


def relative(target, document):
    """A path of the test workflows as the document names it, relative to its folder."""
    return os.path.relpath(target, os.path.dirname(document))


def in_repository(target):
    """Whether the git checkout that holds a missing file has it at its commit."""
    folder = (TEST_WORKFLOWS / target).parent
    while folder != TEST_WORKFLOWS and not (folder / ".git").exists():
        folder = folder.parent
    if folder == TEST_WORKFLOWS:
        return False
    inner = os.path.relpath(TEST_WORKFLOWS / target, folder)
    check = subprocess.run(
        ["git", "-C", str(folder), "cat-file", "-e", f"HEAD:{inner}"], stderr=subprocess.DEVNULL
    )
    return check.returncode == 0


def names(words):
    """`a`, `a` and `b`, or `a`, `b` and `c`."""
    words = [f"`{word}`" for word in dict.fromkeys(words)]
    return words[0] if len(words) == 1 else ", ".join(words[:-1]) + " and " + words[-1]


def reason(document, lines):
    """The kind and the reason of a rejection, from cwltool's error."""
    text = "\n".join(lines)
    # Paths of the test workflows are relative to their root here; others stay absolute.
    absent = re.findall(r"No such file or directory: '([^']+)'", text)
    absent += re.findall(r"Field '(?:run|location|path)' contains undefined reference to "
                         r"'(?:file://)?([^']+)'", text)
    if absent:
        for target in absent:
            if not Path(target).is_absolute() and in_repository(target):
                print(f"{document}: {target} is in its repository; add it to referenced.txt",
                      file=sys.stderr)
        shown = [target if Path(target).is_absolute() else relative(target, document)
                 for target in dict.fromkeys(absent)]
        files = names(shown[:3]) + (" and more" if len(shown) > 3 else "")
        if all(Path(target).is_absolute() for target in shown):
            return "missing", f"the document names {files} on the author's computer"
        entry = document.startswith("realworld/workflowhub/")
        holder = "WorkflowHub entry" if entry else "repository"
        return "missing", f"the document names {files}, which its {holder} does not have"
    yaml = re.search(r"(mapping values are not allowed here|found duplicate key \"[^\"]+\""
                     r"|could not find expected '[^']+'|did not find expected [a-z ]+"
                     r"|found character that cannot start any token|found undefined alias"
                     r"|expected <block end>, but found '[^']+')", text)
    if yaml:
        return "invalid", f"the file is not valid YAML: {yaml.group(1)}"
    if "Missing 'class' field" in text and "tried 'Workflow'" in text:
        return "invalid", "the file has no `class`, so it describes no CWL process"
    step = re.findall(r"Step is missing required parameters? ('[^']+'(?:, '[^']+')*)", text)
    if step:
        inputs = [name.strip("'") for group in step for name in group.split(", ")]
        noun = "input" if len(set(inputs)) == 1 else "inputs"
        return "invalid", f"a step does not give the required {noun} {names(inputs)}"
    source = re.search(r"Source '([^']+)' of type (.+?) is incompatible\s+with sink '([^']+)' "
                       r"of type (.+?)(?:\n|$)", text)
    if source:
        why = (f"the source `{source.group(1)}` of type `{short_type(source.group(2))}` does not "
               f"fit `{source.group(3)}` of type `{short_type(source.group(4))}`")
        pick = re.search(r"pickValue is: (\w+)", text)
        merge = re.search(r"linkMerge method (\w+)", text)
        if pick or merge:
            ways = [f"pickValue {pick.group(1)}"] if pick else []
            ways += [f"linkMerge {merge.group(1)}"] if merge else []
            why += " with " + " and ".join(ways)
        return "invalid", why
    unknown = re.findall(r"Field '(source|outputSource)' references unknown identifier '([^']+)'",
                         text)
    if unknown:
        targets = [identifier(name) for _, name in unknown]
        verb = "names" if len(set(targets)) == 1 else "name"
        return "invalid", f"the `{unknown[0][0]}` {names(targets)} {verb} nothing in the workflow"
    output = re.search(r"Workflow step output '([^']+)' does not correspond to\s+tool output "
                       r"\(expected ([^)]+)\)", text)
    if output:
        expected = [name.strip("' ") for name in output.group(2).split(",")]
        return "invalid", (f"a step names the output `{identifier(output.group(1))}`, which its "
                           f"tool does not have (it has {names(expected)})")
    feature = re.search(r"Workflow contains (\w+(?: \w+)?) but (\w+) not in requirements", text)
    if feature:
        article = "an" if feature.group(1)[0] in "aeiou" else "a"
        what, needs = feature.group(1), feature.group(2)
        return "invalid", f"the workflow has {article} {what} without {needs}"
    circle = re.search(r"The following steps have circular dependency:\s*\[([^\]]*)\]", text)
    if circle:
        steps = [identifier(name.strip("' ")) for name in circle.group(1).split(",")]
        return "invalid", f"the steps {names(steps)} depend on their own outputs"
    parameter = re.search(r"Output '([^']+)' is not a valid OutputParameter", text)
    if parameter:
        return "invalid", f"the output `{parameter.group(1)}` is not a valid output parameter"
    fields = re.findall(r"the '([^']+)' field is not valid because", text)
    last = lines[-1].removeprefix("* ") if lines else "no message"
    if len(last) > 160:
        last = last[:157] + "..."
    if fields:
        return "invalid", f"the field `{fields[-1]}` does not follow the CWL schema: {last}"
    return "invalid", f"cwltool rejects the document: {last}"


def main():
    targets = documents()
    failing = should_fail()
    with ThreadPoolExecutor(max_workers=4) as pool:
        errors = list(pool.map(error, targets))
        for document, verdict in zip(differences(), pool.map(error, differences())):
            if verdict is None:
                print(f"{document}: cwltool accepts it now; remove it from differences.txt",
                      file=sys.stderr)
    lines = [
        "# Test workflows that cwltool rejects and the loader must not load either, relative to",
        "# the root folder of the test workflows; all other documents must load, except those of",
        "# differences.txt. Recorded by rejected.py with cwltool 3.3.20260925135507: each block",
        "# gives the document, then `=`, the kind (`invalid`, `missing` or `unsupported (decision",
        "# N)`) and why in one plain sentence, then the command (`$`) and cwltool's error (`>`).",
    ]
    for document, error_lines in zip(targets, errors):
        if error_lines is None:
            print(f"{document}: cwltool accepts it now", file=sys.stderr)
            continue
        kind, why = REASONS.get(document) or reason(document, error_lines)
        if document in failing:
            why += " (the tool of a should_fail conformance test)"
        lines.append("")
        lines.append(document)
        lines.append(f"= {kind}: {why}")
        lines.append("$ " + " ".join(COMMAND) + " " + document)
        shown = error_lines if len(error_lines) <= 8 else error_lines[:2] + error_lines[-6:]
        lines.extend("> " + line for line in shown)
    OUTPUT.write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()

#!/usr/bin/env -S uv run -q --with pyyaml python3
"""Refresh Argo lint evidence for rejected workflow fixtures."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import os
import re
import shutil
import subprocess
import tempfile

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.environ.get("ARGO_TEST_WORKFLOWS") or os.path.expanduser("~/.cache/uwe/test-workflows/argo")
KINDS = ("WorkflowTemplate", "ClusterWorkflowTemplate")
HEADER = """\
# Why each Argo test workflow that does not pass cannot run, recorded by rejected.py: the
# document, then `=`, its kind and one plain sentence, then for `invalid` and `missing` the
# command (`$`) and the output (`>`) of `argo lint` of Argo v4.1.4, which rejects the document
# the same way, or accepts a workflow that would wait forever in Argo. The kind is `invalid`
# (the workflow itself is broken), `missing` (it needs a WorkflowTemplate that is in no
# repository of the test workflows), or `unsupported (decision N)` (uwe leaves the feature
# out on purpose, as decision N says).
# argo lint ran on a copy of the document and of the WorkflowTemplates it calls, without
# namespaces, the called templates only, and with values for the inputs of a Workflow's
# entrypoint, as `argo submit -p` gives them."""
REPOSITORIES = {}


def outcomes():
    for line in open(os.path.join(HERE, "outcomes.txt")):
        if line.startswith("#") or "\t" not in line:
            continue
        key, outcome = line.rstrip("\n").split("\t", 1)
        if not outcome.startswith("ok") and outcome != "valid":
            yield key, outcome


def load(path):
    try:
        with open(os.path.join(ROOT, path), encoding="utf-8") as handle:
            return [d for d in yaml.safe_load_all(handle) if isinstance(d, dict)]
    except Exception:
        return []


def documents_of(repository):
    if repository in REPOSITORIES:
        return REPOSITORIES[repository]
    found = REPOSITORIES.setdefault(repository, {})
    for directory, _, names in os.walk(os.path.join(ROOT, repository)):
        if "/.git" in directory:
            continue
        for name in names:
            if name.lower().endswith((".yaml", ".yml")):
                path = os.path.relpath(os.path.join(directory, name), ROOT)
                found[path] = load(path)
    return found


def templates(document):
    return (document.get("spec") or {}).get("templates") or []


def kind_of(reference):
    return "ClusterWorkflowTemplate" if reference.get("clusterScope") else "WorkflowTemplate"


def calls(node, local, remote):
    """The names of the templates that a node calls: in its own document, and by templateRef."""
    if isinstance(node, dict):
        reference = node.get("templateRef")
        if isinstance(reference, dict):
            remote.add(((kind_of(reference), reference.get("name")), reference.get("template")))
        whole = node.get("workflowTemplateRef")
        if isinstance(whole, dict):
            remote.add(((kind_of(whole), whole.get("name")), None))
        for key in ("template", "onExit"):
            if isinstance(node.get(key), str):
                local.add(node[key])
        for value in node.values():
            calls(value, local, remote)
    elif isinstance(node, list):
        for value in node:
            calls(value, local, remote)


def dependencies(path, documents):
    """The WorkflowTemplates that a document calls, each cut down to the called templates."""
    wanted = {}
    queue = []
    for document in documents.get(path, []):
        local, remote = set(), set()
        calls(document, local, remote)
        queue.extend(remote)
    while queue:
        name, template = queue.pop()
        kind, name = name
        copies = [(p, d) for p, ds in documents.items() for d in ds
                  if d.get("kind") == kind and (d.get("metadata") or {}).get("name") == name]
        holding = [(p, d) for p, d in copies if any(t.get("name") == template for t in templates(d))]
        if not (holding or copies):
            continue
        dep_path, document = (holding or copies)[0]
        names = wanted.setdefault((dep_path, name), (document, set()))[1]
        pending = [template] if template else [t.get("name") for t in templates(document)]
        while pending:
            current = pending.pop()
            if current in names:
                continue
            names.add(current)
            for node in templates(document):
                if node.get("name") == current:
                    local, remote = set(), set()
                    calls({k: v for k, v in node.items() if k != "name"}, local, remote)
                    pending.extend(local)
                    queue.extend(remote)
    result = {}
    for (dep_path, _), (document, names) in wanted.items():
        pruned = dict(document)
        pruned["spec"] = dict(document.get("spec") or {})
        pruned["spec"]["templates"] = [t for t in templates(document) if t.get("name") in names]
        pruned.setdefault("metadata", {}).pop("namespace", None)
        result.setdefault(dep_path, []).append(pruned)
    return result


def with_entry_values(parsed, text):
    """A Workflow with values for the inputs of its entrypoint, as `argo submit -p` gives them."""
    if not parsed or not all(d.get("kind") == "Workflow" for d in parsed):
        return text
    added = False
    for document in parsed:
        spec = document.get("spec") or {}
        entry = [t for t in templates(document) if t.get("name") == spec.get("entrypoint")]
        given = (spec.get("arguments") or {}).get("parameters") or []
        names = [p.get("name") for p in given]
        for parameter in ((entry[0].get("inputs") or {}).get("parameters") or []) if entry else []:
            if "value" not in parameter and "default" not in parameter and parameter["name"] not in names:
                given.append({"name": parameter["name"], "value": "x"})
                added = True
        if given:
            spec.setdefault("arguments", {})["parameters"] = given
        (document.get("metadata") or {}).pop("namespace", None)
    return yaml.safe_dump_all(parsed, sort_keys=False) if added else text


def lint(key, strict):
    path = key.split("#")[0]
    documents = documents_of(path.split("/")[0])
    extra = dependencies(path, documents)
    extra.pop(path, None)
    work = tempfile.mkdtemp(prefix="argo-lint-")
    text = open(os.path.join(ROOT, path), encoding="utf-8", errors="replace").read()
    text = re.sub(r"(?m)^  namespace:.*$", "", text)
    try:
        text = with_entry_values(documents.get(path), text)
    except (AttributeError, KeyError, TypeError):
        pass  # A malformed document goes to argo lint as it is.
    for file, content in [(path, text)] + [(p, yaml.safe_dump_all(d, sort_keys=False)) for p, d in extra.items()]:
        target = os.path.join(work, file)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "w") as handle:
            handle.write(content)
    command = ["argo", "lint", "--offline", f"--strict={'true' if strict else 'false'}", "--no-color", "-o", "simple", path, *sorted(extra)]
    run = subprocess.run(command, cwd=work, capture_output=True, text=True)
    shutil.rmtree(work)
    lines = []
    for line in (run.stdout + run.stderr).splitlines():
        if any(line.startswith(p + ": ") for p in extra):
            continue
        line = re.sub(r'^time=\S+ ', "", line)
        if line.strip():
            lines.append(line)
    shown = command[:]
    shown[3] = "--strict=true" if strict else "--strict=false"
    return run.returncode, " ".join(a if " " not in a else f"'{a}'" for a in shown if a not in ("--no-color", "-o", "simple")), lines


def defined(name):
    """Whether any repository of the test workflows defines a WorkflowTemplate of this name."""
    for repository in sorted(os.listdir(ROOT)):
        if os.path.isdir(os.path.join(ROOT, repository)) and not repository.startswith(("_", ".")):
            for documents in documents_of(repository).values():
                for document in documents:
                    if document.get("kind") in KINDS and (document.get("metadata") or {}).get("name") == name:
                        return True
    return False


def reason(message, path):
    """The kind and one plain sentence for what argo lint says about the document at `path`."""
    other = re.search(r"from file (\S+):", message)
    if other and other[1] != path:
        kind, why = reason(message, other[1])
        return kind, f"the WorkflowTemplate that it calls, in {other[1]}, is broken: {why}"
    rules = [
        (r'couldn\'t find (?:cluster )?workflow template "([^"]+)"', lambda m: (
            ("invalid", f"it calls WorkflowTemplate {m[1]}, which Argo cannot find") if defined(m[1])
            else ("missing", f"it calls WorkflowTemplate {m[1]}, which is in no repository of the test workflows"))),
        (r"template reference ([^.\s]+)\.(\S+) not found", lambda m: ("invalid", f"it calls template {m[2]} of WorkflowTemplate {m[1]}, which does not have it")),
        (r"inputs\.parameters\.(\S+) was not supplied", lambda m: ("invalid", f"a step or task calls a template without its input parameter {m[1]}")),
        (r"inputs\.artifacts\.(\S+) was not supplied", lambda m: ("invalid", f"a step or task calls a template without its input artifact {m[1]}")),
        (r"template name '([^']+)' undefined", lambda m: ("invalid", f"it calls template {m[1]}, which the document does not define")),
        (r"template '([^']+)' type is unknown", lambda m: ("invalid", f"template {m[1]} has no type, like container, script, steps or dag")),
        (r"failed to get a template", lambda m: ("invalid", "a step or task names no template")),
        (r"name '([^']+)' is not unique", lambda m: ("invalid", f"two entries of a list have the same name {m[1]}")),
        (r"name: '([^']+)' is invalid", lambda m: ("invalid", f"the name {m[1]} has characters that Argo does not allow")),
        (r"name is required", lambda m: ("invalid", "an entry of a list has no name")),
        (r"spec\.entrypoint is required", lambda m: ("invalid", "the workflow names no entrypoint")),
        (r"failed to resolve \{\{([^}]+)\}\}", lambda m: ("invalid", f"the tag {{{{{m[1]}}}}} names a variable that nothing sets")),
        (r"cannot unmarshal \S+ into Go struct field \S*?\.?(spec\.[\w.]+)", lambda m: ("invalid", f"the field {m[1]} has the wrong type")),
        (r"yaml: line (\d+)", lambda m: ("invalid", f"the file is not valid YAML (line {m[1]})")),
    ]
    for pattern, sentence in rules:
        match = re.search(pattern, message)
        if match:
            return sentence(match)
    return "invalid", message


def unsupported(outcome):
    match = re.search(r"(template \S+ (?:is a|has|runs)[^;]*);", outcome)
    return f"= unsupported (decision 18): {match[1] if match else outcome}"


def main():
    blocks = []
    for key, outcome in outcomes():
        if outcome.startswith("33 "):
            blocks.append(f"{key}\n{unsupported(outcome)}")
            continue
        code, command, lines = lint(key, strict=False)
        evidence = [f"$ {command}"] + [f"> {line}" for line in lines]
        if "neither the success condition nor the failure condition holds" in outcome:
            why = ("it waits for a Workflow whose phase never meets the success or failure condition, "
                   "so Argo waits until the run is stopped; argo lint accepts it")
            blocks.append("\n".join([key, f"= invalid: {why}"] + evidence))
            continue
        if code == 0:
            code, command, strict = lint(key, strict=True)
            evidence += [f"$ {command}"] + [f"> {line}" for line in strict]
            kind, why = "invalid", "only the strict mode of Argo rejects it, the default of argo lint and argo submit"
            if "duplicate key" in outcome:
                key_name = re.search(r"duplicate key `([^`]+)`", outcome)
                why = f"it sets the key {key_name[1]} twice, which argo lint and argo submit reject in their default strict mode"
        else:
            kind, why = reason(" ".join(lines), key.split("#")[0])
        blocks.append("\n".join([key, f"= {kind}: {why}"] + evidence))
    with open(os.path.join(HERE, "rejected.txt"), "w") as handle:
        handle.write(HEADER + "\n\n" + "\n\n".join(blocks) + "\n")


main()

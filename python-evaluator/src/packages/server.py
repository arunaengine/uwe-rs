# The Python process of the packages without a stand-in in iunctus (decision 19). It reads
# one JSON request per line on standard input and writes one JSON reply per line on standard
# output. The protocol is in docs/design/snakemake/python-packages.md.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import importlib
import importlib.metadata
import json
import math
import numbers
import operator
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

# The working directory is not on the module path, as it is not when Snakemake runs; the
# workflow's own modules are run by iunctus.
if sys.path and sys.path[0] in ("", os.getcwd()):
    del sys.path[0]

replies = sys.stdout
# What packages print goes to standard error, so that standard output holds only replies.
sys.stdout = sys.stderr

objects = {}  # handle -> object
counts = {}  # handle -> how often it was sent and not yet released
handles = {}  # id(object) -> handle
next_handle = [1]


def handle_of(value):
    number = handles.get(id(value))
    if number is None:
        number = next_handle[0]
        next_handle[0] += 1
        handles[id(value)] = number
        objects[number] = value
        counts[number] = 0
    counts[number] += 1
    handle = {"handle": number, "type": type(value).__name__, "callable": callable(value)}
    if isinstance(value, type):
        handle["name"] = value.__qualname__
        handle["module"] = value.__module__
    return handle


def release(numbers_):
    for number in numbers_:
        if number in counts:
            counts[number] -= 1
            if counts[number] <= 0:
                value = objects.pop(number)
                del counts[number]
                handles.pop(id(value), None)


def encode(value):
    if value is None or isinstance(value, bool):
        return value
    if type(value).__name__ == "bool_":
        return bool(value)
    if isinstance(value, numbers.Integral):
        value = int(value)
        return value if -(2**63) <= value < 2**63 else {"int": str(value)}
    if isinstance(value, numbers.Real):
        value = float(value)
        if math.isnan(value):
            return {"float": "nan"}
        if math.isinf(value):
            return {"float": "inf" if value > 0 else "-inf"}
        return {"float": value}
    if isinstance(value, str):
        return str(value)
    if isinstance(value, (bytes, bytearray)):
        return {"bytes": bytes(value).hex()}
    if type(value) in (pathlib.PosixPath, pathlib.PurePosixPath):
        return {"path": str(value)}
    if isinstance(value, tuple) and not hasattr(value, "_fields"):
        return {"tuple": [encode(item) for item in value]}
    if isinstance(value, list):
        return {"list": [encode(item) for item in value]}
    if isinstance(value, (set, frozenset)):
        return {"set": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"dict": [[encode(key), encode(item)] for key, item in value.items()]}
    return handle_of(value)


def decode(value):
    if isinstance(value, dict):
        tag, inner = next(iter(value.items()))
        if tag == "handle":
            if inner not in objects:
                raise ValueError(f"iunctus sent the unknown handle {inner}")
            return objects[inner]
        if tag == "int":
            return int(inner)
        if tag == "float":
            return float(inner)
        if tag == "bytes":
            return bytes.fromhex(inner)
        if tag == "path":
            return pathlib.Path(inner)
        if tag == "list":
            return [decode(item) for item in inner]
        if tag == "tuple":
            return tuple(decode(item) for item in inner)
        if tag == "set":
            return {decode(item) for item in inner}
        if tag == "dict":
            return {decode(key): decode(item) for key, item in inner}
        raise ValueError(f"iunctus sent a value of the unknown kind {tag}")
    return value


# Where packages that later imports need are installed, in the running container.
EXTRA = "/tmp/iunctus-packages"
CONSTRAINTS = json.loads(os.environ.get("IUNCTUS_PYTHON_CONSTRAINTS", "[]"))


def failures():
    """The packages that could not be installed into the environment, with uv's error."""
    folder = os.path.join(sys.prefix, "iunctus-failed")
    found = {}
    for package in os.listdir(folder) if os.path.isdir(folder) else []:
        with open(os.path.join(folder, package)) as file:
            found[package] = file.read()
    return found


failed = failures()


def first_lines(text):
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return " ".join(lines[:4])


def install(packages):
    """Installs packages from PyPI into the running container with uv, when it has uv, and
    gives the ones that could not be installed, with the first lines of uv's error."""
    uv = shutil.which("uv")
    wanted = [package for package in packages if package not in failed]
    if uv is not None and wanted:
        command = [uv, "pip", "install", "--quiet", "--python", sys.executable, "--target", EXTRA]
        with tempfile.NamedTemporaryFile(mode="w", suffix=".txt") as constraints:
            constraints.write("\n".join(CONSTRAINTS) + "\n")
            constraints.flush()
            command.extend(["--constraint", constraints.name])
            if subprocess.run([*command, *wanted], capture_output=True, text=True).returncode != 0:
                for package in wanted:
                    done = subprocess.run([*command, package], capture_output=True, text=True)
                    if done.returncode != 0:
                        failed[package] = done.stderr
        if EXTRA not in sys.path:
            sys.path.append(EXTRA)
        sys.path_importer_cache.pop(EXTRA, None)
        importlib.invalidate_caches()
    return [package for package in packages if package in failed]


def validate_versions(packages, imported=False):
    requirements = [text for text in [*CONSTRAINTS, *packages] if any(char in text for char in "<>=!~")]
    if not requirements:
        return
    try:
        from packaging.requirements import Requirement
    except ModuleNotFoundError:
        try:
            from pip._vendor.packaging.requirements import Requirement
        except ModuleNotFoundError:
            raise ImportError("validating pinned Python packages requires packaging in the helper image") from None
    normalized = lambda name: re.sub(r"[-_.]+", "-", name).lower()
    requested = {normalized(Requirement(text).name) for text in packages}
    for text in requirements:
        requirement = Requirement(text)
        if requirement.marker is not None and not requirement.marker.evaluate():
            continue
        if not requirement.specifier:
            continue
        try:
            version = importlib.metadata.version(requirement.name)
        except importlib.metadata.PackageNotFoundError:
            if imported and normalized(requirement.name) in requested:
                raise ImportError(f"cannot validate {text}: installed distribution metadata is missing") from None
            continue
        if not requirement.specifier.contains(version):
            raise ImportError(f"installed {requirement.name}=={version} does not satisfy workflow requirement {text}")


def import_module(name, packages):
    validate_versions(packages)
    try:
        module = importlib.import_module(name)
    except ModuleNotFoundError as error:
        top = name.split(".")[0]
        if (error.name or "").split(".")[0] != top:
            raise
        absent = install(packages)
        if absent:
            reason = first_lines(failed[absent[0]])
            message = f"No module named '{name}' (the package {absent[0]} could not be installed from PyPI: {reason})"
            raise ModuleNotFoundError(message, name=name) from None
        module = importlib.import_module(name)
    validate_versions(packages, imported=True)
    return module


def answer(request):
    release(request.get("release", []))
    op = request["op"]
    if op == "import":
        return import_module(request["module"], request.get("packages", []))
    if op == "stop":
        return None
    if op == "binary":
        function = getattr(operator, request["operator"])
        return function(decode(request["left"]), decode(request["right"]))
    target = decode(request["object"])
    if op == "getattr":
        return getattr(target, request["name"])
    if op == "setattr":
        setattr(target, request["name"], decode(request["value"]))
        return None
    if op == "call":
        args = [decode(arg) for arg in request["args"]]
        kwargs = {name: decode(value) for name, value in request["kwargs"]}
        return target(*args, **kwargs)
    if op == "getitem":
        return target[decode(request["key"])]
    if op == "setitem":
        target[decode(request["key"])] = decode(request["value"])
        return None
    if op == "iter":
        return list(target)
    if op == "len":
        return len(target)
    if op == "bool":
        return bool(target)
    if op == "str":
        return str(target)
    if op == "repr":
        return repr(target)
    if op == "contains":
        return decode(request["item"]) in target
    if op == "unary":
        return getattr(operator, request["operator"])(target)
    if op == "isinstance":
        return isinstance(target, decode(request["class"]))
    raise ValueError(f"iunctus sent the unknown request {op}")


def describe(error):
    kind = type(error)
    plain = (str, int, float, bool, type(None))
    return {
        "type": kind.__name__,
        "module": kind.__module__,
        "bases": [base.__name__ for base in kind.__mro__[1:] if issubclass(base, BaseException)],
        "classes": [
            f"{base.__module__}.{base.__qualname__}"
            for base in kind.__mro__
            if issubclass(base, BaseException)
        ],
        "message": str(error),
        "args": [arg if isinstance(arg, plain) else repr(arg) for arg in error.args],
    }


def main():
    while True:
        line = sys.stdin.readline()
        if not line:
            break
        request = json.loads(line)
        try:
            reply = {"value": encode(answer(request))}
        except BaseException as error:  # noqa: BLE001, every error of a package is a reply
            reply = {"error": describe(error)}
        replies.write(json.dumps(reply) + "\n")
        replies.flush()
        if request.get("op") == "stop":
            break


main()

<!-- The design of Python packages without a stand-in, which run in one container per run (decision 19). -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Python packages in a container

Status: design of decision 19, 2026-09-29. The code is in `python-evaluator` (the interface, the protocol, the objects and the server script), `executor` (a container process that answers lines) and `snakemake-runtime` (the Python container of a Snakemake run, with uv).

## 1. The design in short

1. **Stand-ins first.** The native interpreter keeps its stand-ins for the libraries that workflow code uses most (pandas, polars, yaml, csv, json) and for the packages that come with Snakemake (yte). Local `.py` files are still run by the native interpreter.
2. **Any other package runs in one Python process in a container.** The first `import` of such a package starts the container. The container keeps running until the run has no more Python code to evaluate, and then stops.
3. **The native interpreter stays in charge.** It keeps running the workflow code. Each use of a package object (an attribute, a call, an item, an iteration, `repr()`) becomes one request to the Python process.
4. **Objects stay in the container.** The interpreter holds a handle for each of them. Plain values (`None`, `bool`, `int`, `float`, `str`, `bytes`, lists, tuples, sets, dicts, paths) come back as values.
5. **python-evaluator does not know Docker.** It defines the interface of a Python process that answers one line with one line. The embedder provides the real one; tests provide a fake.
6. **Errors of a package are Python exceptions.** Workflow code catches them with `except` as in Python. A missing container runtime is not an exception: it stops the run with an error that names the package.

## 2. Which imports go to the container

`import_module` looks for a module in this order:

1. modules already imported;
2. modules of the host, like Snakemake's `snakemake.utils`;
3. native stand-ins, like `pandas` or `yte`;
4. `.py` files and packages on `sys.path`;
5. a module of the standard library without a stand-in stays unsupported (exit 33): code in the container would see the container, not the machine of the run;
6. everything else is a package for the container.

A submodule of a package in the container, like `Bio.SeqIO`, is imported in the container too.

## 3. The interface

python-evaluator has two traits:

```rust
/// A running Python process that answers each request line with one reply line.
pub trait PythonProcess {
    fn exchange(&mut self, line: &str) -> Result<String, String>;
    fn stop(&mut self);
}

/// Starts the Python process of a run, for example in a container.
pub trait PythonStarter {
    fn start(&self, start: &PythonStart) -> Result<Box<dyn PythonProcess>, String>;
}
```

`PythonStart` holds the server script, the PyPI packages of the first import, and the working directory of the interpreter. The interpreter keeps a `Packages` value: the starter, the running process and the handles to release. An embedder sets the starter with `Packages::set_starter`. Without a starter, an import of a package stops the run with the error of section 7.

`snakemake-runtime` has `ContainerPython`, the starter that runs the process with the local container engine. It uses `executor::process::ContainerProcess`, which starts a container with an open standard input and exchanges lines with it. `executor` does not know Python, and python-evaluator does not know containers. Tests use a fake starter whose process gives prepared replies and records the requests.

## 4. The protocol

The process reads one JSON object per line on its standard input and writes one JSON object per line on its standard output. What the package prints goes to standard error, which the process passes on.

### 4.1 Requests

Every request has an `op`. It may have `release`, a list of handles that the interpreter no longer holds; the server forgets them before it answers.

| `op` | Fields | Reply value |
|---|---|---|
| `import` | `module` (dotted name), `packages` (the PyPI packages to install when the module is missing) | the module |
| `getattr` | `object`, `name` | the attribute |
| `setattr` | `object`, `name`, `value` | `None` |
| `call` | `object`, `args`, `kwargs` (a list of name and value pairs) | the result |
| `getitem`, `setitem` | `object`, `key` (and `value`) | the item, or `None` |
| `iter` | `object` | a list of all items |
| `len`, `bool`, `repr`, `str` | `object` | an `int`, a `bool` or a `str` |
| `contains` | `object`, `item` | a `bool` |
| `binary` | `operator` (a name of Python's `operator` module, like `add` or `eq`), `left`, `right` | the result |
| `isinstance` | `object`, `class` | a `bool` |
| `stop` | | `None`, then the process ends |

### 4.2 Replies

A reply is `{"value": V}` or `{"error": E}`. `E` has the exception's `type`, its `module`, the names of its base classes, nearest first (`bases`), its `message`, and its `args`, where arguments that are not plain values are their `repr()`.

### 4.3 Values

| Python | JSON |
|---|---|
| `None`, `True`, `False` | `null`, `true`, `false` |
| `int` | a number, or `{"int": "digits"}` outside 64 bits |
| `float` | `{"float": 1.5}`; `{"float": "nan"}`, `"inf"` and `"-inf"` |
| `str` | a string |
| `bytes` | `{"bytes": "hex digits"}` |
| `list`, `tuple`, `set` | `{"list": [...]}`, `{"tuple": [...]}`, `{"set": [...]}` |
| `dict` | `{"dict": [[key, value], ...]}` |
| a path | `{"path": "/a/b"}` |
| any other object | `{"handle": 7, "type": "SeqRecord", "callable": false}`, and for a class also its `name` and `module` |

The server sends subclasses of `int`, `float`, `str`, `list` and `dict` as the plain type, and numbers of numpy as `int` or `float`. A named tuple stays a handle, so that its fields keep their names.

## 5. Handles

The server keeps the objects it sent in a table by handle number, and counts how often it sent each. The same object gets the same number. In the interpreter, a handle is a native object, `PackageObject`. Using it sends a request: `obj.name` sends `getattr`, `obj(...)` sends `call`, `obj[k]` sends `getitem`, `for x in obj` sends `iter`, `len(obj)`, `bool(obj)`, `str(obj)` and `repr(obj)` send their requests, and operators send `binary`. An attribute that the object does not have is an `AttributeError` of the interpreter, so that `hasattr` and `getattr(obj, name, default)` work.

A handle crosses back into the package as `{"handle": 7}` in the arguments of a call. Values of the interpreter cross as the values of section 4.3. Native objects of the interpreter cross as what they stand for: a `pathlib.Path` as a path, Snakemake's lists of files as lists. Functions and objects of the stand-ins, like a pandas table, cannot cross: the call fails with a `TypeError` that says so.

When the interpreter drops a handle, its number goes on the list of the next request's `release`. The server forgets an object once every copy it sent is released.

## 6. The container

### 6.1 The image and the packages

The default image is a slim Python image with uv (`ghcr.io/astral-sh/uv`, Python 3.12, a pinned version). The container runs as the user of the run, so that it reads the workflow's files as that user does.

Before the container starts, uv installs the PyPI packages of the first import into a Python environment, in a container of the same image that ends when the install is done. The environment is kept in a folder of the machine (`~/.cache/iunctus/python/environments/<key>`), whose key is made from the image, the install script and the sorted set of packages, so a later run with the same packages installs nothing. A lock file per key makes a second run wait while the first makes the environment. uv's cache (`~/.cache/iunctus/python/uv`) is shared by all containers; uv handles several users of one cache at once. The Python container mounts the cache folder and runs the environment's Python.

uv installs the packages together, and when that fails, one by one. It keeps the error of each package that it cannot install, like one that PyPI does not have. A later import of another missing package installs it with uv into a folder of the running container, which the next run does not keep.

Import names differ from PyPI package names. A table maps them, like `PIL` to `pillow`, `Bio` to `biopython`, `yaml` to `pyyaml`, `sklearn` to `scikit-learn` and `cv2` to `opencv-python-headless`. Other names map to the name of their top package.

A user can give an image with `iunctus run -python-image <image>`. It must have `python3` with the packages the workflow imports; iunctus installs nothing into it unless it has uv. Every Python container runs with `--ulimit core=0`: a tool that crashes inside it gives iunctus nothing to use from a core dump.

### 6.2 Start and stop

The container starts at the first import of a package without a stand-in. It mounts the folders of the workflow (its working directory, the folder of the Snakefile and the target folder) read-only at the same paths, runs in the working directory, and gets the server script in an environment variable. The server answers requests until it reads `stop` or its standard input closes. The interpreter stops the process when the embedder says that no more Python code runs. The container is removed when it stops (`--rm`), and `docker rm -f` removes it when the process does not end within a few seconds.

For Snakemake, Python code runs while the Snakefile is evaluated, while jobs are planned (input functions), and while the tasks of jobs are made (params). After a checkpoint, the workflow is planned again. The driver stops the process once every job that must run has its task and no checkpoint job is still running, and at the end of the run.

At most one container runs per run. Runs of the test workflows start at most four at once.

## 7. Errors

- An exception of the package becomes a Python exception in the interpreter. A builtin exception, like `KeyError`, keeps its class. Another class, like `pyfaidx.FastaIndexingError`, becomes a class of that name and module whose base is the nearest builtin class of its bases, so that `except ValueError` catches it when Python would. An `except` clause with a class of the package, like `except pyfaidx.FastaNotFoundError:`, catches the exceptions of that class and of the classes that derive from it: the error reply names all classes of the exception (`classes`), and a handle of a class names its module and its name.
- A package that uv cannot install from PyPI is a `ModuleNotFoundError` whose message names the package and gives the first lines of uv's error, so `try: import x` / `except ImportError:` works as in Python. So is an import that still fails after the install.
- No starter, or a starter that cannot start the container (no container engine, an engine that does not run), stops the run with an error that `except` does not catch: "the workflow imports the Python package pyfaidx, which has no stand-in in iunctus; it runs in a Python container (decision 19), but ...", followed by the reason.
- A process that ends while the interpreter waits for a reply stops the run with the last lines of its standard error.

## 8. Limits

- Code of the package that calls back into workflow code, like a key function, is not supported: functions cannot cross.
- The container reads files; writes into the workflow's folder fail, since it is mounted read-only. The native interpreter keeps writes in memory, which the container does not see.
- In mock runs, files that tasks make are empty placeholders that only the driver knows; the container does not see them.

## 9. Tests

- Unit tests in `python-evaluator/tests/packages.rs` run code against a fake process: the requests it records must match the protocol, and its replies must become values, handles and exceptions.
- `snakemake-runtime/tests/python_container.rs` runs code with a real container. It runs only when `IUNCTUS_CONTAINER_TESTS=1`.
- The run test of the Snakemake test workflows passes the container starter when `IUNCTUS_CONTAINER_TESTS=1`. `run_test_workflows_containers.txt` lists the workflows that import a package without a stand-in while they are planned, with their outcome when the container runs. Without the variable, such a workflow fails with the error of section 7; it needs a line in that file, and no record. With the variable, the test runs only these workflows, at most four at once, and each must run or have a record in `run_test_workflows_rejected.txt`, as every other workflow must.

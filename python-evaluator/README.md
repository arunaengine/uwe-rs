<!-- Contracts for container package proxies and interpreter-hosted Python stand-ins. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

## src/lib.rs

An interpreter for the Python subset that workflow languages embed, like the Python code of
Snakefiles. It walks the syntax tree of ruff's parser, and gives native stand-ins for the
library modules that such code calls, `pandas.read_csv` included. It needs no Python
install. Code that uses a feature outside the subset fails with an unsupported error.

## src/modules/yte.rs

A stand-in for yte, the YAML template engine that Snakemake depends on: `process_yaml`
renders a YAML document whose values may be Python expressions (`?expr`), whose keys may
be conditions (`?if`, `?elif`, `?else`) and loops (`?for x in y`), and which may define
functions (`__definitions__`) and variables (`__variables__`).

## src/packages/mod.rs

Python packages without a stand-in (decision 19): they run in one Python process, which
the embedder starts, for example in a container. The interpreter sends each use of a
package object to the process as one request line, and gets one reply line. Objects of the
packages stay in the process as handles; plain values come back as values.


<!-- How uwe runs wir documents: detection, inputs, native blocks, outputs and limits. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Running wir documents

wir is the workflow intermediate representation of uwe: one typed graph format that workflow languages are lowered into. The format, its types and its verifier are described in [wir/README.md](../wir/README.md). This page describes how `uwe run` runs a wir document, through the `wir-runtime` crate.

```console
$ uwe run run.wir.json -inputs inputs.json
```

## Detection

uwe takes a file as a wir document when it is JSON and has a top-level `wir_version` key. The file name does not matter, but `<name>.wir.json` is the convention. `-lang wir` picks wir for any file. uwe parses the document, checks its version, runs the verifier and refuses the document with every problem the verifier found before any task starts.

## Inputs

The inputs of the entry graph come from the JSON file of `-inputs` and from parameters like `--lines 3`. A parameter is read by the type of its port: text for a string, a number for an `int` or a `float`, a path for a File or a Directory, and JSON for anything else. File and Directory values use the JSON form of wir, `{"class": "File", "location": ...}`; a relative `location` or `path` in the inputs file starts from the folder of that file. An input without a value is null when its type is optional; otherwise the run stops before any task starts. An `int` that reaches a `float` port becomes a float.

## Ops

An op starts once every connected input is bound and every `after` dependency holds. An op whose input can no longer be bound, or whose dependency can no longer hold, ends `omitted`. A `call` op runs its graph with the call's inputs; its tasks have paths below the path of the call, like `align/sort`.

A failed or errored op fails the run, unless an `after` dependency of another op waits for that status. A failure inside a call fails the call. After a failure that fails the run, no new op starts unless the run has `-keep-going`; running ops end first.

Reserved op kinds, like `map` or `branch`, verify but do not run: uwe stops with exit code 33.

## Native blocks

A `native` op runs a whole document of another language with the driver that uwe has for that language. This version runs CWL blocks. The op's inputs become the job of the CWL run, by port name, and the CWL output object becomes the op's outputs, by port name; each output must fit its port type, or the op fails. The block keeps its own rules: its tasks, cache keys, resume fingerprints, acceptance, retries, timers and deliveries are those of the same CWL document run alone. Its tasks keep their step names, so `-no-cache-steps echo` works as it does for the CWL document, and their names start with the op path, like `block: echo`.

The `source` location of a block is relative to the wir document, or a URL. `entry` names the process of a packed CWL document, like `main`. A block delivers its output files to a folder of its own, the op path below the output folder of the run (`-output-dir`, else the current folder), so two blocks do not overwrite each other's files. The job file of a block is written below the scratch folder of uwe. No block changes the working directory of the process.

The `cache: never` attribute of a block turns off cached results for its tasks. It only makes reuse stricter: a CWL `WorkReuse` with `enableReuse: false` stays as it is.

## Outputs

A real run that succeeds prints the outputs of the entry graph as JSON, like CWL prints its output object, unless `-no-print` is given. An embedding host gets the same object as `Run::outputs`. A simulated run has no outputs.

## Provenance

The run database records a wir run with the language `wir`, and its run crate names the language wir.

## Not yet

- Native blocks of Nextflow, Snakemake, Argo and WDL.
- Task ops, and the attributes `place`, `retry` and `time` of native ops; `cache: resume`.
- Input descriptions for `uwe inputs` and strict admission of embedded runs, and a record of the document's sources and version in the run crate.

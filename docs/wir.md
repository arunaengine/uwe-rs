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

## Task ops

A `task` op becomes one uwe task, with the op path as its step and name. It runs in its image in the working folder `/work`. Each File or Directory input is staged at `inputs/<port>/<basename>`, an item of an array at `inputs/<port>/<index>/<basename>`, with its secondary files beside it. The command is built as the wir README says, without a shell; a `stdout` or `stderr` capture runs it through `/bin/sh` with the stream written to `<output>.stdout` or `<output>.stderr`.

An attempt is accepted only when its exit code is in `success_codes` (only 0 when empty) and every capture fits its output: a glob matches one file for a File or a Directory, any number for an array, none only for an optional output. An attempt that is not accepted runs again while `retry` allows; then the op fails. A task that could not run, and no machine retry can mend, errors. The attributes `resources` and `time` set what the task asks for and `cache: never` keeps it from the cache; none of them enters its cache key. A task op runs without network, so runs can share its results, unless it sets `network: true`, like a CWL tool with `NetworkAccess`.

## Native blocks

A `native` op runs a whole document of another language with the driver that uwe has for that language. This version runs CWL, Nextflow, Snakemake, Argo and WDL blocks. The op's inputs become the job of the CWL run, by port name, and the CWL output object becomes the op's outputs, by port name; each output must fit its port type, or the op fails. The block keeps its own rules: its tasks, cache keys, resume fingerprints, acceptance, retries, timers and deliveries are those of the same CWL document run alone. Its tasks keep their step names, so `-no-cache-steps echo` works as it does for the CWL document, and their names start with the op path, like `block: echo`.

The `source` location of a block is relative to the wir document, or a URL. `entry` names the process of a packed CWL document, like `main`. A block delivers its output files to a folder of its own, the op path below the output folder of the run (`-output-dir`, else the current folder), so two blocks do not overwrite each other's files. The job file of a block is written below the scratch folder of uwe. No block changes the working directory of the process.

The `cache: never` attribute of a block turns off cached results for its tasks. It only makes reuse stricter: a CWL `WorkReuse` with `enableReuse: false` stays as it is.

## Nextflow blocks

A Nextflow block runs a Nextflow script, or the main script of a project folder, as `uwe run` runs it alone. `entry` is the workflow of `-entry`; without it the unnamed workflow runs. `profiles` are the profiles of `-profile`, and `configs` name sources whose files are read like `-c` files, in order. Its tasks, cache keys and resume fingerprints are those of the same run alone, launched in the same folder.

Params: each op input becomes the param of its name, written to a params file like `-params-file`. A File or a Directory becomes its path, arrays and records keep their shape with paths for their files, and a null input stays out, so the default of the pipeline holds. The pipeline checks the params as it does alone.

Launch folder: a block runs as if it was launched in its own folder, the op path below the output folder of the run (`-output-dir`, else the current folder). Relative paths start there: `file()` and `publishDir` paths, `params.outdir`, the `nextflow.config` of the launch folder and `launchDir`. So `publishDir "${params.outdir}/fastqc"` with `outdir` set to `results` publishes to `<output folder>/<op path>/results/fastqc`. Its own files, like operator files and reports, go to a scratch folder of the block. Two blocks of one run never share a folder, and no block changes the working directory of the process.

Results: when the block has finished, each op output takes its value by its name:

1. An emit of the entry workflow with the output's name. A value channel gives its value, or null when it closed empty, and a queue channel the array of all its values, in the order they came, which can follow the order in which tasks finished. A path becomes a File, or a Directory where the output type wants one there. Numbers, strings, booleans, lists and maps keep their JSON form; a value that JSON cannot hold, like a closure, fails the op.
2. Else the files that the block published below the folder that the op input of the same name names, like `outdir`, from the launch folder: a Directory output gets that folder, an array of Files every file published below it, sorted by path, and a File output the one file published there. An optional output is null when nothing was published; otherwise publishing nothing, or several files for a File, fails the op.
3. Else the op fails with a message that the output is neither an emit nor the folder of an input.

The value must then fit the output type, or the op fails. Nextflow emits have no types, so an output that takes any value has the type `dynamic`; wir allows `dynamic` only on the outputs of Nextflow blocks. In a simulated run the files of emits are made up, and the published files are the targets of the publish rules, which nothing writes.

## Snakemake blocks

A Snakemake block runs a Snakefile, or the Snakefile of a project folder, as `uwe run` runs it alone. `entry` names one target, a rule or a file, like a target on the command line; without it the first rule is the target. `configs` name sources whose files are read like `--configfile` files, in order, and `profiles` name profiles: an absolute path, a path below the block's own folder (the op path below the output folder of the run), else a workflow profile that Snakemake finds as it finds `--workflow-profile <name>`, in the working folder, its `profiles` folder or the `profiles` folder beside the Snakefile. Its tasks, cache keys and resume fingerprints are those of the same run alone, started in the same folder.

Config: the op inputs become a config file, read after the `configs` like a last `--configfile`, so they win. Each input becomes the key of its name: a File or a Directory becomes its path, arrays and records keep their shape with paths for their files, and a null input stays out, so the default of the Snakefile holds.

Working folder: a block of a Snakefile works in its own folder, the op path below the output folder of the run (`-output-dir`, else the current folder), as if Snakemake was started there. Relative paths of the Snakefile start there, and its files, its `.snakemake` folder and its lock are there. A block of a project folder works in that folder, as `uwe run <folder>` does, so two blocks of one project folder share it. No block changes the working directory of the process.

Results: when the block has finished, each op output takes the files of the target that `uwe outputs` names alike. For a target rule these are its named outputs, or its named inputs when it has no outputs, like those of `all`; every other file of the targets, and a target file, goes into the output `targets`. A name that one item takes once is a File, or a Directory where the Snakefile marks it `directory()`; a name of several files, like `expand(...)`, is an array of them, in the order of the rule. So `rule all: input: reports=expand("results/{s}.txt", s=SAMPLES)` gives the output `reports`, an array of Files. The files are at their paths in the working folder. An output that the targets do not name is null, and fails the op unless its type is optional; the value must fit the output type. In a simulated run the files are named but not made.

Checkpoints work inside a block: the block plans again when a checkpoint has run, and ends only when the jobs of that plan have run.

## Argo blocks

An Argo block runs the Workflow of an Argo file, as `uwe run` runs it alone. `entry` names the template to start with instead of `spec.entrypoint`, and `configs` name sources whose files hold the WorkflowTemplates and ClusterWorkflowTemplates that `templateRef` finds, like files given with `-c`. Argo has no profiles: a block with `profiles` stops the run with exit code 33. Its tasks, cache keys and resume fingerprints are those of the same run alone, with `-output-dir` set to the folder of the block.

Parameters: the op inputs become a parameter file, like `argo submit --parameter-file`, so each input sets the workflow parameter of its name. A string stays as it is; a File or a Directory becomes its path, and other values their JSON text. A null input stays out, so the default of the workflow holds.

Output folder: the output artifacts of the steps are delivered to `<output folder>/<op path>/<step>/<artifact>`, as a run alone delivers them to `-output-dir`. A block takes no requests of `uwe resume`, so a real run of a block cannot suspend a step without a duration.

Results: when the block has finished, each op output takes the output of the same name, as `uwe outputs` names them: the output parameters and artifacts of the entrypoint, then the global outputs by their `globalName`. An output parameter is a string. An output artifact is a File, or a Directory when it is a folder, named by the basename of its path in the container, like `loud.txt` for `/tmp/loud.txt`; an artifact that the workflow holds as raw text gives null. An output that the workflow does not give is null, and fails the op unless its type is optional. In a simulated run the parameters read from made-up files are empty.

## WDL blocks

A WDL block runs the workflow of a WDL document, or the task of a document without one, as `uwe run` runs it alone. `entry` names the workflow or task to run. WDL has neither profiles nor config files: a block with `profiles` or `configs` stops the run with exit code 33. Its tasks, cache keys and resume fingerprints are those of the same run alone, with `-output-dir` set to the folder of the block.

Inputs: the op inputs become the input object of the run, like an inputs file: the input `x` sets the input `x` of the workflow, as `<workflow>.x` does, and an input whose name has a dot, like `align.threads`, sets the input `threads` of the call `align`. A File or a Directory becomes its path, arrays, maps and structs keep their shape with paths for their files, and a null input stays out, so the default of the workflow holds. A `Pair` input takes a record of `left` and `right`.

Output folder: the files of the outputs are delivered to `<output folder>/<op path>/<output>/`, numbered for arrays, as a run alone delivers them to `-output-dir`.

Results: when the block has finished, each op output takes the workflow output of the same name, without the name of the workflow: `all` for `scatter_names.all`. A File or a Directory is at its delivered path, a pair is an array of two, so a wir tuple of two takes it, and the other values keep the JSON form of WDL. An output that the workflow does not give is null, and fails the op unless its type is optional; the value must fit the output type.

## Outputs

A real run that succeeds delivers the files and folders of the entry graph's outputs that are not in the output folder yet, like those of task ops, to the output folder by their basenames. When two different files or folders would get the same place there, the run fails before it delivers anything, and names that place. It prints the outputs as JSON, like CWL prints its output object, unless `-no-print` is given. An embedding host gets the same object as `Run::outputs`. A simulated run has no outputs.

## Provenance

The run database records a wir run with the language `wir`, and its run crate names the language wir.

## Not yet

- `uwe resume` of the suspended steps of an Argo block.
- Several targets in one Snakemake block; a rule that asks for them can stand in.
- Workflow outputs of the `output { }` block of Nextflow as op outputs by their names; their files count as published files below their folder.
- A remote output folder for Nextflow blocks, and the launch folder in `${launchDir}` of include paths and in relative `conda` files, which still start from the working directory.
- The attributes `place` of every op and `retry` and `time` of native ops; `cache: resume`.
- Input descriptions for `uwe inputs` and strict admission of embedded runs, and a record of the document's sources and version in the run crate.

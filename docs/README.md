<!-- Index of the iunctus documentation. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# iunctus docs

iunctus is a native Rust application. It reads Nextflow, Snakemake, CWL and Argo workflows, turns each into language-neutral container tasks and one execution graph, and sends the tasks to executors, starting with GA4GH TES.

This folder holds the research and the design behind the code. The research was checked on 2026-09-27.

## The command

```sh
iunctus run <workflow file or directory> [-graph graph.json] [-executor simulator|local|tes|slurm|kubernetes] [--<param> <value>]
iunctus graph <workflow file or directory> [-format json|dot|summary|svg] [-view tasks|steps] [-direction down|right] [-fold <n>]
iunctus config [<Nextflow project>] [-profile <names>]
iunctus runs
iunctus page [<run>]
iunctus export <run> [-output <folder>]
iunctus inputs <workflow file or directory> [-format json|text] [-check]
iunctus outputs <workflow file or directory> [-format json|text]
iunctus clean [-keep-days <n>] [-keep-run <run>] [-keep-slower-than <time>] [-remove]
```

`iunctus run` finds the language of its target, or takes it from `-lang`. The default executor is the simulator, which writes the TES task document of each task and makes up its outputs, so any workflow runs through without containers. A simulated run gives a simulated graph, not a prediction of a real run (decision 21). All four languages run: Nextflow, Snakemake, CWL and Argo. The local executor runs tasks in Docker or Podman instead, the TES executor on a GA4GH TES server, the SLURM executor as batch jobs, and the Kubernetes executor as Jobs. See [the engine](design/engine.md) and [the executors](design/executors.md). `iunctus graph` prints the graph of a run with one node per task or one node per step, as a summary, JSON, DOT or an SVG drawn by iunctus itself, and marks the points where a run with real data can add or remove tasks; see [the graphs of a run](design/graph.md).

Storage needs no setup for public files. A config file, `~/.config/iunctus/config.toml`, names the storage providers of private buckets and servers, each with its own endpoint and credentials, and where the cache store lives; see [the config file and storage providers](design/storage.md).

`iunctus inputs` describes the inputs of any workflow, in every language, as one JSON Schema: values, files and directories with their formats, and sample sheets as tables with typed columns; `-format text` gives the inputs as help. Every run checks its values against the schema before any task starts, with the same messages in every language, and warns where the schema and the inputs that the code reads differ (decision 24). `iunctus outputs` describes the outputs of a workflow the same way, from what it declares, so that one workflow can feed another; see [the inputs of a workflow](design/inputs.md).

Every run records itself in the run database. `iunctus runs` lists the runs, `-resume` continues one without running its finished tasks again, `iunctus run -page` and `iunctus page` show runs live on a web page, and `iunctus export` writes the RO-Crate of a run with its provenance. See [runs](design/runs.md).

Every result goes to a content-addressed store, and any run, pipeline or language that asks for the same task takes it from there. Two runs that ask for the same task at the same moment run it once. A simulated run previews which tasks a real run would take from the cache, and `iunctus clean` removes what no rule keeps, never what a run still refers to. See [the cache](design/cache.md).

## Start here

- [Decisions](decisions.md): what has been decided so far. It replaces the report's plan where they differ.
- [Roadmap](roadmap.md): what is still missing for real runs and parity with the native engines, and how iunctus can give deeper insight, higher reproducibility, better efficiency and storage across providers.
- [Feasibility report](feasibility-report.md): verdict per language, proposed architecture, phased plan, open decisions and next steps.

## Design

- [Engine](design/engine.md): how `iunctus run` finds the language, the driver interface every language implements, how tasks run, the execution graph, the executors, and the next steps for the execution layer.
- [Executors](design/executors.md): how the local, TES, SLURM and Kubernetes executors run tasks, how to set each one up, and how the end of a job becomes the outcome of a task.
- [Runs](design/runs.md): the run database that every run fills, resume, the web page of runs and the Workflow Run RO-Crate of a run.
- [The cache](design/cache.md): when two tasks are the same, the store and its entries, one execution of a task that runs elsewhere with leases that end with their process, clean-up by rule, the preview of a run, and the longest chain first.
- [The config file and storage providers](design/storage.md): the config file of the site, the user and a run, secrets from environment variables, and the providers (S3 and compatible services, Google Cloud Storage, Azure Blob Storage, web servers, FTP and folders) that serve the store, remote inputs and results, with an example.
- [The inputs of a workflow](design/inputs.md): the inputs of every workflow as one JSON Schema, how each language describes them, the check of a run before its first task, and the inputs that the code reads.
- [The graphs of a run](design/graph.md): the graph of tasks and the graph of steps, what a simulated graph says about itself, the markers where real data can add or remove tasks, the formats, and the SVG layout.
- [Nextflow runtime](design/nextflow/runtime.md): the chosen design of the Nextflow evaluator, the dataflow runtime and the shared task layer, with milestones.
- [Argo runtime](design/argo/runtime.md): how `argo-runtime` runs Argo Workflows documents as tasks: the template library, substitution, the expression languages, control flow, leaf templates, simulated runs and the results on its test workflows.
- [The context of embedded code](design/snakemake/embedded-code.md): the `snakemake` object that Snakemake scripts, wrappers and notebooks get, as Snakemake 9.27 writes it in each language, the stand-in of the snakemake package for Python, `run:` blocks in the task's container with the Snakefile's globals, and Nextflow `exec:` sections in the interpreter.
- [Python packages in a container](design/snakemake/python-packages.md): how Snakemake workflows use Python packages without a stand-in, which run in one container per run (decision 19): the interface, the protocol, handles, the image and the errors.

## Research notes

Each note answers a set of questions and cites its sources.

- [CWL](research/cwl.md): spec versions, Schema Salad, JavaScript expressions, conformance tests, and the existing Rust engine `commonwl`.
- [Nextflow](research/nextflow.md): strict syntax and its grammar, runtime model, Groovy use, config, plugins and nf-core.
- [Snakemake](research/snakemake.md): parser, planning model, Python use in real workflows, the TES plugin and the test folders.
- [Rust ecosystem](research/rust-ecosystem.md): crates for parsing, embedded interpreters, YAML, storage, containers and conda, plus existing Rust workflow engines.
- [TES and executors](research/tes-and-executors.md): the TES spec, servers, Rust clients, data staging, Aruna, and a common executor trait.
- [Prior work](research/prior-work.md): converters, multi-language platforms, papers, standards and lessons learned.
- [IR design models](research/ir-design-models.md): how Cromwell WOM, Flyte, Bazel, Temporal and others design their intermediate representation.
- [nf-core script inventory](research/nf-core-inventory.md): which Nextflow language and runtime features the nf-core scripts use, by number of pipelines.
- [nf-core config inventory](research/nf-core-config.md): which config features the nf-core pipelines use.
- [Nextflow channels](research/nextflow-channels.md): implementer spec of channels, operators and how processes consume them.
- [Nextflow Groovy subset](research/nextflow-groovy.md): implementer spec of the Groovy runtime semantics that strict-syntax scripts rely on.
- [Nextflow config](research/nextflow-config.md): implementer spec of config files, profiles, params and process selectors.
- [Snakemake test workflows](research/snakemake-test-workflows.md): the Snakemake test workflows, an evaluation of Snakefile parsers, and a feature inventory of real workflows.
- [CWL test workflows](research/cwl-test-workflows.md): the CWL test workflows, an evaluation of Rust CWL loaders, and a feature inventory of real workflows.
- [Argo Workflows](research/argo-workflows.md): the Argo workflow model, its expression languages, running it without Kubernetes, test workflows and a feature inventory.

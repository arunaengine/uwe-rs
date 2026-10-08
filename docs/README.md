<!-- Index of the iunctus documentation. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# iunctus docs

iunctus is a native Rust application. It reads Nextflow, Snakemake, CWL and Argo workflows, turns each into language-neutral container tasks and one execution graph, and sends the tasks to executors, starting with GA4GH TES.

This folder holds the research and the design behind the code. The research was checked on 2026-09-27.

## First run

Start with the [user guide](guide/first-run.md). It covers
[installation](guide/install.md), [Nextflow](guide/nextflow.md),
[Snakemake](guide/snakemake.md), [CWL and Argo](guide/cwl-argo.md),
[executors](guide/executors.md), [configuration](guide/configuration.md),
[storage](guide/storage.md), [troubleshooting](guide/troubleshooting.md) and
[embedding](guide/embedding.md). Release changes are in the
[changelog](../CHANGELOG.md).

From the checkout, with the binary installed and Docker or Podman ready:

```sh
iunctus --version
iunctus check-machine
iunctus run demo/snakemake/hello.smk
iunctus runs
```

The example writes greetings under `demo/snakemake/hello/`.
Use a separate copy with `-executor simulator` for made-up outputs.
The guide explains engine setup and the differences from each workflow engine.

## The command

```sh
iunctus run <workflow file or directory> [-graph graph.json] [-executor simulator|local|tes|slurm|kubernetes] [--<param> <value>]
iunctus run <owner/repo or repository URL> [-r <tag|branch|commit>] [-latest] [-profile <names>] [--<param> <value>]
iunctus graph <workflow file or directory> [-format json|dot|summary|svg] [-view tasks|steps] [-direction down|right] [-fold <n>]
iunctus check <workflow file or directory> [-default-image <image>]
iunctus check-machine [-executor <executor>]
iunctus config [<Nextflow project>] [-profile <names>]
iunctus runs
iunctus page [<run>]
iunctus export <run> [-output <folder>]
iunctus inputs <workflow file or directory> [-format json|text] [-check]
iunctus outputs <workflow file or directory> [-format json|text]
iunctus clean [-keep-days <n>] [-keep-run <run>] [-keep-slower-than <time>] [-remove]
iunctus changes <run>
iunctus repeat <run> [-executor <executor>]
iunctus check-order <workflow file or directory>
iunctus help
```

Repository names use GitHub by default. Full HTTP or HTTPS repository URLs support GitHub, GitLab, Bitbucket Cloud and Gitea-compatible hosts. An existing local path takes precedence. No Git program is needed: iunctus resolves the revision through the host API and downloads its archive with the shared Rust HTTP client.

```sh
iunctus run nf-core/rnaseq -r 3.14.0 -profile test,docker --outdir out
iunctus run https://github.com/nf-core/demo -r 1.2.0 -profile test --outdir out
iunctus run snakemake-workflows/rna-seq-star-deseq2 -r v2.1.2
iunctus run nf-core/demo -latest -profile test --outdir out
```

Without `-r`, the host's default branch is used, with a literal `manifest.defaultBranch` in `nextflow.config` taking precedence. Snakemake repositories also find `workflow/Snakefile`. Checkouts live under `<cache>/pipelines/<host>/<owner>/<repo>/<commit>`. Symbolic requests check the server and retain their cached revision until `-latest`; only a cached full commit id can run offline. Run records and RO-Crate exports retain the resolved commit, and `repeat` uses the recorded sources without resolving the branch again. CWL and Argo documents can also be run by their HTTP or HTTPS file URL.

Private repositories use `NXF_GITHUB_ACCESS_TOKEN` or `GITHUB_TOKEN` for GitHub, `GITLAB_TOKEN`, `BITBUCKET_TOKEN`, or `GITEA_TOKEN` for the other hosts. Tokens are sent in sensitive authorization headers and are never recorded. See [repository limits](support.md#repository-sources).

`iunctus run` finds the language of its target, or takes it from `-lang`. The default executor is local, which runs tasks in Docker or Podman containers. Select `-executor simulator` to write the TES task document of each task and make up its outputs without task containers. A simulated run gives a simulated graph, not a prediction of a real run (decision 21). All four languages run: Nextflow, Snakemake, CWL and Argo. The local executor runs tasks in Docker or Podman, the TES executor on a GA4GH TES server, the SLURM executor as batch jobs, and the Kubernetes executor as Jobs. See [the engine](design/engine.md) and [the executors](design/executors.md). `iunctus graph` prints the graph of a run with one node per task or one node per step, as a summary, JSON, DOT or an SVG drawn by iunctus itself, and marks the points where a run with real data can add or remove tasks; see [the graphs of a run](design/graph.md).

Storage needs no setup for public files. A config file, `~/.config/iunctus/config.toml`, names the storage providers of private buckets and servers, each with its own endpoint and credentials, and where the cache store lives, also as a folder of this machine in front of a store that several machines share; see [the config file and storage providers](design/storage.md).

Snakemake runs accept target controls on `run`, `graph`, `check` and `check-order`. Give a rule name or a concrete output file, separate multiple names with commas, or repeat the option:

- `-until <rules,files>` runs only the selected jobs and their dependencies, including the selected jobs themselves. It excludes downstream jobs and independent siblings.
- `-omit-from <rules,files>` removes the selected jobs and everything downstream, keeping their dependencies and independent branches.
- `-forcerun <rules,files>` reruns the selected jobs and everything downstream, even when their outputs are up to date. Dependencies run when needed.
- `-forceall` reruns every job needed for the selected targets, within any `-until` and `-omit-from` limits.

These controls follow Snakemake 9 target selection: rules with wildcards select matching jobs already in the target graph; rules without wildcards and files given to `-until` or `-forcerun` also become targets. Files given to `-omit-from` do not add targets. Other workflow languages reject these options with exit code 33. Snakemake's double-dash flags and `-R`/`-F` aliases are rejected with guidance to the corresponding iunctus options.

```sh
iunctus run Snakefile -until align
iunctus run Snakefile -omit-from report
iunctus run Snakefile -resume -forcerun align,results/sample.bam
iunctus graph Snakefile -forceall -format json
```

`iunctus inputs` describes the inputs of any workflow, in every language, as one JSON Schema: values, files and directories with their formats, and sample sheets as tables with typed columns; `-format text` gives the inputs as help. Every run checks its values against the schema before any task starts, with the same messages in every language, and warns where the schema and the inputs that the code reads differ (decision 24). `iunctus outputs` describes the outputs of a workflow the same way, from what it declares, so that one workflow can feed another; see [the inputs of a workflow](design/inputs.md).

Every run records itself in the run database. `iunctus runs` lists the runs, `-resume` continues one without running its finished tasks again, `iunctus run -page` and `iunctus page` show runs live on a web page, and `iunctus export` writes the RO-Crate of a run with its provenance. Forced Snakemake jobs bypass both task-cache reuse and finished-task reuse from `-resume`; independent jobs retain normal reuse. Resume is an option of `run` and `graph`, rather than a separate `iunctus resume` command. See [runs](design/runs.md).

Every error has the same form, `iunctus: <where>: <what went wrong>; <what to do>`, in every language and executor, and the exit code tells its kind: 1 for the workflow, 2 for the command line, 3 for the machine or its config, 33 for a feature that iunctus does not support. `iunctus check` reports at once everything that would stop a real run of a workflow, without running a task, and `iunctus check-machine` what is ready on this machine and what is missing. See [errors, exit codes and checks](design/checks.md).

Every run records what pins it: the files of its workflow and command line, the remote files it read, the images it ran in and the inputs of its tasks, each by digest, and a real run keeps them in its store. `iunctus changes` tells what changed since a run, and `iunctus repeat` runs it again with the files, remote files and images it had. `-check-reproducibility` runs a share of the cached tasks again and stops sharing the results of tools that give other bytes, `iunctus check-order` names the steps whose outputs depend on the order in which tasks finish, and `-no-network` runs tasks without network. See [reproducibility](design/reproducibility.md).

Every result goes to a content-addressed store, and any run, pipeline or language that asks for the same task takes it from there. Two runs that ask for the same task at the same moment run it once. A simulated run previews which tasks a real run would take from the cache, `-no-cache` runs every task of one run, and `iunctus clean` removes what no rule keeps, never what a run still refers to. Local inputs that did not change since a run read them are not read again to find their digests. See [the cache](design/cache.md).

Real runs learn from earlier ones: a task asks for the CPUs and memory that the tasks of its step used before, with a margin, short tasks of a step run one after another in one container or one TES task, and with several TES servers a task goes to the site near its inputs. See [efficiency](design/efficiency.md).

## Start here

- [Decisions](decisions.md): what has been decided so far. It replaces the report's plan where they differ.
- [Roadmap](roadmap.md): what is still missing for real runs and parity with the native engines, and how iunctus can give deeper insight, higher reproducibility, better efficiency and storage across providers.
- [Universal workflow audit (2026-10-07)](universal-workflow-audit-2026-10-07.md): source-backed findings, JSON Schema and form gaps, native language coverage, Aruna integration contracts and a prioritized implementation backlog.
- [Universal workflow remediation](universal-workflow-progress.md): current implementation status, verified commits and remaining qualification gates.
- [Feasibility report](feasibility-report.md): verdict per language, proposed architecture, phased plan, open decisions and next steps.

## Design

- [Engine](design/engine.md): how `iunctus run` finds the language, the driver interface every language implements, how tasks run, the execution graph, the executors, and the next steps for the execution layer.
- [Errors, exit codes and checks](design/checks.md): the one form of every error, what each exit code means, and the checks of a workflow and of the machine before a run.
- [Executors](design/executors.md): how the local, TES, SLURM and Kubernetes executors run tasks, how to set each one up, and how the end of a job becomes the outcome of a task.
- [Runs](design/runs.md): the run database that every run fills, resume, the web page of runs and the Workflow Run RO-Crate of a run.
- [Reproducibility](design/reproducibility.md): what each run records to pin it, what changed since a run, repeating a run exactly, the reproducibility check of cached tasks, the order check, and tasks without network.
- [Conda environments](design/conda.md): environment files, match specs, the order of conda versions, and locks made without network from local repodata with `iunctus lock-environment`; how images will be built.
- [The cache](design/cache.md): when two tasks are the same, the store and its entries, one execution of a task that runs elsewhere with leases that end with their process, clean-up by rule, the preview of a run, and the longest chain first.
- [Efficiency](design/efficiency.md): right-sized resources from what tasks used in earlier runs, short tasks together in one container, and tasks near their data with several TES servers.
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

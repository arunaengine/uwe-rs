<!-- Starting CWL and Argo workflows with local containers and explicit inputs. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# CWL and Argo

Both languages use the same executors, run database and task cache as Nextflow
and Snakemake. Start with [a local engine](install.md).

## CWL

Give a CWL document and, optionally, its JSON or YAML job file.
From the iunctus checkout:

```sh
iunctus inputs demo/cwl/hello_world.cwl -format text
iunctus run demo/cwl/hello_world.cwl demo/cwl/hello_world-job.json -default-image debian:stable-slim
```

The input help lists `--message <text> (default: Hello World)`.
The job file supplies a Japanese greeting. This tool declares no outputs, so
the run prints `{}`. Its task stdout is available through the run page.
The example names no Docker image, so the command supplies one.
Real tasks always need an image, through `DockerRequirement` or `-default-image`.

`-inputs job.yml` is an alternative to the positional job file.
`--message 'Hello World'` sets an input directly.
Packed documents can select a process by its name after the workflow argument.
`-output-dir results` selects the output folder.

Supported workflows include scatter, conditional steps and supported JavaScript
expressions. Required image build/load/import behavior, ontology matching beyond
exact formats, and in-place Directory updates are unavailable.
Required unsupported extensions fail explicitly. Hints can be ignored.
See [CWL support](../support.md#workflow-languages).

## Argo

iunctus interprets an Argo Workflow document. It does not submit it to the Argo
controller. Local execution does not need Kubernetes.
For a simulation from the checkout:

```sh
iunctus run demo/argo/hello-world.yaml -executor simulator
```

The run prints one simulated task with a generated hello-world name.
Omit `-executor simulator` to execute it locally with the document's BusyBox image.
Use `--name value` for workflow parameters. Use `-lang argo` if the file cannot
be identified automatically.

Supported templates include steps, DAGs, containers, scripts and container sets.
Supported loops, conditions, parameters, artifacts and workflow hooks are
interpreted by iunctus. Backend configuration supplies remote storage access.

Some cluster controls are accepted without effects, including readiness probes,
garbage collection and memoization. The native task cache handles task reuse.
Controls that can change results, such as suspension, synchronization, pod patches
and unsupported deadlines, are refused. Several artifact transfer forms are also
refused. Read [Argo support](../support.md#workflow-languages) before using a
document written for a live Argo controller.

Next: [Executors](executors.md). Return to [First run](first-run.md).

<!-- Running a small workflow, finding its results and returning to a saved run. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# First run

Complete [installation](install.md) first. Start in an empty working folder.
Create `main.nf`:

```sh
cat > main.nf <<'EOF'
params.outdir = 'out'
process HELLO {
    container 'debian:stable-slim'
    publishDir params.outdir, mode: 'copy'
    output:
    path 'hello.txt'
    script:
    """
    echo 'Hello from iunctus' > hello.txt
    """
}
workflow {
    HELLO()
}
EOF
iunctus check main.nf
iunctus run main.nf
cat out/hello.txt
```

The check reports one task in one step. The run uses local Docker or Podman.
It pulls the small Debian image if the engine does not have it.
The run prints its ID and a completion summary. `cat` prints:

```text
Hello from iunctus
```

Published results are in `out/`. Task files also stay in the content store.
A repeated task may print `[cached] HELLO` instead of `[done] HELLO`.
The first run needs network access if the image is absent.

## Plan without running tools

```sh
iunctus run main.nf -executor simulator
```

This prints `iunctus: simulated run selected; outputs are made-up files, no task containers run`.
Task lines show `[simulated]` or a cache preview. A simulation does not verify tool
results. It may publish made-up files, so use a separate output folder when
planning a workflow that has real results. Discovery that needs package helpers
or JavaScript can still require a local container engine.

## Run a repository workflow

These examples need network access and pipeline images or configured environments:

```sh
iunctus run nf-core/demo -r 1.2.0 -profile test --outdir out
iunctus run snakemake-workflows/rna-seq-star-deseq2 -r v2.1.2
```

The first command fetches the pinned nf-core demo and selects its test inputs.
The second fetches a Snakemake repository and finds `workflow/Snakefile`.
Follow that workflow's input instructions before a real run.
See [Nextflow](nextflow.md), [Snakemake](snakemake.md) and
[Conda setup](configuration.md#conda) for the required settings.
These network examples were checked against CLI help, not executed for this guide.
Existing [demo evidence](../../iunctus/tests/fixtures/real_runs.txt) records a local
run with all eight tasks and 76 published filenames matching the reference,
with documented report normalization and exclusions. This does not qualify every
nf-core pipeline.

## Resume, inspect and repeat

```sh
iunctus run main.nf -resume
iunctus runs
```

`-resume` uses the latest compatible run lineage. `-resume RUN_ID` chooses one.
Finished tasks are reused only when their recorded inputs, outputs and execution
identity still qualify. Ordinary task caching can also reuse work without resume.
`runs` lists IDs, states, executors, task counts and workflow paths.

Replace `RUN_ID` below with an ID from that list:

```sh
iunctus page RUN_ID
iunctus repeat RUN_ID
iunctus report RUN_ID -output report.html
```

`page` prints a local URL and serves the run until Ctrl-C.
`repeat` uses recorded workflow sources, inputs and image pins. It checks changes
and compares the new outputs. Keep the original run's store available.
A simulated run keeps no source copies and cannot restore changed workflow files.
`report` writes a standalone HTML file that you can open without a server.

For a page during execution, use `iunctus run main.nf -page`.
The page stays open when the run ends. See [page access](executors.md#run-page).

Next: [Nextflow](nextflow.md), [Snakemake](snakemake.md) or [CWL and Argo](cwl-argo.md).

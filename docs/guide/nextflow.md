<!-- Selecting Nextflow profiles, parameters, entries and repository revisions. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Nextflow

Give `run` a `.nf` file, a project folder containing `main.nf`, or a repository:

```sh
iunctus run nf-core/demo -r 1.2.0 -profile test --outdir out
```

Local containers are the default. A profile does not need to enable Docker.
Use `-executor` to select an iunctus backend.

## Profiles and parameters

`-profile test,docker` selects named profiles from the workflow's configuration.
`-c site.config` adds a Nextflow config file. This is different from
`-iunctus-config machine.toml`, which configures iunctus itself.
`iunctus config PROJECT -profile test` prints the evaluated Nextflow configuration.

Pass workflow values as `--name value`. For example, `--outdir out` sets
`params.outdir`; it is not a universal output-directory option.
`-params-file params.json` reads workflow values from a JSON or YAML file.
Command-line parameter values override matching file values.
Use `-entry NAME` to run a named DSL2 workflow entry instead of the default entry.

## Choose a revision

`-r TAG`, `-r BRANCH` and `-r COMMIT` select repository revisions.
Without `-r`, the host's default branch is used unless a literal
`manifest.defaultBranch` supplies one. A symbolic request checks the host but
retains its cached commit until `-latest` asks to refresh it.
Only a cached full commit ID can be resolved offline.
Run records keep the resolved commit. `repeat` uses the recorded sources.

Short names use GitHub. HTTPS repository URLs also support GitLab, Bitbucket
Cloud and compatible Gitea hosts. Existing local paths take precedence.
Private repositories use `NXF_GITHUB_ACCESS_TOKEN` or `GITHUB_TOKEN`,
`GITLAB_TOKEN`, `BITBUCKET_TOKEN` or `GITEA_TOKEN`, depending on the host.

## Validate inputs

For a local project, inspect the input schema before a run:

```sh
iunctus inputs PROJECT -format text
iunctus inputs PROJECT -params-file params.json -check
```

Replace `PROJECT` with the project path and prepare the parameter file first.
Runs validate described inputs before tasks start.
The implemented `nf-schema` functions include parameter validation, help,
summaries and sample-sheet conversion. Validation can check a sample sheet
against its declared schema. Unknown plugins or functions fail explicitly.
Schema validation does not prove access to remote file contents.

`iunctus inputs PROJECT -format form` returns a versioned form description from
JSON Schema. This is a contract for an application to render, not a hosted form UI.
See [embedding](embedding.md) for typed submissions.

## Supported subset

iunctus supports DSL2 processes and workflows, channels, implemented operators,
container scripts, declared outputs, retries and supported publishing modes.
It uses a native Groovy subset. It does not start a JVM or install arbitrary
Nextflow plugins. Read the full [support matrix](../support.md#workflow-languages).

`beforeScript` and `afterScript` run inside task containers. `storeDir` reuses
existing outputs and copies new outputs, but cannot be combined with `afterScript`.
`maxForks` limits process tasks. Local `containerOptions` support environment,
CPU, GPU, shared memory and swap flags. Named secrets can be injected into local
tasks. Scheduling hints do not choose the iunctus executor or placement.
Active unsupported settings, including `module`, `arch` and non-copy
`stageOutMode`, fail with exit code 33.
`publishDir` publishes `move` outputs as copies and refuses `failOnError: false`
and active `contentType` or `tags` options. Hash and deep sorting support common
values; unsupported values are refused when two or more items need ordering.
Natural, index, none and supported closure keys are also available.
Inactive Conda or Spack defaults do not enable environment preparation.

Next: [Executors](executors.md). Return to [First run](first-run.md).

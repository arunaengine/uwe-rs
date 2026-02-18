<!-- Index of the iunctus documentation. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# iunctus docs

iunctus is a planned native Rust application. It will read Nextflow, Snakemake, CWL and Argo workflows, turn them into a shared intermediate representation, and send the resulting tasks to executors, starting with GA4GH TES.

This folder holds the research behind the code. All facts were checked on 2026-09-27.

## Start here

- [Decisions](decisions.md): what has been decided so far. It replaces the report's plan where they differ.
- [Feasibility report](feasibility-report.md): verdict per language, proposed architecture, phased plan, open decisions and next steps.

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
- [Snakemake corpus](research/snakemake-corpus.md): the Snakemake corpus, an evaluation of Snakefile parsers, and a feature inventory of real workflows.
- [CWL corpus](research/cwl-corpus.md): the CWL corpus, an evaluation of Rust CWL loaders, and a feature inventory of real workflows.
- [Argo Workflows](research/argo-workflows.md): the Argo workflow model, its expression languages, running it without Kubernetes, a corpus and a feature inventory.

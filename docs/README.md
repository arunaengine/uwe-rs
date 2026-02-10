# iunctus docs

iunctus is a planned native Rust application. It will read Nextflow, Snakemake and CWL workflows, turn them into a shared intermediate representation, and send the resulting tasks to executors, starting with GA4GH TES.

No code exists yet. This folder holds the research done before coding. All facts were checked on 2026-09-27.

## Start here

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

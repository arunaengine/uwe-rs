<!-- Decisions made so far about the scope and design of iunctus. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Decisions

Recorded on 2026-09-27. Where this file differs from the plan in the [feasibility report](feasibility-report.md), this file wins.

1. **One native binary.** iunctus is a single Rust binary. It does not bundle or call the nextflow, snakemake, cwltool or argo binaries, and it needs no JVM and no Python install. The tools inside tasks still run in containers, on a local container runtime or through TES.
2. **Languages in scope:** Nextflow, Snakemake, CWL, Argo Workflows and plain script workflows. The goal is to run a large, commonly used subset of each language, not every feature.
3. **Nextflow first.** It is the largest problem.
4. **Real workflows decide the scope.** For Nextflow, the first target is nf-core: its pipelines, modules and subworkflows. Features are added in the order that unlocks the most pipelines. After that come the Nextflow, CWL and Snakemake workflows on WorkflowHub. Galaxy workflows (49% of WorkflowHub on 2026-09-27) are out of scope for now.
5. **Nextflow strict syntax only.** This is the syntax of the v2 parser, the default since Nextflow 26.04. There is no full Groovy support. Unsupported features fail at load time with a clear error.
6. **Core rules first, library later.** The core must be right from the start: the evaluation rules of the Groovy subset, channel semantics, config semantics and the typed syntax. The library grows with the corpus: GDK methods, operators, stand-ins for Java classes, directives and plugin functions.
7. **No Nextflow work directory.** Tasks run in scratch directories. Inputs and outputs live in content-addressed storage, with Aruna first. A shared cache is keyed on the concrete task and has rules for deterministic and non-deterministic steps.

## Open questions

- What "plain script workflows" should cover.
- The license of iunctus.

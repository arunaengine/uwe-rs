<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Universal workflow remediation progress

Baseline: `945ad6c1a8933d26a1ffea13b8d81879fdb9b450`. Branch: `fix/universal-execution`.

The user authorized implementation of all findings in the [2026-10-07 audit](universal-workflow-audit-2026-10-07.md). The core remains portable across execution backends. Aruna is a required integration using those contracts, rather than the default authority or a dependency of every core type. Workflow tools, package helpers and Conda installation remain container-only.

Four focused implementation workers share one checkout with exclusive files. The parent reviews exact diffs, runs bounded focused checks, and creates single-author signed commits of at most 500 changed lines. Existing support boundaries and user changes are preserved. No push, deployment or release is authorized by this ledger.

Statuses distinguish an implementation from qualification. A safe refusal can close silent misexecution while the actual feature remains pending. Source inspection and fake services do not establish live backend, rootless, platform, full corpus or production acceptance.

| ID | Finding or requirement | State | Commit and evidence |
| --- | --- | --- | --- |
| D01 | Resume trusts output existence rather than output content | Queued | Pending |
| D02 | Lineage reuse has weaker input and image identity than the shared cache | Queued | Pending |
| D03 | Telemetry bypasses existing public-text redaction | In progress | Pending |
| D04 | Versioned S3 output addresses are interpreted as object keys | Queued | Pending |
| D05 | Recorder actions have no rollback boundary | In progress | Pending |
| A01 | No reusable application facade or workflow control API | Queued | Pending |
| A02 | The current TES backend cannot submit to current Aruna TES | Queued | Pending |
| A03 | Tenant authorization and cache isolation are absent | Queued | Pending |
| A04 | Hosted execution needs durable admission and persistence policy | Queued | Pending |
| A05 | Secret values need an explicit artifact and form policy | Queued | Pending |
| A06 | Process-global configuration prevents straightforward in-process hosting | Queued | Pending |
| A07 | Cached remote URLs have no fresh-read policy | Queued | Pending |
| A08 | Repeat can proceed without complete historical inputs | Queued | Pending |
| A09 | Large-run APIs and telemetry need bounded result streaming | Queued | Pending |
| A10 | Engine build identity and Aruna data identity are not a single contract | Queued | Pending |
| S01 | There is no schema-driven submission form | Queued | Pending |
| S02 | Validation is not uniformly mandatory | Queued | Pending |
| S03 | Remote resources and sample-sheet contents are not checked generically | Queued | Pending |
| S04 | Validation errors lose machine-readable locations and branches | Queued | Pending |
| S05 | Validation checks a coerced copy without returning execution values | Queued | Pending |
| S06 | Help/provenance property discovery loses composition semantics | Queued | Pending |
| S07 | Argo schema omits required artifacts and effective template defaults | Queued | Pending |
| S08 | Argo artifact values are incompletely described | Queued | Pending |
| S09 | Nested CWL file fields lose declared details | Queued | Pending |
| S10 | CWL BigInteger projection can round or become null | Queued | Pending |
| S11 | Missing external sample schemas disappear without a diagnostic | Queued | Pending |
| N01 | Glob listing failures become empty channels | In progress | Pending |
| N02 | Malformed resource directives remove limits | In progress | Pending |
| N03 | nf-schema conversion does not resolve reference-defined fields | In progress | Pending |
| N04 | Entry selection and remote project acquisition are incomplete | Queued | Pending |
| N05 | Common operators and collectFile default sorting are missing | Queued | Pending |
| N06 | Groovy/JVM APIs and plugin contracts cover a finite subset | Queued | Pending |
| N07 | Channel data and native helpers are eager | Queued | Pending |
| P01 | Conda-only rules publish fake image references | Queued | Pending |
| P02 | Offline repodata loading lacks path-component validation | In progress | Pending |
| P03 | Native os.walk can follow directory symlink cycles | Queued | Pending |
| P04 | Filesystem mutators return success without performing the operation | Queued | Pending |
| P05 | Python package environments omit immutable base/platform identity | Queued | Pending |
| P06 | Ordinary groups and environment modules are not supported execution modes | Queued | Pending |
| P07 | Conda locking and verified image contexts are not an executable build pipeline | Queued | Pending |
| P08 | FIFO execution and arbitrary Python/package crossings remain unavailable | Queued | Pending |
| P09 | Semantically invalid outputs can win the shared cache | Queued | Pending |
| P10 | Reuse already-built Conda environment containers | In progress | Pending |
| C01 | Argo discards available unknown-field diagnostics | Queued | Pending |
| C02 | Direct Argo artifacts do not apply subPath | Queued | Pending |
| C03 | Argo archive and mode fields are not carried into staging | Queued | Pending |
| C04 | CWL document links reject non-HTTP data locations | Queued | Pending |
| C05 | CWL final relocation skips remote output locations | Queued | Pending |
| C06 | Full CWL image requirements and Argo cluster semantics need dedicated profiles | Queued | Pending |
| M01 | WDL and other workflow families have no native driver | Queued | Pending |
| M02 | Native composition and structured workflow results are incomplete | Queued | Pending |
| E01 | Effective network policy is excluded from shared cache identity | In progress | Pending |
| E02 | Image platform selection can occur after cache identity is formed | Queued | Pending |
| E03 | The local backend does not enforce requested accelerators | In progress | Pending |
| E04 | Other placement fields are incompletely mapped | Queued | Pending |
| E05 | Python helper transport is not resource-bounded | In progress | Pending |
| E06 | Native evaluation has no complete resource/authority boundary | Queued | Pending |
| E07 | Wrapper environment names are inserted as shell source | In progress | Pending |
| E08 | Remote ownership recovery is coordinator-bound | Queued | Pending |
| E09 | Remote outcomes fail the local-path resume check | Queued | Pending |
| E10 | Foreign directory and repository inputs have no native transfer contract | Queued | Pending |
| V01 | Current full workflow/backend parity is not established | Queued | Pending |
| V02 | Release, recovery and deployment acceptance remain open | Queued | Pending |
| V03 | Some roadmap entries disagree with newer support boundaries | Queued | Pending |

## Current sequence

1. Confirmed correctness and containment: recording atomicity, private telemetry, file/cache identity, path and option handling, native filesystem semantics and language schema fidelity.
2. Reusable description, strict admission, normalized typed values, structured errors and result contracts, including bounded file/table validation.
3. Portable Conda image preparation and completed-image cache, with verified existing-image import and cross-workflow/cross-language reuse.
4. Portable backend capability and object transfer interfaces, plus the Aruna execution/data adapter.
5. Schema-driven submission forms, current language/plugin coverage, WDL and native mixed-language workflows.
6. Pinned differential corpora, resource limits, ownership/recovery, rootless/platform and actual backend qualification.

## Suspected cases

H01 (manifest size overflow), H02 (changed-output retry producer state), and H03 (nested Argo enum propagation) require focused reproduction before choosing a patch. The audit's rejected root-normalization and native-interpreter host-fallback claims are not implementation tasks.

## Verification receipts

Focused command output and frozen identities are kept under the locally excluded `.claude/production-readiness-evidence/universal-20261007/`. The checks here use the pinned Rust toolchain, offline locked dependencies, two Cargo jobs, two test threads and the existing systemd memory guard. Required external prerequisites remain separate gates.

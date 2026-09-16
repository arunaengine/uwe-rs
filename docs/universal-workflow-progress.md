<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Universal workflow remediation progress

Baseline: `945ad6c1a8933d26a1ffea13b8d81879fdb9b450`. Branch: `fix/universal-execution`.

The user authorized implementation of all findings in the [2026-10-07 audit](universal-workflow-audit-2026-10-07.md). The core remains portable across execution backends. Aruna is a required integration using those contracts, rather than the default authority or a dependency of every core type. Workflow tools, package helpers and Conda installation remain container-only.

Four focused implementation workers share one checkout with exclusive files. The parent reviews exact diffs, runs bounded focused checks, and creates single-author signed commits of at most 500 changed lines. Existing support boundaries and user changes are preserved. No push, deployment or release is authorized by this ledger.

Statuses distinguish an implementation from qualification. A safe refusal can close silent misexecution while the actual feature remains pending. Source inspection and fake services do not establish live backend, rootless, platform, full corpus or production acceptance.

| ID | Finding or requirement | State | Commit and evidence |
| --- | --- | --- | --- |
| D01 | Resume trusts output existence rather than output content | Guarded | `170c9749`, `77a06fc1`: file digest/kind and affirmative native acceptance checks; directory candidates miss. |
| D02 | Lineage reuse has weaker input and image identity than the shared cache | Guarded | `170c9749`: exact local bytes, canonical pinned images and explicit matching network policy; unknown identities miss. |
| D03 | Telemetry bypasses existing public-text redaction | Fixed | `c5bdedbc`: three trace tests, including written artifacts and export. A05 remains separate. |
| D04 | Versioned S3 output addresses are interpreted as object keys | Fixed locally | `96c1105c`: exact version-bound HEAD/GET and encoded keys. Version-enabled service qualification pending. |
| D05 | Recorder actions have no rollback boundary | Fixed | `41306644`: failing action rollback and prior-action preservation, three tests. |
| A01 | No reusable application facade or workflow control API | Partial | `f6e3916c`: structured describe/validate/admit/execute over existing drivers and executors, four tests. Hosted control/persistence remains pending. |
| A02 | The current TES backend cannot submit to current Aruna TES | In progress | `268bd15`, `3d6d449`, `87e3143`, `7f652f25`, `86198701`: bounded native jobs client, three wire tests and completed-write version receipts. Mutable S3 null versions refuse immutable binding. Backend staging/recovery/collection pending. |
| A03 | Tenant authorization and cache isolation are absent | Queued | Pending |
| A04 | Hosted execution needs durable admission and persistence policy | Queued | Pending |
| A05 | Secret values need an explicit artifact and form policy | Queued | Pending |
| A06 | Process-global configuration prevents straightforward in-process hosting | Queued | Pending |
| A07 | Cached remote URLs have no fresh-read policy | Queued | Pending |
| A08 | Repeat can proceed without complete historical inputs | Queued | Pending |
| A09 | Large-run APIs and telemetry need bounded result streaming | Queued | Pending |
| A10 | Engine build identity and Aruna data identity are not a single contract | Queued | Pending |
| S01 | There is no schema-driven submission form | Queued | Pending |
| S02 | Validation is not uniformly mandatory | Partial | `f6e3916c`: strict embedding admission rejects missing/advisory schemas and simulated assumptions. Existing CLI relaxed policy remains. |
| S03 | Remote resources and sample-sheet contents are not checked generically | Queued | Pending |
| S04 | Validation errors lose machine-readable locations and branches | Implemented | `eb5b3e34`: normalized values and structured pointers, keywords, codes and branches; eleven tests. |
| S05 | Validation checks a coerced copy without returning execution values | Implemented | `48e841bc`: checked values applied through Driver::use_inputs and returned for recording; seven Nextflow driver tests. Defaults remain annotations. |
| S06 | Help/provenance property discovery loses composition semantics | Queued | Pending |
| S07 | Argo schema omits required artifacts and effective template defaults | Implemented | `449234fc`: effective entry template and required artifacts, three schema tests. |
| S08 | Argo artifact values are incompletely described | Partial | `449234fc`: supported defaults match runtime; unsupported HDFS/OSS/plugin descriptions refuse explicitly. Provider/runtime expansion pending. |
| S09 | Nested CWL file fields lose declared details | Fixed | `4ee6f16b`: nested format/secondary-file metadata. |
| S10 | CWL BigInteger projection can round or become null | Fixed | `53fa7f9`: exact i64 projection and explicit out-of-range errors; four schema tests. |
| S11 | Missing external sample schemas disappear without a diagnostic | Fixed | `1ff381fb`: path-specific missing/read/JSON/schema diagnostics and preserved reference scope; six input tests. |
| N01 | Glob listing failures become empty channels | Fixed | `9d3cb9ec`: genuine errors propagate, local no-match preserved; thirty path tests. |
| N02 | Malformed resource directives remove limits | Fixed locally | `fa5fda1a`, `b53c3a0`: parse/range/negative failures, no GPU narrowing, native active CPU minimum; thirteen task tests. Differential corpus pending. |
| N03 | nf-schema conversion does not resolve reference-defined fields | Partial | `f34db9b`: local pointers converted; unsupported reference scopes and structural compositions refuse explicitly; eleven plugin tests. |
| N04 | Entry selection and remote project acquisition are incomplete | Queued | Pending |
| N05 | Common operators and collectFile default sorting are missing | Queued | Pending |
| N06 | Groovy/JVM APIs and plugin contracts cover a finite subset | Queued | Pending |
| N07 | Channel data and native helpers are eager | Queued | Pending |
| P01 | Conda-only rules publish fake image references | Queued | Pending |
| P02 | Offline repodata loading lacks path-component validation | Fixed | `eb6748cf`: shared component checks; twelve lock tests. Symlinks inside caller-controlled caches remain a trust boundary. |
| P03 | Native os.walk can follow directory symlink cycles | Implemented | `7df3fdb`, `a21da5e`: incremental pruning/order/link/cycle handling and error callbacks; program regressions pass. |
| P04 | Filesystem mutators return success without performing the operation | Contained | `7df3fdb`: unsupported mutation calls fail explicitly without changing files. Native mutation implementation remains pending. |
| P05 | Python package environments omit immutable base/platform identity | Queued | Pending |
| P06 | Ordinary groups and environment modules are not supported execution modes | Queued | Pending |
| P07 | Conda locking and verified image contexts are not an executable build pipeline | Queued | Pending |
| P08 | FIFO execution and arbitrary Python/package crossings remain unavailable | Queued | Pending |
| P09 | Semantically invalid outputs can win the shared cache | Implemented locally | `9c5d278`, `f3a64b39`, `aec35e5d`, `ecc84313`, `0fd85092`, `ae7864b2`, `7869a0b7`, `f48e2937`, `77a06fc1`, `aee43a89`: all four native verdicts, deferred publication, exact-observation rejection, terminal-state cleanup and recorded acceptance. Integrated Snakemake/CWL rejection, approval and reuse pass. |
| P10 | Reuse already-built Conda environment containers | Partial | `b2ad2119`, `0df9011d`, `dc64d307`, `60dabd57`, `6799fa4`, `fae413c`, `c8ac519f`, `46045b4d`: receipts, ownership, inspected builds, verified archives and automatic lookup/local reuse. Six cache fixtures plus ten build/transport fixtures pass; workflow/CLI binding and live builds pending. |
| C01 | Argo discards available unknown-field diagnostics | Fixed locally | `484fdb8b`: typed unknown fields fail admission with source locations; raw metadata remains extensible, two tests. |
| C02 | Direct Argo artifacts do not apply subPath | Partial | `3247376c`: confined materialized-directory selection after substitutions; three tests include parent/absolute/symlink escapes. Unmaterialized sources refuse explicitly. |
| C03 | Argo archive and mode fields are not carried into staging | Queued | Pending |
| C04 | CWL document links reject non-HTTP data locations | Queued | Pending |
| C05 | CWL final relocation skips remote output locations | Queued | Pending |
| C06 | Full CWL image requirements and Argo cluster semantics need dedicated profiles | Queued | Pending |
| M01 | WDL and other workflow families have no native driver | Queued | Pending |
| M02 | Native composition and structured workflow results are incomplete | Queued | Pending |
| E01 | Effective network policy is excluded from shared cache identity | Fixed | `0038fe75`: effective offline identity separated, existing online identity preserved; three request tests. |
| E02 | Image platform selection can occur after cache identity is formed | Queued | Pending |
| E03 | The local backend does not enforce requested accelerators | Contained | `ed86d6a2`: unsupported nonzero requests refuse before staging. Actual device mapping remains pending. |
| E04 | Other placement fields are incompletely mapped | Queued | Pending |
| E05 | Python helper transport is not resource-bounded | Implemented | `49883566`: memory/no-swap, frames, queue, exchange and preparation deadlines; actual Docker package fixture passed. |
| E06 | Native evaluation has no complete resource/authority boundary | Queued | Pending |
| E07 | Wrapper environment names are inserted as shell source | Fixed | `26a8afa4`: main/secondary spec names checked and direct wrappers fail safely; twelve model tests and executor checks. |
| E08 | Remote ownership recovery is coordinator-bound | Queued | Pending |
| E09 | Remote outcomes fail the local-path resume check | Partial | `170c9749`, `77a06fc1`: provider materialization verifies regular remote output bytes and acceptance; directory/history-policy cases safely miss. |
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

The initial recorder fault regression failed before the rollback patch and passed afterward. The first delegated checks exposed two compile errors; both were corrected, and all five retry targets passed (59 selected tests). Later checks caught malformed YAML/test setup and an unsupported Bollard import query. The fixtures were corrected, the optional query removed, and the affected targets passed. One unbounded mock listener was stopped, then replaced with bounded acceptance and read deadlines before its five archive tests passed.

The Docker Python package fixture ran through its real container path. Conda build/archive/cache fixtures exercise the Docker-compatible API; they do not qualify an actual Conda installation, Docker classic-builder availability, Podman, rootless execution or other architectures. Aruna fixtures match native jobs revision `51c43dba96198542addb53d3375579c308de0b17`; they do not qualify a live Aruna backend.

## Semantic acceptance verification

All four native drivers validate outputs after task execution. Previously the runner published exit-zero results before those checks, and recording could make rejected attempts resumable. The fix adds one explicit acceptance step across existing driver, executor, engine and recorder interfaces. Returning `Ok` from completion is not sufficient because native ignore, retry and keep-going paths can report semantic failure internally.

The contract is bound to task ID and attempt. Publication candidates and cache-hit observations remain private runner metadata. Build leases must be released before native acceptance so ordered identical-key tasks cannot deadlock. Rejection invalidates only the observed cache entry. Recorded affirmative acceptance must be required for lineage reuse; unknown historical acceptance is not proof. Lease checks continue to be diagnostic rather than an atomic distributed publication fence.

The runner regressions cover provisional publication, rejection, observed replacement protection, returned-earlier reproducibility results, cancellation, supersession, contention and ordered completion. Independent source review also found retained terminal pending state; explicit terminal-state pruning and payload-release tests now pass. The gate is connected through all four native drivers, the engine, runner, recording and resume.

| Focused target | Passing checks | Qualification boundary |
| --- | ---: | --- |
| Workflow graph engine, attempts, order and existing event tests | 37 | Raw finished event order preserved |
| Native acceptance contract and engine validation tests | 3 | Default rejection and exact attempt identity |
| Runner and settlement tests | 45 | Includes replacement races, reverse order and retained payload cleanup |
| Nextflow acceptance | 4 | Ignore, retry and cached output revalidation |
| Argo acceptance | 3 | Output errors use explicit OnError retry policy |
| CWL output collection | 3 | Native paths and output JSON checks |
| Snakemake results and services | 9 | One live service case remains ignored |
| Database and pinned migrations | 21 | Versions 1, 3, 6 and 7 migrate; unknown acceptance is not affirmative |
| Resume | 6 | Unknown/rejected exit-zero outcomes, including empty file lists, miss |
| Application admission | 4 | Strict schema admission and normalized execution |
| Native engine/runner/store/database integrations | 2 | Actual Snakemake/CWL collectors, fake backend writing real files |

The integrated Snakemake fixture checks the exact tool fingerprint because a valid run can also include a virtual goal. It removes only the delivered workflow output before the third run so native up-to-date detection cannot skip the tool being tested. The shared cache stays intact, and the third run revalidates and reuses it. These local checks do not replace differential language corpora or actual backend qualification.

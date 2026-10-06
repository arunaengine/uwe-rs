<!-- Remediation progress and validation evidence for universal workflow execution. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Universal workflow remediation progress

Baseline: `945ad6c1a8933d26a1ffea13b8d81879fdb9b450`. Branch: `fix/universal-execution`.

The user authorized implementation of all findings in the [2026-10-07 audit](universal-workflow-audit-2026-10-07.md). The core remains portable across execution backends. Workflow tools, package helpers and Conda installation remain container-only.

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
| A02 | Portable TES staging through S3 is unfinished | In progress | Generic TES staging is still being implemented. |
| A03 | Tenant authorization and cache isolation are absent | Queued | Pending |
| A04 | Hosted execution needs durable admission and persistence policy | Queued | Pending |
| A05 | Secret values need an explicit artifact and form policy | Queued | Pending |
| A06 | Process-global configuration prevents straightforward in-process hosting | Queued | Pending |
| A07 | Cached remote URLs have no fresh-read policy | Queued | Pending |
| A08 | Repeat can proceed without complete historical inputs | Queued | Pending |
| A09 | Large-run APIs and telemetry need bounded result streaming | Queued | Pending |
| A10 | Engine build identity and remote data identity are not a single contract | Queued | Pending |
| S01 | There is no schema-driven submission form | Partial | `0e539a81`, `d5db493c`: versioned full-schema form description, typed field-error responses and profile-sensitive CLI JSON; four native-driver form tests and one CLI test pass. Browser renderer and hosted submit/control remain pending. |
| S02 | Validation is not uniformly mandatory | Partial | `f6e3916c`: strict embedding admission rejects missing/advisory schemas and simulated assumptions. Existing CLI relaxed policy remains. |
| S03 | Remote resources and sample-sheet contents are not checked generically | Queued | Pending |
| S04 | Validation errors lose machine-readable locations and branches | Implemented | `eb5b3e34`: normalized values and structured pointers, keywords, codes and branches; eleven tests. |
| S05 | Validation checks a coerced copy without returning execution values | Implemented | `48e841bc`, `c3cce2a4`, `0e539a81`: checked values applied through Driver::use_inputs and returned for recording; separate typed-JSON mode preserves submissions without CLI coercion, four added tests. Defaults remain annotations. |
| S06 | Help/provenance property discovery loses composition semantics | Partial | `0e539a81`: forms retain the original schema without flattening references/compositions. Help/provenance metadata projection remains pending. |
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
| P01 | Conda-only rules publish fake image references | Fixed for local YAML | `4803ea10`, `8d7d8ea6`, `b39e814e`, `7c839011`: actual included-rule paths resolve current YAML/repodata into inspected configuration IDs before task keys; stale pins are bypassed. Other preparation modes/backends remain unsupported. |
| P02 | Offline repodata loading lacks path-component validation | Fixed | `eb6748cf`: shared component checks; twelve lock tests. Symlinks inside caller-controlled caches remain a trust boundary. |
| P03 | Native os.walk can follow directory symlink cycles | Implemented | `7df3fdb`, `a21da5e`: incremental pruning/order/link/cycle handling and error callbacks; program regressions pass. |
| P04 | Filesystem mutators return success without performing the operation | Contained | `7df3fdb`: unsupported mutation calls fail explicitly without changing files. Native mutation implementation remains pending. |
| P05 | Python package environments omit immutable base/platform identity | Queued | Pending |
| P06 | Ordinary groups and environment modules are not supported execution modes | Queued | Pending |
| P07 | Conda locking and verified image contexts are not an executable build pipeline | Implemented locally | `b39e814e`, `7c839011`: explicit offline target inputs, bounded snapshots, authoritative virtual packages and container-only installation. Actual Docker classic-build/native Snakemake fixture passed. Online, pip and remote execution pending. |
| P08 | FIFO execution and arbitrary Python/package crossings remain unavailable | Queued | Pending |
| P09 | Semantically invalid outputs can win the shared cache | Implemented locally | `9c5d278`, `f3a64b39`, `aec35e5d`, `ecc84313`, `0fd85092`, `ae7864b2`, `7869a0b7`, `f48e2937`, `77a06fc1`, `aee43a89`: all four native verdicts, deferred publication, exact-observation rejection, terminal-state cleanup and recorded acceptance. Integrated Snakemake/CWL rejection, approval and reuse pass. |
| P10 | Reuse already-built Conda environment containers | Implemented locally | `b2ad2119`, `0df9011d`, `dc64d307`, `60dabd57`, `6799fa4`, `fae413c`, `c8ac519f`, `46045b4d`, `2d2a8230`, `7c839011`: provenance-bound receipts, compatible local reuse and verified shared archives. Twenty current build/transport/cache tests pass. Two actual native workflows reuse the same receipt after package-cache eviction; broader backends/platforms pending. |
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
4. Portable backend capability and object transfer interfaces, including TES staging through S3.
5. Schema-driven submission forms, current language/plugin coverage, WDL and native mixed-language workflows.
6. Pinned differential corpora, resource limits, ownership/recovery, rootless/platform and actual backend qualification.

## Suspected cases

H01 (manifest size overflow), H02 (changed-output retry producer state), and H03 (nested Argo enum propagation) require focused reproduction before choosing a patch. The audit's rejected root-normalization and native-interpreter host-fallback claims are not implementation tasks.

## Verification receipts

Focused command output and frozen identities are kept under the locally excluded `.claude/production-readiness-evidence/universal-20261007/`. The checks here use the pinned Rust toolchain, offline locked dependencies, two Cargo jobs, two test threads and the existing systemd memory guard. Required external prerequisites remain separate gates.

The initial recorder fault regression failed before the rollback patch and passed afterward. The first delegated checks exposed two compile errors; both were corrected, and all five retry targets passed (59 selected tests). Later checks caught malformed YAML/test setup and an unsupported Bollard import query. The fixtures were corrected, the optional query removed, and the affected targets passed. One unbounded mock listener was stopped, then replaced with bounded acceptance and read deadlines before its five archive tests passed.

The Docker Python package fixture ran through its real container path. Conda API fixtures remain distinct from the actual installation and reuse qualification below. Podman, rootless execution and other architectures remain unqualified.

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

## Typed forms and native Conda verification

`c3cce2a4` adds typed JSON validation without string coercion or default insertion; existing CLI validation is preserved. `0e539a81` exposes the original schema and structured validation responses for embedding, and `d5db493c` exposes profile-sensitive descriptions through `iunctus inputs -format form`. Eleven existing validation checks, four typed checks, four application checks, four form checks and one real CLI form check passed. This is a portable form interface; browser rendering, hosted submission and authorization remain separate work.

The Conda path is split into explicit preparation configuration (`b39e814e`), current source/container identity (`4803ea10`, `8d7d8ea6`), cold prerequisites (`2d2a8230`) and executable local workflow wiring (`7c839011`). Twenty build/cache/archive checks, thirteen source/task-key checks and two stale-pin/cancellation checks passed. The application binary target passed 76 checks with one unrelated live case ignored; two ordinary Conda CLI checks and the form CLI check passed. A subsequently added repeat test initially failed to compile because its error lacked `Debug`; only the assertion was corrected, and its focused rerun passed.

Independent review found that historical Conda entries with bare local IDs were filtered out before repeat refusal. `7c839011` preserves those entries for explicit refusal. Source preparation uses nonblocking regular-file opens and bounded snapshots. Its timeout bounds the caller's wait; an already-running blocking worker cannot be forcibly terminated, and the retained semaphore limits concurrent preparation. Capability descriptors remain trusted operator evidence. Diagnostic lease checks are not an atomic distributed fence.

The explicitly enabled `real_conda_rule_reuses_its_image_after_package_cache_eviction` passed against rootful Docker 29.8.2 on Linux amd64. The pinned Micromamba base manifest was `sha256:e583f8e151fd1ebbc78b05cf4feb37ddbb518892dbabf6c0c03ea8ad613bc519`. Target virtual capabilities were probed inside that image with no network and bounded memory/CPU. The SHA-256-verified tzdata package was installed inside the builder, then two fresh native Snakemake workflows produced `timezone-ready`. Task cache reuse was disabled; the package directory was deleted before the second workflow, which retained the same completed-image receipt. The complete inspected image ID was `sha256:8238a3ccf11b984f2cf5f78bc9187175bec23e7fca7c14350ea864cdbf441280`. Frozen metadata and image inspection are retained in the excluded `conda-live/` evidence directory.

This qualifies one small offline local environment and its image reuse. BuildKit, online acquisition, pip locking, remote Conda executors, rootless engines, other platforms, large environments and complete language/backend parity remain open.

Aruna integration uses the generic TES executor plus S3 storage; TES-over-S3 staging is still being implemented.

## Local nf-core execution qualification, 2026-10-08

Sequential real runs use Nextflow 26.04.6 as the reference and the release iunctus binary with local Docker Engine 29.8.2. Sources are temporary copies at the cached nf-core revisions, with unchanged test profiles. Reference tasks have a four CPU, 16 GB executor limit; iunctus has four pending tasks and a fresh cache, with result reuse disabled. Input headers and free space are checked before runs. A 25 GiB free-space reserve is retained; existing images and caches are preserved.

| Pipeline | Revision | Nextflow | iunctus | Verdict and remaining difference |
| --- | --- | --- | --- | --- |
| demo | 555d145 | Exit 0, 76 files | Exit 33, 1 file | Explicit default `collectFile` hash-sort refusal at workflows/demo.nf:91; 70 material files missing. |
| datasync | d25e58a | Exit 0, 33 files | Exit 33, 1 file | Default `collectFile` hash sorting refused at workflows/datasync.nf:209; 27 material files missing. |
| fastqrepair | 70a3820 | Exit 0, 71 files | Exit 33, files=1 | unsupported, parity not established; 65 material files missing. Default `collectFile` hash sorting refused at workflows/fastqrepair.nf:161:45. |
| phyloplace | 68db179 | Exit 0, 33 files | Exit 33, files=1 | unsupported, parity not established; 27 material files missing. Default `collectFile` hash sorting refused at workflows/../subworkflows/nf-core/fasta_hmmsearch_rank_fastas/main.nf:74:5. |
| bamtofastq | daf99df | Exit 1, 8 files | Exit 33, files=1 | Reference test profile lacks FASTA for SAMTOOLS_FAIDX. Iunctus refuses default `collectFile` hash sorting at workflows/bamtofastq.nf:220; parity is unqualified. |
| rnaseq | a1fcddd | Not run | Not run | Disk skip: 44.38 GiB free minus a 23 GiB allowance leaves 21.38 GiB, below the 25 GiB reserve. |

The only comparison exclusion is `pipeline_info/**`; date-bearing filenames are normalized to `<time>`. Scientific outputs, FastQC reports, MultiQC data and plots remain in the comparison. Complete output hashes, names and logs are retained in the locally excluded `.claude/production-readiness-evidence/nfcore-20261008/`; per-pipeline source, run, work and cache folders are removed after recording. These refusals leave local nf-core output parity open. The older SLURM records predate the explicit hash-sort refusal and do not qualify the current revision.

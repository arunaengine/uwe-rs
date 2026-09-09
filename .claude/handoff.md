# Iunctus remediation handoff

Work resumed at the user's request on 2026-10-03. Continue in `/home/beavis/projects/iunctus` on **`fix/production-readiness`**, using the same shared checkout. The original review is [report.md](report.md), covering source `cd37a1843cc183955577804f63b3f8dbd111bf10`; remediation began from `5e5399d4e21647f51ed9997703aa74ca24f29608`.

The latest verified code checkpoint is **`825751d6c511ccc20d45f94bfd428d2cfe4a33cb`**. One hundred fourteen small signed source commits preserve the completed batches, including eighty-five from the resumed work. Author and committer identities/dates match, subjects are single semantic lines, and each commit changes at most 500 lines. No push or pull request was performed. Five resumed waves are committed; continue the remaining work in the same shared checkout.

This remains a trusted local workflow engine. Describe path-related findings as path handling, staging and result correctness under the calling user's permissions. Preserve explicitly configured local inputs and hostPath behavior. Keep all task and package execution in containers; Conda prepares images. Implementation should continue primarily with Sol workers and narrow file ownership.

## Status

| Finding state | Count | Meaning |
| --- | ---: | --- |
| Implemented | 112 | Committed correction with passing local checks. Real backend qualification remains a separate gate. |
| Partial | 11 | Some required paths are corrected; specific caller/backend work remains. |
| Open | 2 | No complete corrective patch is committed. |
| Total | 125 | Every numbered finding in the source review appears below. |

The status register is authoritative for the latest committed checkpoint. Prepared changes do not count as verified fixes. The original report describes the reviewed baseline, so its line references and initial verdict should be interpreted at that revision. The project is not yet qualified for broad unattended production use.

## Verification

The fifth-wave affected checks verify **618 primary tests across 65 targets, zero unresolved failures, and 13 ignored external cases**. Final CWL and Snakemake package reruns replace earlier failed targets; one nested snapshot-permission child summary is excluded. Full pinned workspace formatting and Clippy pass, with final focused Snakemake Clippy after fixture correction. Test/development debug symbols were disabled; runtime assertions remained enabled. This continuation did not run real backend/Docker, hosted CI, browser or artifact qualification.

Fifth-wave fixes cover provider-specific immutable download/version bundles, source-handle version association, directory manifest retention, owned unique transfer/staging temporaries, lease-loss cancellation and late-success rejection, deferred CWL writers and held aliases, Nextflow sorting callbacks/policies, local SLURM recovery admission, and evaluated Snakemake signatures/local protection. Durable incomplete markers prevent partial output delivery from being reused under prior signatures. Failed attempts and intermediate compile/format/Clippy errors remain preserved; latest passing checks supersede them.

The third-wave full run covered **1,537 primary tests across 180 Cargo targets**, with two fixture-only failures corrected and passed in focused reruns. The fourth-wave affected-package checks cover **903 primary tests across 109 targets, zero unresolved failures, and 38 ignored external cases**. Two SLURM fixtures were corrected for real newline output and the intentional independent text termination query. A local runner fixture now waits at most one second for observed maintenance release; 20 isolated repeats already passed, and the complete runner target passed after that correction. A source-backed inherited-descriptor timing explanation remains an inference, not a demonstrated production leak. Nested umask child results are excluded from primary counts.

The **full pinned workspace formatting and Clippy gates pass** without lint suppressions. All 28 doctest targets completed successfully, with no embedded doctest examples. The exact Conda corpus gate remains 2,053 files, 2,032 accepted and all 21 recorded path/message rejections. Seventeen earlier selected real rootful Docker cases and two third-wave cases passed, including ordered service execution and installed transitive package constraints. The pinned UV offline wheel probe disproved the suspected extras-constraint issue, so no speculative extras rewrite was made.

One fourth-wave linker attempt ended with SIGBUS while the filesystem was nearly full; its log remains preserved. Reclaiming only debug symbols from completed task-generated executables restored capacity, then the affected checks completed. This interruption is separate from code failures. Rootless Docker/Podman, live TES/Kubernetes/SLURM/object-store, browser, multi-host filesystem and rollout remain unqualified. Hosted CI and candidate artifact workflows have not run.

Clippy also records a future Rust incompatibility in transitive `attribute-derive-macro 0.10.5`: a trailing semicolon in an expression-position macro. Current compilation passes; retain the pinned toolchain and qualify an upstream upgrade before widening compiler support. The exact compiler report is preserved.

## Evidence

Local receipts, exact staged/reviewed patches, source hashes, test logs, the full JSON finding ledger, corpus baseline and benchmark sources are preserved in [production-readiness-evidence](production-readiness-evidence/). This directory is excluded through the repository's resolved local `info/exclude`; the handoff and review are committed. Verified checkpoint source changes and tests are committed. The working tree was clean at this checkpoint; later continuation packets must be verified separately.

Key receipts include `resumed-wave-3-commits.json`, `resumed-wave-4-commits.json`, `resumed-wave-3-tests-v5.log`, `resumed-wave-3-focused-v6.log`, `resumed-storage-wave-4-v1.log`, `resumed-languages-slurm-wave-4-v1.log`, `resumed-cache-public-wave-4-v2.log`, `resumed-slurm-runner-wave-4-v3.log`, `resumed-clippy-wave-4-final-v2.log`, `resumed-format-wave-4-final-v3.log`, `resumed-doctests-wave-3.log`, `resumed-wave-4-source-v3.json`, `resumed-wave-4-verification.json`, `future-incompatibility-report.json`, `ledger.json`, and each batch's exact reviewed patches. Older receipts and the exact corpus baseline remain preserved. Older `/tmp/iunctus-production-readiness` scratch paths did not survive this provider handoff; the excluded evidence directory remains authoritative.

Fifth-wave receipts include `continued-wave-5-final-source-v4.json`, `continued-wave-5-verification.json`, `continued-wave-5-commits.json`, all fifteen exact staged/reviewed patches, `continued-wave-5-final-tests-v2.log`, `continued-snk-cwl-wave-5-final.log`, `continued-wave-5-format-v3.log`, `continued-wave-5-clippy-v2.log`, and `continued-snk-wave-5-final-clippy.log`. The final Nextflow fixture formatting-only delta is recorded in the source receipt.

Generated test executables alone had debug symbols removed when disk space became tight. The path/size receipts are in `generated-test-binaries.json` and `generated-completed-test-binaries-wave-4.json`; source, test logs and pre-existing artifacts were preserved. Later Cargo builds can regenerate those symbols. Do not remove unrelated build caches or existing worktrees.

## Highest priority remaining work

1. Complete CAS-10 exact-version shared stale-entry replacement and CAS-17 corruption recovery. These are the two wholly open numbered findings.
2. Complete CAS-08 private full-tree hydration and completion publication, including child-first, interrupted, concurrent and empty-directory cases. Directory descriptors/retention are implemented, but existing partial roots remain accepted. Finish CAS-09 owned cleanup for local counter-named temporaries.
3. Provide genuine distributed fencing. Runner loss monitoring, cancellation and publication checks are implemented; expiry checks cannot atomically fence submitted mutations. Remote destructive GC remains refused.
4. Integrate mandatory TES/Kubernetes library recovery admission and qualify live remote recovery. Local SLURM orphan recovery is implemented; ambiguous, foreign, changed-configuration and insufficient-identity work stays retained.
5. Finish NFR-08 bounded emission, NFR-11 directives/error callbacks, SNK-09 lifecycle/resource/shadow/cache/handover contracts, CWL-08 merged parent-directory views, and CWL-05 native/parse/heap/cancellation bounds. Supported sorting and evaluated signatures/local protection are implemented; native hash/deep sorting is explicitly unsupported.
6. Connect immutable Conda context preparation to container build, target capability discovery, transport and CLI reuse. Supply complete pinned corpus bundles and qualify hosted CI/artifacts and real backends.

## Finding register

Implemented entries show the relevant code commits. Partial/open entries state the remaining correction; use the original review for the causal trace and proposed reproducer.

| Finding | State | Remaining work or code evidence |
| --- | --- | --- |
| ENG-01 | Implemented | e8f95fe2 |
| ENG-02 | Implemented | c40c16da |
| ENG-03 | Implemented | d914d987 |
| ENG-04 | Implemented | d914d987 |
| ENG-05 | Implemented | d914d987 |
| ENG-06 | Implemented | d914d987 |
| LOC-01 | Implemented | 73ec27a1 |
| LOC-02 | Implemented | 73ec27a1 |
| LOC-03 | Implemented | 73ec27a1, 16489cd4 |
| LOC-04 | Implemented | 73ec27a1, 6fc692f5 |
| LOC-05 | Implemented | 63919ae0 |
| LOC-06 | Implemented | 0f98258d |
| LOC-07 | Implemented | 0f98258d, cad90602 |
| LOC-08 | Implemented | 63919ae0, 93b042d3, 284d9ecb |
| LOC-09 | Implemented | 63919ae0 |
| LOC-10 | Implemented | a5abecf6, 6fc692f5, 9db8f816 |
| LOC-11 | Implemented | 25aebf81, 87b06696 |
| CON-01 | Implemented | ca64f2ec, a227fb9f |
| CON-02 | Implemented | ca64f2ec |
| REM-01 | Implemented | b736670d |
| REM-02 | Implemented | 32f34f46 |
| REM-03 | Implemented | b736670d |
| REM-04 | Implemented | 32f34f46 |
| REM-05 | Partial | 802b71a5, 6780772a implement matching local SLURM orphan recovery before library admission. TES/Kubernetes library admission and live qualification remain. |
| REM-06 | Implemented | 93b042d3 |
| REM-07 | Implemented | b736670d |
| REM-08 | Implemented | f47fa00b, 344d3ba2 |
| REM-09 | Implemented | b736670d |
| CAS-01 | Implemented | f0388fbb, 6b737545, cad90602 |
| CAS-02 | Partial | Internal links are retained locally; complete remote tree hydration remains CAS-08. |
| CAS-03 | Partial | Local cleanup/admission is synchronized. Destructive cleanup with expiring remote authority is refused until a genuine fence exists. |
| CAS-04 | Partial | 9f8b4857, 420a54f9 implement runner authority checks, loss monitoring, backend cancellation and late-success rejection. Checks cannot atomically fence submitted remote mutations. |
| CAS-05 | Implemented | 4dadf581, 7961ac5a, 6c4910b3 |
| CAS-06 | Implemented | 16489cd4 |
| CAS-07 | Implemented | 2e68446c, 2c7abf20 |
| CAS-08 | Partial | 7961ac5a, 6c4910b3 implement manifest associations and retention. Complete private hydration/publication remain; an existing partial root is still accepted. |
| CAS-09 | Partial | 7961ac5a, 5d9cbb6a add owned unique layered-transfer/runner-staging temporaries. Local counter-named store temporaries still need owned cancellation cleanup. |
| CAS-10 | Open | Separate front eviction from shared stale-entry replacement; condition mutation on the stale version without deleting a concurrent repair. |
| CAS-11 | Implemented | d430989a |
| CAS-12 | Implemented | d430989a |
| CAS-13 | Implemented | d430989a |
| CAS-14 | Implemented | f0388fbb, 6b737545, cad90602 |
| CAS-15 | Implemented | 686a9545 |
| CAS-16 | Implemented | 962a5bfc, 6c4910b3; provider-aware immutable bundles pair source version/bytes. Legacy address-only caches are retained but ignored. |
| CAS-17 | Open | Define corruption recovery for entries, output bytes and logs; quarantine or safely miss damaged cache data instead of silent reuse. |
| NFL-01 | Implemented | 3bc29285, 13064401 |
| NFL-02 | Implemented | 6f8410f9 |
| NFL-03 | Implemented | b2da319a |
| NFL-04 | Implemented | b2da319a |
| NFL-05 | Implemented | b2da319a |
| NFL-06 | Implemented | b2da319a |
| NFL-07 | Implemented | 3bc29285 |
| NFL-08 | Implemented | 6f8410f9 |
| NFR-01 | Implemented | db22a564, 47e33f86 |
| NFR-02 | Implemented | 16489cd4, 925bb09b |
| NFR-03 | Implemented | aa3e4ce7 |
| NFR-04 | Implemented | 0e64bcb4; reserved operator folders are persistent and currently require caller-managed retention. |
| NFR-05 | Implemented | aa3e4ce7 |
| NFR-06 | Implemented | 75c50272 |
| NFR-07 | Implemented | 1400d141, 75c50272, fb1b3e64, 2d12a437 |
| NFR-08 | Partial | Stopped cursors are reclaimed; bounded factory emission and process-round backpressure remain. |
| NFR-09 | Implemented | 5c3fc8df; key/comparator closures and natural/arrival policies preserved. Native hash/deep modes explicitly unsupported. |
| NFR-10 | Implemented | d914d987, d96a4966, a1ee1791 |
| NFR-11 | Partial | 5c3fc8df preserves supported collectFile sorting and refuses hash/deep including default hashing. beforeScript/afterScript/maxErrors/fair and subscribe error callbacks remain. |
| SNK-01 | Implemented | 238b3a82 |
| SNK-02 | Implemented | fdd2e643 |
| SNK-03 | Implemented | 8b1db743 |
| SNK-04 | Implemented | 8b1db743 |
| SNK-05 | Implemented | 8b1db743 |
| SNK-06 | Implemented | 4f4dee76 |
| SNK-07 | Implemented | c4aee6fe; real FIFO execution explicitly refused before task publication; full concurrency remains unsupported. |
| SNK-08 | Implemented | fdd2e643, 4f4dee76 |
| SNK-09 | Partial | 971eb9f7, f732ff33, 825751d6 implement evaluated signatures/dependent reruns, local protection and durable incomplete delivery markers. Lifecycle/resource/shadow/cache/handover contracts remain; legacy outputs without records retain skip behavior and unsupported params are uncomparable. |
| PY-01 | Implemented | ecde037f |
| PY-02 | Implemented | ecde037f |
| PY-03 | Implemented | ecde037f |
| PY-04 | Implemented | 98219fec |
| PY-05 | Implemented | 98219fec |
| PY-06 | Implemented | 98219fec |
| PY-07 | Implemented | a8a39d2a |
| PY-08 | Implemented | 98219fec |
| PY-09 | Implemented | b68e3be6 |
| PY-10 | Implemented | a8a39d2a, 6fc692f5 |
| PY-11 | Implemented | ecde037f |
| PY-12 | Implemented | 98219fec |
| PY-13 | Implemented | a8a39d2a |
| CWL-01 | Implemented | 4bfeba6e, 854a749c, 88a0ae5b, dae77512 |
| CWL-02 | Implemented | d8fab178 |
| CWL-03 | Implemented | d8fab178 |
| CWL-04 | Implemented | 08d96ca8 |
| CWL-05 | Partial | Cooperative VM slices, total work and deadline are implemented. Parse/native builtin/heap limits and external cancellation remain. |
| CWL-06 | Implemented | 08d96ca8 |
| CWL-07 | Implemented | d8fab178 |
| CWL-08 | Partial | 941c72bc, 583699e0 defer raw preparation, refresh held aliases and serialize nested/scatter writer access. Updated children still need merged parent-directory listing/staging/delivery views. |
| CWL-09 | Implemented | 08d96ca8 |
| CWL-10 | Implemented | 4bfeba6e, 854a749c, 88a0ae5b, dae77512 |
| ARG-01 | Implemented | acacacfa |
| ARG-02 | Implemented | acacacfa |
| ARG-03 | Implemented | 734ffe3f |
| ARG-04 | Implemented | 1e002617 |
| ARG-05 | Implemented | 2a59262f |
| ARG-06 | Implemented | 2a59262f |
| ARG-07 | Implemented | 734ffe3f |
| ARG-08 | Implemented | acacacfa |
| ARG-09 | Implemented | ef8202f8 |
| ARG-10 | Implemented | ef8202f8 |
| ARG-11 | Implemented | 2a59262f |
| SCH-01 | Implemented | c97f37c0 |
| SCH-02 | Implemented | c97f37c0 |
| RUN-01 | Implemented | c7fd15bd |
| RUN-02 | Implemented | 4cda193a |
| RUN-03 | Implemented | 96c6b18c, 03644331, da3d27c2, 3bf4ad4a |
| RUN-04 | Implemented | 3393004b |
| RUN-05 | Implemented | 3393004b |
| RUN-06 | Implemented | c7fd15bd |
| RUN-07 | Implemented | 4cda193a |
| RUN-08 | Implemented | 96c6b18c, 03644331, 37c94085 |
| RUN-09 | Implemented | 97305701 |
| WEB-01 | Implemented | 982db390 |
| WEB-02 | Implemented | 982db390 |
| TEL-01 | Implemented | 3d2bda05 |
| QLT-01 | Implemented | ea64b3b0 |
| QLT-02 | Implemented | 347051ee |
| QLT-03 | Implemented | ea64b3b0 |
| QLT-04 | Implemented | 865840ec |


## Shared contracts and integration still required

| Contract | Current state and next integration |
| --- | --- |
| Cache policy | `Reuse::Disabled` means no cache reads or writes. `Denied` retains the existing forced-refresh/publication behavior. Nextflow `cache false` uses Disabled. |
| Failure hooks | Engine cleanup precedes one-shot Nextflow onError/onComplete. The original failure survives secondary hook errors. Native TraceRecord methods remain unsupported. |
| Overlay identity | FORMAT2 and canonical serialization are unchanged. Only aliased/ancestor input destinations append ordered domain-separated bytes to the key. Disjoint keys remain compatible; staged overlays use private copies and keep input order. |
| Process inputs | `TaskFrame` and `TaskRequest` retain per-round environment and stdin. Input environment overrides config values; `Task.stdin` is literal text whose bytes the runner hashes. |
| Image platform | Local, TES, Kubernetes and SLURM use matching selected references and metadata. Unsupported GPU/platform combinations fail explicitly. Implicit image inspection no longer pulls a missing image, and that case passed real Docker qualification. |
| Remote ownership | TES, Kubernetes and SLURM persist ownership before submit and retain accepted identity. Independent terminal confirmation gates cleanup. Unknown/foreign/unconfirmed work retains journals and scratch; local SLURM startup reconciliation is implemented. Mandatory TES/Kubernetes library admission remains. |
| Cleanup synchronization | Local maintenance/admission and inode checks cover preparation, cache use and destructive removal. Remote lease operations are conditional; expiring authority cannot authorize destructive GC. Caller loss handling is implemented; atomic distributed fencing remains CAS-04. |
| Input snapshots | Local task bytes are copied before hashing and execution, and retained under input-snapshots. Internal directory links and effective executable mode are represented consistently. Directory location retention is implemented; full remote directory hydration remains. InPlace keeps explicit mutable semantics. |
| Package cancellation | Additive cancellable start/run/exchange APIs and CLI tokens cover planning. Engine phases default to 600 seconds each, configurable execution/exchange budgets; process stop is bounded to 20 seconds and Python shutdown to 30. Late accepted-ID cleanup requires the coordinator runtime to remain alive; ambiguous transport acceptance remains a recovery limit. |
| Python pins | Complete workflow constraints feed every UV preparation/install and the helper cache identity. Already-installed constrained distributions are validated before returning imports. Custom images need packaging or pip vendored packaging. |
| Conda compatibility | Authoritative virtual capabilities, verified fetch callbacks and immutable base/platform/recipe identity/context APIs are implemented. Container build/transport/CLI integration and target capability discovery remain; never infer capabilities from the driver host. |

The journal gate in `iunctus/src/clean.rs` remains inside the maintenance guard and must be preserved. It protects unresolved jobs whose ordinary run leases have ended. For remote recovery, cancel/status calls are individually bounded within the total grace and cancellation is sent promptly.

## Missing production capabilities

| Area | What remains |
| --- | --- |
| Support contract | README and provisional language/backend support matrix are implemented. Release versioning and qualification remain; plain-script workflow semantics remain undefined. |
| Automated CI | Pinned source, explicit corpus/Docker qualification and candidate artifact workflows are tracked. Local format/Clippy gates pass. Hosted execution, complete bundle availability and required branch checks remain. |
| Backend qualification | Run real Docker/rootless Podman, Funnel/TES, Kubernetes and SLURM cases for cancellation, restart, UID ownership, multi-image limits, GPU placement, timeouts and shared paths. |
| Other remote recovery | All three remote backends retain durable runtime ownership. Local SLURM startup identity reconciliation is implemented; mandatory TES/Kubernetes library caller admission remains. |
| Portable remote data | Shared driver paths remain a deployment prerequisite. Remote directory fidelity, empty directories, provider listings and staging without shared filesystems need completion. |
| Conda image preparation | Verified immutable context APIs are implemented. Connect transport, target capability discovery and context submission through the container API/CLI, then verify final image reuse. Pip-bearing locks fail explicitly; no host Conda execution. |
| Package environment reproducibility | Helper cancellation and complete transitive constraints are implemented; immutable helper image identity, system-library requirements and coordinator-crash cleanup remain. |
| Network and cache policy | Decide documented sharing policy for network-dependent results and offline reuse; current behavior was explicitly an unresolved policy, not silently changed here. |
| Historical computation | Define preserved local inputs, environment/provider config and image availability. Existing replay intentionally prefers present inputs; distinguish it from an exact historical repeat. |
| Distribution | Source installation, MIT/Apache texts, support/upgrade guidance and candidate build/checksum workflows are present. Qualified platform artifacts, independent signatures/attestation and release publication remain. |
| Retention and recovery | Test backup restoration, record/commit failure, historical schemas and cache payload compatibility, and unresolved remote ownership across coordinator recovery. |
| Dependency assurance | Advisory/dependency-license/independent provenance gates remain. The exact future Rust incompatibility report for attribute-derive-macro 0.10.5 also needs upstream upgrade qualification. |

## Performance and future behavior still needing evidence

| Risk | Work remaining |
| --- | --- |
| Pending bookkeeping | Ordinary completion/removal are indexed with reference-order tests and source-matched native measurements. CLI defaults to 1024 pending real tasks, preserving overrides. Exact shuffled ordinal selection intentionally remains linear. |
| Eager fanout | Nextflow factories/process rounds, Argo loops and CWL Cartesian scatter allocate broadly. Add lazy/bounded emission without arbitrary rejection of otherwise supported large workflows. |
| Retained graph history | Large runs retain task/event graph state. Measure peak memory and cancellation latency at representative sizes. |
| Large CAS inputs and logs | Stream verified remote staging and bounded visible log excerpts while retaining full stored logs and successful task status. |
| Trace export | All events/spans and cloned request chunks consume run-scale memory. Remove clones and introduce incremental event/export/send APIs where necessary. |
| Simulator work | Add a consistent configurable expansion/work/output budget; preserve ordinary synthetic task behavior. |
| Tree retention closure | Committed closure traversal retains descendants of imported/no-cache/recent manifests. Bare directory URL associations now retain manifests through descriptors; full-tree hydration remains CAS-08. |
| Best-effort leases | Remote lease mutation is conditional and old owners cannot delete successors. Runner loss monitoring is implemented; atomic distributed fencing remains an explicit gap. |
| Stable identity | Nextflow task identity still depends on arrival order. CLI run IDs use time/PID only. Qualify resume/replay under reordered dynamic inputs and shared hosts. |
| Native evaluation | VM budgets do not automatically bound JavaScript builtins/parse/heap or every Python native helper. Keep exact guarantees and residual limits visible. |
| os.walk | Generator expressions, live lists/custom iterators and supported generator functions now suspend correctly. os.walk link/topdown/pruning and unsupported function-suspension forms remain. |
| Schema evolution | Test old-run export/resume/replay and old-cache compatibility across the supported version window. |
| Recorder transactions | Add action savepoints/rollback and fault injection for partial action, disk-full, kill and commit failure. |
| Rootless and platform behavior | Qualify mixed UID ownership and exact selected-image execution on each advertised backend, including non-native platforms. |

The isolated Pending comparison uses mock task/outcome payloads and the actual frozen bookkeeping sources, with Rust `1.97.1`, identical `-O` builds and three alternating trials. A native C `wait4` observer avoids the Python pre-exec RSS floor in earlier receipts. At 30,000 items across Finished/AsAsked/Reversed, median wall time fell from 3.617 to 0.084 seconds and CPU from 3.584 to 0.083 seconds; peak measured RSS rose from 5,032 to 10,684 KiB. This measures bookkeeping, rather than complete workflow throughput. Compiler, sources, binaries, observer and per-phase output are sealed in `pending-indexed-source-manifest.json` and `pending-indexed-native-results.json`. Exact shuffled ordinal selection remains linear.

Denied validation retries execute freshly and lineage resume respects the same policy. The raw executor cache still publishes before language-level validation, and first-writer entries are not repaired by a successful forced retry. Preserve this distinction when deciding the documented policy for nondeterministic/network-dependent results.

## Code organization and documentation remaining

Accepted-option enforcement, error propagation and simulator/real mode boundaries still need the open semantic fixes. Cache policy and eviction/invalidation contracts remain duplicated. Temporary publication schemes, current-location versus historical-digest rules, frontend/backend terminal rules, source-span diagnostics and oracle version identities need consistent tests and documentation.

Large files remain a reviewability concern. Any restructuring should have a concrete behavioral reason and its own scoped patch. Workspace dependency centralization and existing crate responsibilities remain useful. Update the stale roadmap and documentation guarantees after the corresponding behavior is tested; historical receipts should retain their original dates and limits.

## Continue from this branch

1. Read this handoff, `report.md`, `ledger.json`, and the current Git diff/status. Keep the shared branch and preserve user changes made after this checkpoint.
2. Reconfirm the code revision and local tools. Restore evidence paths from this repository if `/tmp` was cleared. Use Sol implementation workers with exclusive directories; parent owns Git, root manifests, shared CLI contracts and checks.
3. Start with the seven partial and eight open findings above, using the prepared caller-loss, hydration, provider-cache and writable-scheduling plans. Confirm false positives from source rather than blindly implementing every historical suggestion.
4. Before edits, state the narrow cause, minimum correction and exact files. Produce coherent batches of at most 450 changed lines so final signed commits stay below 500.
5. Freeze each batch, format only touched files, review the exact diff, and run meaningful bounded checks. Guarded Cargo needs approved host access to the systemd user scope; preserve the guard and signing hooks.
6. Update each finding with a commit and exact validation, and retain unresolved policy or external qualification explicitly. Do not mark production readiness from local unit results alone.

Useful checks:

```sh
git switch fix/production-readiness
git status --short
cargo test --offline --locked --workspace --all-targets --no-fail-fast -j 2
cargo +nightly-2026-09-14 fmt --all -- --check
cargo +nightly-2026-09-14 clippy --offline --locked --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test --offline --locked --workspace --doc -j 2
```

Run expensive external/corpus gates only with their documented prerequisites. QLT-02 has a committed exact 21-file rejection fixture and a passing 2,053-file corpus receipt. The next trace, generator, image-build, maintenance and scheduling changes need technically necessary interface coordination; preserve supported behavior rather than silently discarding options.

## Ownership for parallel continuation

| Area | Exclusive paths |
| --- | --- |
| Shared engine and model | `workflow-graph/`, `task-model/` |
| Cache and storage | `content-store/`, `storage/`, `task-runner/` |
| Local execution and package bridge | `executor/`, `python-evaluator/src/packages/` |
| Remote execution | TES/Kubernetes/SLURM crates and their three CLI adapter files |
| Nextflow language | Parser/evaluator/config/plugins crates |
| Nextflow runtime | `nextflow-runtime/` |
| Snakemake | Parser/runtime; reserve `tests/python_container.rs` for gate owner |
| Python semantics | `python-evaluator/` except the package bridge |
| CWL and Argo | Separate parser/runtime pairs |
| Run history and presentation | Run database/crate/page/telemetry and their CLI reconstruction adapters |
| Quality and release | Input schema, reserved corpus/container gate files, roadmap/support/README/licenses/CI |
| Conda resolver | `conda-environment/` except corpus gate owned by quality |
| Parent integration | Root manifests/lock, justfile, shared CLI main/cleanup, review/handoff and coordination |

The 99 verified source commits remain on the separate branch. Four resumed waves are committed; proceed with the remaining numbered findings and capability gates in the same shared checkout. Implementation workers pause after each packet until the parent acknowledges a verified commit.

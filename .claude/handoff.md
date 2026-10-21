# uwe remediation handoff

Work resumed at the user's request on 2026-10-03. Continue in `/home/beavis/projects/uwe-rs` on **`fix/production-readiness`**, using the same shared checkout. The original review is [report.md](report.md), covering source `cd37a1843cc183955577804f63b3f8dbd111bf10`; remediation began from `5e5399d4e21647f51ed9997703aa74ca24f29608`.

The latest verified code checkpoint is **`9937441ad18bdf993967e78ff36608aa75096596`**. One hundred thirty-one small signed source commits preserve the completed batches, including one hundred two from the resumed work. Author and committer identities/dates match, subjects are single semantic lines, and each commit changes at most 500 lines. No push or pull request was performed. The remaining numbered remediation is committed in the same shared checkout.

This remains a trusted local workflow engine. Describe path-related findings as path handling, staging and result correctness under the calling user's permissions. Preserve explicitly configured local inputs and hostPath behavior. Keep all task and package execution in containers; Conda prepares images. Default to Sol workers with narrow file ownership. Use Astra only when explicitly requested or the task is especially hard.

## Status

| Finding state | Count | Meaning |
| --- | ---: | --- |
| Implemented | 125 | Reported behavior corrected or explicitly refused at its unsupported boundary, with passing checks. External qualification remains separate. |
| Partial | 0 | No numbered finding retains silent incorrect behavior at its supported boundary. |
| Open | 0 | No numbered finding lacks its corrective patch or explicit supported boundary. |
| Total | 125 | Every numbered finding in the source review appears below. |

The status register is authoritative for the latest committed checkpoint. Implemented includes explicit rejection where the source review allowed it; it does not claim full native feature parity. Prepared changes do not count as verified fixes. The original report describes the reviewed baseline, so its line references and initial verdict refer to that revision. The project is not yet qualified for broad unattended production use.

## Verification

Final remaining-work verification covers **1,657 primary tests across 192 workspace targets, zero failures, and 77 ignored external cases** under `--workspace --all-targets --all-features`. One nested snapshot-permission child summary is excluded. All **six real rootful Docker Boa helper cases pass**, including native parser/builtin/allocation bounds, oversized/deep results, cancellation, ordinary semantics, default-driver cancellation and Tokio evaluation/drop. Three Docker OOM events were observed during the earlier five-case resource qualification. Pinned formatting, full workspace Clippy and the loader-alias packaging regression pass. The final binary/helper image identities are sealed. No hosted CI, release publication, live TES/Kubernetes/SLURM/object-store or rootless qualification was performed.

Seventeen new signed source/test commits complete shared stale-entry repair, private complete directory hydration and retention, owned local temporaries, digest-checked cache/staging, read-only previews, bounded order-preserving Nextflow requests, explicit language option boundaries and mandatory TES/Kubernetes library admission. Boa stays native and uses the existing binary in a helper container: effective cgroup memory/swap proof, 20-second operation budget, 16 MiB framing, depth 64 and a separate removal grace of up to ten seconds. Helper operations and drop work inside Tokio callers without nested runtimes. Unsupported active directives, conflicting in-place Directory views and destructive remote GC remain explicitly refused.

The fifth-wave affected checks verify **618 primary tests across 65 targets, zero unresolved failures, and 13 ignored external cases**. Final CWL and Snakemake package reruns replace earlier failed targets; one nested snapshot-permission child summary is excluded. Full pinned workspace formatting and Clippy pass, with final focused Snakemake Clippy after fixture correction. Test/development debug symbols were disabled; runtime assertions remained enabled. This continuation did not run real backend/Docker, hosted CI, browser or artifact qualification.

Fifth-wave fixes cover provider-specific immutable download/version bundles, source-handle version association, directory manifest retention, owned unique transfer/staging temporaries, lease-loss cancellation and late-success rejection, deferred CWL writers and held aliases, Nextflow sorting callbacks/policies, local SLURM recovery admission, and evaluated Snakemake signatures/local protection. Durable incomplete markers prevent partial output delivery from being reused under prior signatures. Failed attempts and intermediate compile/format/Clippy errors remain preserved; latest passing checks supersede them.

The third-wave full run covered **1,537 primary tests across 180 Cargo targets**, with two fixture-only failures corrected and passed in focused reruns. The fourth-wave affected-package checks cover **903 primary tests across 109 targets, zero unresolved failures, and 38 ignored external cases**. Two SLURM fixtures were corrected for real newline output and the intentional independent text termination query. A local runner fixture now waits at most one second for observed maintenance release; 20 isolated repeats already passed, and the complete runner target passed after that correction. A source-backed inherited-descriptor timing explanation remains an inference, not a demonstrated production leak. Nested umask child results are excluded from primary counts.

The **full pinned workspace formatting and Clippy gates pass** without lint suppressions. All 28 doctest targets completed successfully, with no embedded doctest examples. The exact Conda corpus gate remains 2,053 files, 2,032 accepted and all 21 recorded path/message rejections. Seventeen earlier selected real rootful Docker cases and two third-wave cases passed, including ordered service execution and installed transitive package constraints. The pinned UV offline wheel probe disproved the suspected extras-constraint issue, so no speculative extras rewrite was made.

One fourth-wave linker attempt ended with SIGBUS while the filesystem was nearly full; its log remains preserved. Reclaiming only debug symbols from completed task-generated executables restored capacity, then the affected checks completed. This interruption is separate from code failures. Rootless Docker/Podman, live TES/Kubernetes/SLURM/object-store, browser, multi-host filesystem and rollout remain unqualified. Hosted CI and candidate artifact workflows have not run.

Clippy also records a future Rust incompatibility in transitive `attribute-derive-macro 0.10.5`: a trailing semicolon in an expression-position macro. Current compilation passes; retain the pinned toolchain and qualify an upstream upgrade before widening compiler support. The exact compiler report is preserved.

## Evidence

Local receipts, exact staged/reviewed patches, source hashes, test logs, the full JSON finding ledger, corpus baseline and benchmark sources are preserved in [production-readiness-evidence](production-readiness-evidence/). This directory is excluded through the repository's resolved local `info/exclude`; the handoff and review are committed. Verified checkpoint source changes and tests are committed. The working tree was clean at this checkpoint; later continuation packets must be verified separately.

Key receipts include `resumed-wave-3-commits.json`, `resumed-wave-4-commits.json`, `resumed-wave-3-tests-v5.log`, `resumed-wave-3-focused-v6.log`, `resumed-storage-wave-4-v1.log`, `resumed-languages-slurm-wave-4-v1.log`, `resumed-cache-public-wave-4-v2.log`, `resumed-slurm-runner-wave-4-v3.log`, `resumed-clippy-wave-4-final-v2.log`, `resumed-format-wave-4-final-v3.log`, `resumed-doctests-wave-3.log`, `resumed-wave-4-source-v3.json`, `resumed-wave-4-verification.json`, `future-incompatibility-report.json`, `ledger.json`, and each batch's exact reviewed patches. Older receipts and the exact corpus baseline remain preserved. Older `/tmp/uwe-production-readiness` scratch paths did not survive this provider handoff; the excluded evidence directory remains authoritative.

Fifth-wave receipts include `continued-wave-5-final-source-v4.json`, `continued-wave-5-verification.json`, `continued-wave-5-commits.json`, all fifteen exact staged/reviewed patches, `continued-wave-5-final-tests-v2.log`, `continued-snk-cwl-wave-5-final.log`, `continued-wave-5-format-v3.log`, `continued-wave-5-clippy-v2.log`, and `continued-snk-wave-5-final-clippy.log`. The final Nextflow fixture formatting-only delta is recorded in the source receipt.

Final receipts include `remaining-final-source-v2.json`, `remaining-final-verification.json`, `remaining-language-commits.json`, `remaining-final-commits.json`, exact staged/reviewed patches, `remaining-full-workspace-v2.log`, `remaining-cwl-real-helper-v4.log`, `remaining-all-clippy-v5.log`, `remaining-all-format-v3.log`, `remaining-final-helper-identity.json`, `remaining-helper-v3-oom-events.jsonl` and `remaining-helper-packaging-tests.log`. Failed intermediate attempts and the loader-incomplete image archive remain preserved. Final helper image: `sha256:85dfedbe7e172135a1890614269ef358e1ae82db4578db0f9e30dd411de0c081`.

Generated test executables alone had debug symbols removed when disk space became tight. The path/size receipts are in `generated-test-binaries.json` and `generated-completed-test-binaries-wave-4.json`; source, test logs and pre-existing artifacts were preserved. Later Cargo builds can regenerate those symbols. Do not remove unrelated build caches or existing worktrees.

## Remaining production capabilities and qualification

1. Qualify real TES/Kubernetes/SLURM/object-store, rootless and shared-filesystem recovery against this revision. Library admission is implemented; foreign, changed, ambiguous and insufficient-identity work stays retained. Host boot changes and ambiguous accepted responses require operator reconciliation.
2. Provide genuine distributed fencing before enabling destructive remote GC. Conditional leases and caller loss handling are implemented; already-submitted remote mutations are not atomically fenced. Memory/content-derived ETags retain their documented identical-byte replacement ambiguity.
3. Connect immutable Conda context preparation to container build, target capability discovery, transport and CLI reuse. Supply complete pinned corpus bundles and run hosted CI/artifact workflows.
4. Implement native behavior for currently refused Nextflow/Snakemake directives and in-place Directory combinations only through separately scoped feature work. Current refusal is the corrective support contract. Native hash/deep sorting remains unsupported.
5. Qualify helper images/artifacts across advertised architectures and rootless engines, independent provenance/signatures, backup restoration, historical schemas and rollout. Candidate build/image preparation is tracked; no release was published.
6. Measure input-scale factory/path/aggregate retention, trace export, graph history, run identity and persistent operator scratch. The runtime request bound is 128; it does not claim constant memory for inherently retained input data.

## Finding register

Implemented entries show the relevant correction and any explicit support boundary; use the original review for the causal trace. Production capability and qualification gaps remain separate from the numbered defect register.

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
| REM-05 | Implemented | 062aa6c5, 9937441a add mandatory TES/Kubernetes admission and per-attempt locks; matching same-process orphans recover. Prior SLURM recovery remains. Foreign/ambiguous records stay retained; live qualification separate. |
| REM-06 | Implemented | 93b042d3 |
| REM-07 | Implemented | b736670d |
| REM-08 | Implemented | f47fa00b, 344d3ba2 |
| REM-09 | Implemented | b736670d |
| CAS-01 | Implemented | f0388fbb, 6b737545, cad90602 |
| CAS-02 | Implemented | 3286d842, 88714b4d preserve verified internal links and empty directories in complete private hydration. Malformed/corrupt trees fail explicitly. |
| CAS-03 | Implemented | Local cleanup/admission and inode authority prevent stale-plan deletion. Destructive cleanup with expiring remote authority is explicitly refused; enabling genuine distributed fencing is separate capability work. |
| CAS-04 | Implemented | Conditional takeover/renewal/release and 9f8b4857, 420a54f9 reject lost ownership and late success. No claim of atomic fencing for already-submitted remote mutations. |
| CAS-05 | Implemented | 4dadf581, 7961ac5a, 6c4910b3 |
| CAS-06 | Implemented | 16489cd4 |
| CAS-07 | Implemented | 2e68446c, 2c7abf20 |
| CAS-08 | Implemented | 3286d842, fd1d9833 publish complete private snapshots and integrate stored URL/local callers, read-only previews and retention. Child-created partial roots are not accepted. |
| CAS-09 | Implemented | c173e5ad completes owned local publication temporaries through queued operations, preserving permissions and copy behavior. Earlier owned storage/staging transfers remain verified. |
| CAS-10 | Implemented | f7b1f6fb, 51e185f2 condition shared stale invalidation on observed versions, preserve changed successors and separate local eviction. Unsupported non-CAS recovery fails explicitly; content/ETag identity limits documented. |
| CAS-11 | Implemented | d430989a |
| CAS-12 | Implemented | d430989a |
| CAS-13 | Implemented | d430989a |
| CAS-14 | Implemented | f0388fbb, 6b737545, cad90602 |
| CAS-15 | Implemented | 686a9545 |
| CAS-16 | Implemented | 962a5bfc, 6c4910b3; provider-aware immutable bundles pair source version/bytes. Legacy address-only caches are retained but ignored. |
| CAS-17 | Implemented | fd1d9833 safely misses malformed entries, verifies output/CAS/log bytes and rejects corrupted staging before execution. Damaged immutable trees/snapshots return explicit integrity errors; backend failures remain errors. |
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
| NFR-08 | Implemented | 7d36a77d lazily materializes source messages, releases stopped readers, preserves order and expands each incrementally under 128 outstanding requests. Source/path/necessary aggregate retention remains input-scale. |
| NFR-09 | Implemented | 5c3fc8df; key/comparator closures and natural/arrival policies preserved. Native hash/deep modes explicitly unsupported. |
| NFR-10 | Implemented | d914d987, d96a4966, a1ee1791 |
| NFR-11 | Implemented | 7d36a77d explicitly rejects active unsupported beforeScript/afterScript/fair/maxErrors and subscribe error callbacks; inactive defaults retained. Supported sorting remains verified; hash/deep explicitly refused. |
| SNK-01 | Implemented | 238b3a82 |
| SNK-02 | Implemented | fdd2e643 |
| SNK-03 | Implemented | 8b1db743 |
| SNK-04 | Implemented | 8b1db743 |
| SNK-05 | Implemented | 8b1db743 |
| SNK-06 | Implemented | 4f4dee76 |
| SNK-07 | Implemented | c4aee6fe; real FIFO execution explicitly refused before task publication; full concurrency remains unsupported. |
| SNK-08 | Implemented | fdd2e643, 4f4dee76 |
| SNK-09 | Implemented | Prior evaluated signatures/protection/incomplete delivery correction plus 4ed2c0dc refuse active unsupported lifecycle/resource-scopes/shadow/cache/handover options. Legacy no-record skip and uncomparable values remain explicit contracts. |
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
| CWL-05 | Implemented | 89c21080, efa57018, 2c9df699, d7f93ec0, dc6b1634 isolate native Boa with verified container memory/swap, whole-operation deadline, bounded framing, engine cancellation and Tokio-safe operations/drop. Six real helper cases pass. |
| CWL-06 | Implemented | 08d96ca8 |
| CWL-07 | Implemented | d8fab178 |
| CWL-08 | Implemented | Prior deferred writer/alias correction plus b22525f7 explicitly refuse conflicting held/synthesized parent Directory views and in-place Directory updates. Unrelated directories/completed readers remain supported; unresolved alias scans fail clearly. |
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
| Remote ownership | TES, Kubernetes and SLURM persist ownership before submit, reconcile before library admission and retain accepted identity. Per-attempt locks distinguish live work from dropped futures. Independent terminal confirmation gates cleanup; unknown/foreign/unconfirmed work retains journals and scratch. |
| Cleanup synchronization | Local maintenance/admission and inode checks cover preparation, cache use and destructive removal. Remote lease operations are conditional; expiring authority cannot authorize destructive GC. Caller loss handling is implemented; enabling genuine atomic distributed fencing remains separate capability work. |
| Input snapshots | Local task bytes are copied before hashing and execution. Complete private directory snapshots preserve internal links, effective executable mode and empty directories, with manifest retention. Mode::Look uses a non-caching view; InPlace keeps explicit mutable semantics. |
| Package cancellation | Additive cancellable start/run/exchange APIs and CLI tokens cover planning. Engine phases default to 600 seconds each, configurable execution/exchange budgets; process stop is bounded to 20 seconds and Python shutdown to 30. Late accepted-ID cleanup requires the coordinator runtime to remain alive; ambiguous transport acceptance remains a recovery limit. |
| Python pins | Complete workflow constraints feed every UV preparation/install and the helper cache identity. Already-installed constrained distributions are validated before returning imports. Custom images need packaging or pip vendored packaging. |
| Conda compatibility | Authoritative virtual capabilities, verified fetch callbacks and immutable base/platform/recipe identity/context APIs are implemented. Container build/transport/CLI integration and target capability discovery remain; never infer capabilities from the driver host. |

The journal gate in `uwe/src/clean.rs` remains inside the maintenance guard and must be preserved. It protects unresolved jobs whose ordinary run leases have ended. For remote recovery, cancel/status calls are individually bounded within the total grace and cancellation is sent promptly.

## Missing production capabilities

| Area | What remains |
| --- | --- |
| Support contract | README and provisional language/backend support matrix are implemented. Release versioning and qualification remain; plain-script workflow semantics remain undefined. |
| Automated CI | Pinned source, explicit corpus/Docker qualification and candidate artifact workflows are tracked. Local format/Clippy gates pass. Hosted execution, complete bundle availability and required branch checks remain. |
| Backend qualification | Run real Docker/rootless Podman, Funnel/TES, Kubernetes and SLURM cases for cancellation, restart, UID ownership, multi-image limits, GPU placement, timeouts and shared paths. |
| Other remote recovery | All three backends have mandatory library recovery admission. Live service/shared-filesystem qualification, changed-host recovery and ambiguous accepted responses remain separate gates. |
| Portable remote data | Complete registered directory hydration is implemented. Shared driver paths remain a remote task deployment prerequisite; removing it needs separately scoped transport work and provider qualification. |
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
| Tree retention closure | Committed closure traversal retains descendants of imported/no-cache/recent manifests and completed directory snapshots. Bare directory URL associations retain manifests through descriptors. |
| Best-effort leases | Remote lease mutation is conditional and old owners cannot delete successors. Runner loss monitoring is implemented; atomic distributed fencing remains an explicit gap. |
| Stable identity | Nextflow task identity still depends on arrival order. CLI run IDs use time/PID only. Qualify resume/replay under reordered dynamic inputs and shared hosts. |
| Native evaluation | CWL parsing/builtins/heap are isolated under verified helper limits. Python native helpers and other input-scale controller work still need their separately scoped work/output limits and qualification. |
| os.walk | Generator expressions, live lists/custom iterators and supported generator functions now suspend correctly. os.walk link/topdown/pruning and unsupported function-suspension forms remain. |
| Schema evolution | Test old-run export/resume/replay and old-cache compatibility across the supported version window. |
| Recorder transactions | Add action savepoints/rollback and fault injection for partial action, disk-full, kill and commit failure. |
| Rootless and platform behavior | Qualify mixed UID ownership and exact selected-image execution on each advertised backend, including non-native platforms. |

The isolated Pending comparison uses mock task/outcome payloads and the actual frozen bookkeeping sources, with Rust `1.97.1`, identical `-O` builds and three alternating trials. A native C `wait4` observer avoids the Python pre-exec RSS floor in earlier receipts. At 30,000 items across Finished/AsAsked/Reversed, median wall time fell from 3.617 to 0.084 seconds and CPU from 3.584 to 0.083 seconds; peak measured RSS rose from 5,032 to 10,684 KiB. This measures bookkeeping, rather than complete workflow throughput. Compiler, sources, binaries, observer and per-phase output are sealed in `pending-indexed-source-manifest.json` and `pending-indexed-native-results.json`. Exact shuffled ordinal selection remains linear.

Denied validation retries execute freshly and lineage resume respects the same policy. The raw executor cache still publishes before language-level validation, and first-writer entries are not repaired by a successful forced retry. Preserve this distinction when deciding the documented policy for nondeterministic/network-dependent results.

## Code organization and documentation remaining

Reported accepted-option and cache eviction/invalidation defects are corrected with explicit support boundaries and regressions. Broader source-span diagnostics, current-location versus historical-digest policy, oracle identities and organization remain separately scoped future work; they do not authorize cleanup in these patches.

Large files remain a reviewability concern. Any restructuring should have a concrete behavioral reason and its own scoped patch. Workspace dependency centralization and existing crate responsibilities remain useful. Update the stale roadmap and documentation guarantees after the corresponding behavior is tested; historical receipts should retain their original dates and limits.

## Continue from this branch

1. Read this handoff, `report.md`, `ledger.json`, and the current Git diff/status. Keep the shared branch and preserve user changes made after this checkpoint.
2. Reconfirm the code revision and local tools. Restore evidence paths from this repository if `/tmp` was cleared. Use Sol implementation workers with exclusive directories; parent owns Git, root manifests, shared CLI contracts and checks.
3. Numbered remediation is complete within the documented supported boundaries. Start new capability or qualification work from its concrete requirements; do not silently widen support claims or implement historical suggestions without checking their current cause.
4. Before edits, state the narrow cause, minimum correction and exact files. Produce coherent batches of at most 450 changed lines so final signed commits stay below 500.
5. Freeze each batch, format only touched files, review the exact diff, and run meaningful bounded checks. Guarded Cargo needs approved host access to the systemd user scope; preserve the guard and signing hooks.
6. Update each finding with a commit and exact validation, and retain unresolved policy or external qualification explicitly. Do not mark production readiness from local unit results alone.

Useful checks:

```sh
git switch fix/production-readiness
git status --short
cargo test --offline --locked --workspace --all-targets --no-fail-fast -j 2
cargo fmt --all -- --check
cargo clippy --offline --locked --workspace --all-targets --all-features -j 2 -- -D warnings
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

The 131 verified source commits remain on the separate branch. All numbered findings now have corrective patches or explicit supported boundaries. Future capability/qualification work remains in the same shared checkout, with Sol workers by default and parent-owned checks/signing.

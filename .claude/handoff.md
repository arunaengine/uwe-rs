# Iunctus remediation handoff

Work resumed at the user's request on 2026-10-03. Continue in `/home/beavis/projects/iunctus` on **`fix/production-readiness`**, using the same shared checkout. The original review is [report.md](report.md), covering source `cd37a1843cc183955577804f63b3f8dbd111bf10`; remediation began from `5e5399d4e21647f51ed9997703aa74ca24f29608`.

The latest verified code checkpoint is **`37c940856bf8733fd6999fa89cca402e9e708683`**. Sixty-nine small signed code commits preserve the completed batches, including forty from the resumed work. Author and committer identities/dates match, subjects are single semantic lines, and each commit changes at most 500 lines. No push or pull request was performed. Two resumed waves are committed; continue the remaining work in the same shared checkout.

This remains a trusted local workflow engine. Describe path-related findings as path handling, staging and result correctness under the calling user's permissions. Preserve explicitly configured local inputs and hostPath behavior. Keep all task and package execution in containers; Conda prepares images. Implementation should continue primarily with Sol workers and narrow file ownership.

## Status

| Finding state | Count | Meaning |
| --- | ---: | --- |
| Implemented | 91 | Committed correction with passing local checks. Real backend qualification remains a separate gate. |
| Partial | 8 | Some required paths are corrected; specific caller/backend work remains. |
| Open | 26 | No complete corrective patch is committed. |
| Total | 125 | Every numbered finding in the source review appears below. |

The status register is authoritative for the latest committed checkpoint. Prepared changes do not count as verified fixes. The original report describes the reviewed baseline, so its line references and initial verdict should be interpreted at that revision. The project is not yet qualified for broad unattended production use.

## Verification

The final second-wave check verifies **1,487 primary tests, zero unresolved failures, and 69 ignored cases across 173 Cargo targets**. The full v6 run had 1,486 passes and one cancellation-fixture ordering failure. That fixture was corrected to wait for observed task-lease ownership, then passed the focused v7 rerun. Only that test file changed after the full run. A child-process umask regression also passed and is excluded from the primary count. Source hashes, full logs, focused rerun and interpretation are in `resumed-wave-2-verification.json`. The exact Conda corpus gate passed for 2,053 files, 2,032 accepted, and the 21 recorded path/message rejections.

Earlier attempts exposed test-fixture and assertion errors, NaN alignment and mixed CSV handling regressions, plain host-file subpath handling, and cancellation/platform integration gaps. Those were corrected before the final pass. One linker attempt ended with SIGBUS; the retained logs distinguish infrastructure interruption from code checks.

Formatting used `nightly-2026-09-14` on changed Rust files with `skip_children=true`. No full Clippy or doctest pass was performed after remediation. **Seventeen selected real rootful Docker cases passed** against the final frozen source, including helper cancellation/removal, missing-image inspection without a registry pull, and Argo reading a private input as UID 0 and UID 65534 while preserving the original file mode. Rootless Docker/Podman, Funnel, Kubernetes, SLURM, live object-store, browser, multi-host filesystem and production rollout remain unqualified. Ignored cases remain explicit external or corpus gates.

The cached Conda corpus was separately read with the freshly built native parser: **2,053 files, 2,032 accepted, 21 rejected**. Exact relative paths and messages are preserved in `conda-corpus-baseline.json` for QLT-02. The rejected set should be checked by identity, not just its count.

## Evidence

Local receipts, exact staged/reviewed patches, source hashes, test logs, the full JSON finding ledger, corpus baseline and benchmark sources are preserved in [production-readiness-evidence](production-readiness-evidence/). This directory is excluded through the repository's resolved local `info/exclude`; the handoff and review are committed. Verified checkpoint source changes and tests are committed.

Key resumed receipts are `resumed-wave-1-commits.json`, `resumed-wave-2-commits.json`, `resumed-wave-2-tests-v6.log`, `resumed-wave-2-source-v7.json`, `resumed-wave-2-verification.json`, `resumed-runner-cancel-v7.log`, `resumed-docker-final.log`, `resumed-conda-corpus.log`, `ledger.json`, and the batch receipts. Earlier wave receipts remain preserved. Original scratch copies also remain under `/tmp/iunctus-production-readiness`; use the preserved repository copy after a restart.

Generated test executables alone had debug symbols removed when disk space became tight. The path/size receipts are in `generated-test-binaries.json`; source, test logs and pre-existing artifacts were preserved. Later Cargo builds can regenerate those symbols. Do not remove unrelated build caches or existing worktrees.

## Highest priority remaining work

1. Complete tree completeness/hydration, conditional lease mutation, corruption recovery and cleanup synchronization. Persistent copied input bytes and internal directory links are implemented; remote hydration remains.
2. Connect verified engine lifecycle handling to Nextflow onError/onComplete handlers, and complete supported typed writes and lazy generator expressions.
3. Complete SLURM termination ownership and scheduler deadlines. TES and Kubernetes journals/recovery are implemented; foreign/ambiguous records require an explicit operator disposition.
4. Finish lazy/bounded emission, generator-expression and iterator timing, service/pipe behavior and group retry, in-place writes, sorting/directives, real Argo parallelism and virtual-machine work limits.
5. Correct run liveness, shared-filesystem detection, public URL redaction, SSE lifecycle and telemetry deadlines; add CI/distribution gates and finish container-only Conda image preparation.

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
| LOC-11 | Open | Apply all workflow constraints during UV preparation and later imports, and validate already-installed transitive distribution versions. |
| CON-01 | Implemented | ca64f2ec, a227fb9f |
| CON-02 | Implemented | ca64f2ec |
| REM-01 | Implemented | b736670d |
| REM-02 | Implemented | 32f34f46 |
| REM-03 | Implemented | b736670d |
| REM-04 | Implemented | 32f34f46 |
| REM-05 | Partial | TES and Kubernetes durable ownership/confirmed cleanup implemented; SLURM ownership remains. |
| REM-06 | Implemented | 93b042d3 |
| REM-07 | Implemented | b736670d |
| REM-08 | Open | Bound and cancel sbatch, squeue, sacct and scancel processes; avoid unbounded shared polling-lock waits. |
| REM-09 | Implemented | b736670d |
| CAS-01 | Implemented | f0388fbb, 6b737545, cad90602 |
| CAS-02 | Partial | Internal directory links retained locally; remote directory URL hydration remains CAS-08. |
| CAS-03 | Partial | Local maintenance/admission and fresh deletion synchronized; remote version ownership and expiry remain CAS-04. |
| CAS-04 | Open | Bind remote lease reads, renewal, takeover and deletion to the same version/token; handle lost or expired ownership. Serialize memory-store tests separately. |
| CAS-05 | Open | Resume only against a stable remote version with validated ranges, or restart downloads using unique temporary files. |
| CAS-06 | Implemented | 16489cd4 |
| CAS-07 | Open | Validate directory manifest contents and every descendant before accepting a cache hit. |
| CAS-08 | Open | Hydrate complete trees privately from manifests, then publish; add directory location descriptors and retain empty directories. |
| CAS-09 | Open | Use unique operation temporary names or per-target serialization for downloads, staging and folder-provider uploads. |
| CAS-10 | Open | Separate front eviction from shared stale-entry replacement; condition mutation on the stale version without deleting a concurrent repair. |
| CAS-11 | Implemented | d430989a |
| CAS-12 | Implemented | d430989a |
| CAS-13 | Implemented | d430989a |
| CAS-14 | Implemented | f0388fbb, 6b737545, cad90602 |
| CAS-15 | Open | After locking a cleanup descriptor, verify it still names the current lease inode before unlinking. |
| CAS-16 | Open | Namespace completed downloads by stable provider identity plus address, excluding secrets and defining legacy-cache migration. |
| CAS-17 | Open | Define corruption recovery for entries, output bytes and logs; quarantine or safely miss damaged cache data instead of silent reuse. |
| NFL-01 | Partial | Typed call/default/return boundaries preserved; local declarations, parameter reassignment and captured writes remain. |
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
| NFR-04 | Open | Allocate implicit collectFile and split outputs in unique run scratch while preserving explicit user destinations. |
| NFR-05 | Implemented | aa3e4ce7 |
| NFR-06 | Implemented | 75c50272 |
| NFR-07 | Implemented | 1400d141, 75c50272, fb1b3e64, 2d12a437 |
| NFR-08 | Partial | Stopped cursors reclaimed; factory/process request backpressure remains. |
| NFR-09 | Open | Preserve comparator/key closures and named sorting modes instead of collapsing them to natural sort. |
| NFR-10 | Partial | Engine failure hook invoked after cleanup; Nextflow onError/onComplete and terminal metadata remain. |
| NFR-11 | Open | Implement beforeScript/afterScript/maxErrors/fair and subscribe error callbacks; preserve collectFile sorting contracts or explicitly describe unsupported modes. |
| SNK-01 | Implemented | 238b3a82 |
| SNK-02 | Implemented | fdd2e643 |
| SNK-03 | Implemented | 8b1db743 |
| SNK-04 | Implemented | 8b1db743 |
| SNK-05 | Implemented | 8b1db743 |
| SNK-06 | Open | Schedule service consumers with internal dependencies and retain the service through their ordered execution. |
| SNK-07 | Open | Provide actual FIFO and concurrent producer/consumer behavior, or emit a precise unsupported error before launching a blocking producer. |
| SNK-08 | Partial | Single-job validation retries implemented; service-group retries remain. |
| SNK-09 | Open | Persist successful evaluated job signatures for command/params/image/input-set reruns; implement protection and enforce or describe accepted lifecycle/resource directives. |
| PY-01 | Implemented | ecde037f |
| PY-02 | Implemented | ecde037f |
| PY-03 | Implemented | ecde037f |
| PY-04 | Implemented | 98219fec |
| PY-05 | Implemented | 98219fec |
| PY-06 | Implemented | 98219fec |
| PY-07 | Implemented | a8a39d2a |
| PY-08 | Implemented | 98219fec |
| PY-09 | Partial | Generator functions suspend and consumers reject reentry; generator expressions and custom/list iterator timing remain. |
| PY-10 | Implemented | a8a39d2a, 6fc692f5 |
| PY-11 | Implemented | ecde037f |
| PY-12 | Implemented | 98219fec |
| PY-13 | Implemented | a8a39d2a |
| CWL-01 | Implemented | 4bfeba6e, 854a749c, 88a0ae5b, dae77512 |
| CWL-02 | Implemented | d8fab178 |
| CWL-03 | Implemented | d8fab178 |
| CWL-04 | Implemented | 08d96ca8 |
| CWL-05 | Open | Use Boa async VM work slices with a total budget and deadline; parsing, native builtin and heap bounds remain separately unresolved. |
| CWL-06 | Implemented | 08d96ca8 |
| CWL-07 | Implemented | d8fab178 |
| CWL-08 | Open | Defer raw jobs around in-place writers, refresh inputs after updates, and serialize conflicting access including scatter/nested workflows. |
| CWL-09 | Implemented | 08d96ca8 |
| CWL-10 | Implemented | 4bfeba6e, 854a749c, 88a0ae5b, dae77512 |
| ARG-01 | Implemented | acacacfa |
| ARG-02 | Implemented | acacacfa |
| ARG-03 | Implemented | 734ffe3f |
| ARG-04 | Open | Defer unadmitted leaves at workflow/template limits; count active descendants and qualify child-workflow and hook behavior. |
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
| RUN-04 | Open | Add host/boot/process-start identity or durable liveness; conservatively handle foreign/legacy rows and test PID reuse. |
| RUN-05 | Open | Detect journal mode after parent creation or inspect the closest existing ancestor on shared filesystems. |
| RUN-06 | Implemented | c7fd15bd |
| RUN-07 | Implemented | 4cda193a |
| RUN-08 | Implemented | 96c6b18c, 03644331, 37c94085 |
| RUN-09 | Open | Redact credential-bearing URLs in public details, reports and crates while retaining private replay credentials. |
| WEB-01 | Open | Drain complete SSE backlogs before exposing terminal state or refresh the full view; coordinate browser closing behavior. |
| WEB-02 | Open | Propagate shutdown into active SSE or bound server grace so open clients cannot prevent termination. |
| TEL-01 | Open | Add a total send deadline and bounded response consumption, preserving workflow completion when collectors misbehave. |
| QLT-01 | Implemented | ea64b3b0 |
| QLT-02 | Implemented | 347051ee |
| QLT-03 | Implemented | ea64b3b0 |
| QLT-04 | Implemented | 865840ec |


## Shared contracts and integration still required

| Contract | Current state and next integration |
| --- | --- |
| Cache policy | `Reuse::Disabled` means no cache reads or writes. `Denied` retains the existing forced-refresh/publication behavior. Nextflow `cache false` uses Disabled. |
| Failure hooks | Engine invokes `Driver::failed` once after terminal cleanup, preserves the cause and exposes hook errors in CLI diagnostics. Nextflow onError/onComplete integration remains. |
| Overlay identity | FORMAT2 and canonical serialization are unchanged. Only aliased/ancestor input destinations append ordered domain-separated bytes to the key. Disjoint keys remain compatible; staged overlays use private copies and keep input order. |
| Process inputs | `TaskFrame` and `TaskRequest` retain per-round environment and stdin. Input environment overrides config values; `Task.stdin` is literal text whose bytes the runner hashes. |
| Image platform | Local, TES, Kubernetes and SLURM use matching selected references and metadata. Unsupported GPU/platform combinations fail explicitly. Implicit image inspection no longer pulls a missing image, and that case passed real Docker qualification. |
| Remote ownership | TES and Kubernetes persist `remote.json` before submit and retain accepted identity. Kubernetes requires matching UID plus Job and pod absence before cleanup. Unknown/foreign/unconfirmed work retains journals and scratch; CLI removal is blocked. SLURM remains. |
| Cleanup synchronization | Local maintenance/admission guards cover preparation, cache use, fresh planning/removal and CLI scratch deletion. First run-lease errors propagate and retry. Remote conditional ownership/expiry remains CAS-04; CAS-03 stays partial. |
| Input snapshots | Local task bytes are copied before hashing and execution, and retained under input-snapshots. Internal directory links and effective executable mode are represented consistently. Retention and remote directory hydration remain; InPlace keeps explicit mutable semantics. |
| Package cancellation | Additive cancellable start/run/exchange APIs and CLI tokens cover planning. Engine phases default to 600 seconds each, configurable execution/exchange budgets; process stop is bounded to 20 seconds and Python shutdown to 30. Late accepted-ID cleanup requires the coordinator runtime to remain alive; ambiguous transport acceptance remains a recovery limit. |
| Python pins | Planned `PythonStart.constraints` and `IUNCTUS_PYTHON_CONSTRAINTS` feed UV preparation and later installs. Validate installed distribution pins before returning an already-importable transitive package. |
| Conda compatibility | `lock_with_virtual` checks authoritative versions/builds, distinct from unknown legacy compatibility. Callback-based streaming repodata/package fetches verify publication and SHA-256. Target capability discovery, transport integration, immutable image identity/context/build remain. Do not infer target capabilities from the driver host. |

The journal gate in `iunctus/src/clean.rs` remains inside the maintenance guard and must be preserved. It protects unresolved jobs whose ordinary run leases have ended. For remote recovery, cancel/status calls are individually bounded within the total grace and cancellation is sent promptly.

## Missing production capabilities

| Area | What remains |
| --- | --- |
| Support contract | README and provisional language/backend support matrix are implemented. Release versioning and qualification remain; plain-script workflow semantics remain undefined. |
| Automated CI | Add required pinned formatting, Clippy, unit/integration and corpus jobs; enforce complete inventories and explicit opt-in prerequisites. No tracked CI workflow has been added. |
| Backend qualification | Run real Docker/rootless Podman, Funnel/TES, Kubernetes and SLURM cases for cancellation, restart, UID ownership, multi-image limits, GPU placement, timeouts and shared paths. |
| Other remote recovery | Kubernetes and TES persist accepted identity and conservatively recover unresolved work. SLURM needs durable ownership, ambiguous-submit handling and confirmed cleanup. |
| Portable remote data | Shared driver paths remain a deployment prerequisite. Remote directory fidelity, empty directories, provider listings and staging without shared filesystems need completion. |
| Conda image preparation | Connect the implemented verified callback fetchers to transport; finish immutable base/platform/recipe identity, tar build context through the container API, target capability discovery and final image reuse. No host Conda execution. Pip pins remain incomplete. |
| Package environment reproducibility | Helper cancellation/time bounds are implemented; locked image identity, system-library requirements, transitive version validation and coordinator-crash cleanup remain. |
| Network and cache policy | Decide documented sharing policy for network-dependent results and offline reuse; current behavior was explicitly an unresolved policy, not silently changed here. |
| Historical computation | Define preserved local inputs, environment/provider config and image availability. Existing replay intentionally prefers present inputs; distinguish it from an exact historical repeat. |
| Distribution | README/source install instructions are present; project MIT/Apache license texts, release build/checksum artifacts, supported platforms, upgrade/backup/rollback procedures and operator guidance remain. |
| Retention and recovery | Test backup restoration, record/commit failure, historical schemas and cache payload compatibility, and unresolved remote ownership across coordinator recovery. |
| Dependency assurance | Current advisory/license/artifact checks were not run or added. Compilation does not establish their status. |

## Performance and future behavior still needing evidence

| Risk | Work remaining |
| --- | --- |
| Pending bookkeeping | Ordinary completion/removal are indexed with reference-order tests and source-matched native measurements. CLI defaults to 1024 pending real tasks, preserving overrides. Exact shuffled ordinal selection intentionally remains linear. |
| Eager fanout | Nextflow factories/process rounds, Argo loops and CWL Cartesian scatter allocate broadly. Add lazy/bounded emission without arbitrary rejection of otherwise supported large workflows. |
| Retained graph history | Large runs retain task/event graph state. Measure peak memory and cancellation latency at representative sizes. |
| Large CAS inputs and logs | Stream verified remote staging and bounded visible log excerpts while retaining full stored logs and successful task status. |
| Trace export | All events/spans and cloned request chunks consume run-scale memory. Remove clones and introduce incremental event/export/send APIs where necessary. |
| Simulator work | Add a consistent configurable expansion/work/output budget; preserve ordinary synthetic task behavior. |
| Tree retention closure | Expand every retained manifest's descendant digests, including imported directories and no-cache outputs, not only kept cache-entry outputs. |
| Best-effort leases | Initial run-lease failures now propagate and retry; ordinary blocked preparation cancels promptly. Remote ownership/renewal and complete slow-I/O cancellation remain. |
| Stable identity | Nextflow task identity still depends on arrival order. CLI run IDs use time/PID only. Qualify resume/replay under reordered dynamic inputs and shared hosts. |
| Native evaluation | VM budgets do not automatically bound JavaScript builtins/parse/heap or every Python native helper. Keep exact guarantees and residual limits visible. |
| os.walk | Eager traversal ignores link/topdown/pruning semantics; add bounded behavior and correct native iterator timing. Generator functions now suspend and all native consumers reject reentry; expressions/custom iterators and full walk timing remain. |
| Schema evolution | Test old-run export/resume/replay and old-cache compatibility across the supported version window. |
| Recorder transactions | Add action savepoints/rollback and fault injection for partial action, disk-full, kill and commit failure. |
| Rootless and platform behavior | Qualify mixed UID ownership and exact selected-image execution on each advertised backend, including non-native platforms. |

The isolated Pending comparison uses mock task/outcome payloads and the actual frozen bookkeeping sources, with Rust `1.97.1`, identical `-O` builds and three alternating trials. A native C `wait4` observer avoids the Python pre-exec RSS floor in earlier receipts. At 30,000 items across Finished/AsAsked/Reversed, median wall time fell from 3.617 to 0.084 seconds and CPU from 3.584 to 0.083 seconds; peak measured RSS rose from 5,032 to 10,684 KiB. This measures bookkeeping, rather than complete workflow throughput. Compiler, sources, binaries, observer and per-phase output are sealed in `pending-indexed-source-manifest.json` and `pending-indexed-native-results.json`. Exact shuffled ordinal selection remains linear.

Denied validation retries execute freshly and lineage resume respects the same policy. The raw executor cache still publishes before language-level validation, and first-writer entries are not repaired by a successful forced retry. Preserve this distinction when deciding the documented policy for nondeterministic/network-dependent results.

## Code organization and documentation remaining

Accepted-option enforcement, error propagation and simulator/real mode boundaries still need the open semantic fixes. Cache policy and eviction/invalidation contracts remain duplicated. Temporary publication schemes, current-location versus historical-digest rules, frontend/backend terminal rules, source-span diagnostics and oracle version identities need consistent tests and documentation.

Large files remain a reviewability concern. Any restructuring should have a concrete behavioral reason and its own scoped patch. Workspace dependency centralization and existing crate responsibilities remain useful. Update the stale roadmap and documentation guarantees after the corresponding behavior is tested; historical receipts should retain their original dates and limits.

## Continue from this branch

1. Read this handoff, `report.md`, `ledger.json`, and the current Git diff/status. Keep the shared branch and preserve user changes made after this pause.
2. Reconfirm the code revision and local tools. Restore evidence paths from this repository if `/tmp` was cleared. Use Sol implementation workers with exclusive directories; parent owns Git, root manifests, shared CLI contracts and checks.
3. Start with cache identity/leases/GC and engine lifecycle, then language contracts and the four partial integrations. Confirm false positives from source rather than blindly implementing every historical suggestion.
4. Before edits, state the narrow cause, minimum correction and exact files. Produce coherent batches of at most 450 changed lines so final signed commits stay below 500.
5. Freeze each batch, format only touched files, review the exact diff, and run meaningful bounded checks. Guarded Cargo needs approved host access to the systemd user scope; preserve the guard and signing hooks.
6. Update each finding with a commit and exact validation, and retain unresolved policy or external qualification explicitly. Do not mark production readiness from local unit results alone.

Useful checks:

```sh
git switch fix/production-readiness
git status --short
cargo test --offline --locked --workspace --all-targets --no-fail-fast -j 2
cargo +nightly-2026-09-14 fmt --all -- --check
cargo +nightly-2026-09-14 clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --offline --locked --workspace --doc -j 2
```

Run expensive external/corpus gates only with their documented prerequisites. QLT-02 has an exact preserved 21-file baseline ready to turn into a regression fixture. The next trace, generator, image-build, maintenance and scheduling changes need technically necessary interface coordination; preserve supported behavior rather than silently discarding options.

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

The 69 verified code commits remain on the separate branch. Both resumed waves are committed; proceed with the remaining numbered findings and capability gates in the same shared checkout. Implementation workers pause after each packet until the parent acknowledges a verified commit.

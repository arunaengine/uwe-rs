# Iunctus remediation handoff

Work is paused at the user's request on 2026-10-02. Continue in `/home/beavis/projects/iunctus` on **`fix/production-readiness`**, using the same shared checkout. The original review is [report.md](report.md), covering source `cd37a1843cc183955577804f63b3f8dbd111bf10`; remediation began from `5e5399d4e21647f51ed9997703aa74ca24f29608`.

The verified code revision at this pause is **`c0a8af3d755ec5343a9e5485c4b285802d301e97`**. Twenty-nine small signed code commits preserve the completed batches. Author and committer identities/dates match, subjects are single semantic lines, and each commit changes at most 500 lines. No push or pull request was performed. All implementation workers are stopped, and no further remediation should run until the user resumes it.

This remains a trusted local workflow engine. Describe path-related findings as path handling, staging and result correctness under the calling user's permissions. Preserve explicitly configured local inputs and hostPath behavior. Keep all task and package execution in containers; Conda prepares images. Implementation should continue primarily with Sol workers and narrow file ownership.

## Status

| Finding state | Count | Meaning |
| --- | ---: | --- |
| Implemented | 60 | Committed correction with passing local checks. Real backend qualification remains a separate gate. |
| Partial | 4 | Some required paths are corrected; specific caller/backend work remains. |
| Open | 61 | No complete corrective patch is committed. |
| Total | 125 | Every numbered finding in the source review appears below. |

The status register is authoritative for this pause. The original report describes the reviewed baseline, so its line references and initial verdict should be interpreted at that revision. The project is not yet qualified for broad unattended production use.

## Verification

The final stable code pass ran `cargo test --offline --locked --workspace --all-targets --no-fail-fast -j 2`: **1,376 passed, 0 failed, 66 ignored across 165 test targets**. Tested source hashes remained unchanged. This includes the new cleanup, metadata, checkpoint, table, configuration, input and remote fake-server regressions.

Earlier attempts exposed test-fixture and assertion errors, NaN alignment and mixed CSV handling regressions, plain host-file subpath handling, and cancellation/platform integration gaps. Those were corrected before the final pass. One linker attempt ended with SIGBUS; the retained logs distinguish infrastructure interruption from code checks.

Formatting used `nightly-2026-09-14` on changed Rust files with `skip_children=true`. No full Clippy or doctest pass was performed after remediation. No real Docker/rootless, Funnel, Kubernetes, SLURM, live object-store, browser, multi-host filesystem, or production rollout qualification was performed. The 66 ignored cases remain intentional external or corpus gates, not passing integration evidence.

The cached Conda corpus was separately read with the freshly built native parser: **2,053 files, 2,032 accepted, 21 rejected**. Exact relative paths and messages are preserved in `conda-corpus-baseline.json` for QLT-02. The rejected set should be checked by identity, not just its count.

## Evidence

Local receipts, exact staged/reviewed patches, source hashes, test logs, the full JSON finding ledger, corpus baseline and benchmark sources are preserved in [production-readiness-evidence](production-readiness-evidence/). This directory is excluded through the repository's resolved local `info/exclude`; the handoff and review are committed. Source changes and their tests are all committed.

Key receipts are `wave-1-commits.json`, `wave-2-commits.json`, `wave-2-tests-v4.log`, `wave-2-source-v4.json`, `ledger.json`, `conda-corpus-baseline.json`, and the batch receipts. Original scratch copies also remain under `/tmp/iunctus-production-readiness`; use the preserved repository copy after a restart.

Generated test executables alone had debug symbols removed when disk space became tight. The path/size receipts are in `generated-test-binaries.json`; source, test logs and pre-existing artifacts were preserved. Later Cargo builds can regenerate those symbols. Do not remove unrelated build caches or existing worktrees.

## Highest priority remaining work

1. Bind cache identity to stable input bytes and correct tree completeness, hydration, lease mutation and cleanup synchronization. Keep unresolved remote inputs alive across failure and restart.
2. Complete engine cancellation, panic outcomes, terminal recording and draining. `Driver::failed` exists but is not invoked; Nextflow failure handlers remain open.
3. Complete Kubernetes/SLURM termination ownership and scheduler deadlines. TES journal recovery is implemented; foreign/ambiguous records require an explicit operator disposition.
4. Finish silent language-contract omissions: join integrity, arity, output failure policy, generators, ensure, service/pipe behavior, in-place writes, real Argo loop/parallelism and parameter aggregation.
5. Correct historical export/liveness/page behavior and make release/corpus/backend gates trustworthy before claiming production readiness.

## Finding register

Implemented entries show the relevant code commits. Partial/open entries state the remaining correction; use the original review for the causal trace and proposed reproducer.

| Finding | State | Remaining work or code evidence |
| --- | --- | --- |
| ENG-01 | Implemented | e8f95fe2 |
| ENG-02 | Implemented | c40c16da |
| ENG-03 | Open | Check cancellation before driver start, each admission, synchronous outcomes and final delivery; add pre-cancelled and mid-batch regressions. |
| ENG-04 | Open | Emit and record one terminal result for each attempt, including exhausted errors and cleanup; keep graph, observer and database states consistent. |
| ENG-05 | Open | Observe spawned future failures and convert a panic or JoinError into a terminal outcome instead of losing its notification. |
| ENG-06 | Open | Suppress replacement attempts after draining begins, preserving the original workflow failure. |
| LOC-01 | Implemented | 73ec27a1 |
| LOC-02 | Implemented | 73ec27a1 |
| LOC-03 | Implemented | 73ec27a1, 16489cd4 |
| LOC-04 | Partial | Created-container cleanup fixed; globally unique Snakemake Python helper names remain. |
| LOC-05 | Implemented | 63919ae0 |
| LOC-06 | Open | Make summed per-image CPU and memory limits match the aggregate pool reservation; handle minimum container memory explicitly. |
| LOC-07 | Open | Compute access for every image group, open shared scratch where needed, and recover caller ownership after mixed-UID rootless jobs. |
| LOC-08 | Partial | Local and TES variant selection fixed; Kubernetes and SLURM still use legacy metadata lookup and need explicit platform alignment. |
| LOC-09 | Implemented | 63919ae0 |
| LOC-10 | Open | Add cancellable bounded helper exchanges and shutdown, then plumb the run token into Snakemake before planning/import evaluation. |
| LOC-11 | Open | Apply all workflow constraints during UV preparation and later imports, and validate already-installed transitive distribution versions. |
| CON-01 | Implemented | ca64f2ec, a227fb9f |
| CON-02 | Implemented | ca64f2ec |
| REM-01 | Implemented | b736670d |
| REM-02 | Implemented | 32f34f46 |
| REM-03 | Implemented | b736670d |
| REM-04 | Implemented | 32f34f46 |
| REM-05 | Partial | TES journals, bounded cancellation/recovery and CLI cleanup gate fixed; Kubernetes/SLURM termination ownership remain. |
| REM-06 | Open | Keep Kubernetes summed GPU allocation consistent with task placement; narrowly refuse unsupported multi-image sharing if necessary. |
| REM-07 | Implemented | b736670d |
| REM-08 | Open | Bound and cancel sbatch, squeue, sacct and scancel processes; avoid unbounded shared polling-lock waits. |
| REM-09 | Implemented | b736670d |
| CAS-01 | Open | Hash stable copied bytes and execute those same bytes; persist snapshots needed by unresolved remote jobs instead of dropping temporary guards. |
| CAS-02 | Open | Hash and preserve directory links consistently, including cycles and selected roots, or explicitly reject unsupported link shapes. |
| CAS-03 | Open | Synchronize run admission and destructive cleanup with a maintenance guard held over fresh snapshots, planning and deletion. |
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
| CAS-14 | Open | Include effective executable mode in identity or normalize staged mode, with unchanged-byte chmod regressions. |
| CAS-15 | Open | After locking a cleanup descriptor, verify it still names the current lease inode before unlinking. |
| CAS-16 | Open | Namespace completed downloads by stable provider identity plus address, excluding secrets and defining legacy-cache migration. |
| CAS-17 | Open | Define corruption recovery for entries, output bytes and logs; quarantine or safely miss damaged cache data instead of silent reuse. |
| NFL-01 | Open | Preserve typed local, parameter, capture and return metadata and use existing supported conversions during assignment and calls. |
| NFL-02 | Open | Populate referenced and scalar path validation lookups; delegate schema reference/branch semantics to the validator. |
| NFL-03 | Implemented | b2da319a |
| NFL-04 | Implemented | b2da319a |
| NFL-05 | Implemented | b2da319a |
| NFL-06 | Implemented | b2da319a |
| NFL-07 | Open | Rank exact numeric matches and valid widening conversions; reject genuinely ambiguous equal-arity overloads. |
| NFL-08 | Open | Reject unsupported extends/implements at load time until inherited resolution and constructors are implemented. |
| NFR-01 | Implemented | db22a564, 47e33f86 |
| NFR-02 | Implemented | 16489cd4, 925bb09b |
| NFR-03 | Open | Honor duplicate/mismatch join options, matched-key history, strict defaults and remainder compatibility. |
| NFR-04 | Open | Allocate implicit collectFile and split outputs in unique run scratch while preserving explicit user destinations. |
| NFR-05 | Open | Inspect STOP across all inputs before waiting for a complete round; cover closed/unbound combinations in both orders. |
| NFR-06 | Open | Parse output cardinality, permit zero minima, reject excess files and return the prescribed singleton shape. |
| NFR-07 | Open | Send output and eval failures through the owning process retry/ignore/keep-going policy. |
| NFR-08 | Open | Unregister stopped channel cursors and bound request emission; factories and process rounds also need backpressure. |
| NFR-09 | Open | Preserve comparator/key closures and named sorting modes instead of collapsing them to natural sort. |
| NFR-10 | Open | Invoke Driver::failed once after cleanup, set workflow terminal metadata and execute onError/onComplete without duplicate handlers. |
| NFR-11 | Open | Implement beforeScript/afterScript/maxErrors/fair and subscribe error callbacks; preserve collectFile sorting contracts or explicitly describe unsupported modes. |
| SNK-01 | Implemented | 238b3a82 |
| SNK-02 | Open | Stream non-empty and requested SHA256/MD5/SHA1 checks before output acceptance and delivery. |
| SNK-03 | Implemented | 8b1db743 |
| SNK-04 | Implemented | 8b1db743 |
| SNK-05 | Implemented | 8b1db743 |
| SNK-06 | Open | Schedule service consumers with internal dependencies and retain the service through their ordered execution. |
| SNK-07 | Open | Provide actual FIFO and concurrent producer/consumer behavior, or emit a precise unsupported error before launching a blocking producer. |
| SNK-08 | Open | Apply retries to output validation and service-group failures, rebuilding attempt resources and avoiding partial delivery. |
| SNK-09 | Open | Persist successful evaluated job signatures for command/params/image/input-set reruns; implement protection and enforce or describe accepted lifecycle/resource directives. |
| PY-01 | Implemented | ecde037f |
| PY-02 | Implemented | ecde037f |
| PY-03 | Implemented | ecde037f |
| PY-04 | Implemented | 98219fec |
| PY-05 | Implemented | 98219fec |
| PY-06 | Implemented | 98219fec |
| PY-07 | Implemented | a8a39d2a |
| PY-08 | Implemented | 98219fec |
| PY-09 | Open | Implement suspended generators and short-circuit iteration across next, for, any/all and generator expressions; no eager tail fallback. |
| PY-10 | Partial | Native subprocess cwd/options corrected; Snakemake CommandRunner must still apply Command.cwd. |
| PY-11 | Implemented | ecde037f |
| PY-12 | Implemented | 98219fec |
| PY-13 | Implemented | a8a39d2a |
| CWL-01 | Open | Resolve output JSON through actual work and staged-input mappings; reject locations inconsistent with the task data view before reading them. |
| CWL-02 | Implemented | d8fab178 |
| CWL-03 | Implemented | d8fab178 |
| CWL-04 | Implemented | 08d96ca8 |
| CWL-05 | Open | Use Boa async VM work slices with a total budget and deadline; parsing, native builtin and heap bounds remain separately unresolved. |
| CWL-06 | Implemented | 08d96ca8 |
| CWL-07 | Implemented | d8fab178 |
| CWL-08 | Open | Defer raw jobs around in-place writers, refresh inputs after updates, and serialize conflicting access including scatter/nested workflows. |
| CWL-09 | Implemented | 08d96ca8 |
| CWL-10 | Open | Use provider listings/materialization for requested directory listings and propagate read/list errors. |
| ARG-01 | Implemented | acacacfa |
| ARG-02 | Implemented | acacacfa |
| ARG-03 | Open | Restrict placeholder/empty/unresolved loop leniency to simulation while preserving genuine empty lists and skipped-node semantics. |
| ARG-04 | Open | Defer unadmitted leaves at workflow/template limits; count active descendants and qualify child-workflow and hook behavior. |
| ARG-05 | Implemented | 2a59262f |
| ARG-06 | Implemented | 2a59262f |
| ARG-07 | Open | Preserve output parameters as strings during aggregate access while keeping result JSON handling separate. |
| ARG-08 | Implemented | acacacfa |
| ARG-09 | Open | Send leading-at-sign HTTP bodies as literal data rather than file references. |
| ARG-10 | Open | Implement or explicitly refuse bodyFrom/timeoutSeconds, capture response headers, and derive retry timing from actual outcomes. |
| ARG-11 | Implemented | 2a59262f |
| SCH-01 | Implemented | c97f37c0 |
| SCH-02 | Implemented | c97f37c0 |
| RUN-01 | Implemented | c7fd15bd |
| RUN-02 | Implemented | 4cda193a |
| RUN-03 | Open | Export recorded input bytes by digest, verify current candidates, and mark unavailable historical content rather than substituting replacements. |
| RUN-04 | Open | Add host/boot/process-start identity or durable liveness; conservatively handle foreign/legacy rows and test PID reuse. |
| RUN-05 | Open | Detect journal mode after parent creation or inspect the closest existing ancestor on shared filesystems. |
| RUN-06 | Implemented | c7fd15bd |
| RUN-07 | Implemented | 4cda193a |
| RUN-08 | Open | Resolve and validate the kept main workflow before requiring the deleted original checkout. |
| RUN-09 | Open | Redact credential-bearing URLs in public details, reports and crates while retaining private replay credentials. |
| WEB-01 | Open | Drain complete SSE backlogs before exposing terminal state or refresh the full view; coordinate browser closing behavior. |
| WEB-02 | Open | Propagate shutdown into active SSE or bound server grace so open clients cannot prevent termination. |
| TEL-01 | Open | Add a total send deadline and bounded response consumption, preserving workflow completion when collectors misbehave. |
| QLT-01 | Implemented | ea64b3b0 |
| QLT-02 | Open | Gate the exact 2053-file inventory and 21 known rejection identities/messages; fail read errors and unexpected parse changes. |
| QLT-03 | Implemented | ea64b3b0 |
| QLT-04 | Open | Reconcile stale roadmap execution/delivery claims and label historical counts separately from current qualification. |


## Shared contracts and integration still required

| Contract | Current state and next integration |
| --- | --- |
| Cache policy | `Reuse::Disabled` means no cache reads or writes. `Denied` retains the existing forced-refresh/publication behavior. Nextflow `cache false` uses Disabled. |
| Failure hooks | `Driver::failed(&ExecuteError)` is a default method only. Invoke once after terminal cleanup; preserve the original failure and expose hook failures separately. Then connect Nextflow onError/onComplete and CLI diagnostics. |
| Overlay identity | FORMAT2 and canonical serialization are unchanged. Only aliased/ancestor input destinations append ordered domain-separated bytes to the key. Disjoint keys remain compatible; staged overlays use private copies and keep input order. |
| Process inputs | `TaskFrame` and `TaskRequest` retain per-round environment and stdin. Input environment overrides config values; `Task.stdin` is literal text whose bytes the runner hashes. |
| Image platform | Local acquisition/commands/creation and TES selected references now agree. `ImageConfigs::resolved_for` returns reference plus config; Kubernetes and SLURM must integrate matching selection or explicit capability refusal. |
| Remote ownership | TES writes/syncs `remote.json` before submit and records accepted ID before polling. Terminal confirmation removes it. Unknown/foreign/unconfirmed work retains its journal and scratch. CLI removal is blocked by these records. |
| Cleanup synchronization | The journal check is implemented; the maintenance lease protocol is not. Proposed `clean::maintenance`, `remove_guarded`, and brief runner admission gate must cover a fresh plan and deletion without dropping run protection. |
| Input snapshots | Snapshot guards cannot disappear while unresolved jobs still refer to their paths. Prefer persistent content-addressed bytes or explicitly retained snapshot ownership. InPlace retains explicit mutable semantics. |
| Package cancellation | Planned additive cancellable start/run helpers should preserve current entrypoints. Plumb the token before planning/imports. Existing `ContainerSpec` need not gain fields solely for this. |
| Python pins | Planned `PythonStart.constraints` and `IUNCTUS_PYTHON_CONSTRAINTS` feed UV preparation and later installs. Validate installed distribution pins before returning an already-importable transitive package. |
| Conda compatibility | Valid negated prefixes now work. An additive `lock_with_virtual` can accept authoritative virtual package versions/builds; no target libc/kernel capabilities or image builder are implemented. Do not infer them from the driver host. |

The journal gate in `iunctus/src/clean.rs` must remain when maintenance synchronization is added. It protects unresolved jobs whose ordinary run leases have ended. For remote recovery, cancel/status calls are individually bounded within the total grace and cancellation is sent promptly.

## Missing production capabilities

| Area | What remains |
| --- | --- |
| Support contract | Publish a versioned language/feature/backend matrix, including implemented, simulated, explicitly unsupported and intentionally different behavior. Plain-script workflow semantics remain undefined. |
| Automated CI | Add required pinned formatting, Clippy, unit/integration and corpus jobs; enforce complete inventories and explicit opt-in prerequisites. No tracked CI workflow has been added. |
| Backend qualification | Run real Docker/rootless Podman, Funnel/TES, Kubernetes and SLURM cases for cancellation, restart, UID ownership, multi-image limits, GPU placement, timeouts and shared paths. |
| Other remote recovery | Kubernetes/SLURM need durable accepted-ID ownership, ambiguous-submit handling and confirmed cleanup. TES recovery conservatively blocks unknown or changed-host records. |
| Portable remote data | Shared driver paths remain a deployment prerequisite. Remote directory fidelity, empty directories, provider listings and staging without shared filesystems need completion. |
| Conda image preparation | Implement repodata/package fetching, immutable base/platform/recipe identity, tar build context through the container API, verified virtual compatibility and final image reuse. No host Conda execution. Pip pins remain incomplete. |
| Package environment reproducibility | Locked/helper image identity, system-library requirements, transitive version validation and bounded helper lifecycle remain. |
| Network and cache policy | Decide documented sharing policy for network-dependent results and offline reuse; current behavior was explicitly an unresolved policy, not silently changed here. |
| Historical computation | Define preserved local inputs, environment/provider config and image availability. Existing replay intentionally prefers present inputs; distinguish it from an exact historical repeat. |
| Distribution | Root README/install instructions, project MIT/Apache license texts, release build/checksum artifacts, supported platforms, upgrade/backup/rollback procedures and operator guidance remain. |
| Retention and recovery | Test backup restoration, record/commit failure, historical schemas and cache payload compatibility, and unresolved remote ownership across coordinator recovery. |
| Dependency assurance | Current advisory/license/artifact checks were not run or added. Compilation does not establish their status. |

## Performance and future behavior still needing evidence

| Risk | Work remaining |
| --- | --- |
| Pending bookkeeping | Baseline diagnosis confirms quadratic growth; indexed completion bookkeeping and finite CLI admission are unimplemented. Preserve all completion-order modes. |
| Eager fanout | Nextflow factories/process rounds, Argo loops and CWL Cartesian scatter allocate broadly. Add lazy/bounded emission without arbitrary rejection of otherwise supported large workflows. |
| Retained graph history | Large runs retain task/event graph state. Measure peak memory and cancellation latency at representative sizes. |
| Large CAS inputs and logs | Stream verified remote staging and bounded visible log excerpts while retaining full stored logs and successful task status. |
| Trace export | All events/spans and cloned request chunks consume run-scale memory. Remove clones and introduce incremental event/export/send APIs where necessary. |
| Simulator work | Add a consistent configurable expansion/work/output budget; preserve ordinary synthetic task behavior. |
| Tree retention closure | Expand every retained manifest's descendant digests, including imported directories and no-cache outputs, not only kept cache-entry outputs. |
| Best-effort leases | First run-lease failure is currently cached as absent; slow remote I/O is not comprehensively cancellation-aware. Make admission/protection explicit. |
| Stable identity | Nextflow task identity still depends on arrival order. CLI run IDs use time/PID only. Qualify resume/replay under reordered dynamic inputs and shared hosts. |
| Native evaluation | VM budgets do not automatically bound JavaScript builtins/parse/heap or every Python native helper. Keep exact guarantees and residual limits visible. |
| os.walk | Eager traversal ignores link/topdown/pruning semantics; add bounded behavior and correct native iterator timing. |
| Schema evolution | Test old-run export/resume/replay and old-cache compatibility across the supported version window. |
| Recorder transactions | Add action savepoints/rollback and fault injection for partial action, disk-full, kill and commit failure. |
| Rootless and platform behavior | Qualify mixed UID ownership and exact selected-image execution on each advertised backend, including non-native platforms. |

The isolated Pending baseline uses mock task/outcome payloads and the actual unchanged bookkeeping source. It is not a workflow throughput benchmark. Rust `1.97.1`, optimized compilation, source hashes and commands are preserved. Across Finished/AsAsked/Reversed, 10,000 items took 0.365 seconds wall and 0.355 seconds CPU; 30,000 took 3.602 seconds wall and 3.576 seconds CPU. Peak RSS was 12,332 KiB. No candidate comparison has been run.

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

The 29 code commits remain on the separate branch. The final handoff commit contains documentation only. No active worker or test process is left continuing remediation after this handoff.

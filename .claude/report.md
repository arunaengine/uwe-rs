# Iunctus production-readiness review

Reviewed source: `cd37a1843cc183955577804f63b3f8dbd111bf10` on `main`. Review date: 2026-10-02. Scope: all 29 workspace crates, CLI integration, documentation, tests, deployment assumptions, and code organization. Twelve parallel reviewers investigated separate areas; the parent integrated their findings and checked important cross-crate traces.

## Verdict

The project has substantial implemented functionality, but it is not ready to serve as a broadly deployed, unattended universal workflow executor. The strongest blockers concern host filesystem boundaries, correctness of cache and resume identity, incomplete remote lifecycle ownership, and silent semantic differences in accepted workflow constructs. A controlled deployment of a specifically validated workflow/backend combination is a narrower proposition and needs its own evidence.

The existing separation into language drivers, a shared task model, execution graph, backends, storage, and provenance is useful. The problem is not the absence of an architecture. Several boundaries accept stronger contracts than their implementations enforce, and current test gates do not reliably expose those gaps.

"Universal" must have a published meaning. Current decisions target supported subsets of Nextflow, Snakemake, CWL, and Argo, plus a still-undefined plain-script interface. Full Groovy, all Python packages, arbitrary Kubernetes resources, Galaxy, and every historical engine behavior are not current requirements. Tasks must remain container-based; Conda prepares containers. The simulator intentionally makes synthetic outputs and its graph is not a prediction of a real run (`docs/decisions.md`, decisions 11, 18, 19, 21, 26, and 28).

## How to read the findings

P1 means a release blocker for the affected supported use case: wrong results, data loss, a filesystem-boundary violation, or a serious operational failure. P2 means a material correctness, reliability, compatibility, or scaling issue. P3 means a lower-impact diagnostic, documentation, or verification issue. No P0 finding was established. Priorities describe the affected trigger, not every deployment.

Findings listed in the Checks performed probe table have the stated **reproduced observations**; other findings remain **static**: a concrete trigger and causal source trace. Reproduction establishes only the observations described, without extending that evidence to an end-to-end run. Backend-dependent findings and unmeasured scaling risks are labeled separately. Suggested corrections and regressions are recommendations only; no project code was changed.

Source references are relative to the repository root and refer to the reviewed revision. Documentation changes committed for this report do not change the reviewed code. This is a broad review with selective deep inspection, not a formal proof or a line-by-line audit of every branch. Fresh-check results and remaining verification boundaries appear at the end.

## Immediate production gates

| Gate | Why it blocks broader production use | Required evidence |
| --- | --- | --- |
| Filesystem confinement | Staging, output adoption, CWL delivery, Argo writeback, and simulation have concrete escape paths. | Sentinel tests for traversal, aliases, symlinks, overlapping inputs, and host-only output references. |
| Data and cache integrity | Directory completeness, mutable inputs, overlapping destinations, and resume identity can return wrong bytes. | Exact-byte tests, corruption tests, concurrent publication tests, and directory-input resume tests. |
| Remote lifecycle ownership | Polling and cancellation can abandon active jobs while removing local state. | Transient API failures, ambiguous submit, refused cancellation, coordinator restart, and eventual cleanup on each backend. |
| Explicit language support | Accepted options can disappear before execution or validation. | Differential examples for each supported option; unsupported behavior must fail clearly. |
| Bounded execution | Embedded evaluation, eager fanout, file hashing, and pending bookkeeping have insufficient bounds. | Memory/CPU/cancellation measurements on large files and large dynamic graphs. |
| Enforced release verification | Important tests are ignored, self-skipping, incomplete, or based on simulators. | Pinned-corpus completeness, real backend jobs, rootless runs, and enforced CI against the release revision. |

## Shared engine and task model

### ENG-01 (P1): Simulator output paths escape their work directory

`workflow-graph/src/simulator/mod.rs:280`, `:292`, `:344` preserves parent components in generated output names and joins them onto the task directory without the input-side containment check. A declared output such as `../../../escaped.txt` can create a file outside the simulator root. Existing files are skipped in this particular writer, so this trace establishes creation rather than arbitrary overwrite.

Reject escaping output paths and symlink ancestors before materialization. Regression: simulate into a temporary nested directory and assert no file appears outside it, including script-derived and glob-generated names. The default synthetic executor must still confine its host effects.

### ENG-02 (P1): Equivalent cache keys can describe different staged bytes

`task-model/src/spec.rs:166` rejects only duplicate raw strings; `:234` sorts inputs before hashing. `executor/src/stage.rs:44`, `:126` stages in original order with overwrite semantics. Inputs `x` and `./x`, or directory `d` and file `d/x`, can therefore produce different final bytes when reordered while keeping the same cache key.

Normalize destinations and reject ambiguous aliases/overlaps unless their exact overlay semantics are encoded. Regression: reverse two conflicting copied inputs and compare both keys and actual staged bytes. Keep the existing order independence for nonconflicting inputs.

### ENG-03 (P2): Synchronous outcomes bypass cancellation

`workflow-graph/src/engine.rs:235`, `:248`, `:318` checks cancellation through the asynchronous receive path, while `Running::ended` outcomes can repeatedly advance the driver without that receive. An already-cancelled synthetic or synchronous run can start work and report success; a large launch batch can keep submitting before observing cancellation.

Check cancellation before driver start, each launch, subsequent driver iterations, and successful delivery. Regression: a pre-cancelled token must cause zero executor calls; cancellation during the first synchronous call must prevent the second launch.

### ENG-04 (P2): Terminal errors and cleanup outcomes leave inconsistent attempt records

`workflow-graph/src/engine.rs:263`, `:295` removes a terminal executor error from Pending and returns without a Finished event. `:367` emits cleanup outcomes without updating the returned graph. `run-database/src/recorder.rs:215`, `:504` then leaves the causal attempt without its normal end/status, while graph and observer/database states can disagree.

Record one terminal outcome for every started attempt and update graph state while draining cleanup. Regression: exhaust machine retries and compare the final attempt, graph status, and database rows; repeat with another attempt ending during stop.

### ENG-05 (P2): A panicking executor future can leave the engine waiting forever

`workflow-graph/src/attempts.rs:49` discards the spawned JoinHandle and sends completion only after the future returns. A panic skips that send. The Attempts object retains a sender, so the receiver also stays open and the pending slot never ends. This finding requires a panicking backend future; no ordinary production-input panic in a backend was established here.

Observe task joins or convert unwind/JoinError into a terminal attempt error. Regression: a deliberately panicking fake future must produce a bounded failure, with an external watchdog so the regression cannot hang the runner.

### ENG-06 (P2): Machine retries start new attempts after draining begins

`workflow-graph/src/engine.rs:243`, `:285` clears ordinary queued work during draining but unconditionally restarts a machine-error attempt. `Executor::hold` can be a no-op. A finish-style failure can therefore submit new remote work after committing to drain only attempts already running.

Suppress replacement attempts once draining starts and preserve the appropriate original failure. Regression: one task triggers draining, another returns Machine afterward, and its executor call count must remain one.

## Local execution, image identity, and package containers

### LOC-01 (P1): Double-leading-slash paths escape scratch staging and output lookup

`executor/src/stage.rs:31`, `executor/src/job.rs:132` remove only one leading slash. Joining the remainder of `//tmp/victim` discards the scratch prefix because it is still absolute. `task-model/src/spec.rs:255` accepts the spelling. A copied input can overwrite a host file; the corresponding output path can address a host file.

Normalize or reject repeated leading separators and independently validate container-produced manifest paths. Regression: use only a temporary sentinel, stage to `/` plus its absolute pathname, and require the sentinel to remain unchanged.

### LOC-02 (P1): Overlapping staged inputs write through a read-only host input

`executor/src/stage.rs:60`, `:70`, `:126` first symlinks a read-only directory source at `d`, then copies a later input to `d/config` through that symlink. The original host source is overwritten before container read-only mounts exist. Raw-string duplicate validation does not prevent this.

Reject conflicting destinations or copy the overlay into independent scratch storage. Regression: stage a directory plus a child copied input and verify the original source bytes remain intact. This is the host-write consequence of the alias/overlap identity problem in ENG-02.

### LOC-03 (P1): Output symlinks are dereferenced in the host namespace

`executor/src/wrapper.rs:239` uses `cp -a`, preserving absolute symlinks. `task-runner/src/outputs.rs:248` canonicalizes them on the host and allows final symlinks; `content-store/src/local.rs:53` copies their targets. A container output pointing to `/etc/...` can consequently read and publish the host's file rather than the container's file. A container-only target can instead fail after exit.

Resolve exported symlink content inside the container where appropriate, and confine host adoption to known output/staged-input roots. Regression: different host/container sentinel contents at the same pathname, plus a host-only target, must never publish host-only bytes. Preserve legitimate links to declared inputs deliberately.

### LOC-04 (P2): Failed creation can delete a pre-existing container

`executor/src/process.rs:152`, `:360` removes by name after create/start errors, including a name-conflict failure in which no new container was created. `snakemake-runtime/src/python.rs:247` names helpers using PID plus a counter local to each starter, allowing two starters in one process to collide.

Remove only an ID returned by successful creation, and give helper starts unique names across starters. Regression: create helper A, attempt same-name B, and verify A still serves requests after B fails.

### LOC-05 (P2): Digest-pinned images fail on a cold engine

`executor/src/image.rs:19` returns immediately for an already-pinned reference without ensuring it is present. Later local inspect/create paths do not pull it. Python helper/preparation paths in `executor/src/process.rs:152`, `:360` also create their image directly. Fresh installations can fail with valid registry images while tag-based task acquisition succeeds.

Ensure each required image is locally available before inspection/creation while preserving its digest. Regression: a cold engine must pull and execute a pinned task image and start the documented default Python helper image.

### LOC-06 (P2): Multi-image tasks multiply their resource reservation

`executor/src/local.rs:427`, `:435` gives every image group the entire pool-granted task share while charging the pool once. Two concurrently active groups can each consume the task's full memory/CPU allocation, defeating admission accounting.

Make aggregate reservation and per-group enforcement agree. Regression: inspect two generated configs under one grant and measure combined usage; do not assume per-container limits constitute a task-level bound.

### LOC-07 (P2): Rootless mixed-UID image groups can fail or stall

`executor/src/local.rs:397`, `:439` computes Access from the main image and reuses it for all groups. With a rootless UID-0 main image and a secondary `USER nobody` image, shared scratch/control folders may never be opened for the second UID. It cannot write outputs or completion records, leaving dependent wrappers waiting.

Compute group access needs separately and open/recover shared storage consistently. Regression: a rootless two-image task whose main depends on a differently owned secondary image must complete and leave caller-owned artifacts.

### LOC-08 (P2): Registry metadata assumes linux/amd64

`executor/src/registry.rs:172` selects linux/amd64, with a first-entry fallback, rather than the placement's platform. Remote entrypoint/default-command/user reconstruction can use another architecture's image configuration.

Carry the target platform into config selection and its cache identity. Regression: a multiarchitecture index with deliberately different command/user metadata must select the requested ARM and x86 configurations correctly.

### LOC-09 (P2): Image pinning can combine a digest with the wrong repository

`executor/src/image.rs:107` selects the first RepoDigests hash and attaches the requested repository. A local image associated with different repository manifest digests can produce a repository/digest pair that does not exist remotely.

Select the digest belonging to the normalized requested repository. Regression: inspect data with two distinct repository digests and pin each requested repository independently.

### LOC-10 (P2): Python package calls have no cancellable reply wait

`executor/src/process.rs:223` blocks for a line-server reply without cancellation. Package stop also exchanges with that process before reaching cleanup timeout. A hanging imported-package call can keep planning and shutdown stuck even after the run is cancelled.

Allow cancellation and a bounded deadline to terminate the helper independently of the blocked interpreter exchange. Regression: a helper deliberately never replies, then cancellation must stop the run and remove the helper within the deadline.

### LOC-11 (P2): A transitively installed package can bypass its requested version pin

`python-evaluator/src/packages/server.py:159` returns an already-importable package before checking the pinned requirement. If first-imported A installs B version 2, a later explicit B version-1 import returns version 2. Initial preparation includes only first-import requirements (`python-evaluator/src/packages/mod.rs:154`).

Apply the relevant workflow constraints to dependency installation and validate installed distribution versions before import. Regression: two tiny packages with a transitive dependency and a workflow pin must either resolve that pin or fail clearly.

## Conda locking

### CON-01 (P2): Unsupported dependency constraints are silently dropped

`conda-environment/src/solve.rs:150` discards constraint parse errors. `version.rs:290` rejects negated wildcard syntax such as `numpy !=1.2.*`, so a package exclusion can disappear and an invalid environment can be locked.

Propagate unsupported constraint errors; implement syntax only within the intended scope. Regression: local repodata with that exclusion and only NumPy 1.2 must not yield a successful lock.

### CON-02 (P2): A later channel-qualified spec can contradict the selected package

`conda-environment/src/solve.rs:88` checks already-selected packages with `MatchSpec::matches`; `spec.rs:98` omits channel matching. Requirements `A::foo` and `B::foo` can be accepted after choosing A's record.

Apply channel constraints in every already-selected and constraint-check path. Regression: identical-version records from A/B with both explicit requirements must conflict.

## Remote executors

### REM-01 (P1): Task-level network denial bypasses remote capability checks

`iunctus/src/main.rs:684` preflights only global `none_by_default`. `task-runner/src/request.rs:152` can still create offline placement from a workflow requirement; Kubernetes hardcodes online wrappers (`backend.rs:159`), TES ignores the field, and SLURM/Singularity omits isolation. A CWL denied-network task can run online without global `-no-network`.

Reject unsupported offline placement at the backend boundary before submission. Regression: each affected backend receives an offline task directly and must refuse it before API create/sbatch; add the CWL integration trigger.

### REM-02 (P1): TES polling failure abandons accepted work and enables duplicates

`tes-executor/src/backend.rs:108`, `:144`, `:54` propagates a status error, removes scratch, and never cancels the known accepted task. Engine retry can submit a replacement while the original continues with side effects or in-place writes.

Retain task identity across recoverable reads and resolve/cancel accepted work before finalizing the attempt. Regression: one 503 after acceptance followed by RUNNING must not cause another create or premature scratch removal.

### REM-03 (P1): Kubernetes polling converts authorization errors into an endless wait

`kubernetes-executor/src/pods.rs:90` suppresses list errors through `.ok()?`; `backend.rs:274` treats Unknown as active and resets the trouble timer. Credentials able to create Jobs but forbidden to list Pods can leave the run pending forever, even after TTL removes a finished Job.

Preserve permanent errors and bound transient-error sequences while retaining cleanup responsibility. Regression: fake create succeeds and every Pod list returns 403; require a bounded diagnostic carrying the real cause.

### REM-04 (P1): TES silently ignores task time and GPU requirements

`tes-executor/src/backend.rs:122`, `:286` neither enforces `placement.time_seconds` nor carries accelerators into meaningful placement. Its HTTP request timeout does not bound job duration. A task requesting one second can remain RUNNING indefinitely.

Enforce supported limits through backend lifecycle or reject unsupported requirements. Regression: an always-running fake TES task times out and receives cancellation; a GPU request must be represented correctly or refused.

### REM-05 (P1): Cancellation can release scratch while remote work continues

TES `backend.rs:133`, Kubernetes `backend.rs:238`, `:296`, and SLURM `backend.rs:616` suppress cancellation errors and eventually report Cancelled without confirmed termination. Kubernetes also reports a group failed while siblings remain active (`pods.rs:125`), background-deletes it, and immediately finalizes. Scratch cleanup can race with those workers.

Represent unresolved cleanup explicitly, retain remote IDs/scratch, and continue cleanup until termination is confirmed or responsibility is durably transferred. Regression: refused cancel, delayed termination beyond grace, and failed-container/running-sibling cases must retain state and avoid replacement work.

### REM-06 (P2): Kubernetes duplicates the full GPU count onto every image group

`kubernetes-executor/src/manifest.rs:96` copies full task GPU limits into every container. A two-image task asking for one GPU becomes a two-GPU Pod and can remain unschedulable or exceed quota. GPU limits imply requests under the [Kubernetes contract](https://kubernetes.io/docs/tasks/manage-gpus/scheduling-gpus/).

Define per-group allocation with a sum consistent with task placement, or reject unsupported sharing. Regression: sum resource requests in a two-image manifest, followed by a real one-GPU scheduling test.

### REM-07 (P2, backend validation needed): TES coordination directories are not shared volumes

`tes-executor/src/backend.rs:303` declares work, HOME, and /tmp volumes, while wrappers exchange state under `/iunctus/control` and `/iunctus/outbox` (`executor/src/containers.rs:164`). Separate executor filesystems can lose predecessor records and stall. The [TES specification](https://ga4gh.github.io/task-execution-schemas/docs/) guarantees declared volumes for inter-executor sharing; some servers may incidentally share more.

Declare the required coordination volumes and validate stdin/output interactions. Regression: execute two wrappers in distinct containers sharing exactly the declared mounts. Existing fake-server output fabrication cannot establish this behavior.

### REM-08 (P2): SLURM tool invocations can block cancellation indefinitely

`slurm-executor/src/queue.rs:90`, `:124`, `:144` awaits external commands without deadlines. Polling holds a shared snapshot mutex during squeue; one stuck command can block other jobs. The nominal cancellation grace does not bound an awaited scancel or scheduler query.

Bound and cancel scheduler subprocesses, and avoid holding shared polling state across unbounded I/O. Regression: fake squeue/scancel blocks after starting; cancelling the attempt must terminate helpers and finish within a deadline.

### REM-09 (P2): Task Pods inherit unnecessary ServiceAccount credentials

`kubernetes-executor/src/manifest.rs:124` does not disable token automounting or specify a restricted account. On normal cluster defaults, task containers receive the default account's API token despite not needing it. The exposure depends on that account's permissions. See [Kubernetes service-account behavior](https://kubernetes.io/docs/tasks/run-application/access-api-from-pod/).

Set `automountServiceAccountToken: false` in generated task Pods. Regression: assert the manifest field and verify absence of projected credentials on a real cluster.

## Content store, task cache, staging, and cleanup

### CAS-01 (P1): Prepared digests are not bound to the input bytes executed

`task-runner/src/inputs.rs:55`, `:178`, `:211` hashes a local file but retains its mutable source path for later staging. Waiting for resources or another lease widens the interval. Imported inputs and `content-store/src/store.rs:173` hash then reopen/copy into the previously chosen digest. `DigestCache` avoids memoizing a concurrent change but still returns the prepared digest (`digests.rs:125`). Bytes B can execute or be uploaded under the identity of A.

Bind execution to a stable snapshot hashed from that snapshot; otherwise detect change and reprepare, recognizing that metadata alone does not freeze a live mount. Regression: prepare A, mutate to B, execute, restore A, and check both the result key and stored bytes. Coordinate a hash/copy interleaving separately.

### CAS-02 (P1): Directory links disappear from identity while local tasks can follow them

`task-runner/src/outputs.rs:341` skips symlinks to directories when building trees; `inputs.rs:81` uses that tree for identity. Local staging mounts the original directory (`executor/src/stage.rs:69`), preserving an internal selected->A/B link. Retargeting only that link leaves the digest unchanged while task reads change. Import/publication also loses those directories.

Represent supported links consistently or reject them; following them requires cycle/containment semantics. Regression: retarget an internal directory symlink without changing the real subtrees, then compare keys, actual reads, and imported views.

### CAS-03 (P1): Cleanup deletes from a stale plan after objects become active

`iunctus/src/clean.rs:73`, `:134` snapshots runs/leases and later applies a plan. `task-runner/src/clean.rs:241` deletes unconditionally, without acquisition/publication barriers or effective revalidation. A new run can reuse an old entry after planning and lose its referenced output when that earlier plan is removed.

Synchronize acquisition/publication with deletion using effective pins/epochs or an equivalent protocol. Another unsynchronized snapshot is insufficient. Regression: obtain a plan, start a cache-hit consumer, apply the old plan, and require its live artifacts to survive.

### CAS-04 (P1): Expired object-lease holders can clobber successor ownership

`content-store/src/lease.rs:303` checks ownership then renews with an unconditional write; `:140` and expired cleanup delete after separate reads. Conditional takeover exists (`:274`), but an old holder stalled between read/mutation can later overwrite/delete its successor's lease. Expired-token renewal is also not refused.

Condition renewal/release/GC on the precise version read and handle lost ownership. Regression: barriers around read/mutate, successor takeover between them, then verify the successor token survives renewal, release, and expired cleanup.

### CAS-05 (P1): Download resume can combine different remote object versions

`storage/src/downloads.rs:53`, `:151` resumes from partial length without binding old bytes to an ETag/version. HTTP Range (`web.rs:165`), object-store offsets (`objects.rs:239`), and FTP can append the new suffix to the old prefix and permanently cache a hybrid that never existed remotely.

Persist a strong partial version, condition resumption, validate range semantics, and restart safely when no stable version exists. Regression: interrupted AAAA prefix, changed BBBBCCCC representation, and a resumed suffix must yield the full new representation rather than AAAACCCC.

### CAS-06 (P2): Moving output targets makes later output symlinks dangling

`task-runner/src/outputs.rs:46`, `:248`, `:278` sorts/exports outputs and moves a regular file into the local store before canonicalizing a later link to it. Successful outputs a and longlink->a, or a corresponding directory tree, can fail publication with ENOENT. Runner error cleanup removes remaining scratch (`runner.rs:411`).

Resolve/snapshot links before moving targets or keep originals until dependent copies complete. Regression: regular/link output pairs and internal directory links must both publish correct bytes and retain recoverable state on error.

### CAS-07 (P2): Directory cache completeness does not check descendants

`task-runner/src/entry.rs:72` checks local root plus tree-manifest existence, or only the manifest for remote directories. Deleting one child leaves a cache hit valid and can feed incomplete data downstream.

Read/verify the manifest and check its members before accepting the directory. Regression: two-file directory, delete one child while retaining root/manifest, then require a miss/rerun. Empty directories need deliberate treatment.

### CAS-08 (P2): Layered directory hydration permanently accepts a partial root

`content-store/src/layers.rs:76` returns any existing path. Child hydration creates final parent directories before the whole tree arrives (`:121`, `:145`), so fetching a first child then its parent leaves missing siblings forever. Concurrent readers or interrupted hydration expose the same defect; listing alone also loses manifest-only empty directories.

Build privately from the manifest and publish only a complete tree with a completion signal individual child fetches cannot impersonate. Regression: child-first hydration, mid-download failure, concurrent readers, and empty directories.

### CAS-09 (P2): Concurrent operations reuse a PID-only temporary filename

`content-store/src/layers.rs:128`, `task-runner/src/staging.rs:101`, and `storage/src/folder.rs:91` name parts by target/PID only. Distinct task keys staging one object in the same process can truncate/write the same part; one rename consumes it and another fails, or a published file still has another writer. Same-target provider uploads can mix data.

Use unique operation names or shared per-target serialization. Regression: barrier two opens/writes/renames of the same absent object and require complete bytes plus successful consumers. The counter-based local-store scheme already supplies a useful reference.

### CAS-10 (P2): Layered stale-entry invalidation cannot repair the shared entry

`task-runner/src/entry.rs:108` invalidates an incomplete entry through `content-store/src/objects.rs:178`, which removes only the front. Subsequent reads reload the stale shared entry (`layers.rs:44`); replacement put_entry loses its conditional write to the old shared record (`store.rs:306`). Every later request can rerun while new outputs accumulate unused.

Distinguish local eviction from exact-version shared invalidation/replacement. Regression: shared stale record, front read/remove, then replacement must become the current shared entry without deleting a concurrently repaired one.

### CAS-11 (P2): Short-task grouping changes image entrypoint behavior

`task-runner/src/together.rs:287` replaces arguments with sh together.sh while preserving replaces_entrypoint=false. `executor/src/local.rs:339` then prepends an executable entrypoint, such as python, yielding python sh together.sh. Member wrappers also omit ordinary per-task entrypoint resolution. Historical timings can make previously successful isolated tasks fail after grouping activates.

Exclude unsupported entrypoint cases or resolve member commands individually and launch the group shell independently. Regression: executable-entrypoint image, identical grouped/isolated tasks, correct bytes/status in both modes.

### CAS-12 (P2): Grouping loses per-member deadlines

`task-runner/src/together.rs:257`, `:287`, `:337` removes time from grouping identity and sums deadlines into one placement limit. Member wrappers lack individual deadlines; one unbounded member makes the group unbounded. A member with a ten-second limit can run fifteen seconds successfully.

Enforce each member's deadline or exclude time-limited tasks until it can be preserved. Regression: mixed bounded/unbounded members and overlong first member, comparing per-task status to isolated runs.

### CAS-13 (P2): Cache preview ignores the unreproducible-tool marker

`task-runner/src/lookup.rs:47` returns an old entry based on scope/reuse while `runner.rs:345` also checks the tool marker. After reproducibility failure disables sharing, preview still reports a hit and can misdescribe downstream remaining work.

Apply the effective marker policy to lookup. Regression: cache a task, mark its tool, then real execution must rerun and preview must miss.

### CAS-14 (P2): File executable mode changes behavior without changing identity

`task-runner/src/inputs.rs:216` hashes bytes and records requested executable, not actual source mode. For requested executable=false, local staging keeps original permissions (`staging.rs:27`). chmod on an otherwise identical ./script can switch successful execution to permission denial with the same key; directory manifests already include mode.

Normalize effective staged mode or hash the relevant effective mode. Regression: byte-identical script before/after chmod, comparing keys and behavior.

### CAS-15 (P2): Concurrent local cleaners can unlink a newly acquired lease

`content-store/src/lease.rs:220` locks an already-open old inode and unlinks by pathname without the same_file check used by acquisition (`:180`). Cleaner A removes the old path, a task acquires a new inode there, and cleaner B locks its old descriptor but unlinks the task's new path.

Verify descriptor/path identity after locking before unlinking. Regression: barriers after two cleaners open the old inode and before B unlinks, with a new acquisition in between; duplicate execution must remain impossible.

### CAS-16 (P2): Download caching conflates provider namespaces

`storage/src/downloads.rs:18`, `:41` identifies completed copies by publicized address only. `storage/src/lib.rs:230` can return that copy before selecting the current provider. Two configs mapping the same s3://bucket/key to different endpoints/folder roots but sharing downloads can consume the first provider's bytes.

Include stable, secret-free provider identity in the download namespace and define migration behavior. Regression: two folder-provider roots with the same URL and different contents, sharing one downloads directory.

### CAS-17 (P2, integrity/recovery gap): Cache corruption does not consistently cause a safe miss

`task-runner/src/entry.rs:111` propagates malformed entry JSON instead of treating it as a recoverable stale entry. Ordinary output completeness checks only existence, so externally modified output bytes can also remain accepted; log corruption is checked later during reconstruction. Immutable-store assumptions are therefore stronger than corruption detection.

Define a repair/quarantine policy and distinguish unavailable/corrupt artifacts from backend outages. Regression: truncated entry, damaged logs, and modified output bytes must produce explicit safe recovery or integrity errors, never silent reuse.

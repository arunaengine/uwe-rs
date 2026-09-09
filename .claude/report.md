# Iunctus production-readiness review

Reviewed source: `cd37a1843cc183955577804f63b3f8dbd111bf10` on `main`. Review date: 2026-10-02. Scope: all 29 workspace crates, CLI integration, documentation, tests, deployment assumptions, and code organization. Twelve parallel reviewers investigated separate areas; the parent integrated their findings and checked important cross-crate traces.

## Verdict

The project has substantial implemented functionality, but it is not ready to serve as a broadly deployed, unattended universal workflow executor. The strongest blockers concern path mapping and staging correctness, correctness of cache and resume identity, incomplete remote lifecycle ownership, and silent semantic differences in accepted workflow constructs. A controlled deployment of a specifically validated workflow/backend combination is a narrower proposition and needs its own evidence.

The existing separation into language drivers, a shared task model, execution graph, backends, storage, and provenance is useful. The problem is not the absence of an architecture. Several boundaries accept stronger contracts than their implementations enforce, and current test gates do not reliably expose those gaps.

"Universal" must have a published meaning. Current decisions target supported subsets of Nextflow, Snakemake, CWL, and Argo, plus a still-undefined plain-script interface. Full Groovy, all Python packages, arbitrary Kubernetes resources, Galaxy, and every historical engine behavior are not current requirements. Tasks must remain container-based; Conda prepares containers. The simulator intentionally makes synthetic outputs and its graph is not a prediction of a real run (`docs/decisions.md`, decisions 11, 18, 19, 21, 26, and 28).

## How to read the findings

P1 means a release blocker for the affected supported use case: wrong results, data loss, incorrect reads or writes to user data, or a serious operational failure. P2 means a material correctness, reliability, compatibility, or scaling issue. P3 means a lower-impact diagnostic, documentation, or verification issue. No P0 finding was established. Priorities describe the affected trigger, not every deployment.

The user clarified that this project executes trusted local workflows under the calling user's permissions. Path-related findings describe unexpected mapping, staging or result behavior in that model. The remediation status and remaining work at the latest verified checkpoint are recorded in [handoff.md](handoff.md).

Findings listed in the Checks performed probe table have the stated **reproduced observations**; other findings remain **static**: a concrete trigger and causal source trace. Reproduction establishes only the observations described, without extending that evidence to an end-to-end run. Backend-dependent findings and unmeasured scaling risks are labeled separately. Suggested corrections and regressions are recommendations only; no project code was changed.

Source references are relative to the repository root and refer to the reviewed revision. Documentation changes committed for this report do not change the reviewed code. This is a broad review with selective deep inspection, not a formal proof or a line-by-line audit of every branch. Fresh-check results and remaining verification boundaries appear at the end.

## Immediate production gates

| Gate | Why it blocks broader production use | Required evidence |
| --- | --- | --- |
| Path mapping and staging | Staging, output adoption, CWL delivery, Argo writeback, and simulation can resolve data outside their configured work paths. | Sentinel tests for traversal, aliases, symlinks, overlapping inputs, and unexpected output references. |
| Data and cache integrity | Directory completeness, mutable inputs, overlapping destinations, and resume identity can return wrong bytes. | Exact-byte tests, corruption tests, concurrent publication tests, and directory-input resume tests. |
| Remote lifecycle ownership | Polling and cancellation can abandon active jobs while removing local state. | Transient API failures, ambiguous submit, refused cancellation, coordinator restart, and eventual cleanup on each backend. |
| Explicit language support | Accepted options can disappear before execution or validation. | Differential examples for each supported option; unsupported behavior must fail clearly. |
| Bounded execution | Embedded evaluation, eager fanout, file hashing, and pending bookkeeping have insufficient bounds. | Memory/CPU/cancellation measurements on large files and large dynamic graphs. |
| Enforced release verification | Important tests are ignored, self-skipping, incomplete, or based on simulators. | Pinned-corpus completeness, real backend jobs, rootless runs, and enforced CI against the release revision. |

## Shared engine and task model

### ENG-01 (P1): Simulator output paths can resolve outside their work directory

`workflow-graph/src/simulator/mod.rs:280`, `:292`, `:344` preserves parent components in generated output names and joins them onto the task directory without the input-side containment check. A declared output such as `../../../escaped.txt` can create a file outside the simulator root. Existing files are skipped in this particular writer, so this trace establishes creation rather than arbitrary overwrite.

Keep generated output paths under their task directory and check symlink ancestors before materialization. Regression: simulate into a temporary nested directory and assert no file appears outside it, including script-derived and glob-generated names. The default synthetic executor must still confine its host effects.

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

### LOC-01 (P1): Double-leading-slash paths use the wrong staging and output root

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

## Nextflow parser, evaluator, configuration, and plugins

### NFL-01 (P2): Script type annotations disappear during lowering

`nextflow-evaluator/src/lower/mod.rs:429` drops function parameter/return annotations; `nextflow-evaluator/src/lower/statement.rs:38` drops local annotations. Invocation checks arity rather than those types (`nextflow-evaluator/src/interpreter/call.rs:85`). For example, a String-typed local initialized with 5 can compute numeric 6 instead of string '51'; an Integer parameter can accept a Long contrary to the recorded oracle (`docs/research/nextflow-groovy.md:29`).

Preserve and enforce supported conversions/dispatch, or reject unsupported annotations. Regression: typed initialization/reassignment, same-arity parameter matching, and return conversion, rather than only typed class fields.

### NFL-02 (P2): nf-schema path validation silently skips referenced or root schemas

`nextflow-plugins/src/nf_schema/paths.rs:40`, `:129` collects inline property paths without resolving references. Required custom checks return success when lookup state is absent (`:169`, `:232`). A property referencing a `$defs` file-path schema with `exists: true`, or the same schema at a scalar root, can accept a missing file.

Resolve schema references while pairing instance paths and never interpret missing required lookup state as success. Regression: missing/wrong-kind paths under `$ref`, inline properties, array items, and scalar roots.

### NFL-03 (P2): Aliased static imports invoke the alias instead of the imported member

`nextflow-evaluator/src/lower/mod.rs:489` retains alias/class but loses the original member; `nextflow-evaluator/src/lower/expression.rs:445` emits the alias as the method name. `import static java.lang.Math.max as min` followed by `min(1,2)` can call Math.min and return 1 rather than 2.

Retain original member identity when lowering local aliases. Regression: alias collides with another real method, and alias has no real method of its own.

### NFL-04 (P2): Inclusive integer-limit ranges wrap after their final element

`nextflow-evaluator/src/value/range.rs:142` appends the inclusive endpoint and then increments it; numeric arithmetic wraps (`nextflow-evaluator/src/number/arithmetic.rs:57`). `(2147483647..2147483647).size()` can grow to the ten-million-element guard instead of returning 1, consuming substantial memory for a tiny expression.

Terminate immediately after appending the final inclusive element. Regression: ascending maximum and descending minimum endpoints for supported integer widths.

### NFL-05 (P2): Config include cycles overflow the native stack

`nextflow-config/src/file.rs:481` recursively evaluates includes without an active include stack/depth guard. Its source cache (`:181`) does not track evaluation. `includeConfig 'nextflow.config'` within that same file repeatedly reparses cached text and can abort the process.

Detect cycles using normalized active source identities and report the include chain. Regression: direct/indirect cycles in a guarded child process, plus repeated sequential noncyclic includes that must remain valid.

### NFL-06 (P2): Explicit default `standard` profile is rejected without a declared block

`nextflow-config/src/lib.rs:137` checks all explicitly selected profiles against declared names. Empty config plus exactly `-profile standard` fails, although the recorded contract exempts that singleton (`docs/research/nextflow-config.md:138`).

Apply the singleton exception without exempting mixed unknown selections. Regression: implicit standard, singleton explicit standard, declared standard, and standard combined with another profile.

### NFL-07 (P2): Numeric overload selection depends on declaration order

`nextflow-evaluator/src/interpreter/classes.rs:470`, `:502` treats every supported numeric variant as fitting every numeric type and ranks by typed-parameter count. Same-arity Long and Integer overloads therefore tie; placing Long first can cause an Integer call to select it.

Rank exact types and supported conversion specificity, then reject unresolved ambiguity. Regression: both declaration orders for equal-arity numeric overloads. Existing different-arity overload tests cannot establish this behavior.

### NFL-08 (P2): Class inheritance is accepted and discarded

`nextflow-parser/src/parser/class.rs:67` consumes extends/implements but excludes them from the resulting AST. A Child extending Parent parses successfully yet cannot call Parent's inherited member when executed. The evaluator supports instances as well as statics, so this is not merely discarded metadata.

Reject inheritance at load time until its intended subset is implemented. Regression: inherited static/instance members and constructors, or explicit unsupported diagnostics. Complete Groovy inheritance is not required to stop silent acceptance.

## Nextflow channels and task runtime

### NFR-01 (P1): Process env and stdin inputs are discarded

`nextflow-evaluator/src/interpreter/task.rs:209` does nothing for accepted env/stdin input kinds. `nextflow-runtime/src/process.rs:29` carries neither value into TaskRequest; `driver.rs:683` sets config-only environment and no stdin. `input: stdin` with `cat`, or `env 'SAMPLE'` read by a literal shell script, executes with missing data and can omit channel differences from cache identity.

Carry per-round env/stdin into the concrete task and its hash, including tuple forms, or reject those inputs. Regression: inspect emitted tasks for distinct values, then verify execution bytes and distinct keys.

### NFR-02 (P1): `cache false` does not prevent cache reuse

`nextflow-runtime/src/driver.rs:692` always assigns Reuse::Allowed; no effective directive handling reaches the runner's reuse decision (`task-runner/src/request.rs:67`). A process explicitly forbidding cache can receive an old result on a repeat run.

Map its evaluated cache policy to the existing task reuse controls. Regression: cache a task, set/retain `cache false`, and verify emitted policy plus actual backend invocation on subsequent runs.

### NFR-03 (P1): Join integrity options are accepted but ignored

`nextflow-runtime/src/build.rs:831` extracts by/remainder but drops failOnDuplicate/failOnMismatch; `keyed.rs:168` has no corresponding enforcement. Joining unmatched keys with `failOnMismatch: true` silently drops samples, while duplicate IDs can pair despite requested uniqueness.

Enforce or explicitly reject those options and incompatible combinations. Regression: unmatched IDs, duplicates before/after a match, and remainder combined with failOnMismatch (`docs/research/nextflow-channels.md:703`).

### NFR-04 (P1): Default operator files collide between concurrent runs

`nextflow-runtime/src/build.rs:925`, `:974` names collectFile and split-file locations under `work/<operator>/<channel-id>` without a run namespace. IDs restart per network, and `files.rs:118` writes directly there. Two runs in one launch directory can overwrite each other's intermediates before downstream staging.

Use unique run-owned scratch for implicit destinations while retaining explicit user destinations. Regression: pause staging in two drivers with the same launch directory and assert distinct operator paths and correct bytes.

### NFR-05 (P2): Closed process inputs are ignored until other inputs become ready

`nextflow-runtime/src/process.rs:119` returns on an unavailable input before checking accumulated STOP messages; `route.rs:94` has a related ordering problem. `P(channel.empty(), channel.value())` can remain open and report deadlock even though it can never fire.

Check termination independently of complete round readiness. Regression: closed/unbound inputs in both orders and downstream ifEmpty; compare with the STOP contract at `docs/research/nextflow-channels.md:1077`.

### NFR-06 (P2): Output arity is neither validated nor reflected in singleton shape

`nextflow-runtime/src/process.rs:486` only uses arity presence to force a list; bounds are never applied. Earlier missing-output handling (`:452`) ignores a zero minimum. `arity: '1'` accepts two files and returns a list for one; `0..1` fails on zero.

Parse/enforce cardinality and return the specified singleton shape. Regression: 1, 2, 0..1, and 1..* with below/within/above-bound matches (`docs/research/nextflow-channels.md:1121`).

### NFR-07 (P2): Output collection failures bypass process failure policy

`nextflow-runtime/src/process.rs:150` applies failed/errorStrategy only to nonzero exit status. Missing-output errors escape directly; eval failures become DriverError before the process handles them (`driver.rs:767`). A zero-exit task with missing output aborts despite retry/ignore, and keep-going is bypassed.

Route output/eval failures through the owning process's failure policy, preserving the cause. Regression: exit-zero missing output and failing eval under retry, ignore, and keep-going. Individual exception parity still needs oracle confirmation.

### NFR-08 (P2): Early-stopped readers retain later channel messages

`nextflow-runtime/src/channel.rs:118` reclaims against every cursor. Node stop (`network.rs:407`) never unregisters inputs; take/until stops advancing its cursor (`operator.rs:183`). A live draining subscriber therefore cannot free later messages while another subscriber has already stopped.

Unsubscribe inactive readers or exclude them from reclamation. Regression: one take(1) reader plus one draining reader across many batches, with retained-buffer size bounded rather than merely correct final values.

### NFR-09 (P2): groupTuple custom sort is replaced with natural ordering

`nextflow-runtime/src/build.rs:817` collapses any truthy sort policy, including a closure, to a boolean; `keyed.rs:153` then uses number-aware comparison. A descending comparator returns ascending values instead. Hash/deep modes also collapse.

Preserve supported sort semantics or reject unsupported modes. Regression: descending comparator, key closure, and supported named modes with observably different ordering.

### NFR-10 (P2): Registered workflow failure handlers never execute

`nextflow-runtime/src/lib.rs:171` invokes handlers only on success with on_error=false. Failure returns earlier; `workflow.rs:185` nevertheless accepts both onError/onComplete registration. Task failure or cancellation can omit expected failure reporting/cleanup and completion callbacks.

Finalize metadata and invoke appropriate handlers on each terminal path, or refuse unsupported handlers. Regression: markers and workflow.success/status for success, task failure, and cancellation.

### NFR-11 (P2): Several accepted directives and callbacks have no effective handling

Accepted beforeScript/afterScript/fair/maxErrors and subscribe(onError) do not reach the corresponding runtime behavior. Option extraction in `nextflow-runtime/src/build.rs` and task conversion in `driver.rs` discards them. A beforeScript that creates a required file fails; fair output ordering and process-wide error limits are not enforced. collectFile also uses lexical sorting where its own comment records a hash-based upstream default (`files.rs:24`, `:97`).

Publish precise support status and enforce or reject behavior-changing options at their existing dispatch points. Regression: one observable example per supported directive/mode; parser acceptance alone is insufficient.

## Snakemake parsing, planning, and execution

### SNK-01 (P1): Existing checkpoint outputs can prevent a needed checkpoint rerun

`snakemake-runtime/src/objects.rs:562` considers checkpoint output existence sufficient for get(). `plan.rs:456` records consulted checkpoints but builds dependencies from returned filenames; `driver.rs:530` orders only checkpoint tasks already present. An old manifest selecting sample A can therefore plan downstream A after the checkpoint's source changes to B, without scheduling the checkpoint.

Make consulted checkpoints actual dependencies and defer dynamic expansion while they require rerun. Regression: run A, change its source to B, remove only the aggregate output, then require checkpoint execution and B selection.

### SNK-02 (P1): ensure output integrity checks disappear at completion

`snakemake-runtime/src/functions.rs:1245` records non_empty/checksum ensure options, but `driver.rs:915` converts tags to strings and checks only output presence. `ensure('result', non_empty=True)` with touch, or a wrong checksum, can succeed and feed downstream work.

Validate supported tags before delivery and refuse unsupported options. Regression: exit-zero empty and wrong-checksum outputs must fail before acceptance.

### SNK-03 (P1): Storage outputs remain local while consumers fetch stale remote data

`snakemake-runtime/src/task.rs:678` loses the tagged remote destination during output construction; `driver.rs:751` delivers the local path. Downstream `task.rs:383` prefers the storage tag's remote URL over the produced artifact. A producer writes new bytes while its consumer reads an older bucket object, or fails if that object is absent.

Preserve output destinations and resolve produced dependencies before remote lookup, or reject unsupported storage-output semantics. Regression: a fake provider containing old bytes, producer of new bytes, and matching consumer must publish/read new bytes.

### SNK-04 (P1): before_update snapshots collide on basename

`snakemake-runtime/src/task.rs:414` snapshots to `before_update/<task-id>/<basename>`. Inputs a/data.txt and b/data.txt overwrite the same snapshot, so both task inputs can contain the second file's bytes.

Use a unique complete-path or input-index identity. Regression: same-basename inputs with different contents must remain distinct after task construction and staging.

### SNK-05 (P2): Remote checkpoint directories bypass file resolution

`snakemake-runtime/src/files.rs:164` lists with std::fs::read_dir on the recorded location, whereas read uses Files. A remote directory URI fails during glob_wildcards/traversal instead of materializing the directory through its provider.

Resolve directories through Files before listing. Regression: extend the remote Elsewhere fake with directory listing, including nested entries.

### SNK-06 (P2): A service group stalls when consumers depend on each other

`snakemake-runtime/src/driver.rs:835` starts a service group only when every consumer has exactly one remaining dependency. With S feeding B and C, and B also feeding C, C has two dependencies; B waits for S and the whole valid acyclic group stalls.

Account for internal consumer dependencies and sequence them, or emit a targeted unsupported error. Regression: the three-rule graph must complete with S alive through both consumers.

### SNK-07 (P2): Accepted pipes lack FIFO and concurrent execution semantics

`snakemake-runtime/src/driver.rs:494` validates pipe consumers, but `services.rs:17` groups only services. A pipe producer runs alone and its consumer waits for completion. Redirection creates a regular file instead of a FIFO; an explicit FIFO producer can deadlock waiting for the unscheduled consumer.

Reject pipe workflows until supported, or provide FIFO creation and concurrent producer/consumer execution. Regression must transfer actual streamed bytes; synthetic output creation cannot establish pipe behavior.

### SNK-08 (P2): Output-validation and service failures bypass retries

`snakemake-runtime/src/driver.rs:657`, `:685` retries ordinary nonzero task exits, while exit-zero missing outputs and group failures go directly to failed. Declared retries therefore do not cover those task failures.

Apply the appropriate retry policy after validation and define group retry/resource reevaluation. Regression: first exit-zero outcome lacks an output, second supplies it; add a service group failing its first attempt.

### SNK-09 (P2, support gap): Rerun and protection contracts remain narrower than native execution

`snakemake-runtime/src/plan.rs:639` bases needrun on existence/dependency/incompleteness and mtime/checksum, so changing params, command, image, or input set can leave old outputs untouched. This broader rerun work is historically staged later. protected outputs are not made read-only; lifecycle hooks/resource scopes (`workflow.rs:809`) and shadow/cache/handover (`:1164`) are ignored.

Publish these exact boundaries and reject ignored options where they change computation or safety. Add behavioral parity checks before claiming native rerun/protection equivalence; this is a documented scope gap plus unsafe acceptance risk, not authorization for redesign.

## Native Python and pandas/Polars compatibility

### PY-01 (P1): math.isqrt can panic or fail to terminate

`python-evaluator/src/modules/misc.rs:355` squares i64 values in correction loops seeded by a floating square root. `math.isqrt(9223372036854775807)` overflows with checks; under wrapping arithmetic its second comparison can remain true forever. Interpreter step limits do not cover this native loop.

Use overflow-safe integer square root/comparisons. Regression: maximum i64 must return 3037000499 under both debug and release, guarded by a subprocess deadline.

### PY-02 (P1): Formatting cyclic containers can overflow the native stack

`python-evaluator/src/interpreter/mod.rs:363` recursively formats lists/dicts without active-object detection or interpreter call accounting. `a=[]; a.append(a); print(a)` can abort the process instead of producing a bounded recursive representation.

Track formatting recursion identities and emit appropriate placeholders/errors. Regression: cyclic lists/dicts in a core-disabled child process; ordinary function recursion tests do not cover this path.

### PY-03 (P1): Invalid dataframe index lengths reach unchecked native indexing

`python-evaluator/src/pandas/reshape.rs:74` installs an external index without checking row count; `frame.rs:70` indexes its selected position unchecked. A one-row frame given labels a,b, followed by `.loc['b','x']`, can panic. Unequal MultiIndex levels also index unchecked (`labels.rs:39`).

Validate row/level lengths before installing indexes. Regression: mismatched Index/MultiIndex shapes must produce catchable errors, preserving valid duplicate-index behavior.

### PY-04 (P2): Series arithmetic pairs positions instead of labels

`python-evaluator/src/pandas/series.rs:181`, `values.rs:318`, `sequence.rs:30` strips the second index and zips values. Series [1,2] at A,B plus [10,20] at B,A yields [11,22] instead of aligned [21,12]. Comparisons likewise lose their specific index contract.

Align supported arithmetic or reject incompatible indexes before computing. Regression: reversed/disjoint indexes and index-sensitive comparisons to prevent metrics being assigned to the wrong samples.

### PY-05 (P2): Boolean Series masks ignore their labels

`python-evaluator/src/pandas/indexer.rs:71` and `sequence.rs:75` recognize equal-length booleans then select positions after discarding labels. Mask True,False at B,A selects A from a frame at A,B, rather than B. Disjoint labels can also pass rather than fail alignment.

Align Series masks and reject unalignable labels; retain positional list masks. Regression: independently reordered masks and completely disjoint indexes.

### PY-06 (P2): Horizontal concat drops duplicate-index rows

`python-evaluator/src/pandas/combine.rs:135` builds a unique label union and takes only each label's first position. Two two-row frames both indexed S,S concatenate into one row, losing a replicate instead of preserving both rows.

Preserve positions for identical duplicate indexes and explicitly reject unsupported ambiguous reindexing. Regression: duplicate axis=1 concat and nonidentical duplicate-index cases.

### PY-07 (P2): Accepted CSV options silently alter data interpretation

`python-evaluator/src/pandas/read.rs:71`, `:334`, `:372` accepts decimal, thousands, parse_dates, encoding, and encoding_errors without applying them. For decimal=',', textual 1,5 remains text instead of numeric 1.5. chunksize/iterator also return a full frame rather than a bounded iterator.

Implement the supported behavior or reject nondefault unsupported options. Regression: decimal separators, non-UTF8 identifiers, date parsing, and iterator result shape/memory.

### PY-08 (P2): Polars named filter constraints are ignored

`python-evaluator/src/pandas/polars/methods.rs:86` evaluates positional predicates only. `df.filter(sample='A')` leaves all rows selected; LazyFrame delegates to the same implementation. A supported-looking filter can plan unwanted samples.

Apply named equality constraints or reject them. Regression: positional/named/combined filters on both eager and lazy frames.

### PY-09 (P2): Generators eagerly execute unused tails

`python-evaluator/src/interpreter/expression.rs:162`, `call.rs:96` materializes generator expressions/functions before returning an iterator. `next(1 // x for x in [1,0])` fails before yielding 1; a yield followed by an unused raise also fails immediately. Bounded consumers cannot prevent large eager allocations or early side effects.

Implement suspended evaluation for the supported subset or reject incompatible constructs. Regression: one-item consumption with an exceptional/large unused tail, plus effect timing.

### PY-10 (P2): Subprocess options are dropped before container dispatch

`python-evaluator/src/modules/subprocess.rs:47`, `:159` ignores cwd/env/input/timeout kwargs; `host.rs:25` cannot carry several of them. A valid container host receives the wrong working directory/environment and no requested deadline. This trace is not a host-command fallback: commands still use Host dispatch.

Honor supported arguments and explicitly reject the rest. Regression: a mock command host checks cwd and whether environment/stdin/deadline are forwarded or refused.

### PY-11 (P2): Virtual append hides the original disk content

`python-evaluator/src/host.rs:285` creates an empty overlay on first append; `builtins.rs:1664` flushes only new bytes into it. Disk-backed old plus appended new is subsequently read as new rather than oldnew. The actual host file is not modified by this LocalHost overlay.

Seed first append from the currently visible file. Regression: existing disk file and subsequent virtual append/read, alongside new-file and deleted-overlay cases.

### PY-12 (P2): Label slices ignore step and reverse direction

`python-evaluator/src/pandas/indexer.rs:48` considers only slice endpoints and creates an ascending contiguous selection. `.loc[::-1]` remains unreversed, and `.loc[::2]` retains every row.

Honor signed nonzero steps/inclusive endpoints or reject unsupported steps. Regression: reversed, strided, bounded, and empty label slices.

### PY-13 (P2): Validation flags, truthiness, and flush do not honor accepted contracts

`python-evaluator/src/pandas/reshape.rs:51` discards verify_integrity; merge cardinality validation is not applied. `python-evaluator/src/interpreter/operators.rs:574` gives pandas objects length-based truthiness rather than ambiguous-truth errors. Public flush is a no-op (`builtins.rs:1631`), so reopening after write/flush sees stale data until close.

Enforce or reject behavioral flags, provide correct supported truth semantics, and implement repeat-safe virtual flush. Regression: duplicate-index integrity, merge validation, bool(Series), and successive write/flush/read cycles. These are separate API cases sharing the same acceptance-without-enforcement pattern.

## CWL loading and runtime

### CWL-01 (P1): cwl.output.json can publish host-only files

`cwl-runtime/src/tool/collect.rs:257`, `:280` accepts arbitrary file: locations/absolute paths and unchecked relative traversal. Collection then checksums and final delivery reads those host locations. A task naming a host-only sentinel in its output JSON can access bytes it was never mounted.

Accept only confined output locations and explicitly staged input mappings, including symlink resolution. Regression: absolute path, file: URL, and ../ output references must fail before reading a sentinel outside task storage.

### CWL-02 (P1): Output basenames can select a different output folder

`cwl-runtime/src/results.rs:43`, `:77` joins an unvalidated basename and immediately writes literals. An ExpressionTool literal File with basename ../victim or an absolute name overwrites outside output_dir; literal Directory listings share the path.

Require one normal basename component and prevent destination-symlink redirection. Regression: sibling sentinel, absolute name, and nested literal Directory must fail without modifying anything outside output_dir.

### CWL-03 (P1): Same-basename outputs overwrite one another

`cwl-runtime/src/results.rs:28` assigns every array/scatter output folder/basename; `workflow-graph/src/files.rs:56` removes an existing destination before copying. Distinct result.txt files become the same final pathname and only the last survives; equal-name directories can replace whole results.

Allocate distinct destinations while preserving secondary-file grouping, or fail atomically on collision. Regression: two scattered files with distinct bytes and the same basename must remain separately addressable or produce an explicit safe failure.

### CWL-04 (P1): Output checksum calculation loads whole files into memory

`cwl-runtime/src/files.rs:328` reads the entire file before hashing; local std::fs::read and remote read_to_end (`:233`, `:268`) are unbounded. A completed scientific output larger than available memory can kill the coordinator although hashing needs constant space.

Stream checksum and size with bounded buffers, keeping loadContents semantics separate. Regression: a large sparse/output file under a memory ceiling with reference digest and peak-RSS measurement.

### CWL-05 (P1): JavaScript loop limits do not bound total expression work

`cwl-runtime/src/javascript.rs:43` configures a loop-iteration limit but evaluates synchronously in the coordinator. A depth-55 binary-recursive function can perform enormous work without loops or reaching Boa's recursion limit. Cancellation/task progress can remain blocked; allocation-heavy expressions also lack a total memory budget.

Enforce a total evaluation/cancellation budget covering calls and builtins. Regression: a loop-free recursive expression must fail within its configured budget in a watchdog-protected process. A loop-count test cannot establish this guarantee.

### CWL-06 (P1): Same-document fragment imports bypass cycle detection

`cwl-parser/src/salad/import.rs:22` tracks document cycles, while same-file fragment imports at `:69` recurse directly through resolve. A #loop fragment importing itself, or two such fragments, can overflow the native stack before model validation.

Track active fragment expansions as well as documents. Regression: direct/indirect fragment cycles in a guarded process plus valid repeated fragment references.

### CWL-07 (P2): Literal output write failures are silently successful

`cwl-runtime/src/results.rs:89` discards write errors; its vector-only delivery API gives `driver.rs:274` no error to propagate. An existing regular file selected as output_dir can make an ExpressionTool report success without producing its literal result.

Make materialization fallible and carry errors through finish. Regression: output_dir is an existing file, avoiding root-sensitive permission assumptions; require a failure and no successful result object.

### CWL-08 (P1): In-place writers lack exclusive-access scheduling

`cwl-runtime/src/run/mod.rs:594` launches every source-ready step without conflict checks; `cwl-runtime/src/tool/mod.rs:522` creates no ordering. Concurrent InplaceUpdate writers see the original snapshot, and completion rewrites skip already-created tasks (`cwl-runtime/src/run/mod.rs:246`), so a later update can disappear from downstream state. The exclusive-access requirement is recorded at `docs/research/cwl.md:54`.

Serialize conflicting reads/writes by original identity and construct delayed inputs after updates, or reject the unsupported requirement. Regression: two writers and a downstream reader with both completion orders must be deterministic or explicitly rejected. Static confidence is high; differential execution remains needed.

### CWL-09 (P2): Any declared ontology disables format compatibility globally

`cwl-runtime/src/driver.rs:61` passes only whether schemas exist; `inputs.rs:350` accepts every input format when true. An unrelated ontology can therefore allow a known incompatible file format.

Resolve supported subclass/equivalence relationships or explicitly refuse unsupported ontology checking. Regression: unrelated schema plus incompatible format must still fail.

### CWL-10 (P2): Remote directory listings become empty in real runs

`cwl-runtime/src/files.rs:381` gives nonlocal directories listing=[] even when actual deep/shallow listing is requested. A workflow scattering over that listing silently emits no work.

Use provider-backed listing/materialization or return a clear unsupported/error result. Regression: local and remote representations of the same directory must enumerate the same supported entries.

## Argo parsing and runtime

### ARG-01 (P1): Nested workflows lose the Files service

`argo-runtime/src/lib.rs:147` installs Files only on the root; `child.rs:150` creates children without inheriting it. Remote output parameters fall back to host reads, and `run.rs:204` suppresses failures into empty text. Nested conditions/fanout can therefore change silently.

Install the parent's Files before child operation and propagate genuine read failures. Regression: child JSON-list parameters at a fake S3 URI must produce the same fanout as local files.

### ARG-02 (P1): Remote directory outputs vanish from shared volumes and hostPath writeback

`argo-runtime/src/volumes.rs:340`, `:395` turns URI locations into PathBuf and read_dir, suppressing errors into empty lists. A remote writer succeeds while dependent volume readers and hostPath recipients receive no files.

Enumerate/materialize through Files or reject unsupported locations before accepting completion. Regression: fake remote directory containing a.txt must reach both a dependent reader and writable hostPath destination.

### ARG-03 (P1): Simulator leniency silently removes real fanout

`argo-runtime/src/items.rs:51`, `:79` converts whitespace-only, unresolved, or placeholder loop input to zero iterations without checking assume_inputs. A real producer returning invalid JSON-list data can cause required downstream work to disappear while the final workflow succeeds. Related output-resolution suppression occurs in `scope.rs:233`.

Limit accommodations to simulation and fail invalid real data; genuine [] remains valid. Regression: real-mode whitespace/unresolved withParam must fail, while [] and skipped steps retain their intended behavior.

### ARG-04 (P1): Workflow/template parallelism is accepted but unenforced

`argo-parser/src/model.rs:135`, `:236` parses limits, but runtime checks only validity (`checks.rs:219`). `steps.rs:125` and `dag.rs:449` emit all ready tasks. parallelism=1 fails to serialize shared-volume updates and can lose increments or overload resources.

Apply admission limits at workflow/template scope or reject them explicitly. Regression: two ready leaves under limit one, followed by a shared counter update scenario.

### ARG-05 (P1): hostPath subPath reads the host root rather than the selected subtree

`argo-runtime/src/volumes.rs:213` uses host.path as initial input regardless of subPath, while writeback uses the subpath (`:358`). /data plus sample-a reads the wrong relative tree and writes back elsewhere; file subpaths also lack correct representation.

Resolve and validate the selected host path before type/existence/input construction. Regression: root and selected-subtree sentinels, plus file subPath, must expose only the requested source.

### ARG-06 (P1): hostPath writeback can select a different directory

`argo-runtime/src/volumes.rs:358` joins unchecked subPath into host destinations; :370 copies through existing destination symlinks. ../outside writes into a sibling with native user permissions. Arbitrary explicit hostPath workflows are trusted, but this still violates the selected mount's boundary.

Reject absolute/parent-traversing subpaths and confine writes across symlink components. Regression: temporary sibling directories and redirected destination symlink must remain untouched outside the selected root.

### ARG-07 (P2): Aggregated parameter strings change type

`argo-runtime/src/scope.rs:154` reparses strings as JSON for outputs.parameters.NAME. Text 42/null becomes [42,null] rather than ["42","null"], changing downstream conditions and items. The [Argo controller](https://raw.githubusercontent.com/argoproj/argo-workflows/v4.1.4/workflow/controller/operator.go) maintains parameter aggregates as string lists.

Preserve strings separately from result JSON aggregation. Regression: numeric/null/object-looking/quoted parameter text across both aggregate access forms.

### ARG-08 (P2): Main completion replays older init-volume writes

`argo-runtime/src/volumes.rs:340` processes every completed Written slot after each init and main completion (`pod.rs:383`, `:441`). Init A writes old, independent B writes new, then A's main without that mount finishes and can restore old again.

Consume writeback per newly completed slot once. Regression: that controlled completion order must leave B's newer bytes available to the next reader.

### ARG-09 (P2): Literal HTTP bodies starting with @ become filenames

`argo-runtime/src/http.rs:60` uses curl --data, which interprets a leading @ as a file reference regardless of shell quoting. body='@literal' can fail or transmit image-file content instead of the requested bytes.

Use literal-body semantics such as --data-raw, with explicit binary handling where needed. Regression: capture a request containing a leading @ and require exact literal bytes.

### ARG-10 (P2): Accepted HTTP fields and retry timing are fabricated or ignored

`argo-parser/src/model.rs:510` accepts bodyFrom/timeoutSeconds without runtime consumers; `argo-runtime/src/http.rs:97` always supplies empty response.headers. `retry.rs:58` sets lastRetry.duration to zero, and node timing uses workflow creation time (`run.rs:789`, `:824`). Header-dependent conditions and duration-based retries can be wrong.

Implement supported fields/timing or reject unsupported contracts. Regression: bodyFrom, deadline, response-header condition, and a timed failure tested with a duration-dependent retry expression. Documented absent backoff sleeping is a separate intentional boundary.

### ARG-11 (P2): Required ConfigMap item keys can disappear silently

`argo-runtime/src/volumes.rs:288` skips missing requested keys instead of reporting the required missing file. A ConfigMap containing a while its items requires b can let fallback-driven tools compute different results.

Fail missing required keys and honor explicit optional behavior. Regression: present, missing-required, and missing-optional selected items.

## Input schema and CLI contracts

### SCH-01 (P2): Recursive input schemas can crash help or preprocessing

`input-schema/src/text.rs:109` recursively renders array items after $ref resolution without active-reference tracking. A valid tree array schema referring to itself can overflow the stack through `iunctus inputs -format text`. `values.rs:12` similarly traverses recursive allOf before validator creation (`check.rs:49`).

Terminate recursive rendering/preprocessing using active identities or bounded expansion without discarding valid recursive schemas. Regression: valid recursive help schema, recursive preprocessing, and nonrecursive nested schemas in guarded processes.

### SCH-02 (P2, behavioral validation needed): Alternative schemas turn missing simulated paths into fatal errors

`input-schema/src/check.rs:140` processes anyOf/oneOf children with simulation flags false and leaves the enclosing error fatal. A simulated CWL [File,Directory] input (`cwl-runtime/src/schema.rs:227`) can fail for a missing path while a plain File yields a warning.

Evaluate alternatives under simulated existence rules while preserving real type errors. Regression: missing file/directory union versus a direct file schema in simulated and real checks.

## Run database, resume, repeats, and provenance

### RUN-01 (P1): Directory input edits can reuse stale resumed outputs

`run-database/src/fingerprint.rs:30` records only the source directory's own size/mtime. Editing an existing descendant changes neither in the ordinary case. `iunctus/src/resume.rs:64` can return previous outputs despite changed directory data. This is distinct from intentionally preserving a regular file's timestamp.

Use descendant identity for directories or conservatively exclude them from resume reuse. Regression: modify one existing child after a run, then resume and require execution and changed outputs.

### RUN-02 (P1): Repeating a repeated run drops effective original options

`iunctus/src/record.rs:63` records process argv, which is repeat <old-id>, while execution uses reconstructed original arguments. `changes.rs:174` later rebuilds solely from that argv, not the separately recorded parameters/effective configs/input options. A second repeat can use defaults instead of the actual first repeat's settings.

Record effective run options separately or reconstruct ancestry faithfully. Regression: nondefault parameter plus named config/input file, then repeat twice and compare effective inputs and commands.

### RUN-03 (P1): RO-Crate export substitutes current bytes for historical inputs

`run-crate/src/lib.rs:108` places task inputs by current location; `data.rs:76` hashes/copies current bytes and `files.rs:45` publishes the new digest without comparing FileRow.digest. Replacing an input after the run can cause the exported action to attribute replacement bytes to its historical execution.

Resolve recorded content by its digest, verify it, or mark it unavailable instead of substituting current content. Regression: record A, replace with B, export, and require A or an explicit missing-history result, never B as the historical input.

### RUN-04 (P1): Shared-database liveness uses only the reader's local PID

`run-database/src/runs.rs:213`, `:222` checks /proc/<pid> without host/boot/start identity. Another node's active run can appear stopped; a reused unrelated PID can make a dead run appear running. This affects resume refusal, live pages, and running_runs protection (`keep.rs:33`), subject to separate store leases.

Use durable heartbeat/lease or verified host/process identity with conservative remote handling. Regression: shared database across hosts/PID namespaces and local PID reuse must not misclassify ownership.

### RUN-05 (P2): New network-filesystem database directories select the wrong journal mode

`run-database/src/lib.rs:88` asks journal(path) before creating the parent at :98. `journal.rs:64` treats failed statfs as local. Opening below a nonexistent NFS/CephFS subdirectory can therefore select WAL despite the intended shared-filesystem rollback mode.

Detect after directory creation or inspect its nearest existing ancestor. Regression: create a database beneath a new shared-FS subdirectory and verify the selected journal mode.

### RUN-06 (P2): Resume loses previously known output/input digest metadata

`run-database/src/resume.rs:104` reconstructs outputs without stored digest/size and defaults the input-digest map. The next recorded run loses identity needed by comparisons, provenance, and later recovery even though the original knew it.

Copy known output identity and reconstruct recorded input hashes. Regression: resume a fully hashed task and compare those fields in both runs.

### RUN-07 (P2): Concurrent repeats delete each other's restored sources

`iunctus/src/repeat.rs:48` gives every repeat of one original run the same cache/repeats/<run> directory; :141 removes it before restoration. A second repeat can delete/rewrite workflow/config files still used by the first.

Use invocation-owned restore locations or an immutable synchronized restoration. Regression: pause the first repeat after restoration, start the second, and verify the first can still read its exact sources.

### RUN-08 (P2): Export requires the old checkout even when a kept workflow copy exists

`iunctus/src/runs.rs:117`, `:261` resolves the old main file before stored sources; `run-crate/src/lib.rs:86` also requires the original path to be a file. Removing a checkout can prevent archival export while all kept source content survives.

Resolve the logical main from recorded sources and validate the chosen stored copy. Regression: remove only the checkout after a real retained run, then export successfully from store-backed sources.

### RUN-09 (P2): FTP credential redaction is inconsistent across artifacts

`storage/src/journal.rs:81` redacts password-bearing addresses, but `run-database/src/recorder.rs:163`, :185 serializes raw task/input.source. `run-page/src/report.rs:63` embeds task details and `run-crate/src/data.rs:70` retains raw entity URLs. ftp://user:password@host/file can consequently enter shareable HTML/metadata despite redacted remote-file records.

Redact credential-bearing addresses at artifact boundaries and keep replay credentials separate. Regression: a sentinel password must be absent from generated reports/crate metadata and public task details while execution still authenticates correctly.

## Run page and telemetry

### WEB-01 (P2): Completed runs truncate event backlogs

`run-page/src/live.rs:104` closes SSE on completed run state even when the fetched 10,000-event batch is full and later events remain. `assets/run.js:619` also closes on that state. A slow/reconnected subscriber can permanently miss final task states while the header says complete.

Drain the backlog before exposing terminal stream state, or refresh the complete view at completion. Regression: completed run with more than one batch after an early cursor must show every final task/state.

### WEB-02 (P2): Active SSE can prevent page shutdown

`run-page/src/lib.rs:101`, :134 gracefully shuts down then joins without a deadline; `live.rs:87` continues active-run streams without a shutdown signal. Ctrl-C can leave page serving stuck behind an open client.

Propagate server shutdown into SSE or bound the graceful interval. Regression: keep an active-run SSE client open and require bounded server termination.

### TEL-01 (P2): Collector responses can hang post-run completion or grow memory

`run-telemetry/src/send.rs:79` lacks a total request deadline and :87 reads the whole response before truncating error text. Its shared HTTP client has only connect/per-read limits (`storage/src/web.rs:38`), so an endless trickle avoids them. `iunctus/src/insight/telemetry.rs:132` waits synchronously after the run.

Use telemetry-specific total timeout and bounded response reads. Regression: slow infinite response and oversized error body must not keep workflow completion waiting or allocate proportional memory.

## Verification and release defects

### QLT-01 (P2): Config parity can pass with no pipeline comparisons

`iunctus/tests/config_test_workflows.rs:60` iterates only locally present pipelines, and :107 checks only an empty mismatch list. An existing empty corpus/pipelines directory succeeds with zero comparisons; a partial corpus silently reduces coverage.

Check the pinned expected inventory and fail incomplete coverage, rather than relying only on comparison failures. Regression: empty and missing-member corpora must fail the explicit parity gate.

### QLT-02 (P2): Conda corpus testing passes even if every parse fails

`conda-environment/tests/test_workflows.rs:60` collects failures only for printing; :76 asserts only that candidate files exist. An all-failing parser can still pass its corpus test.

Separate inventory output from regression gates and assert expected outcomes. Regression: a deliberately failing corpus must fail the invoked gate.

### QLT-03 (P3): Python container integration reports success when disabled

`snakemake-runtime/tests/python_container.rs:12` is an ordinary test that returns unless IUNCTUS_CONTAINER_TESTS is set. Captured skip output hides the absence of execution in normal results.

Make the prerequisite/skip explicit and enforce an integration invocation for release. The ordinary green test count must not be interpreted as container-bridge validation.

### QLT-04 (P3): Roadmap statements contradict current code and later completion records

`docs/roadmap.md:9` says no tasks run for real; :85 describes only serial local execution. Later lines :188-203 mark real/parallel execution and delivery complete, and `docs/README.md:44` exposes real backends. The linked gap inventory is therefore misleading.

Reconcile the introductory claims, tables, and historical completion entries with actual tested capability. Keep historical run counts labeled as historical evidence rather than refreshed review results.

## Missing capabilities and operational requirements

These gaps are separate from the defects above. Implementing them is future work, not an implicit instruction to patch or redesign the project.

| Missing or unresolved area | Current evidence and consequence | Production acceptance criterion |
| --- | --- | --- |
| Published support matrix | Decisions name four language subsets, while accepted constructs and runtime enforcement diverge. Plain-script workflows remain undefined. | Versioned language/feature/backend matrix distinguishing executed, simulated, rejected, and intentionally different behavior. |
| Enforced CI/release gates | No tracked CI workflow or release configuration was found; justfile contains intended recipes. Host-side CI settings were not queried. | Required checks run automatically against the release revision, with complete corpora and visible integration prerequisites. |
| Real backend qualification | Kubernetes and SLURM coverage primarily uses fake/simulated systems; local/Funnel historical evidence is documented but not refreshed here. | Real Docker/rootless Podman, Funnel/TES, Kubernetes, and SLURM qualification for each advertised configuration, including failures and ownership. |
| Durable remote reattachment | Accepted remote IDs live in attempt futures, without restart-driven reattach/cancel mapping. Finished-Job TTL does not clean active orphans. | Persist submission ownership and reconcile active jobs after restart, including ambiguous successful create responses. |
| Remote storage portability | Shared driver filesystem paths are explicit prerequisites (`docs/design/executors.md:25`). Ordinary remote-directory inputs and some remote listings remain unsupported. | Either portable upload/download staging with directory fidelity or precisely bounded shared-filesystem deployment requirements. |
| Conda-to-image completion | Current crate locks local repodata; image preparation remains future work (`docs/design/conda.md`). Virtual package ABI/version constraints are not fully validated. | Reproducible build recipe/base/platform/lock identity, immutable final digest, and tested environment compatibility, all installed inside containers. |
| Python environment reproducibility | Helpers can install unpinned packages and ignore transitive pins; custom system libraries require supplied images. | Locked supported package sets, recorded helper image/environment identity, and explicit limits for native-object interaction. |
| Network-aware shared caching | `docs/design/reproducibility.md:124` explicitly admits network tasks share and online results can answer offline tasks; default policy awaits a decision. | Decide and publish trust/reproducibility policy, then make it effective in identity/reuse rules. This acknowledged policy is not counted as an undisclosed implementation bug. |
| Exact historical repeat boundary | Local input data, live environment/provider configuration, and all image availability are not fully frozen. Replay intentionally prefers existing current inputs (`docs/design/runs.md:229`). | State which content/configuration is archived and fail or clearly distinguish a best-effort/current-input replay from an exact historical computation. |
| Distribution and support | No project-level license texts, root README, tracked installation/release packaging, or supported-host policy were found. Cargo licenses are consistent; demo licenses cover upstream examples. | Installable/verifiable binary artifacts, project license texts, supported platforms, upgrade/rollback procedures, and an operator guide. No legal conclusion is made here. |
| Local execution and page access | Explicit paths, rootless execution and the intentionally local run page need clear supported behavior. | Preserve the trusted local workflow model and document explicit executor permissions and page binding choices. |
| Backup, retention, and recovery | SQLite schema upgrades exist, but shared-store GC, DB recording failures, and archived export need fault evidence. | Tested backups/restoration, schema compatibility/rollback rules, retention under active readers, and operator-visible unresolved cleanup. |
| Dependency and artifact assurance | Lockfile pins dependencies, but no tracked advisory/license/provenance release gate was found. This review did not scan current advisories. | Run and record the chosen dependency/security/license checks for release artifacts; do not infer vulnerability-free status from compilation. |

## Performance and future-breakage risks

These are source-backed risks whose workload impact was not benchmarked in this review. They should not be presented as measured slowdowns.

| Risk | Evidence | Needed bound or experiment |
| --- | --- | --- |
| Quadratic pending bookkeeping | `workflow-graph/src/pending.rs:86`, :111 uses linear lookup/min scans and vector removal; real CLI defaults max_pending to unlimited (`iunctus/src/main.rs:311`). | Large scatter completion/cancellation with bookkeeping CPU and memory; define admission bounds and appropriate lookup/completion data structures when justified. |
| Eager fanout and retained graph state | CWL materializes scatter crossproducts and repeatedly clones gathered inputs (`cwl-runtime/src/run/step.rs:130`); Argo eagerly clones loop YAML; task/event graphs remain in memory. | Scaling at representative 10k/100k-task sizes, including peak memory, planner latency, and cancellation latency. Do not treat executor CPU slots as planner backpressure. |
| Whole-object staging and logs | Remote CAS staging calls get_content (`task-runner/src/staging.rs:103`); Outcome reconstruction loads and copies full stdout/stderr (`executor.rs:278`). | Large input/log tests under memory limits; decide bounded or file-backed log/result interfaces separately from small local fixes. |
| Telemetry duplicates run-scale data | `run-telemetry/src/spans.rs:38`, :66 loads all events and clones all spans into request chunks. Chunk count does not bound total memory. | Large-run trace peak memory and incremental export/sending requirements. |
| Simulator expansion remains broad | Shell/braces/output expansion can create large collections without a consistent global work budget. | Adversarial but finite expansion tests and explicit synthetic-work/output limits. |
| Directory retention closure is incomplete | `task-runner/src/clean.rs:176` expands children from kept cache directory outputs, not every arbitrary retained tree digest. Imported directory/no-cache output restoration needs a caller-level probe. | Retain a normal directory input/no-cache output, clean, then restore it and verify every child. Do not claim source-file loss: source references already include individual file digests (`run-database/src/keep.rs:59`). |
| Best-effort run leases weaken retention guarantees | `task-runner/src/runner.rs:524` permanently caches first lease-acquisition failure as None. Remote lookup/staging I/O is not generally selected against cancellation (`:502`). | First-acquire transient failure followed by recovery, plus slow store operations during cancellation/cleanup. Specify behavior when cleanup protection cannot be obtained. |
| Task/run identity is arrival/host sensitive | Nextflow task IDs use global arrival order rather than the design's canonical-input/occurrence identity. CLI run IDs use milliseconds and PID only (`iunctus/src/main.rs:897`). | Dynamic-order resume/replay probes and shared-host uniqueness expectations. These are design/validation risks, not demonstrated cache collisions from task IDs alone. |
| os.walk semantics are eager and follow links | `python-evaluator/src/modules/os.rs:388` materializes traversal, ignores followlinks/topdown, and cannot honor in-place dirs pruning. | Large directory/symlink/pruning cases under bounds. OS path limits can bound some cycles; universal nontermination was not established. |
| Image/user/platform defaults change over time | Registry platform selection, mutable image tags, and per-image UID handling interact with cache/ownership. | Multiarchitecture, rootless, tag-change, cold-cache, and mixed-UID release cases tied to exact image digests. |
| Model/schema evolution affects historical runs | TaskSpec FORMAT, serde deny_unknown_fields, stored task JSON, cache entries, and SQLite migrations are independently versioned. | Old-run export/resume/replay and old-cache rejection/migration across an explicit support window. Existing migration checks are useful but do not prove every historical payload path. |
| Recorder partial-failure handling needs fault injection | `run-database/src/recorder.rs:583` stores an action error while later code can commit earlier statements (:611). No normal supported-input trigger was established for inconsistent partial events. | Disk-full, constraint failure, process kill, and commit failure with transaction/event invariants. Keep this as an open risk until a causal reproducer exists. |

## Code style and organization

Formatting and naming preferences alone do not justify rewrites. The following organization issues matter because they already hide behavior or make correctness harder to verify.

1. **Accepted syntax is separated from enforcement.** Tags/directives/options pass parser/evaluator stages but disappear in task conversion or completion. Examples: ensure, cache false, env/stdin, HTTP timeouts, integrity flags, and parallelism. At each existing boundary, preserve implemented semantics or explicitly reject unsupported behavior.
2. **Error swallowing changes failures into valid-looking states.** `.ok()?`, default empty text/list, and literal-write early returns cause endless polling, skipped work, lost artifacts, and successful missing outputs. Relevant examples are REM-03, ARG-01/02/03, and CWL-07. Suppression needs a precise documented recovery meaning.
3. **Simulator accommodations leak into shared real-run helpers.** Argo items/output reading and some host mapping mix synthetic assumptions with real data. Keep mode requirements explicit at these existing call sites and test both modes with the same trigger.
4. **Cache policy is duplicated.** Lookup and execution already disagree on unreproducible markers (CAS-13); local eviction and invalidation share a helper despite different ownership semantics (CAS-10). Correct those contracts locally before considering broader abstractions.
5. **Path mapping is inconsistent across components.** Single-slash stripping, unchecked basename joins, hostPath subpaths, canonicalization, and symlink-preserving copies interpret locations differently. Fix each proven boundary and test a common alias/traversal/link matrix; a wholesale filesystem rewrite is not implied.
6. **Temporary-publication schemes are independently implemented.** Local CAS has a counter, while layered staging and folder upload use PID only. CAS-09 demonstrates the consequence. Unique operation identity is a behavioral requirement, not a naming preference.
7. **Mutable location and historical identity are mixed.** Export verifies kept workflow identity but places task inputs from current paths; repeat records invocation rather than effective arguments; resume drops digest fields. See RUN-02/03/06. Historical APIs need explicit identity contracts.
8. **Frontend and backend duplicate terminal-state decisions.** Both SSE server and page close on a run's ended state, causing WEB-01. Specify one backlog/completion protocol and verify both sides against it.
9. **Large semantic dispatch files make selective review harder.** Examples: `python-evaluator/src/modules/misc.rs` (2,227 lines), `snakemake-runtime/src/workflow.rs` (1,756), and Python builtins (1,706). Size is not itself a defect. When changing these modules for a real issue, keep fixes narrow and require examples covering dispatch/error behavior rather than reorganizing them speculatively.
10. **Runtime diagnostics often lose source spans.** Default-span operator/output errors can point away from the offending declaration. Preserve the location already available when fixing the corresponding semantic boundary.
11. **Oracle provenance is incomplete.** Python fixture comments identify CPython 3.11 while native pandas reports 3.0.6 (`python-evaluator/src/pandas/mod.rs:135`); exact package/executable versions used to record fixtures are not comprehensively recorded. Capture identities and supported versions before treating oracle outputs as permanent compatibility contracts.
12. **Documentation mixes plans, historical receipts, and present guarantees.** The roadmap, cache promises, cross-step grouping description, and test names such as real run can overstate actual coverage. Argo's hostPath "real" fixture uses a Scripted Simulator (`argo-runtime/tests/run.rs:16`), and Nextflow capture tests execute Bash directly on the host (`nextflow-runtime/tests/task.rs:49`). These tests have value but do not prove container/backend behavior.

Positive style evidence: workspace package/dependency settings are centralized; crate boundaries generally match responsibility; secret wrappers intentionally redact Debug; only a small number of production Clippy suppressions were found. There is no basis here for a broad style cleanup or wholesale reorganization.

## Review coverage

| Area | Crates and integration reviewed | Important limits |
| --- | --- | --- |
| Shared execution | workflow-graph, task-model, task-runner | Concurrency failures largely source-traced; only selected API boundaries reproduced. |
| Storage/cache | content-store, storage | No live S3/GCS/Azure/FTP qualification or barrier-driven distributed race runs. |
| Backends/environments | executor, tes-executor, kubernetes-executor, slurm-executor, conda-environment | No real cluster/rootless/container runs in this review. |
| Nextflow | nextflow-parser, nextflow-evaluator, nextflow-config, nextflow-plugins, nextflow-runtime | No fresh native-Nextflow differential corpus execution. |
| Snakemake/Python | snakemake-parser, snakemake-runtime, python-evaluator | Selected interpreter probes; no complete module/package/embedded-language parity audit. |
| CWL/Argo | cwl-parser, cwl-runtime, argo-parser, argo-runtime | Selected source traces; no complete real-command conformance qualification. |
| Run-facing systems | run-database, run-crate, run-page, run-telemetry, iunctus | No browser/live-service/NFS fault-injection run. |
| Cross-project quality | config-file, input-schema, manifests, justfile, docs, test harnesses | No demonstrated config-file defect; release-host configuration and current dependency advisories not queried. |

No full legacy-language parity, cluster deployment, arbitrary package compatibility, or vulnerability-free status is inferred from this coverage.

## Checks performed

The initial working tree was clean and the source revision was frozen. Tests ran with locked dependencies and offline resolution through the installed Cargo resource guard, with two build jobs. The first sandbox attempt could not reach systemd's bounded scope; the same guarded command succeeded with approved host access. The guard was preserved.

- `cargo test --offline --locked --workspace --lib -j 2`: passed all workspace library test targets at the reviewed code revision. This excludes integration tests, corpus gates, doctests, and the CLI binary.
- `cargo test --offline --locked -p iunctus --bin iunctus -j 2`: passed 58 CLI unit tests, with zero failures or ignored tests.
- `cargo +nightly-2026-09-14 fmt --all -- --check`: passed, with no source changes.
- Isolated probe executable: compiled from `/tmp/iunctus-review-probes.rs`, linked to newly built debug libraries selected from this run; no project source edits or stale release binary were used. Fixtures were confined to a new temporary directory. Each subprocess had an eight-second wall timeout, five-second CPU ceiling, two-GiB address-space ceiling, and core dumps disabled.

**Twenty issue observations were reproduced across fourteen isolated probe cases:**

| Finding | Observed result |
| --- | --- |
| LOC-01 | Double-slash copied destination changed only the temporary host victim from OLD to NEW. |
| LOC-02 | Child overlay changed the temporary original read-only input directory's config to NEW. |
| ENG-01 | Synthetic output created escaped.txt outside the simulator root. |
| ENG-02 | x/./x inputs validated and reversed input lists had the same key; staging-order consequences remain source-traced. |
| RUN-01 | Editing a directory's existing child left its resume fingerprint unchanged. |
| CAS-07 | CacheEntry::complete returned true with a manifest-named child missing. |
| CAS-08 | Fetching one child then its parent left sibling b absent. |
| CAS-10 | Front invalidation followed by replacement returned false and the shared entry stayed old. |
| SCH-01 | Valid recursive help schema aborted with native stack overflow, signal 6. |
| PY-01 | Maximum-i64 isqrt panicked at misc.rs:364 with multiplication overflow, exit 101; release nontermination is source-derived. |
| PY-02 | Cyclic-list printing aborted with native stack overflow, signal 6. |
| PY-03 | Mismatched set_index followed by .loc panicked at indexer.rs:254, exit 101. |
| PY-04 | Reversed-index arithmetic returned [11,22], instead of aligned [21,12]. |
| PY-05 | Reordered label mask selected [1], instead of [2]. |
| PY-06 | Duplicate-index horizontal concat returned shape (1,2), rather than preserving two rows. |
| PY-07 | decimal=',' returned the string '1,5', rather than numeric 1.5. |
| PY-08 | Named Polars filter returned both A and B. |
| PY-09 | next() raised ZeroDivisionError from the unconsumed generator tail. |
| PY-11 | Virtual append/read returned new, omitting old. |
| PY-12 | Reverse .loc returned [1,2,3], unchanged. |

Raw probe receipts are `/tmp/iunctus-review-probe-results.json`; CLI check output is `/tmp/iunctus-review-cli-tests.log`. These are local scratch receipts, not committed project artifacts. Probe crashes were intentional observations in isolated child processes; they did not modify project source or real user data. Other findings remain static unless explicitly qualified above.

The full all-targets/all-features suite, doctests, Clippy, ignored corpus tests, advisory scans, browser checks, real executor qualification, distributed lease races, and scalability benchmarks were not run. Existing passing unit tests do not invalidate these uncovered boundary failures.

## Remediation order and release decision

1. **Correct path handling and data identity first.** Prioritize LOC-01/02/03, CWL-01/02/03, ARG-06, CAS-01/02/03/04/05, and RUN-01/03. Each change should have a narrowly scoped byte/sentinel/interleaving regression and its own review.
2. **Own remote work until terminal cleanup.** Resolve REM-01/02/03/04/05/08 and durable restart reconciliation. Prove no new attempts overlap unresolved prior ownership.
3. **Stop silently accepting unsupported computation contracts.** Prioritize Nextflow env/stdin/cache/join, Snakemake checkpoint/ensure/storage, Argo real-loop/parallelism, and Python/dataframe semantic failures. Explicit rejection is safer and smaller where semantics are deferred.
4. **Bound planning, native evaluation, staging, and logs.** Fix reproduced crashes, add total evaluation budgets, stream large-file operations, and measure large dynamic graphs before selecting wider data-structure changes.
5. **Establish trustworthy release gates.** Fix empty/self-skipping harnesses, record oracle/corpus identities, qualify each advertised real backend/platform, update the support matrix/roadmap, and produce installable verified releases.

A production release should identify its supported workflow/backend/storage combinations and show passing exact-result, cancellation/restart, retention/corruption, rootless/ownership, and resource-bound checks for those combinations. Until then, classify this revision as an implemented prototype with meaningful capabilities and material unclosed correctness/reliability boundaries.

## Original review scope

This review creates only `.claude/report.md`. All recommendations remain unimplemented. No source, interface, dependency, test, configuration, formatting, or architecture was changed, and no unrelated cleanup was included. The report is committed in small signed sections; no push or pull request was requested or performed.

## Resumed remediation checkpoint, 2026-10-03

The source review remains bound to its original revision. The latest verified source checkpoint is `825751d6c511ccc20d45f94bfd428d2cfe4a33cb` on `fix/production-readiness`. Eighty-five resumed signed source commits bring the register to **112 implemented, eleven partial and two open findings**. Every source commit changes at most 500 lines and has verified signing/identity/date evidence. See [handoff.md](handoff.md) for all 125 finding states and exact remaining work. No push or pull request was performed.

The third-wave full matrix verifies 1,537 primary tests across 180 targets after two fixture-only focused corrections. Latest fourth-wave affected checks verify 903 primary tests across 109 targets, zero unresolved failures and 38 ignored external cases after SLURM fixture/parser and runner release-barrier corrections. The full pinned formatting and Clippy gates pass without suppressions. All 28 doctest targets completed, with zero embedded examples. The exact Conda corpus remains 2,053 files, 2,032 accepted and 21 exact known rejections. Seventeen earlier rootful Docker cases plus ordered-service and transitive-pin cases passed. Hosted CI, browser, rootless and live remote/backend qualification remain separate.

New committed corrections cover complete directory membership/parent/link/byte checks and manifest retention; conditional leases and local inode authority; fresh version-bound downloads and queued-copy cancellation ownership; full workflow package pins; scalar typed writes; one-shot failure handlers; lazy generator expressions/custom iterators; ordered service retry and early unsupported FIFO rejection; cooperative JavaScript work limits; Argo workflow/template admission; run process identity/shared-filesystem selection; terminal SSE draining/shutdown; public password redaction with stable resource identities; telemetry deadlines; unique persistent Nextflow operator scratch; SLURM durable submit/termination ownership; declared license texts, pinned CI and immutable Conda context APIs.

Fifth-wave affected checks verify 618 primary tests across 65 targets, zero unresolved failures and 13 ignored external cases, using final CWL/Snakemake reruns to replace earlier failed targets. Nested snapshot-permission child results are excluded. Full pinned formatting and workspace Clippy pass, followed by final focused Snakemake Clippy. This continuation did not run real backend/Docker, hosted CI, browser or artifact qualification. Test/development debug symbols were disabled while runtime assertions remained enabled.

The fifteen new signed source/test commits add provider-aware immutable download/version bundles, source-handle version association, directory descriptors/retention, owned transfer/staging temporaries, lease-loss cancellation and late-success rejection, deferred CWL writers and held aliases, Nextflow sorting callbacks/policies, local SLURM recovery admission, and evaluated Snakemake signatures/local protection. Durable incomplete markers prevent partial output delivery from being reused under prior signatures.

CAS-10 shared stale-entry repair and CAS-17 corruption recovery remain wholly open. Eleven partial findings retain full private tree hydration, local temporary cleanup, distributed atomic fencing, TES/Kubernetes library recovery admission, bounded emission, Nextflow directives/error callbacks, Snakemake lifecycle/resource/shadow/cache/handover contracts, CWL merged parent-directory views, and hard JavaScript limit work. Native hash/deep sorting, including collectFile default hashing, is explicitly refused. Remote destructive cleanup remains refused for expiring authority; persistent operator scratch still lacks engine-managed retention. Raw cache publication still precedes language validation and successful forced retries do not replace first-writer entries. Missing Conda container build/transport/CLI integration and external qualification remain separate gates.

The current toolchain also reports a future Rust incompatibility in `attribute-derive-macro 0.10.5` (expression-position macro trailing semicolon). Current compilation passes; upstream upgrade/compiler qualification remains. One linker SIGBUS attempt during disk pressure is recorded separately from code failures; the rerun passed after debug-only reclamation of completed task-generated binaries. Evidence and source/patch hashes are preserved in the locally excluded production-readiness evidence directory.

Isolated native Pending measurements at 30,000 items show median wall time 3.617 seconds before indexing and 0.084 seconds after, with peak RSS 5,032 and 10,684 KiB respectively. These are bookkeeping measurements with frozen source/compiler/binary/observer identities; full workflow performance remains unqualified. Exact shuffled selection remains linear. Raw executor cache publication precedes language validation, and a forced retry does not replace an existing first-writer entry; this remains visible alongside the unresolved network-dependent cache policy.

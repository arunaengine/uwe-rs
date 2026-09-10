<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Universal workflow execution audit

Audit date: 2026-10-07. Source revision: `f3256bf0598af219a712eb7001fb5da3a7a4f181`.
Aruna contract revision: `51c43dba96198542addb53d3375579c308de0b17`, inspected locally from Git objects.

## Assessment and boundaries

Iunctus has a substantial native Rust execution foundation: four language drivers, a shared dynamic graph, container executors, content-addressed storage, JSON Schema input descriptions and validation, a run database, a run viewer, provenance export and telemetry. It does not yet provide a universal workflow service or a complete schema-driven submission interface. Existing support is explicitly a subset of Nextflow, Snakemake, CWL and Argo.

The most immediate work is to close correctness gaps in reuse and validation, expose a reusable describe/validate/submit contract, and connect task and data identities to Aruna. Language expansion should follow explicit compatibility targets and reference results. All task and package execution remains container-only; native interpretation does not authorize host task execution.

This is a workspace-wide structural audit with focused source tracing, four independent reviews and a parent cross-check. The inventory contains 29 crates, 571 Rust source files and 180,039 source lines, including inline tests. It is not a claim that every line or every interleaving has been proved correct. No application build, test suite, container task, live backend, deployment or hosted CI was run for this audit. Confirmed means the source demonstrates the behavior; dynamic reproduction remains an acceptance step. Historical corpus percentages and test counts were not refreshed.

P1 means resolve before advertising the affected capability or admitting hosted Aruna workflows. P2 means an explicit feature expansion or operational improvement. A missing capability is kept separate from an accidental defect, including limitations intentionally documented in the current implementation.

The register contains 64 prioritized findings and capability requirements, plus three suspected cases. Each entry names current-source evidence, its consequence, a narrow next action and acceptance criteria. The Conda environment container cache is explicitly included as a user-required feature, with reuse/import and concurrency requirements below.

## Coverage

| Area | Workspace crates | Main paths traced |
| --- | --- | --- |
| Nextflow | `nextflow-parser`, `nextflow-evaluator`, `nextflow-runtime`, `nextflow-config`, `nextflow-plugins` | Parsing, lowering, configuration, native interpretation, channels, tasks, plugins and schemas |
| Snakemake and environments | `snakemake-parser`, `snakemake-runtime`, `python-evaluator`, `conda-environment` | Rules, Python evaluation, checkpoints, grouping, package bridge, validation and environment images |
| CWL, Argo and schemas | `cwl-parser`, `cwl-runtime`, `argo-parser`, `argo-runtime`, `input-schema` | Loading, requirements, expressions, control flow, schema extraction and input checking |
| Shared execution | `workflow-graph`, `task-model`, `task-runner`, `executor`, `content-store`, `tes-executor`, `slurm-executor`, `kubernetes-executor` | Task identity, graph admission, cache reuse, input snapshots, outputs, leases, cancellation and remote ownership |
| Application and integration | `iunctus`, `config-file`, `storage`, `run-database`, `run-page`, `run-crate`, `run-telemetry` | CLI assembly, validation gates, credentials, object versions, resume/repeat, persistence, viewer, exports and telemetry |
| Delivery and evidence | Root manifests, `scripts`, `containers`, `.github`, `justfile`, docs and representative fixtures/tests | Packaging, qualification prerequisites, support claims and existing regression coverage |

## Confirmed defects in application reuse and data handling

### D01 [P1, confirmed] Resume trusts output existence rather than output content

Evidence: [iunctus/src/resume.rs:65](../iunctus/src/resume.rs#L65). A matching lineage result is returned when all output paths exist. Recorded output digests, file kinds and directory manifests are not checked here. Replacing an existing output with different bytes can therefore return success with the old digest and outcome, bypassing the runner's integrity checks.
Smallest fix: verify each candidate against its recorded digest and kind before reuse; otherwise execute normally. Acceptance: change a file, a directory descendant and a file/directory kind while preserving their paths; all must become misses, with an unchanged result still reused.

### D02 [P1, confirmed] Lineage reuse has weaker input and image identity than the shared cache

Evidence: [run-database/src/fingerprint.rs:23](../run-database/src/fingerprint.rs#L23), [iunctus/src/resume.rs:65](../iunctus/src/resume.rs#L65), [task-runner/src/request.rs:77](../task-runner/src/request.rs#L77). The fingerprint uses local size/mtime and the original task description; URLs have no input state. Resume answers before the backend resolves an image tag or the runner hashes current input bytes. An input replaced with same-size bytes and restored mtime, or a changed image behind the same tag, can reuse the previous computation. This is a deliberate metadata fingerprint, but it breaks the stronger universal reuse guarantee.
Smallest safe first fix: refuse candidates without immutable image/input identities, then compare recorded and current identities. Acceptance: same-size/same-mtime input changes, changed remote object versions and retagged images must not reuse stale outcomes. Do not weaken the normal content cache to match resume.

### D03 [P1, confirmed] Telemetry bypasses existing public-text redaction

Evidence: [run-telemetry/src/spans.rs:191](../run-telemetry/src/spans.rs#L191), [iunctus/src/insight/telemetry.rs:96](../iunctus/src/insight/telemetry.rs#L96), [run-database/src/public.rs:9](../run-database/src/public.rs#L9). Trace generation copies private run parameter values into OTLP attributes and sends them without the public projection. A parameter such as a credential-bearing FTP URL can expose its password to the collector even though the viewer's JSON projection masks URL passwords.
Smallest fix: apply the existing safe projection at trace generation/export, preserving private replay records. Acceptance: inspect both written traces and HTTP payloads for credential-bearing URLs; retain readable non-secret metadata. Sensitive ordinary parameters and signed URL queries also require the explicit policy in A05.

### D04 [P1, confirmed integration mismatch] Versioned S3 output addresses are interpreted as object keys

Evidence: [storage/src/address.rs:44](../storage/src/address.rs#L44), [storage/src/objects.rs:236](../storage/src/objects.rs#L236), and Aruna [api/src/routes/execution/tes.rs:1883](https://github.com/arunaengine/aruna/blob/51c43dba96198542addb53d3375579c308de0b17/api/src/routes/execution/tes.rs#L1883). Aruna returns exact output URLs such as `s3://bucket/key?versionId=...`. Iunctus splits the address into bucket/key without parsing the query, and calls object `stat`/`reader_with` with that key. A configured object provider therefore requests a literal query-bearing key rather than the recorded object version.
Smallest fix: represent bucket, key and version separately and use version-aware provider reads. Acceptance: retrieve an old Aruna output after another write to the same key; verify exact bytes and version identity. Keep literal key characters and URL escaping unambiguous.

### D05 [P1, fault-path finding] Recorder actions have no rollback boundary

Evidence: [run-database/src/recorder.rs:562](../run-database/src/recorder.rs#L562), [run-database/src/recorder.rs:615](../run-database/src/recorder.rs#L615). One logical event executes several SQL statements inside a shared batch. If a later statement fails without aborting the whole SQLite transaction, the writer remembers the error but retains earlier changes; the flusher or close still attempts COMMIT. A failed requested-event edge insertion, for example, can leave task/input rows without its matching event. A normal supported-workflow trigger was not reproduced.
Smallest containment: an action savepoint with rollback on action failure, preserving completed earlier actions. Acceptance: inject a constraint failure after the first successful statement and assert no partial event state survives; separately cover disk-full and commit failure. Treat occurrence frequency and recovery consequences as unqualified until those probes run.

## Application, persistence and hosted integration gaps

### A01 [P1, missing] No reusable application facade or workflow control API

Evidence: [iunctus/src/main.rs:234](../iunctus/src/main.rs#L234), [iunctus/src/main.rs:474](../iunctus/src/main.rs#L474), [iunctus/Cargo.toml](../iunctus/Cargo.toml), [run-page/src/api.rs:23](../run-page/src/api.rs#L23). Public core traits exist, but application assembly, validation, configuration, recording and executor choice are private binary modules. The HTTP router is a run viewer with GET routes, without describe, validate, submit or cancel operations.
Minimum addition: a thin Rust application facade over the existing crates, plus a versioned workflow service in Aruna. Acceptance: CLI and service callers use the same description and validation path, explicit real/simulated mode, cancellation and result contract. Implement the public workflow operations using [GA4GH WES](https://ga4gh.github.io/workflow-execution-service-schemas/docs/), with schema/form operations as explicit extensions.

### A02 [P1, confirmed mismatch] The current TES backend cannot submit to current Aruna TES

Evidence: [tes-executor/src/backend.rs:505](../tes-executor/src/backend.rs#L505), [tes-executor/src/backend.rs:546](../tes-executor/src/backend.rs#L546), [tes-executor/src/backend.rs:577](../tes-executor/src/backend.rs#L577), and Aruna [api/src/routes/execution/tes.rs:535](https://github.com/arunaengine/aruna/blob/51c43dba96198542addb53d3375579c308de0b17/api/src/routes/execution/tes.rs#L535). Iunctus stages the wrapper/inputs through `file://`, collects work/control as DIRECTORY outputs, requests volumes and sends only `iunctus-attempt`. Aruna requires S3 object URLs, rejects directories and volumes, and requires a group tag for bearer submission. Its task I/O paths also cannot overlap.
Minimum addition: an Aruna executor/data adapter with staged wrapper/control files, explicit file outputs or a supported packed directory contract, caller group and stable idempotency tags. A native Aruna job adapter can preserve richer exact-reference inputs than its TES facade. Acceptance: submit, retrieve exact versions, cancel and recover one real four-language workflow through Aruna without coordinator host-path assumptions.

### A03 [P1, missing] Tenant authorization and cache isolation are absent

Evidence: [run-database/src/schema.rs:22](../run-database/src/schema.rs#L22), [run-page/src/api.rs:23](../run-page/src/api.rs#L23), [storage/src/backend.rs:39](../storage/src/backend.rs#L39), [storage/src/downloads.rs:27](../storage/src/downloads.rs#L27). The viewer exposes the shared database without per-run authorization. Download identities exclude credentials, and an existing copy is returned without another authorized remote read. Sharing that cache across callers can let a caller read a copy obtained using a different caller's access.
Minimum addition: Aruna-owned run authorization and authorization-scoped local caches, with explicit permission checks before any cross-principal reuse. Keep credentials outside hash keys; use a stable non-secret security scope. Acceptance: two users with different object and run permissions cannot read each other's cached data, logs, exports or events, including after permission revocation.

### A04 [P1, missing] Hosted execution needs durable admission and persistence policy

Evidence: [run-database/src/recorder.rs:59](../run-database/src/recorder.rs#L59), [iunctus/src/main.rs:411](../iunctus/src/main.rs#L411), [run-database/src/lib.rs:125](../run-database/src/lib.rs#L125). Recording becomes best effort after a write failure; the CLI reports the error and computation can still succeed. SQLite uses NORMAL synchronization. These choices suit the documented local trust model, but do not establish durable service admission, restart reconciliation or a durable terminal result.
Minimum addition: persist authenticated workflow admission and ownership before dispatch; define a hosted failure policy for lost recording and acknowledged transitions. Reuse existing observer events with an Aruna durable sink. Acceptance: disk-full, recorder failure, abrupt process exit and power-loss recovery cannot acknowledge an unrecorded run or duplicate an already-owned remote attempt.

### A05 [P1, missing] Secret values need an explicit artifact and form policy

Evidence: [run-database/src/public.rs:13](../run-database/src/public.rs#L13), [run-page/src/files.rs:24](../run-page/src/files.rs#L24), [run-crate/src/lib.rs:184](../run-crate/src/lib.rs#L184). The public projection masks URL authority passwords only. Signed query tokens and ordinary parameter values remain readable; whole log and output downloads serve actual file contents. This is not a complete secret boundary for shared reports or hosted workflows.
Minimum addition: explicit secret parameter metadata, a credential reference broker, field-aware public metadata projection and artifact authorization. Treat outputs/logs as protected data, with an intentional export decision. Acceptance: form secrets, query credentials and task environment secrets stay out of shared metadata, reports and telemetry; authorized private replay still works.

### A06 [P1, missing] Process-global configuration prevents straightforward in-process hosting

Evidence: [iunctus/src/main.rs:906](../iunctus/src/main.rs#L906), [storage/src/web.rs:35](../storage/src/web.rs#L35), [storage/src/web.rs:53](../storage/src/web.rs#L53), [iunctus/src/repeat.rs:65](../iunctus/src/repeat.rs#L65). Cache/proxy/client setup is set once per process, and repeat changes the process working directory. Reusing CLI composition concurrently inside Aruna would mix incompatible assumptions about directories and network settings.
Minimum addition: pass run context into the facade, or initially supervise one isolated native worker process per run. Preserve core crates; avoid reusing global CLI setup in concurrent request handlers. Acceptance: two simultaneous runs with different roots, proxies, credentials and caches remain independent, including repeat and schema discovery.

### A07 [P1, known behavior] Cached remote URLs have no fresh-read policy

Evidence: [storage/src/downloads.rs:72](../storage/src/downloads.rs#L72), especially line 89, and [storage/src/downloads.rs:180](../storage/src/downloads.rs#L180). Once a ready bundle exists, later runs return it without revalidating the remote version. This implements the documented 'download once' policy. A floating URL updated between runs nevertheless keeps supplying the old content, and retained credentials/permissions are not rechecked.
Minimum addition: distinguish immutable/version-pinned references from floating addresses; revalidate floating resources at new-run admission or require an explicit stale/offline policy. Acceptance: changed ETags/object versions are visible to a new floating run, while repeat reads the originally pinned bytes without network drift.

### A08 [P1, known limitation] Repeat can proceed without complete historical inputs

Evidence: [iunctus/src/repeat.rs:219](../iunctus/src/repeat.rs#L219), [iunctus/src/repeat.rs:48](../iunctus/src/repeat.rs#L48), [task-runner/src/inputs.rs:39](../task-runner/src/inputs.rs#L39). Missing retained remote bytes fall back to the address after a note. Repeat restores changed workflow/config sources and pins retained remote files, but it does not restore all changed external local task inputs; local input import is optional. A repeat may therefore use today's input data while being described as exact.
Minimum addition: an explicit exact-repeat preflight that refuses missing identities/bytes, plus a separately named best-effort replay mode. Acceptance: remove retained remote content or change an external local input; exact repeat either restores the recorded bytes or refuses before dispatch.

### A09 [P2, missing] Large-run APIs and telemetry need bounded result streaming

Evidence: [run-page/src/api.rs:21](../run-page/src/api.rs#L21), [run-page/src/api.rs:86](../run-page/src/api.rs#L86), [run-telemetry/src/spans.rs:25](../run-telemetry/src/spans.rs#L25), [run-page/src/report.rs:44](../run-page/src/report.rs#L44). The run list truncates at 200 without pagination; a run view loads all tasks; telemetry materializes all tasks, attempts and events before chunking requests; reports materialize per-task detail. Request chunking does not bound whole-run memory.
Minimum addition: stable run/task/event pagination and streamed export, retaining the current SSE cursor contract. Acceptance: a large fan-out workflow can be browsed and exported under a fixed memory budget, and every run remains discoverable beyond the first 200.

### A10 [P1, missing] Engine build identity and Aruna data identity are not a single contract

Evidence: [iunctus/src/record.rs:72](../iunctus/src/record.rs#L72), [task-model/src/spec.rs:13](../task-model/src/spec.rs#L13), [storage/src/journal.rs:23](../storage/src/journal.rs#L23), and Aruna [core/src/structs/execution/job/input.rs:182](https://github.com/arunaengine/aruna/blob/51c43dba96198542addb53d3375579c308de0b17/core/src/structs/execution/job/input.rs#L182). Runs record an `iunctus 0.1.0` program label; content uses SHA-256. Aruna captured inputs carry source node, object version and raw BLAKE3. A package version or one digest field cannot stand for all those identities.
Minimum addition: record the engine source/binary identity, workflow bundle/schema identity, exact object version, algorithm-qualified content digests and resolved image/platform identity. Preserve Iunctus SHA-256 and Aruna BLAKE3 as separate fields. Acceptance: audit/replay distinguishes two engine builds with the same package version and two versions of an Aruna object without conflating content or ciphertext identities.

## JSON Schema validation and input forms

### S01 [P1, missing] There is no schema-driven submission form

Evidence: [iunctus/src/inputs.rs:64](../iunctus/src/inputs.rs#L64), [run-page/src/api.rs:23](../run-page/src/api.rs#L23), [run-page/assets/index.html](../run-page/assets/index.html). Input descriptions are available as CLI JSON/text, while the web page displays existing runs. It has no workflow selection, profile-sensitive input form, file picker, validation action or submit action.
Minimum addition: expose a versioned input description with JSON Schema, effective defaults, validation policy, optional UI hints and capability diagnostics; render it in the Aruna portal. A standard schema renderer such as [JSON Forms](https://jsonforms.io/docs/) can supply controls, with custom Aruna object/table widgets. Acceptance: one form submits typed inputs for each supported language and shows server field errors before admission.

### S02 [P1, intentional limit] Validation is not uniformly mandatory

Evidence: [input-schema/src/check.rs:66](../input-schema/src/check.rs#L66), [workflow-graph/src/inputs.rs:36](../workflow-graph/src/inputs.rs#L36). Every problem of an advisory schema becomes a warning, and intentionally ignored parameters skip value errors. Schema discovery may already evaluate workflow/configuration code. The present policy preserves native workflow behavior, but cannot be presented as strict universal admission.
Minimum addition: keep compatibility mode and offer an explicit service admission policy that enforces the submitted contract before task dispatch. Define behavior for ignored, unknown and read-only fields without silently changing native defaults. Acceptance: invalid inferred/advisory inputs are rejected in strict hosted mode, compatibility mode retains its documented behavior, and schema discovery runs under the same authorization/budget boundary as planning.

### S03 [P1, missing] Remote resources and sample-sheet contents are not checked generically

Evidence: [input-schema/src/paths.rs:47](../input-schema/src/paths.rs#L47), [input-schema/src/check.rs:48](../input-schema/src/check.rs#L48), [cwl-runtime/src/files.rs:370](../cwl-runtime/src/files.rs#L370). Path checking skips remote addresses and globs; the generic validator does not read a file's custom `schema` annotation. CWL's remote existence check returns true for any non-local location. Language-specific readers can still validate later, but generic admission cannot establish that the selected file exists, is readable or has valid rows.
Minimum addition: a provider-aware validation phase with bounded content readers, row/column diagnostics and explicit deferred/offline states; resolve Aruna authorization and exact versions here. Acceptance: missing/inaccessible remote inputs and invalid local/remote CSV, TSV, JSON and YAML content fail before compute, with a stable object version carried into execution.

### S04 [P1, missing] Validation errors lose machine-readable locations and branches

Evidence: [input-schema/src/check.rs:29](../input-schema/src/check.rs#L29), [input-schema/src/check.rs:60](../input-schema/src/check.rs#L60). `Problem` retains severity, a display-style input name and human text. The original instance pointer, schema pointer, keyword and nested alternative errors are discarded. Dot/bracket names can also be ambiguous for literal property names.
Minimum addition: a serializable error contract with JSON pointers, stable codes/keywords and branch/row context, while retaining readable messages. Acceptance: array, escaped property, required, conditional and `oneOf` failures highlight the correct control without parsing messages; secret values are not echoed into public errors.

### S05 [P1, contract gap] Validation checks a coerced copy without returning execution values

Evidence: [input-schema/src/check.rs:49](../input-schema/src/check.rs#L49), [input-schema/src/values.rs:13](../input-schema/src/values.rs#L13), [iunctus/src/inputs.rs:23](../iunctus/src/inputs.rs#L23). The validator coerces a cloned value tree and returns only problems. The CLI records the original `Inputs.values`. A generic caller cannot obtain the authoritative normalized object or prove that execution uses the representation that passed checking.
Minimum addition: return normalized values with validation results, with explicit CLI coercion versus typed-JSON behavior; apply defaults only under a stated language/service policy. Acceptance: nested numbers, booleans, unions, nulls and defaults use identical values in the form, admitted request, driver and recorded provenance. JSON Schema `default` and standard `format` are annotations under the default vocabulary, so do not silently promise automatic default insertion or format assertion. [JSON Schema validation](https://json-schema.org/draft/2020-12/json-schema-validation)

### S06 [P2, confirmed projection limit] Help/provenance property discovery loses composition semantics

Evidence: [input-schema/src/properties.rs:20](../input-schema/src/properties.rs#L20), [input-schema/src/properties.rs:48](../input-schema/src/properties.rs#L48). Discovery traverses direct `allOf` groups, resolves one local reference and keeps the first property with a name. Nested compositions are omitted; a root `required` applied to a property declared only in a group can be missed. The full validator remains independent and can be correct while displayed metadata is incomplete.
Minimum correction: a bounded recursive metadata projection that combines required state and preserves overlapping constraints, or use the original schema directly in the form renderer. Acceptance: nested/ref/conditional schemas and duplicate property constraints agree between rendered help, required markers, validation and provenance.

### S07 [P1, confirmed schema mismatch] Argo schema omits required artifacts and effective template defaults

Evidence: [argo-runtime/src/schema.rs:36](../argo-runtime/src/schema.rs#L36), [argo-runtime/src/schema.rs:68](../argo-runtime/src/schema.rs#L68), [argo-runtime/src/checks.rs:84](../argo-runtime/src/checks.rs#L84), [argo-runtime/src/run.rs:467](../argo-runtime/src/run.rs#L467). Artifact properties are not added to `required`, although the runtime requires an unsupplied non-optional artifact. Schema generation uses the raw entry template while execution merges `templateDefaults`. Missing mandatory artifacts can pass schema checking, and displayed defaults/required state can differ from execution.
Smallest fix: derive description from the same effective entry template and artifact rules as execution. Acceptance: required/optional artifacts and template-default parameters have matching schema values, form controls and runtime admission outcomes. Artifact override support must be explicit where the schema currently says `readOnly`.

### S08 [P2, confirmed schema mismatch] Argo artifact values are incompletely described

Evidence: [argo-runtime/src/schema.rs:214](../argo-runtime/src/schema.rs#L214), [argo-runtime/src/inputs.rs:185](../argo-runtime/src/inputs.rs#L185). Runtime location conversion includes Azure/HDFS/OSS shapes absent from schema conversion; raw artifacts are described as empty `data:,` regardless of actual content. This does not mean every resulting backend URL is executable by storage, but it does mean description and runtime disagree.
Smallest fix: keep artifact-location description consistent with effective runtime locations and supported provider capabilities. Acceptance: every advertised artifact kind has the same value/default in the description and admitted run, and unsupported providers are diagnosed before dispatch.

### S09 [P1, confirmed schema mismatch] Nested CWL file fields lose declared details

Evidence: [cwl-runtime/src/schema.rs:205](../cwl-runtime/src/schema.rs#L205), [cwl-runtime/src/inputs.rs:251](../cwl-runtime/src/inputs.rs#L251). Record-field schema generation preserves the type, label and doc but omits the field's format and secondary-file declarations. Runtime checks those declarations. A form cannot describe or collect the full nested file requirement from the emitted schema.
Smallest fix: apply existing file-detail conversion to record fields. Acceptance: nested record files retain format/secondary-file metadata and prompt for the same companion objects that execution requires; content/object checks remain the separate provider-aware phase in S03.

### S10 [P2, confirmed conversion limit] CWL BigInteger projection can round or become null

Evidence: [cwl-runtime/src/schema.rs:263](../cwl-runtime/src/schema.rs#L263). Internal `BigInteger` values are converted through `f64` into JSON numbers. Values outside the exact/range limits can be rounded or turn into null, changing schema defaults or recorded input descriptions. Ordinary i64 integers use a separate exact branch and are not implicated by this finding.
Smallest fix: preserve an agreed exact-number representation or reject values that the public schema contract cannot represent. Acceptance: internal large-integer defaults/values round-trip exactly or fail explicitly; include valid CWL long bounds and distinguish out-of-range typed inputs.

### S11 [P1, confirmed incomplete description] Missing external sample schemas disappear without a diagnostic

Evidence: [nextflow-runtime/src/inputs.rs:39](../nextflow-runtime/src/inputs.rs#L39), [nextflow-runtime/src/inputs.rs:74](../nextflow-runtime/src/inputs.rs#L74). Sample-schema embedding converts read/parse failure to `None` and leaves the original `schema` filename in place. Description succeeds without indicating that a required file-content contract could not be embedded. A later plugin may fail, but the universal description already looks complete.
Smallest fix: expose a path-specific embedding error or incomplete-schema diagnostic and prevent strict admission against an incomplete contract. Acceptance: missing, malformed and invalid-draft sample schemas cannot be reported as successfully resolved; valid nested/reference schemas retain their resource identities.

## Native Nextflow gaps

### N01 [P1, confirmed] Glob listing failures become empty channels

Evidence: [nextflow-evaluator/src/interpreter/glob.rs:77](../nextflow-evaluator/src/interpreter/glob.rs#L77), [nextflow-runtime/src/build.rs:755](../nextflow-runtime/src/build.rs#L755). `fromPath` continues after any host listing error. Unless `checkIfExists` catches an empty final result, a denied bucket listing, network timeout or failed subtree listing can silently skip inputs and let the workflow succeed with incomplete work.
Smallest fix: propagate genuine provider/I/O errors, retaining the native behavior for an ordinary no-match path. Acceptance: absent local prefixes remain compatible, while denied/failed remote listings and failed recursive subtrees cannot turn into successful empty/partial channels.

### N02 [P1, confirmed] Malformed resource directives remove limits

Evidence: [nextflow-runtime/src/task.rs:101](../nextflow-runtime/src/task.rs#L101). Failed parsing of active CPU, memory, disk, time or accelerator values becomes `None`; the runtime can therefore publish a task with executor defaults rather than the requested resource constraint. Numeric negatives are also clamped instead of diagnosed.
Smallest fix: reject invalid active resource values during task construction. Acceptance: malformed and dynamically invalid values fail before task publication; valid units and native numeric semantics remain supported on every backend.

### N03 [P1, confirmed] nf-schema conversion does not resolve reference-defined fields

Evidence: [nextflow-plugins/src/nf_schema/samplesheet.rs:27](../nextflow-plugins/src/nf_schema/samplesheet.rs#L27), [nextflow-plugins/src/nf_schema/samplesheet.rs:78](../nextflow-plugins/src/nf_schema/samplesheet.rs#L78). Row validation uses the full JSON schema, but conversion reads raw `items`/property definitions. An `items` reference to a reusable row can validate and then produce an empty field list; referenced path/meta fields can lose their intended conversion.
Smallest fix: resolve the conversion schema with bounded reference handling, or reject unsupported conversion shapes explicitly. Acceptance: referenced rows, referenced path/meta fields, defaults and composed schemas produce the same tuple and metadata values as the pinned plugin.

### N04 [P2, explicit limit] Entry selection and remote project acquisition are incomplete

Evidence: [nextflow-runtime/src/driver.rs:107](../nextflow-runtime/src/driver.rs#L107), [iunctus/src/detect.rs:42](../iunctus/src/detect.rs#L42). A non-empty launch target list is refused by the Nextflow driver, and target detection expects local files/directories. Running a named entry or a Git/registry workflow reference is not an end-to-end application capability.
Minimum addition: native entry selection and pinned workflow acquisition with a manifest of all resolved modules/configs. Acceptance: named workflows, immutable repository revisions, remote imports, submodules and offline retained bundles select the same code without mutable-reference drift.

### N05 [P1 for affected workflows, explicit limit] Common operators and collectFile default sorting are missing

Evidence: [nextflow-runtime/src/build.rs:28](../nextflow-runtime/src/build.rs#L28), [nextflow-runtime/src/build.rs:1082](../nextflow-runtime/src/build.rs#L1082), [nextflow-runtime/src/operator.rs:567](../nextflow-runtime/src/operator.rs#L567). Operator recognition exceeds implemented dispatch, including names such as `randomSample`, `chain`, `groupBy` and `mean`. `collectFile` refuses native default `hash` and `deep` sorting. These paths report unsupported, but remain barriers to ordinary pipelines.
Minimum addition: complete prioritized operator/sort semantics and expose actual operator support in preflight. Acceptance: pinned reference cases cover empty/value/queue channels, duplicates, order, dynamic emission, cancellation and native hash/deep ordering, without substituting a convenient sort order.

### N06 [P2, explicit limit] Groovy/JVM APIs and plugin contracts cover a finite subset

Evidence: [nextflow-evaluator/src/interpreter/access.rs:641](../nextflow-evaluator/src/interpreter/access.rs#L641), [nextflow-runtime/src/loader.rs:347](../nextflow-runtime/src/loader.rs#L347), [nextflow-plugins/src/lib.rs:66](../nextflow-plugins/src/lib.rs#L66). Static native shims and selected plugin functions cannot execute arbitrary Java classes, JVM plugins or every helper called by upstream utility modules. A recognized plugin/function name is not full plugin parity, and plugin version compatibility needs an explicit contract. The upstream plugin ecosystem remains broader. [Nextflow plugin registry](https://registry.nextflow.io/plugins)
Minimum addition: publish tested plugin/API versions and add native shims or container-bounded helpers for high-value calls. Acceptance: real utility modules and terminal hooks match pinned upstream behavior; unsupported imports/functions fail with useful source diagnostics before dependent work where possible. External commands remain container-only.

### N07 [P1 for hosted scale, known limit] Channel data and native helpers are eager

Evidence: [nextflow-evaluator/src/interpreter/glob.rs:69](../nextflow-evaluator/src/interpreter/glob.rs#L69), [nextflow-runtime/src/channel.rs:116](../nextflow-runtime/src/channel.rs#L116), [nextflow-runtime/src/process.rs:234](../nextflow-runtime/src/process.rs#L234), [nextflow-runtime/src/split.rs:101](../nextflow-runtime/src/split.rs#L101). Glob results, channel factories, `each` choices and split contents materialize input-scale collections. Task admission backpressure does not itself bound these buffers or Cartesian fan-out.
Minimum addition: explicit planning budgets, lazy sources and spillable collection/grouping where required by workload size. Acceptance: large globs, Cartesian fan-out, splits and grouping either stay within a fixed coordinator budget or fail clearly and promptly on cancellation.

## Native Snakemake, Python and environment gaps

### P01 [P1, confirmed] Conda-only rules publish fake image references

Evidence: [snakemake-runtime/src/task.rs:688](../snakemake-runtime/src/task.rs#L688). A rule with a Conda environment and no container becomes an image named `conda-env:<environment path>`. No image build/resolution occurs there. It can evade missing-image diagnostics and reach the backend as an unusable registry reference, or fail with an unrelated image error.
Smallest safe fix before builder integration: explicitly reject unresolved active Conda environments. Acceptance: Conda-only rules receive an accurate diagnostic through check and real execution, then use a resolved digest once P07 is implemented; simulation never counts as an image-build result.

### P02 [P1 for hosted locking, confirmed] Offline repodata loading lacks path-component validation

Evidence: [conda-environment/src/repodata.rs:71](../conda-environment/src/repodata.rs#L71), [conda-environment/src/repodata.rs:97](../conda-environment/src/repodata.rs#L97), [conda-environment/src/fetch.rs:26](../conda-environment/src/fetch.rs#L26). Fetch validates channel/platform components, but direct index loading joins unvalidated channel and subdirectory paths. Crafted absolute or parent components can read `repodata.json` outside the selected cache. This finding establishes an unexpected metadata read, not arbitrary code execution.
Smallest fix: enforce the same component/containment policy at the loading boundary, preserving legal multi-component channels such as `pkgs/main`. Acceptance: parent/absolute components and escaped cache paths are refused; normal channels and documented offline caches still work.

### P03 [P1, confirmed] Native os.walk can follow directory symlink cycles

Evidence: [python-evaluator/src/modules/os.rs:388](../python-evaluator/src/modules/os.rs#L388), [python-evaluator/src/host.rs:235](../python-evaluator/src/host.rs#L235). Walk materializes its whole result and classifies children through following metadata, without a visited set or native `followlinks`/pruning semantics. A real directory link to an ancestor can grow the walk without reaching the Python statement budget.
Smallest containment: preserve symlink identity and enforce bounded traversal; then implement native topdown/pruning semantics. Acceptance: ancestor cycles, broken links, default followlinks behavior, bottom-up order, pruning and cancellation match the documented [Python walk contract](https://docs.python.org/3/library/os.html#os.walk).

### P04 [P1, confirmed] Filesystem mutators return success without performing the operation

Evidence: [python-evaluator/src/modules/os.rs:195](../python-evaluator/src/modules/os.rs#L195). `os.symlink`, `os.link`, `os.chmod` and `os.utime` return None without changing the native virtual filesystem. Workflow code can take a success branch and later observe an unchanged file state.
Smallest fix: implement the supported virtual operation or raise explicit unsupported errors. Acceptance: existence/link identity, mode and timestamp checks after each call agree with the supported Python contract. Do not make host tool execution a fallback.

### P05 [P1, confirmed identity gap] Python package environments omit immutable base/platform identity

Evidence: [snakemake-runtime/src/python.rs:19](../snakemake-runtime/src/python.rs#L19), [snakemake-runtime/src/python.rs:133](../snakemake-runtime/src/python.rs#L133), [snakemake-runtime/src/python.rs:186](../snakemake-runtime/src/python.rs#L186). The environment directory key is a 64-bit FNV of a mutable base-image tag, preparation script and requested package/constraint strings. Existing directories are reused without a resolved image/platform/dependency manifest in the key.
Minimum correction: bind environments to immutable image digest, Python ABI, target platform and resolved dependency identity, with a suitable content digest. Acceptance: retagged bases, architecture/ABI changes and differing resolved dependencies cannot reuse an incompatible environment. No collision exploit was established by this source review.

### P06 [P2, deployment limit] Ordinary groups and environment modules are not supported execution modes

Evidence: [snakemake-runtime/src/workflow.rs:1161](../snakemake-runtime/src/workflow.rs#L1161), [snakemake-runtime/src/workflow.rs:1168](../snakemake-runtime/src/workflow.rs#L1168), [snakemake-runtime/src/plan.rs:744](../snakemake-runtime/src/plan.rs#L744), [snakemake-runtime/src/objects.rs:698](../snakemake-runtime/src/objects.rs#L698). The declarations are retained, but normal group membership is not lowered as a remote group job; input functions get local group identity. Environment-module use is false. Native local group behavior and inactive envmodules declarations should not be mislabeled as execution defects.
Minimum addition: advertise the current mode precisely, and implement or refuse requested remote grouping/module-dependent execution. Acceptance: grouped remote resources, groupid, retries and group failure match pinned Snakemake; module-derived tools must be provided by a declared container environment under the container-only rule.

### P07 [P1, missing] Conda locking and verified image contexts are not an executable build pipeline

Evidence: [iunctus/src/conda.rs:31](../iunctus/src/conda.rs#L31), [conda-environment/src/image.rs:63](../conda-environment/src/image.rs#L63), [nextflow-runtime/src/task.rs:75](../nextflow-runtime/src/task.rs#L75), [docs/design/conda.md](design/conda.md). Local solving, virtual-package-aware APIs and verified context/package handling exist, but CLI/runtime target discovery, container build/transport and final digest selection are unfinished. Pip requirements are refused by image preparation. Nextflow selects images from `container`, without constructing one from `conda`.
Minimum addition: complete target capability discovery, locked dependency preparation, container-only build and image digest publication, including a stated pip-lock policy. Acceptance: Nextflow and Snakemake Conda-only workflows run with the requested packages; rootful/rootless builds, offline retained contexts, target platform, base image and build recipe are part of image/cache identity.

### P08 [P2, explicit limits] FIFO execution and arbitrary Python/package crossings remain unavailable

Evidence: [snakemake-runtime/src/driver.rs:501](../snakemake-runtime/src/driver.rs#L501), [docs/design/snakemake/embedded-code.md](design/snakemake/embedded-code.md), [docs/design/snakemake/python-packages.md](design/snakemake/python-packages.md). FIFO pipes are refused in real execution; services use bounded same-image groups. Native Python objects and some embedded globals cannot cross the package/task boundary. Native pandas stand-ins are a finite compatibility layer, with real package access through container helpers.
Minimum addition: supported streaming/group lifetimes and explicit Python/object transfer profiles driven by real workflows. Acceptance: pipe/service dependencies, cleanup and failure ordering are correct, and package/embedded-code cases either match pinned Snakemake or return an explicit diagnostic. Continue testing pandas indexes, alignment, missing values and mutation semantics rather than treating API-name coverage as parity.

### P09 [P1, confirmed] Semantically invalid outputs can win the shared cache

Evidence: [task-runner/src/runner.rs:495](../task-runner/src/runner.rs#L495), [snakemake-runtime/src/driver.rs:809](../snakemake-runtime/src/driver.rs#L809), [snakemake-runtime/src/driver.rs:1334](../snakemake-runtime/src/driver.rs#L1334), [content-store/src/store.rs:536](../content-store/src/store.rs#L536). A zero-exit result is published before Snakemake checks non-empty/checksum requirements. A present empty or wrong-checksum output can satisfy raw cache completeness but fail the driver. A retry denies lookup, yet first-writer publication can retain the original rejected entry for later runs.
Minimum correction: connect language acceptance to publication or conditionally invalidate the observed rejected entry, preserving a newer valid winner. Acceptance: an initial semantic failure followed by a good retry leaves a reusable good result; later runs do not keep retrieving the rejected entry. This finding concerns present but semantically invalid files, not a blanket claim that every missing-output result survives cache completeness checks.

### P10 [P1, user-required feature] Reuse already-built Conda environment containers

Evidence: [conda-environment/src/image.rs:15](../conda-environment/src/image.rs#L15), [conda-environment/src/image.rs:96](../conda-environment/src/image.rs#L96), [conda-environment/tests/image.rs:136](../conda-environment/tests/image.rs#L136). ImagePreparation already binds a lock, immutable base, target platform/capabilities and recipe into a preparation digest. Package/context reuse is not a completed-image cache: there is no durable preparation-to-OCI-image mapping or runtime lookup that selects a previously built environment. The user explicitly requires this addition, including reuse of compatible containers already built.
Minimum addition: a Conda environment image cache keyed by the verified preparation identity, with a build/import receipt mapping to the final immutable OCI image and selected platform. Consult it before build, pull/import a verified retained image when needed, and build only on a safe miss. Share the mechanism between Nextflow and Snakemake. Acceptance: identical compatible environments across runs/workflows/languages reuse one built image; changed packages, base, platform, target capabilities or recipe do not; verified external builds can be registered and reused. See the required cache contract below.

## Native CWL and Argo gaps

### C01 [P1, confirmed] Argo discards available unknown-field diagnostics

Evidence: [argo-parser/src/decode.rs:32](../argo-parser/src/decode.rs#L32), [argo-runtime/src/run.rs:248](../argo-runtime/src/run.rs#L248), [argo-runtime/src/validate.rs:137](../argo-runtime/src/validate.rs#L137), [argo-runtime/src/validate.rs:511](../argo-runtime/src/validate.rs#L511). Typed decoding collects unknown field paths/positions, but runtime callers discard them. A typo in a typed option such as workflow parallelism can lose the intended behavior without surfacing that diagnostic. Some malformed templates are caught by separate validation, so this is not a claim that every typo executes.
Smallest fix: surface unknown typed fields at loading/validation boundaries under an explicit extension policy. Acceptance: typed-option typos and unsupported new fields report their path/position before work; allowed extension/raw payload fields remain supported.

### C02 [P1, confirmed] Direct Argo artifacts do not apply subPath

Evidence: [argo-runtime/src/inputs.rs:174](../argo-runtime/src/inputs.rs#L174), [argo-runtime/src/scope.rs:314](../argo-runtime/src/scope.rs#L314). `from`/`fromExpression` artifact resolution applies `subPath`, while directly located artifact construction does not. An input artifact with a supported directory source and selected child can stage the whole source instead of the requested child.
Smallest fix: apply the supported subPath contract consistently for direct artifact sources, with containment checks. Acceptance: direct and referenced local/remote directory artifacts select identical nested children, and invalid/escaping paths are refused. Validate reference-engine behavior for source kinds where subPath is inapplicable.

### C03 [P1, confirmed ignored behavior] Argo archive and mode fields are not carried into staging

Evidence: [argo-parser/src/model.rs:327](../argo-parser/src/model.rs#L327), [argo-runtime/src/inputs.rs:185](../argo-runtime/src/inputs.rs#L185), [argo-runtime/src/pod.rs:727](../argo-runtime/src/pod.rs#L727). Artifact models accept archive and permission metadata, but location conversion and task input staging do not preserve extraction/packaging/mode instructions. Explicit archive behavior or permissions can therefore produce different bytes/layout/access than the workflow requested.
Smallest safe fix: reject unsupported active transfer options, then implement the supported archive/mode contract with bounded extraction. Acceptance: tar/gzip/none and recursive permission cases either match pinned Argo behavior or fail explicitly; preserve directory links, executable bits and path containment.

### C04 [P1, confirmed provider mismatch] CWL document links reject non-HTTP data locations

Evidence: [cwl-parser/src/source.rs:36](../cwl-parser/src/source.rs#L36), [cwl-parser/src/source.rs:100](../cwl-parser/src/source.rs#L100), [cwl-parser/src/salad/links.rs:112](../cwl-parser/src/salad/links.rs#L112). The built-in document link checker counts file/HTTP/HTTPS/mailto schemes as existing but rejects other schemes. Data `location` links in a workflow document can fail loading even when a configured storage provider can read them. Arbitrary custom Source implementations are a separate library option.
Smallest fix: distinguish document-reference validation from authorized data-location resolution, or make the default checker provider-aware. Acceptance: a configured S3 file/directory in an InitialWorkDir listing loads and reaches the same provider checks as a job input; genuinely unresolved workflow references still fail.

### C05 [P1, confirmed delivery gap] CWL final relocation skips remote output locations

Evidence: [cwl-runtime/src/results.rs:77](../cwl-runtime/src/results.rs#L77), [cwl-runtime/src/driver.rs:263](../cwl-runtime/src/driver.rs#L263), [cwl-runtime/src/driver.rs:219](../cwl-runtime/src/driver.rs#L219). A final output that retains a remote location is returned from relocation without a Delivery. The driver attaches Files for intermediate access, but final relocation does not use it. A requested local output directory can therefore remain empty while the printed object retains remote addresses.
Smallest fix: emit provider-backed deliveries and rewrite locations consistently, or reject incompatible requests explicitly. Acceptance: remote file/directory/secondary outputs are delivered to a requested local or remote destination through Files, with exact bytes and propagated delivery failure.

### C06 [P2, explicit limits] Full CWL image requirements and Argo cluster semantics need dedicated profiles

Evidence: [cwl-runtime/src/tool/mod.rs:165](../cwl-runtime/src/tool/mod.rs#L165), [cwl-runtime/src/scope.rs:137](../cwl-runtime/src/scope.rs#L137), [argo-runtime/src/checks.rs:96](../argo-runtime/src/checks.rs#L96). Required CWL image build/load/import and software requirements are unavailable. Argo arbitrary Kubernetes resource templates, daemons, sidecars and data templates are outside the adopted scientific subset. These are explicit refusals, not silently executed features.
Minimum addition: versioned capability profiles, image preparation for supported CWL requirements, and cluster-aware native handlers/lifetimes for chosen Argo features. Acceptance: [CWL requirements](https://www.commonwl.org/v1.2/Workflow.html) and [Argo resource operations](https://argo-workflows.readthedocs.io/en/latest/walk-through/kubernetes-resources/) are qualified with their actual backend prerequisites; cancellation, retries, ownership, synchronization and cleanup preserve native semantics.

## Broader universality gaps

### M01 [P2, missing] WDL and other workflow families have no native driver

Evidence: [workflow-graph/src/task.rs:13](../workflow-graph/src/task.rs#L13), [Cargo.toml:7](../Cargo.toml#L7). Language dispatch and workspace members cover four families. WDL has no parser/evaluator/driver. Supporting arbitrary workflow engines cannot be inferred from a shared container task model: workflow languages and engine-specific extensions need separately versioned semantics.
Minimum expansion: prioritize a native WDL driver with pinned [OpenWDL specification](https://github.com/openwdl/wdl) and conformance cases, including types, imports, expressions, optional values, scatter, conditionals and runtime requirements. Establish separate targets for Galaxy or other requested families. Durable event/approval/schedule-oriented engines require additional lifecycle contracts beyond a batch DAG. Acceptance: advertise only tested language versions/features and fail unsupported constructs clearly; a reference-engine wrapper is not native implementation.

### M02 [P2, missing] Native composition and structured workflow results are incomplete

Evidence: [snakemake-runtime/src/task.rs:654](../snakemake-runtime/src/task.rs#L654), [workflow-graph/src/driver.rs:134](../workflow-graph/src/driver.rs#L134), [cwl-runtime/src/driver.rs:272](../cwl-runtime/src/driver.rs#L272). Snakemake `cwl:` launches `cwltool` as a container command rather than invoking Iunctus's native CWL driver. Output schemas describe contracts, but the common driver finish path does not return one typed workflow result object for chaining. A common graph alone does not provide native nested workflows or schema-safe composition.
Minimum addition: a native child-workflow dispatch/result contract that carries schema-checked values and immutable file/directory references, lineage and cancellation into the same execution services. Acceptance: one language calls a workflow of another without an upstream-engine dependency, and its outputs become typed inputs with preserved object/digest/provenance identities.

## Shared execution, backend and resource gaps

### E01 [P1, confirmed] Effective network policy is excluded from shared cache identity

Evidence: [task-model/src/request.rs:38](../task-model/src/request.rs#L38), [task-runner/src/request.rs:134](../task-runner/src/request.rs#L134), [task-runner/src/request.rs:156](../task-runner/src/request.rs#L156). Offline mode is placement data, outside the hashed TaskSpec; pinned images qualify for shared scope regardless of network policy. Identical tasks with allowed and denied network can therefore share a result, including bytes originally obtained using undeclared network access.
Smallest correction: bind reuse to the effective network/side-effect policy, and decide whether network-dependent tasks qualify for shared scope at all. Acceptance: an offline request cannot reuse an online computation as a hermetic result; inherited/operator network overrides are reflected before key formation. Mere absence of network I/O during a cache hit does not prove how its result was computed.

### E02 [P1, confirmed] Image platform selection can occur after cache identity is formed

Evidence: [task-runner/src/request.rs:89](../task-runner/src/request.rs#L89), [executor/src/registry.rs:84](../executor/src/registry.rs#L84), [kubernetes-executor/src/backend.rs:169](../kubernetes-executor/src/backend.rs#L169), [slurm-executor/src/backend.rs:230](../slurm-executor/src/backend.rs#L230). The graph-to-request builder emits platform None. Registry pinning can retain an image-index digest; platform-aware config/child-manifest selection exists later, but its result is not universally part of the pre-execution key. One index can identify different executable children on different machines.
Minimum correction: resolve the actual platform and selected child before shared key formation, carrying both into execution and provenance. Acceptance: two architectures from one index cannot share architecture-specific results, and image command/user metadata is obtained from the exact child that runs. Existing platform-aware registry code is useful and should be reused.

### E03 [P1, confirmed] The local backend does not enforce requested accelerators

Evidence: [task-runner/src/request.rs:153](../task-runner/src/request.rs#L153), [executor/src/local.rs:95](../executor/src/local.rs#L95). GPU placement is produced, but the local pool/container path carries CPU/memory without a GPU/device mapping or an admission refusal. A task can run without the accelerator its workflow requested.
Smallest safe fix: refuse unsupported local accelerator requests, then add engine-specific device support where qualified. Acceptance: Docker and Podman requests either visibly contain the required device assignment or fail before execution; a GPU-dependent task is not silently scheduled as an ordinary CPU task.

### E04 [P2, confirmed capability gap] Other placement fields are incompletely mapped

Evidence: [task-model/src/request.rs:44](../task-model/src/request.rs#L44), [kubernetes-executor/src/manifest.rs:90](../kubernetes-executor/src/manifest.rs#L90), [slurm-executor/src/script.rs:35](../slurm-executor/src/script.rs#L35), [tes-executor/src/client.rs:27](../tes-executor/src/client.rs#L27). Disk capacity, preemptible behavior and free backend parameters lack complete backend mappings. Kubernetes requests CPU but the shown manifest does not set a CPU ceiling. The TES client has no general backend parameter/server-deadline transport path.
Minimum correction: advertise resource semantics per backend and reject unsupported requested behavior; add mappings where native semantics are available. Acceptance: requests, limits, hints and unsupported options are distinguished for CPU, memory, disk, accelerators, preemption, queues and parameters. Do not advertise a scheduling request as an enforced maximum.

### E05 [P1 for hosted helpers, confirmed] Python helper transport is not resource-bounded

Evidence: [snakemake-runtime/src/python.rs:239](../snakemake-runtime/src/python.rs#L239), [snakemake-runtime/src/python.rs:305](../snakemake-runtime/src/python.rs#L305), [executor/src/process.rs:300](../executor/src/process.rs#L300). Preparation and package helpers use memory None and the unbounded startup path. Unterminated output grows a buffer; completed lines use the unbounded response path. Containerization alone does not cap helper memory or coordinator protocol allocation.
Minimum correction: use the existing bounded helper startup with enforced memory/swap, frame, queue and deadline budgets, plus bounded preparation. Acceptance: oversized/unterminated stdout, large stderr, reply floods, cancellation and allocation pressure fail within a fixed bound. Keep CWL's already bounded helper behavior intact.

### E06 [P1 for hosted planning, missing] Native evaluation has no complete resource/authority boundary

Evidence: [nextflow-evaluator/src/interpreter/statement.rs:219](../nextflow-evaluator/src/interpreter/statement.rs#L219), [python-evaluator/src/interpreter/mod.rs:186](../python-evaluator/src/interpreter/mod.rs#L186), [python-evaluator/src/pandas/read.rs:129](../python-evaluator/src/pandas/read.rs#L129), [nextflow-evaluator/src/interpreter/mod.rs:81](../nextflow-evaluator/src/interpreter/mod.rs#L81). Python has statement/recursion limits, but native file/dataframe helpers can materialize large inputs outside those counters. Native Nextflow evaluation can loop and use host file APIs without a comparable overall budget. Schema discovery and `exec:` evaluation can exercise these paths before task execution.
Minimum addition: an isolated native run/discovery worker with restricted file/data callbacks and hard memory/time budgets, supplemented by cooperative interpreter/helper cancellation. Acceptance: infinite loops, large native allocations, huge tables and unauthorized file/URL access cannot consume or expose the Aruna service's own resources. Preserve native interpretation; external commands and packages stay in containers.

### E07 [P2, confirmed] Wrapper environment names are inserted as shell source

Evidence: [executor/src/wrapper.rs:97](../executor/src/wrapper.rs#L97), [task-model/src/spec.rs:155](../task-model/src/spec.rs#L155). Values are quoted, but environment names are inserted directly into `export` without a shell-identifier check. An invalid or syntax-bearing name can change the wrapper's interpretation or make it fail inside the task container. A host escape was not established.
Smallest fix: validate names accepted by the shell wrapper or inject them through a supported environment interface. Acceptance: ordinary names work and invalid names produce deterministic preflight errors across every wrapper backend; preserve the task's argument/value quoting.

### E08 [P1 for HA, intentional limit] Remote ownership recovery is coordinator-bound

Evidence: [tes-executor/src/backend.rs:157](../tes-executor/src/backend.rs#L157), [tes-executor/src/backend.rs:247](../tes-executor/src/backend.rs#L247), [task-runner/src/clean.rs:74](../task-runner/src/clean.rs#L74), [content-store/src/lease.rs:94](../content-store/src/lease.rs#L94). Journals conservatively retain ambiguous or changed-coordinator ownership, and destructive remote cleanup is refused without a non-expiring local lease. Those protections are valuable, but they do not implement multi-coordinator adoption or server-side fencing for hosted cache maintenance/publication.
Minimum addition: Aruna-authenticated durable attempt receipts, stable idempotency identity and an explicit adoption/fencing protocol, with remote retention policy. Acceptance: a different coordinator can reconcile owned work after reboot/failover without canceling foreign work, accepting stale results or duplicating acknowledged logical submissions. Retain current fail-closed cleanup until the required authority is enforceable.

### E09 [P1 for portable resume, confirmed] Remote outcomes fail the local-path resume check

Evidence: [task-runner/src/executor.rs:297](../task-runner/src/executor.rs#L297), [iunctus/src/resume.rs:69](../iunctus/src/resume.rs#L69). Remote store outcomes are represented as addresses, while resume tests every output with local Path existence. Valid remote results therefore fail lineage reuse and are sent back to execution; the ordinary shared cache may still avoid some computation independently.
Smallest fix: validate retained outcomes through configured Files/Store and exact object identities. Acceptance: a remote-store run resumes on another machine with the same unchanged objects, and missing or changed versions are rejected. Coordinate with D01/D02 rather than adding a second weak reuse policy.

### E10 [P1 for remote directories, explicit staging limit] Foreign directory and repository inputs have no native transfer contract

Evidence: [task-runner/src/inputs.rs:270](../task-runner/src/inputs.rs#L270), [argo-runtime/src/inputs.rs:181](../argo-runtime/src/inputs.rs#L181), [argo-runtime/src/inputs.rs:205](../argo-runtime/src/inputs.rs#L205). The shared runner refuses a remote URI marked as a directory outside its own store-hydration path. Argo Git artifacts become directory inputs with a repository URL and optional revision fragment; this path does not perform a native Git checkout. Planning acceptance therefore does not imply that these sources can execute.
Minimum addition: provider-aware versioned directory manifests and a pinned repository-artifact resolver, or an accurate early unsupported diagnostic. Acceptance: foreign S3/Aruna directories retain complete entries, links, empty folders and modes; Git inputs bind exact commits/submodule identities and authenticate safely. Existing hydration of the runner's own stored directory outputs remains supported.

## Qualification and documentation gaps

### V01 [P1, unqualified] Current full workflow/backend parity is not established

Evidence: [docs/support.md](support.md), [.github/workflows/qualification.yml](../.github/workflows/qualification.yml), [nextflow-parser/tests/test_workflows.rs:12](../nextflow-parser/tests/test_workflows.rs#L12). Ordinary source tests and selected fake/oracle fixtures do not execute all ignored corpora or prove real outputs. Explicit qualification currently selects Conda corpus/config parity and one real Python bridge case, with a separately supplied complete bundle. It is not a four-language/four-backend result qualification gate.
Required evidence: a pinned manifest of workflow revisions, complete inputs, native engine/plugin versions, selected images/platforms and exact expected results. Compare real outputs and failure/cancel/retry/delivery behavior, including unsupported outcomes and dynamic planning. Keep semantic coverage, corpus planning, container integration and live backend acceptance as separate results.

### V02 [P1 for release, unqualified] Release, recovery and deployment acceptance remain open

Evidence: [.github/workflows/source.yml](../.github/workflows/source.yml), [.github/workflows/artifacts.yml](../.github/workflows/artifacts.yml), [docs/support.md](support.md). Candidate packaging records source/toolchain/checksums and a Linux x86_64 helper archive, but does not establish independent artifact attestation, required hosted checks, dependency/license review, other architectures, rootless engines or restored-state correctness. Hosted settings and live checks were not inspected in this audit.
Required evidence: qualify the exact final source/binary/helper image on chosen platforms, rootless/shared-filesystem modes and live TES/Kubernetes/SLURM/object stores/Aruna. Exercise upgrade, backup restoration, incomplete recording and stale ownership. Publication and production claims follow those receipts, without expanding local checks into repeated broad suites per edit.

### V03 [P2, confirmed documentation drift] Some roadmap entries disagree with newer support boundaries

Evidence: [docs/roadmap.md:67](roadmap.md#L67), [docs/roadmap.md:102](roadmap.md#L102), [docs/support.md](support.md), [docs/design/inputs.md:199](design/inputs.md#L199). The roadmap still lists internal service ordering/group retries as unfinished while newer support notes describe them as implemented. Input-design next steps still mention output-schema persistence despite current database/export support. Historical comparisons and mixed completed/planned feature tables are easy to misread as current acceptance.
Smallest documentation correction in a later task: reconcile each capability against current source and link exact qualification receipts; keep implementation and acceptance separate. Acceptance: no feature is simultaneously presented as both missing and implemented, and native-engine comparisons use current primary evidence. This audit records the drift without rewriting the existing roadmap.

## Suspected cases requiring focused reproduction

- H01: [task-model/src/tree.rs:63](../task-model/src/tree.rs#L63) adds caller-supplied manifest sizes with ordinary u64 sum. An oversized synthetic manifest can overflow; an actual import/admission reproducer and its error boundary were not exercised. Check manifest loading and reject overflow with checked arithmetic if reachable.
- H02: [workflow-graph/src/recorder.rs:49](../workflow-graph/src/recorder.rs#L49) replaces a same-ID retried node, while completion appends producer/output mappings at line 215. A driver that retries after semantic rejection and changes output sets may leave stale graph mappings. Validate a changed-output retry before changing recorder identity behavior.
- H03: Argo has enum checks in argument validation at [argo-runtime/src/checks.rs:269](../argo-runtime/src/checks.rs#L269), and CLI schema admission enforces advertised root enums. Resolved nested template enum propagation needs a pinned reference test; this audit does not claim that all Argo enums are unenforced.

## Proposed Aruna integration contract

The initial design can retain the current Rust runtimes and execution traits. Aruna owns authenticated workflow registration, durable admission, quotas and object access. A supervised native Iunctus worker constructs drivers, checks the admitted contract and advances the graph. An Aruna adapter implements Executor/Files and emits durable Observer events. Existing Docker/TES/Kubernetes/SLURM paths remain separately selectable capabilities. This avoids making the local run viewer the workflow service or requiring an upstream engine executable for native interpretation.

Suggested interface, a proposal rather than an existing API:

| Operation | Required result |
| --- | --- |
| Describe registered workflow/version/profile | Workflow bundle digest, engine build identity, input/output schemas, schema digest, effective defaults, UI hints, backend/feature diagnostics and validation policy |
| Validate typed values | Authoritative normalized values, structured field/row errors, authorized immutable object bindings and deferred checks; no compute admission |
| Submit validated request | Authenticated tenant/group, stable idempotency key, exact workflow/schema/object identities, executor profile and explicit real/simulated mode; durable run receipt |
| Read/list/cancel run | Per-run authorization, stable pagination/status/error classes, cancellation acknowledgement and final cleanup/reconciliation state |
| Stream task/run events | Durable sequence cursor, replay/reconnect behavior, attempt identity, resource observations and protected log/object links |
| Read/export results | Typed output values, exact object versions, algorithm-qualified digests, image/platform identity and authorized provenance export |

Use standard [WES](https://ga4gh.github.io/workflow-execution-service-schemas/docs/) for external workflow submit/status/log/cancel operations. Schema/form and Aruna object-binding operations are explicit extensions. [TRS](https://ga4gh.github.io/tool-registry-service-schemas/) can supply portable workflow discovery and versioned descriptors; [DRS](https://ga4gh.github.io/data-repository-service-schemas/) can expose data resolution where required. Those protocols are interfaces, not substitutes for native language parity or Aruna authorization.

For forms, use the original JSON Schema with optional UI layout hints, not a lossy flat property list. Support nested records/arrays, unions/conditions, enums, defaults, nullable values, help, hidden/advanced/read-only controls and field-level server errors. File controls should select or upload an authorized Aruna object/version; directory controls should select a versioned manifest; sample-sheet controls should provide a bounded table editor/import with row diagnostics. Credentials are references, never ordinary saved form text. Browser validation provides feedback, and Rust validation plus object authorization remains authoritative.

For data binding, store object locator/version, raw-content identity, authorized access scope and any relevant generation/grant state separately. Resolve latest/floating selections once at admission and pass exact references to tasks. Keep Iunctus SHA-256 content addressing distinct from Aruna raw BLAKE3 and encrypted-transfer identity. Carry permitted placements and output ownership through native Aruna admission rather than bypassing its quotas or scheduler. Do not pass host paths, coordinator Docker sockets or broad service credentials as an implicit user capability.

## Required Conda environment container cache

This is an explicit user requirement added during the audit. The implementation remains open alongside P07/P10. A package-download cache, Docker's layer cache and a task-result cache do not by themselves select and reuse completed Conda environments.

- **Identity:** start with ImagePreparation.digest, preserving the resolved lock/package digests, immutable base image, OCI platform/variant, authoritative virtual capabilities and build recipe. Include a resolved pip lock when supported. YAML filenames, workflow names and display labels must not be the sole identity. Equivalent definitions should converge to the same verified preparation where their resolved semantics match.
- **Completed-image receipt:** persist preparation key, lock/context identity, final image/child digest, platform, recipe version and build/import provenance. Credentials stay outside public cache metadata. Only complete validated builds become ready entries; failed or interrupted builds do not become reusable successes.
- **Lookup order:** check the authorized completed-image catalog first, then availability in the selected local engine; if absent locally, retrieve the same verified image from an allowed registry or retained OCI artifact. Build once on a genuine miss. Online and offline modes report whether a retained image is locally usable without silently substituting a different environment.
- **Already-built images:** provide an explicit import/registration path for compatible external or earlier builds, using their immutable image identity and a trustworthy environment/base/platform/recipe manifest. A matching tag, environment name or claimed package list alone is insufficient proof of equivalence. Initial reuse should be exact; environment-superset reuse needs a separately documented compatibility policy.
- **Concurrent reuse:** coordinate builds per preparation key, publish receipts atomically and conditionally, and let waiters adopt the completed image. Validate image presence/integrity and platform on each use. A stale catalog record or deleted image is a recoverable miss, with bounded retry and preserved ownership after interrupted remote builds.
- **Scope and retention:** reuse across workflows and languages under an explicit authorized sharing scope; Aruna can retain OCI artifacts and receipts by exact object versions. Separate image retention from task-output retention, keep images referenced by retained runs, and expose list/inspect/import/prune operations with the existing preview-before-deletion convention.
- **Execution identity:** tasks use the returned final immutable image digest/platform in the normal task cache and provenance. The environment preparation key is not the executable image digest, and registry/index, selected-child and archive-transfer digests remain distinct.

Focused acceptance cases: a first build followed by two cross-language hits with zero builds; reuse after restarting Iunctus; import of an already-built compatible image; a missing local image retrieved by exact digest; offline hit and unavailable-image miss; changed package/base/platform/recipe causing misses; concurrent requests yielding one published build; rejected partial/corrupt receipts; failed-build recovery; authorization-separated reuse; retention of an image required by a recorded run. Existing image identity/context tests are foundations, not evidence that this completed-image cache already exists.

## Recommended implementation order

1. Correct the source-backed semantic failures first: resume integrity/identity, network/platform cache identity, semantic result publication, swallowed listing/resource errors, fake Conda references, Python no-op/walk behavior, Argo discarded options and schema/reference conversion. Start with narrow fixes and one focused regression per behavior.
2. Establish the reusable describe/validate/result contract and strict hosted admission policy. Preserve CLI compatibility and full schema semantics. Add structured errors and provider-aware bounded content/object validation.
3. Integrate one native Aruna Executor/Files path with durable run/attempt ownership and exact object versions. Demonstrate one real workflow per current language, including cancel/restart and final result delivery, without shared host paths.
4. Build the Aruna schema-driven workflow form on that contract, then complete Conda image preparation with the required environment container cache and the highest-impact existing-language gaps. Support verified already-built image import and reuse before launching unnecessary builds. Qualify package helpers and planning budgets before accepting untrusted hosted workflow code.
5. Expand native language/plugin/backend profiles, starting with WDL and targeted CWL/Argo/Nextflow/Snakemake compatibility cases. Add native mixed-language dispatch after typed workflow results are stable.
6. Gate release/production claims on complete pinned differential corpora, backend failure/recovery tests, bounded scale runs, rootless/platform qualification and recorded CI/artifact identities.

Each backlog entry is a candidate for a separately scoped task. Review findings do not authorize unrelated implementation changes. No source fix or refactoring is included in this audit.

## Cross-check decisions and limitations

- Rejected the reported absolute-path root-loss bug: Rust PathBuf::pop does nothing at a root without a parent. The inspected normalize loop therefore does not turn `/../../tmp/file` into a relative path. [Rust PathBuf contract](https://doc.rust-lang.org/std/path/struct.PathBuf.html#method.pop)
- Rejected a blanket claim that native Nextflow exec is an external host-command fallback. [Decision 17](decisions.md) and [embedded-code design](design/snakemake/embedded-code.md#6-nextflow-exec-sections) deliberately treat it as native evaluation; it still needs the hosted authority/resource boundary in E06. External tools/packages remain container-only.
- Corrected the initial Snakemake group/envmodules classifications to preserve deliberate local/inactive semantics. Retained the precise remote capability gap, and narrowed cache rejection to present files failing semantic checks.
- Did not infer production readiness from simulation, parser acceptance, historical receipts or source inspection. Did not reintroduce remediated findings about missing env/stdin, ignored join flags, config include cycles, unbounded CWL helper execution or unconditional destructive remote cleanup.
- Deferred the three suspected cases above to focused reproduction. This report does not exhaust every numerical edge case, interpreter API, filesystem race or workflow fixture; qualification and ongoing differential review remain necessary.

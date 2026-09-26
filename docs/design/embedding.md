<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Embedding iunctus

The existing `iunctus` library owns driver and executor construction, config setup, the run database, resume, workflow sources and the content store. The CLI calls those same services. No separate library crate or workflow executable is needed.

`RunRequest::new(target)` selects strict admission. Its `settings: RunSettings` uses the existing CLI configuration fields: language, profiles, config files, parameters, input file, executor, database, store, resume and task scheduling. Set `settings.inputs` to the admitted JSON input document. This uses each native driver's existing input binding and keeps its file in the source inventory. Strict admission validates typed values without CLI coercion, default insertion or simulated assumptions. `Policy::Relaxed` permits missing and advisory schemas, while preserving typed validation in the embedding path. The CLI retains its existing relaxed checks and coercion.

```rust
use iunctus::{CancellationToken, RunRequest};

async fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let cancel = CancellationToken::new();
    let mut request = RunRequest::new("bundle/workflow.cwl");
    request.settings.inputs = Some("bundle/admitted-inputs.json".into());
    request.settings.executor = Some("local".into());
    request.settings.database = Some("runs.sqlite".into());
    request.settings.store = Some("cache/store".into());
    let completed = request.run(cancel).await?;
    let graph = completed.execution.into_result()?;
    println!("{} tasks", graph.nodes.len());
    Ok(())
}
```

`RunRequest::run(cancel).await` is safe inside a Tokio runtime, including a current-thread runtime. It constructs and uses the non-`Send` driver, executor and blocking engine inside `tokio::task::spawn_blocking`. It installs no process signal handler. The CLI installs its existing handlers separately. A child cancellation token keeps completion from cancelling the host's token. To stop a run, cancel the host token and await the request's future. Dropping the future requests cancellation, but the worker continues task cleanup and recording. Blocking workers cannot be forcibly aborted, and cancellation does not add hard budgets for native interpretation.

`RunRequest::run_blocking(cancel)` is the synchronous equivalent, for callers outside an async runtime. The lower-level `application::execute` and `application::execute_with` remain blocking too. Call the async request API from Tokio instead of calling these methods directly.

`Run` contains its database `id`, `execution` and optional local viewer `page`. Setup and admission failures return `Failure`, with the existing message and exit-code classification. A workflow failure or cancellation remains in `execution.error` alongside the partial graph; use `execution.into_result()` when a successful workflow is required. Recording retains the request's effective run arguments rather than the host application's command line. Resume uses the same lineage and integrity checks as the CLI. Run IDs are unique across concurrent requests in one process while retaining their existing textual shape.

`RunRequest::describe(cancel).await` constructs the configured driver and returns `application::Description` without starting workflow tasks or recording a run. It can still read workflow resources and use container helpers during discovery. Pass its result to `forms::Form::new(description, policy)` for form discovery and typed JSON validation. Keep the same workflow, profile and config inputs when creating the execution request.

With `request.setup = Some(setup::Setup::default())`, or a host-configured `Setup`, execution skips `Setup::load` and global CLI defaults. Without it, the request reads the same config files as `iunctus run`. An optional local viewer still reads its page configuration. Existing process-wide cache location, storage proxy and environment settings remain shared; this API does not implement tenant authorization, cache isolation, durable hosted admission or interpreter isolation. Those boundaries remain in the [universal workflow audit](../universal-workflow-audit-2026-10-07.md).

The form format remains `FORMAT = 1`. `remote_content_checked: false` is accurate: schema validation does not fetch remote bytes, authorize remote object access or bind exact object versions. Execution downloads are a separate operation. Aruna must validate and authorize immutable object bindings before admitting a run.

`Form::response()` returns `FormResponse`, and `Form::validate_response(values)` returns `ValidationResponse`. Both derive serde serialization and deserialization, including nested diagnostic branches. The existing `json()` and `validate()` methods retain their version 1 JSON format. Round-trip checks cover accepted, rejected and unavailable responses and preserve serialized bytes.

# Aruna execution boundary

`aruna-executor::client` retains the native submit, status and cancel contract. Job state, job kind, logical family state and physical execution state use separate enums with `Unknown(String)` for future wire values. Transport and malformed-JSON errors retain their sources. HTTP errors retain at most 8 KiB of body evidence, expose whether it was truncated, and omit those bytes from Display and Debug because they may contain secrets. Successful response bodies remain bounded to 1 MiB.

`aruna-executor::ArunaBackend` implements `executor::Backend` behind the existing `RunnerExecutor`. Select it with `settings.executor = Some("aruna".into())`. The server, group, result bucket and caller scope can come from `RunSettings` (`aruna_server`, `aruna_group`, `aruna_bucket`, `aruna_scope`) or `[executor.aruna]`. CLI options override configuration. Tokens come from `IUNCTUS_ARUNA_TOKEN` or `token = { environment = "ARUNA_TOKEN" }`; literal config tokens are refused and no token option is recorded.

The embedding host must supply `Setup.aruna_objects: Option<Arc<dyn iunctus::aruna::Objects>>`. `Objects::write(group, key, file)` uploads a completed input bundle and returns its immutable bucket/key/version reference. `Objects::read(object, maximum)` reads the exact result version, authorizes the output owner's endpoint, verifies integrity and enforces the byte limit while streaming. The jobs wire client models no upload or download route. The standalone CLI recognizes Aruna settings but stops before submission without this host adapter.

```rust
fn configure_aruna(
    request: &mut iunctus::RunRequest,
    objects: std::sync::Arc<dyn iunctus::aruna::Objects>,
) {
    request.settings.executor = Some("aruna".into());
    let mut setup = iunctus::setup::Setup::default();
    setup.aruna_objects = Some(objects);
    setup.executors.aruna_server = Some("https://aruna.example/api/v1".into());
    setup.executors.aruna_group = Some("authorized-group-id".into());
    setup.executors.aruna_bucket = Some("authorized-results".into());
    setup.executors.aruna_scope = Some("caller-stable-scope".into());
    request.setup = Some(setup);
}
```

The adapter snapshots staged inputs, stdin and the existing wrapper into an uncompressed tar object, then submits an exact reference. The image needs `sh`, `tar` and the wrapper's usual tools. The wrapper produces one tar result object containing control files, logs, the output inventory and an outbox of files and directories. Both bundles are limited to 256 MiB. Result extraction permits at most 100,000 regular files/directories below `control` and `outbox`, refuses links and escaping paths, and requires complete wrapper records. The runner adopts restored outputs and logs into the content store and retains the existing native acceptance and cache-publication rules. Aruna raw BLAKE3 identity and iunctus SHA-256 cache identity remain distinct.

The idempotency key hashes the task cache key with the configured server, caller scope, group and result bucket. A locked, synced journal in `<scratch>/aruna/journals` freezes the original submit document and exact input version before submission. Replays reuse that document, including after an uncertain response or a coordinator restart. Changed resource settings under the same identity are refused. The host must bind `scope` to the authenticated caller, retain journals and staged objects, and replay interrupted keys. There is no scan or cancellation of unrelated journals on admission. Use a new scope for forced fresh work, changed sizing or reproducibility runs; `-no-cache` alone does not bypass Aruna idempotency.

Polling and termination use the shared executor helper. Only a complete logical-family terminal verdict ends polling; partial/local exhaustion is insufficient. Cancellation records intent before requests, polls beyond 202, and retains the observed terminal verdict, including late success. A cancelled caller receives cancellation and publishes no result. Unconfirmed termination leaves the journal for replay. Offline execution, multiple containers, user overrides, host input mutation, GPUs and unmodeled placement settings are refused.

Confirm these contracts against the real Aruna API before deployment:

- Authenticated immutable object writes and exact-version reads, including group/bucket authorization, digest verification, endpoint allowlists and credential handling.
- Exact-reference staging at `container_path`, writable container scratch, entrypoint/command semantics and literal capture of the single result bundle at the requested bucket/key.
- Complete logical-family verdicts, canonical execution output identity, cancellation convergence and durable caller-scoped idempotency retention.
- CPU, memory, walltime, executor constraints, image platform selection and container permissions.

Fake-server checks cover jobs HTTP requests, in-memory object transfer, successful file/directory/log collection into the store, failure reasons, cancellation intent and late success, partial-family polling, exact resubmission after backend recreation and changed-plan refusal. Bundle tests cover staging and extraction. These fixtures do not run the wrapper in a real container or qualify a live Aruna service.

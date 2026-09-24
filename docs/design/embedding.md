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

The requested serde form response types are blocked by the current iunctus manifest, which has `serde_json` but no direct `serde` dependency. The manifest owner must add `serde.workspace = true` before this crate can derive the requested serialization traits. No manifest or lockfile change belongs to this implementation scope.

# Aruna execution boundary

`aruna-executor::client` retains the native submit, status and cancel contract. Job state, job kind, logical family state and physical execution state use separate enums with `Unknown(String)` for future wire values. Transport and malformed-JSON errors retain their sources. HTTP errors retain at most 8 KiB of body evidence, expose whether it was truncated, and omit those bytes from Display and Debug because they may contain secrets. Successful response bodies remain bounded to 1 MiB.

There is no usable Aruna execution backend or `-executor aruna` option yet. The current aruna-executor manifest has no direct `executor`, `content-store` or `task-model` dependencies, and Tokio is only a dev dependency. Implementing `executor::Backend` requires those direct dependencies, plus Tokio as a normal dependency. The existing executor re-export of `CancellationToken` can avoid adding tokio-util directly. Dependency edits belong to the manifest owner; this implementation stops at that dependency boundary instead of duplicating portable task types or adding an unusable CLI option.

After those dependencies are available, the adapter must implement `executor::Backend` behind `RunnerExecutor`. Its data-transfer boundary must stage the wrapper and inputs as authorized immutable Aruna object versions and return one result bundle produced inside the task container. The bundle must contain the existing control files, logs and output inventory, including directory manifests. Literal output capture alone has the failure identified in [Next Aruna backend boundary](../universal-workflow-progress.md#next-aruna-backend-boundary).

The missing adapter contracts are:

- An authenticated object writer that returns the completed write's immutable version and binds the caller, group and destination. The jobs wire client provides no upload operation.
- An exact-version reader that verifies the output owner's allowed endpoint and bounds bundle size, extraction paths and contents. The jobs wire client provides no object download operation.
- A durable attempt journal and recovery policy that bind the task cache key to caller-scoped idempotency and an unchanged submission plan. Run names, resource sizing and object versions cannot silently change on replay of the same key.
- Family reconciliation and cancellation that preserve partial, unknown, indeterminate and late-success evidence. A 202 cancellation response is intent, and local exhaustion or one physical success is not a complete logical-family verdict.
- Collection into the content store through the runner, followed by the existing native acceptance and cache-publication rules. Aruna raw BLAKE3 identity and iunctus SHA-256 cache identity remain distinct.

The client fake-server tests cover submission with exact references and caller-scoped idempotency, status, partial family evidence, cancellation with late success, unknown states, retained error sources and bounded bodies. They do not qualify staging, polling, result collection, recovery or a live Aruna service. No server API for those data-transfer and ownership contracts is invented here.

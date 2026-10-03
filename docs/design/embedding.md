<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Embedding iunctus

The existing `iunctus` library owns driver and executor construction, config setup, the run database, resume, workflow sources and the content store. The CLI calls those same services. No separate library crate or workflow executable is needed.

`RunRequest::new(target)` selects strict admission. Its `settings: RunSettings` uses the existing CLI configuration fields: language, profiles, config files, parameters, input file, executor, database, store, resume and task scheduling. The retry delay is `settings.retry_delay_ms`, in milliseconds; the CLI flag remains `-machine-retry-delay-ms`. Set `settings.inputs` to the admitted JSON input document. This uses each native driver's existing input binding and keeps its file in the source inventory. Strict admission validates typed values without CLI coercion, default insertion or simulated assumptions. `Policy::Relaxed` permits missing and advisory schemas, while preserving typed validation in the embedding path. The CLI retains its existing relaxed checks and coercion.

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

The form format remains `FORMAT = 1`. `remote_content_checked: false` is accurate: schema validation does not fetch remote bytes, authorize remote object access or bind exact object versions. Execution downloads are a separate operation.

`Form::response()` returns `FormResponse`, and `Form::validate_response(values)` returns `ValidationResponse`. Both derive serde serialization and deserialization, including nested diagnostic branches. The existing `json()` and `validate()` methods retain their version 1 JSON format. Round-trip checks cover accepted, rejected and unavailable responses and preserve serialized bytes.

Aruna integration uses the generic TES executor plus S3 storage; TES-over-S3 staging is still being implemented.

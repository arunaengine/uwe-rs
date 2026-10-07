<!-- Embedding services, run ownership and typed form contracts. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Embedding iunctus

The existing `iunctus` library owns driver and executor construction, config setup, the run database, resume, workflow sources and the content store. The CLI calls those same services. No separate library crate or workflow executable is needed.

`run::Settings` is a non-exhaustive embedding type, independent of CLI flags and presentation. Construct it with `Settings::new(target)` or `Settings::default()`, then assign the fields needed by the host. `RunRequest::new(settings)` selects strict admission and defaults to local container execution. Set `settings.executor` to `Some("simulator".into())` for made-up outputs without task containers. Executor credentials and backend configuration belong in `settings.setup: Option<setup::Setup>`.

`settings.inputs` accepts a typed JSON object, and `settings.params` accepts typed values that override matching input keys. Values reach the native driver's input binding through a temporary JSON file kept alive through discovery or execution and included in the source inventory. Use absolute paths for local file values. Strict admission validates without CLI coercion, default insertion or simulated assumptions. `Policy::Relaxed` permits missing and advisory schemas while retaining typed validation. The CLI converts its arguments once into these settings, retaining its existing input parsing, flags, help and output.

```rust
use iunctus::{RunRequest, run::Settings, setup::Setup};
use serde_json::json;

async fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let mut settings = Settings::new("bundle/workflow.cwl");
    settings.inputs = Some(json!({"count": 3}));
    settings.setup = Some(Setup::default());
    settings.output_dir = Some("results".into());
    settings.database = Some("runs.sqlite".into());
    let completed = RunRequest::new(settings).run().await?;
    let graph = completed.execution.into_result()?;
    println!("{} tasks", graph.nodes.len());
    Ok(())
}
```

`RunRequest::run().await` is safe inside a Tokio runtime, including a current-thread runtime. It constructs and uses the non-`Send` driver, executor and blocking engine inside `tokio::task::spawn_blocking`. It installs no process signal handler. The CLI installs its existing handlers separately. Assign a clone of the host's cancellation token to `settings.cancel`; completion cancels only a child token. To stop a run, cancel the host token and await the request's future. Dropping the future requests cancellation, but the worker continues task cleanup and recording. Blocking workers cannot be forcibly aborted, and cancellation does not add hard budgets for native interpretation.

`RunRequest::run_blocking()` is the synchronous equivalent, for callers outside an async runtime. The lower-level `application::execute` and `application::execute_with` remain blocking too. Call the async request API from Tokio instead of calling these methods directly.

Settings include `language`, `profiles`, workflow `configs`, iunctus `config_file`, `resume`, `cache`, `targets`, `until`, `omit_from`, `forcerun`, `forceall`, `default_image`, `network`, `cpus`, `memory_bytes`, `max_pending`, `retries`, `retry_delay`, `output_dir`, `database` and `store`. CPU and memory limits cap each task's backend request, including memory when the workflow leaves it unspecified. CPU limits must be at least one and memory limits at least 6 MiB. `max_pending` bounds pending tasks; these settings do not set a process-wide resource budget. `retry_delay` is a `Duration`; the CLI retains `-machine-retry-delay-ms`. `cache` controls shared cache reads and writes, while `resume` selects recorded lineage reuse.

`Run` contains its database `id`, `execution` and optional local viewer `page` used by the CLI. Setup and admission failures return `Failure`, with the existing message and exit-code classification. A workflow failure or cancellation remains in `execution.error` alongside the partial graph; use `execution.into_result()` when a successful workflow is required. Recording retains the request's effective run arguments rather than the host application's command line. Resume uses the same lineage and integrity checks as the CLI. Run IDs are unique across concurrent requests in one process while retaining their existing textual shape.

`RunRequest::describe().await` constructs the configured driver and returns `application::Description` without starting workflow tasks or recording a run. It can still read workflow resources and use container helpers during discovery. Pass its result to `forms::Form::new(description, policy)` for form discovery and typed JSON validation. For an accepted `ValidationResponse::Checked(response)`, assign `response.values` to the execution settings' `inputs`. Keep the same workflow, profile and config inputs when creating the execution request; execution validates them again before starting tasks.

With `settings.setup = Some(setup::Setup::default())`, or a host-configured `Setup`, execution skips `Setup::load` and global CLI defaults. Without it, the request reads the same config files as `iunctus run`. Existing process-wide cache location, storage proxy and environment settings remain shared; this API does not implement tenant authorization, cache isolation, durable hosted admission or interpreter isolation. Those boundaries remain in the [universal workflow audit](../universal-workflow-audit-2026-10-07.md).

The form format remains `FORMAT = 1`. `remote_content_checked: false` is accurate: schema validation does not fetch remote bytes, authorize remote object access or bind exact object versions. Execution downloads are a separate operation.

`Form::response()` returns `FormResponse`, and `Form::validate_response(values)` returns `ValidationResponse`. Both derive serde serialization and deserialization, including nested diagnostic branches. The existing `json()` and `validate()` methods retain their version 1 JSON format. Round-trip checks cover accepted, rejected and unavailable responses and preserve serialized bytes.

Aruna integration uses the generic TES executor plus S3 storage; TES-over-S3 staging is still being implemented.

## Configuration and validation boundaries

Drivers are not `Send`; callers construct and use them on their owner thread. Validation performs input-schema checks, including declared local path checks, without fetching or verifying remote content. Form defaults remain annotations. Callers control authorization and disclosure of schemas and values. Input validation does not check remote object access or bind immutable versions, and later execution downloads do not turn it into a remote-content check.

Command defaults are read before options, cache folders or networking. `[executor].default` supplies the executor for `run` and `check-machine` when `-executor` is absent; otherwise local Docker/Podman execution is the default. Inspection commands retain their simulated planning defaults. `[cache].folder` wins over `XDG_CACHE_HOME`. `[network].proxy` applies to iunctus requests unless the environment supplies a proxy. `[report].file` selects each run's report, with relative paths starting in the run folder.

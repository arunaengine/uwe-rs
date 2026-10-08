<!-- Starting an embedded run and finding the input-form and cancellation contracts. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Embed iunctus

The `iunctus` crate exposes the same run services that the CLI uses.
Add it and `serde_json` to your application's dependencies.
Construct the non-exhaustive `run::Settings` through `Settings::new`, then set
the fields your application needs:

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

Use a real workflow that declares `count` for this example.
The default executor is local containers. Set
`settings.executor = Some("simulator".into())` to select simulation.
An explicit `Setup` avoids global CLI configuration loading.
Typed inputs use strict admission without CLI coercion or default insertion.
Use absolute paths for local file values.

`RunRequest::describe().await` and `forms::Form` provide JSON Schema form discovery
and typed validation responses. The caller renders the form and submits checked
values as `settings.inputs`. Discovery can read resources and use container helpers.

Read [the embedding contract](../design/embedding.md) for synchronous execution,
Tokio ownership, cancellation, resource limits and form validation.
Task failures remain in `execution.error`; `into_result()` requires success.
This API does not provide tenant authorization or isolated per-tenant caches.

Return to [First run](first-run.md) or [Configuration](configuration.md).

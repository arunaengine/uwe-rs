<!-- Diagnosing command failures, finding task logs and cleaning retained run data. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Troubleshooting

Start with `iunctus check-machine` and `iunctus check WORKFLOW`.
Replace `WORKFLOW` with your file or project path.
These checks report several problems at once. They do not establish that a
workflow's real outputs will match its original engine.

## Exit codes

| Code | Meaning | Next step |
| --- | --- | --- |
| `1` | Workflow, task or input failure | Read the failing task and input diagnostic. |
| `2` | Command-line error | Use `iunctus COMMAND -help` and correct the option. |
| `3` | Machine or configuration failure | Check the engine, paths, credentials and backend settings. |
| `33` | Unsupported feature | Read the named feature and the [support matrix](../support.md). |
| `130` | Interrupted workflow execution | Inspect recorded tasks before resuming. |

A page-only command can stop normally on Ctrl-C with code 0.

## Common failures

| Symptom | What to check |
| --- | --- |
| No container engine answers | Start Docker or enable the Podman socket. Check socket permissions and IUNCTUS_ENGINE. |
| Unsupported feature | Match the exact construct to the language or executor limits. A simulator does not enable unsupported semantics. |
| Task has no image | Declare the image in the workflow or supply `-default-image IMAGE`. |
| Image cannot be found or pulled | Check the image name, platform, registry access and network. A default image only fills missing declarations. |
| Disk space exhausted | Check the cache, Docker image storage and published-output filesystem. Select a larger cache folder or clean retained data. |
| Registry authentication refused | Check `[registry."HOST"]` or inline Docker/Podman auth. Credential helper programs are unavailable. Remote workers need their own registry access. |
| TES server refuses access | Check bearer or Basic authentication and worker S3 access separately. |
| Repeat cannot restore simulated sources | Restore the unchanged files or start a new run. Simulations keep no source archive. |

`check-machine` reports free store space. It does not reserve space for future
tasks or image pulls. A container engine can use a different filesystem.

## Logs and task details

```sh
iunctus run main.nf -v
IUNCTUS_LOG=debug iunctus run main.nf
```

Logs go to stderr. The default filter is `warn`.
`-v` selects `debug` and overrides `IUNCTUS_LOG`; `-quiet` turns tracing off.
`IUNCTUS_LOG` accepts a tracing filter, including module-specific levels.
The run page shows task commands, inputs, outputs, exit codes and stdout/stderr.
Use `iunctus runs` to find the run ID, then `iunctus page RUN_ID`.
Save a standalone report with `iunctus report RUN_ID -output report.html`.

## Where data lives

The cache defaults to `$XDG_CACHE_HOME/iunctus`, or `~/.cache/iunctus`.
`[cache] folder` replaces it. Important paths are:

| Path | Contents |
| --- | --- |
| `<cache>/runs.sqlite` | Run records, tasks, attempts, logs and provenance. `-database FILE` overrides it. |
| `<cache>/store` | Content-addressed files and task results. `[store] location` or `-store` overrides it. |
| `<cache>/scratch` | Task and executor working folders. |
| `<cache>/pipelines` | Repository archives indexed by host, owner, repository and commit. |
| `<cache>/downloads` | Downloaded workflow resources and inputs. |
| Configured Conda `repodata` and `packages` | Environment metadata and verified package bytes. |

Workflow-published outputs stay where the workflow or output options put them.
They are separate from the store. Removing them does not delete run records.

## Clean the store

Preview a retention rule first:

```sh
iunctus clean -keep-days 30
```

Add `-keep-run RUN_ID` to retain everything needed by a particular run.
`-keep-slower-than 10m` retains results of tasks that took at least ten minutes.
Rules combine. Active runs and their referenced data stay protected.
Without a retention rule, `clean` returns a usage error.

After reviewing the preview, apply it:

```sh
iunctus clean -keep-days 30 -remove
```

This removes unretained store entries and eligible completed scratch folders.
Old run records may then refer to files that no longer exist, so keep runs you
need to repeat. The command does not prune engine images or remove published
results, pipeline archives or all download caches.

Return to [First run](first-run.md) or [Configuration](configuration.md).

<!-- User-visible changes prepared for the first iunctus beta. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Changelog

## Unreleased (version 0.1.0)

Changes since `945ad6c1`, grouped by language and area.
This heading does not change the current binary version, which remains 0.1.0.

### Nextflow

- Select named workflow entries with `-entry`.
- Run pinned hosted repositories and retain their resolved revisions for repeats.
- Support subscription error callbacks, debug/echo directives, UUID values and nested null channels.
- Stage tracked project helpers for container tasks.
- Inject named task secrets from the local store or environment references; refuse secret values in workflow expressions, unsupported directives and unsafe publishing options.
- Improve YAML output and resource validation.

### Snakemake

- Select jobs with `-until` and `-omit-from`. Force jobs with `-forcerun` and `-forceall`.
- Choose freshness checks with profiles or `-rerun-triggers`.
- Keep current dependency branches and checkpoint results when outputs are up to date.
- Run lifecycle hook commands in local containers.
- Support minimal, shallow and full shadow jobs and connected same-image groups.
- Build local Conda environments in reusable images, including conda+container rules.
- Resolve and install supported `pip:` requirements with pinned artifacts.
- Apply supported profile config, resources, threads and retries.

### CWL

- Preserve exact long values, nested file schemas and secondary-file checks.
- Load file-array contents for expressions and collect initial-workdir outputs.
- Apply output filtering and retain simulated record paths.
- Refuse unsupported required execution extensions and invalid resource or policy values.

### Argo

- Validate workflow inputs, resources, retries and lifecycle expressions.
- Confine artifact subpaths and refuse unsupported artifact sources and transfers.
- Support selected task deadlines and workflow hooks.
- Accept readiness probes, garbage collection and memoization without cluster effects.
- Refuse timing, pod and synchronization controls that cannot be honored.

### Executors and storage

- Default real runs to local Docker or Podman containers.
- Authenticate registry requests with direct iunctus or Docker/Podman credentials.
- Stage TES inputs and outputs through S3 without shared host paths.
- Support TES Basic authentication and optional server-backed idempotency tags.
- Preserve task failure status, cancellation and known-task restart recovery on TES.
- Read exact S3 object versions and verify transferred content.
- Apply machine retry backoff and task network policy.

### Runs, inputs and embedding

- Validate native results before cache publication and resume reuse.
- Verify historical resume identities and repair or reuse verified Conda image caches.
- Expose JSON Schema form descriptions and typed validation responses.
- Provide async embedding, cancellation and per-task resource limits through run settings.
- Export committed task telemetry during execution.
- Protect remotely bound run pages with access tokens.
- Add focused CLI help, operational logs and guidance for rejected engine flags.
- Add a task-oriented user guide with configuration and troubleshooting references.

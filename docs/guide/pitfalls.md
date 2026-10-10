<!-- Known beta limitations, operational surprises and qualification gaps. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Known pitfalls

This page covers the 0.1.0 beta as of 2026-10-08. The version stays 0.1.0.
Record the source commit and binary digest when reporting a problem.
Older design notes can describe plans or superseded behavior.
The [support matrix](../support.md) and recorded results describe qualification.

- [General and installation](#general-and-installation)
- [Nextflow](#nextflow)
- [Snakemake](#snakemake)
- [Conda and pip](#conda-and-pip)
- [CWL](#cwl)
- [Argo](#argo)
- [Executors](#executors)
- [Storage, cache and disk use](#storage-cache-and-disk-use)
- [Security](#security)
- [Embedding](#embedding)
- [Testing and qualification status](#testing-and-qualification-status)

## General and installation

- **Native language subsets.** A workflow accepted by its original engine can
  fail here. iunctus implements the languages in Rust rather than invoking those
  engines. Use supported constructs or the original engine for unsupported work.
  An unsupported feature normally gives exit code 33.
- **Containers are required.** Host-installed tools cannot fill missing task
  images. External task commands run in containers. Declare an image or supply
  `-default-image IMAGE`, and make the selected executor available.
- **Simulation makes up results.** `-executor simulator` can finish successfully
  without running tools. Its files and graph do not prove real output correctness.
  Use a separate working and output folder, then verify a real container run.
  Planning can still need local Python package or CWL JavaScript helpers.
- **Checks have limits.** `iunctus check WORKFLOW` and `iunctus check-machine`
  check planning and prerequisites. They cannot establish results from real tools
  or all paths selected by real data. Compare real outputs before relying on them.
- **Build and platform assumptions.** Source builds need the pinned Rust
  toolchain and native build tools. Building can download dependencies.
  A normal build links against the host's glibc and may not start on an older
  Linux. Use `just release-binary` for a static musl binary that runs on any
  x86_64 Linux. macOS, Windows and ARM hosts are unqualified. Follow
  [installation](install.md) and validate your own platform.
- **Engine flags differ.** Many Nextflow and Snakemake command-line flags are
  rejected. iunctus uses its own options, usually one hyphen, while workflow
  parameters use two. Consult `iunctus run -help` and the language guides.
- **Configuration is separate.** Nextflow `-c FILE` does not configure iunctus.
  Site, user and explicit iunctus TOML files merge, so inherited settings can
  affect a run. Use `-iunctus-config FILE`; check the
  [configuration reference](configuration.md). TOML paths do not expand `~`.
- **Repository retrieval has limits.** SSH URLs, arbitrary Git servers,
  submodules and archive links are unsupported. Hosted retrieval uses supported
  HTTP APIs and bounded archives. Materialize unsupported repositories locally.
- **Cached branches do not automatically advance.** Symbolic revisions contact
  the host but retain the remembered commit until `-latest`. Only a cached full
  commit ID works offline. Use `-r COMMIT` to pin a run, or `-latest` to refresh.

## Nextflow

- **Language features can be refused.** DSL1, Groovy class inheritance and typed
  process sections are unavailable. The native evaluator implements a subset.
  Use DSL2 and untyped process inputs and outputs, or run the original engine.
- **Some process settings have limits.** `beforeScript` and `afterScript` run
  inside the task container. `storeDir` skips a task when its outputs already
  exist and copies new outputs there, but not together with `afterScript`.
  Secrets come from Nextflow's local secret store or environment references;
  cache keys record only their names, and remote executors cannot receive them.
  `containerOptions` maps CPU, GPU, shared memory, swap and environment flags;
  mounts, devices and privileged flags are refused.
- **Conda becomes an image; Spack is unavailable.** With `conda.enabled`, YAML
  files and inline package lists are built into container images like Snakemake
  environments. Named host environments and Spack are refused.
- **Backend settings belong to iunctus.** Nextflow scheduling hints do not select
  an iunctus executor or cluster placement. Backend selection is separate from
  process configuration. Use `-executor` and the iunctus executor settings.
- **Workflow sections differ from callbacks.** `onComplete:` and `onError:`
  sections inside a workflow are refused by lowering. The existing
  `workflow.onComplete { ... }` and `workflow.onError { ... }` callbacks are
  supported. Use those callbacks with supported native code.
- **Hash sorting covers common values.** `collectFile` and `groupTuple` hash
  ordering matches Nextflow for strings, numbers, booleans, lists, maps and local
  paths. Other value types and remote file hashes are refused when two or more
  items need ordering.
- **Completion order changes results built from arrival order.** Operators such
  as `distinct()` depend on arrival order; it only removes adjacent duplicates.
  Recorded methylseq metadata repeats identical version blocks in a different
  pattern. Sort by stable keys or use appropriate uniqueness before collecting.
  `iunctus check-order WORKFLOW` can reveal ordering dependencies in a simulation.
- **There is no Nextflow work layout.** iunctus uses its own scratch folders and
  content store. A staged `.command.run` can be an iunctus helper wrapper, rather
  than Nextflow's launcher. Use the run page for commands, logs and outputs;
  do not depend on Nextflow work-directory inspection scripts.
- **Resume belongs to iunctus.** `-resume` reads iunctus run records and verified
  stored results. It cannot resume a Nextflow cache or work directory.
  Resume an iunctus run and retain its database, inputs, images and store.
- **Trace measurements are conditional.** Unmeasured CPU and memory columns
  show `-`. `%cpu` uses recorded CPU time; `peak_rss` uses container peak memory,
  including page cache. Check executor measurements before treating these as
  process CPU or RSS measurements.
- **Published links depend on retained data.** With a local store, the default
  `publishDir` mode links to stored outputs. Removing contents can break those links.
  Use `mode: 'copy'` for independent results. `mode: 'move'` also publishes a copy
  here, because the stored output must remain available.
- **Some publishing options are refused.** `failOnError: false` and active
  `contentType` or `tags` are unsupported. The delivery path cannot honor them.
  Remove these options only if their behavior is unnecessary; otherwise use
  Nextflow or perform the required storage operation separately.
- **Plugins are native implementations.** Installing a JVM plugin does not add
  its behavior. Only implemented plugins and functions are available.
  Use a supported function or the original engine for missing plugin behavior.
- **Reference pipelines can fail upstream.** Pinned methylseq `cc11bd9` fails
  with Nextflow 26.04.6's default parser because an inactive AWS include is absent.
  Its recorded comparison uses Nextflow's legacy parser and unchanged sources.
  Match that reference setup when reproducing the comparison.

See [Nextflow usage](nextflow.md) and the
[recorded pipeline results](../universal-workflow-progress.md#local-nf-core-execution-qualification-2026-10-08).

## Snakemake

- **Workflow Python is a subset.** Snakefiles and local Python modules use a
  native interpreter, not CPython. Some standard-library calls and Python
  features are unavailable. Use supported code or the original engine.
- **Package helpers have a boundary.** Packages without a native stand-in run
  in a local Python container. Workflow-defined classes, native functions and
  stand-in objects such as pandas tables cannot freely cross that boundary.
  Pass plain values and package handles; use `-python-image IMAGE` for packages
  needing system libraries absent from the default helper image.
- **Pandas follows version 3 semantics.** Older arguments such as `squeeze` and
  `delim_whitespace` fail. The stand-in deliberately follows pandas 3 behavior.
  Update those calls and check the supported table operations.
- **Profiles cannot select execution policy.** Profile keys such as `cores`,
  `jobs`, `executor` and `use-conda` are ignored. iunctus owns those choices.
  Use `-max-pending`, `-executor` and `[conda]` instead. Unsupported profile
  settings outside the accepted subset fail with code 33.
- **Rerun triggers do not change cache identity.** `-rerun-triggers` selects
  freshness checks for `mtime,params,input,software-env,code`. They compare input
  times, evaluated params, input paths, software references and rule or script
  source. The shared cache still includes software and input content.
  Use `-forcerun` or `-forceall` to bypass cache and resume reuse for selected jobs.
- **Cache options keep software identity.** `cache: True` and `"omit-software"`
  use iunctus's cache, whose key retains the software environment. Other
  unsupported omission modes can be refused. Expect recomputation after a
  software change rather than assuming Snakemake's cache layout or key rules.
- **Group counts and retries differ.** Connected same-image `group:` jobs run
  sequentially as one task and retry as a whole. Different images use separate
  tasks; local rules and checkpoints remain separate. Compare member jobs rather
  than assuming task counts equal Snakemake job counts.
- **FIFO pipes need one image.** Jobs connected by `pipe()` run together in one
  container with named pipes; a failure or retry affects the whole group.
  Pipes between jobs with different images, `run:` jobs, several consumers and
  cycles are refused. Give the connected rules one container image.
- **Hooks still need a local engine.** Hook shell and supported subprocess
  commands run uncached in a local container, even with remote rule execution.
  They mount the workflow folder writable and use the global or default image.
  Provide that engine and image. Simulations skip hook commands.
- **Error hooks can delay shutdown indefinitely.** Snakemake `onerror` commands
  receive a fresh token in [commands.rs](../../snakemake-runtime/src/commands.rs),
  and [hooks.rs](../../snakemake-runtime/src/hooks.rs) supplies no task deadline.
  Cancelling the run therefore does not stop an error hook that never exits.
  Use finite cleanup commands. A separate hook cancellation or deadline policy
  is needed; an arbitrary timeout could interrupt valid cleanup.
- **Shadow paths are constrained.** Minimal shadow stages declared inputs and
  task code; shallow shadow links top-level entries. A shallow output under an
  existing directory link would escape scratch and is refused. Use full shadow
  for that layout and keep outputs inside scratch. `copy-minimal` is unsupported.
- **Storage plugins map to iunctus providers.** S3, GCS, Azure, HTTP, FTP and
  `fs` storage use the configured iunctus storage providers, with remote freshness
  checks, staged inputs and uploaded outputs. SFTP, XRootD, Zenodo, iRODS,
  plugin-specific options and protected storage outputs are refused.
  HTTP and FTP outputs need a writable provider.

See [Snakemake usage](snakemake.md) and [Python package boundaries](../design/snakemake/python-packages.md).

## Conda and pip

- **Environments become images.** A host Conda installation is not used.
  Locked packages and installation scripts run inside the image build.
  Configure `[conda]` with a pinned base, platform and matching target capabilities.
  A Conda-only base must provide micromamba; the classic image builder is required.
  BuildKit support is not implemented.
- **Packages come from the network once.** Conda metadata and packages are
  fetched over HTTPS, cached and checked by SHA-256. With `-no-network`, an
  offline Conda setting or `[network] default = "none"`, a cold cache fails
  with a message that says how to prepare it.
- **Environment sources are limited.** Use local YAML files. Remote executors
  need `[conda] registry` and a local Docker-compatible engine: iunctus builds the
  image, pushes it and gives TES, SLURM or Kubernetes the image by digest; the
  workers need pull access. Remote YAML sources and named host environments
  remain unsupported.
- **Combined rules need matching bases.** A configured `conda:` plus
  `container:` rule builds on that rule's image. Its manifest must match the
  configured target base. Match the target configuration; without `[conda]`,
  the container alone supplies the environment.
- **Pip supports named requirements.** Editable installs, local paths and
  archives, direct URLs, VCS requirements and custom-index options are refused.
  The native resolver locks compatible artifacts and SHA-256 hashes.
  Use supported named requirements from PyPI or prepare your own task image.
- **Source builds need static metadata and tools.** Dynamic source metadata
  and local build backends are refused. Source builds run inside the image and
  need build requirements and compilers in the locked Conda environment.
  Prefer compatible wheels or include those tools for a supported source build.
- **Pip pins need JSON locks.** A `.txt` explicit Conda list cannot retain pip
  artifact pins and is refused for pip environments. Keep JSON output from
  `iunctus lock-environment` when the environment has a `pip:` section.

See [Conda configuration](configuration.md#conda) and [environment limits](../design/conda.md#limits).

## CWL

- **JavaScript needs a separate helper image.** Expressions require
  `iunctus-cwl-js:0.1.0`, including simulated runs. The bounded native JavaScript
  engine runs in a local helper container. Load the supplied helper artifact or
  build it from a trusted Linux binary using the [helper instructions](../support.md#installation-and-operation).
  `IUNCTUS_CWL_JS_IMAGE` can select a replacement image.
- **Expression budgets can stop evaluation.** The helper has a 20-second
  operation budget, 256 MiB memory limit and 16 MiB message limit.
  Large or long expressions can exceed them. Simplify expressions or move the
  computation into a normal container task.
- **Image builds are local only.** `dockerFile`, `dockerLoad` and
  `dockerImport` are prepared on the local engine and tagged by content. Remote
  executors cannot use them; push the image to a registry and use `dockerPull`.
- **Formats follow the ontology.** RDF/XML and Turtle ontologies from
  `$schemas` are read, and subclass and equivalent-class relationships are checked
  transitively. Other ontology formats are not read.
- **Directory updates use copies.** Writable and in-place Directory inputs get a
  copy whose updated version flows downstream. Conflicting File updates through a
  parent Directory view are refused.
- **Required extensions can fail.** `ShmSize` works on the local executor.
  Required `Secrets` and `RuntimeConstraints` are unsupported; CUDA requirements support static device
  counts only. Hints may be ignored. Use a supported requirement and executor,
  or another runner when the required behavior must be preserved.

See [CWL usage](cwl-argo.md#cwl). Simulation coverage is listed below.

## Argo

- **The Argo controller is not used.** Workflows run through iunctus's task
  graph, without requiring Kubernetes. Controller-specific behavior is not
  implied by document acceptance. Use supported templates or a real Argo
  deployment for cluster-dependent work.
- **Cluster templates are limited.** Resource templates that manage Jobs,
  Deployments, Services and similar objects are refused. Supported claim and
  child-workflow operations are exceptions. Daemons, sidecars and data templates
  are unavailable. Use ordinary container tasks or the Argo controller.
- **Some controls are refused.** Suspend without a duration (manual resume),
  `podSpecPatch`, daemon and sidecar containers, container lifecycle hooks,
  `livenessProbe`, `startupProbe` and restart policies cannot be honored.
  Timed suspend, mutexes and semaphores within a run are supported. Documents using them fail, including
  affected Kubeflow output. Rework the workflow only if those semantics are
  unnecessary; otherwise use Argo.
- **Retries have no placement control.** Retry backoff (duration, factor, cap,
  `maxDuration`) and all retry policies are honored. Retry host affinity and
  readiness probes have no effect. Use an Argo controller if placement matters.
- **Deadlines are run-local.** Workflow and template `activeDeadlineSeconds`
  and template `timeout` are enforced and still run exit handlers. Pending
  timeouts that depend on cluster scheduling are not modeled.
- **Cluster cleanup and memoization are no-ops.** Garbage-collection fields have
  no cluster objects to remove; user-keyed memoization uses no Argo cache.
  Native task caching still applies. Manage retention through iunctus and storage
  settings rather than relying on these fields.
- **Artifact transfers are a subset.** Tar, gzip and zip archive options and
  S3 delivery with an explicit bucket and key work. Git/HDFS/OSS/plugin transfers,
  external archive extraction and inline S3 endpoints are refused.
  Storage access belongs to iunctus configuration. Use supported transfers with a
  configured provider, or the controller for the missing artifact behavior.
- **Requests and limits do not keep separate roles.** CPU and memory limits
  take precedence for reservation and caps. Requests apply when limits are absent;
  local CPU requests round up to whole cores. Check the effective resources;
  do not assume Kubernetes scheduling behavior.

See [Argo usage](cwl-argo.md#argo) and the [execution boundaries](../support.md#workflow-languages).

## Executors

### Local

- **Local containers are the default.** A missing Docker or Podman socket stops
  real execution. No host fallback exists. Start the engine, check permissions
  and use `iunctus check-machine -executor local` before a run.
- **Rootless behavior is unqualified.** Selected rootful Docker runs passed,
  but rootless Podman and broader mixed-user permissions have no current live
  qualification. Engine and image users affect file access. Verify those cases
  on the intended host before depending on them.
- **Resources can adapt between runs.** Earlier measurements can reduce
  resource requests and combine short tasks in one container. These optimizations
  can change scheduling and measurement detail. Use `-no-right-sizing` and
  `-no-grouping` when checking the workflow's original requests and task isolation.

### TES and S3

- **Shared-path staging is the default.** Workers need the same absolute store
  and scratch paths. A reachable TES endpoint alone is insufficient.
  Configure shared paths or select `staging = "object-store"` with writable S3 staging.
- **Worker credentials are separate.** iunctus's S3 and registry credentials
  do not configure TES workers. Workers must reach the same bucket, endpoint and
  images. Configure and verify access on both sides.
- **Ambiguous submission needs reconciliation.** `idempotency_tag` is safe only
  when the server deduplicates that tag. Funnel 0.11.14 ignores it, and replay
  returned different task IDs. Leave the setting unset without that contract;
  reconcile uncertain submissions before resubmitting.
- **Some task forms are refused.** TES submits one executor and cannot preserve
  stdin, symbolic-link inputs, several containers, in-place input updates,
  offline tasks or GPUs. Staging also bounds input and output counts.
  Use a supported task form or another executor; see [TES limits](executors.md#tes).
- **Small live checks qualify one deployment.** Funnel and MinIO passed selected
  output, resume, failure, cancellation and known-task restart checks.
  They do not qualify Aruna's TES endpoint or another service's authentication,
  immutable versions or lost-response deduplication. Verify your deployment.

### SLURM

- **The cluster needs shared paths and tools.** Scheduler commands must be on
  `PATH`; compute nodes need matching store, scratch and image paths.
  Apptainer is the default runtime. Follow [SLURM setup](executors.md#slurm)
  and run `iunctus check-machine -executor slurm`.
- **Apptainer user behavior differs.** It runs as the submitting user rather
  than the Argo image user. Its writable temporary filesystem can also have a
  small site-defined limit. Check user, working-directory and writable-space
  assumptions; use a suitable image or another container runtime when needed.
- **Historical runs are not current qualification.** Scheduler simulations and
  older cluster results do not validate this beta binary or site configuration.
  Current live qualification remains outstanding. Check real output bytes,
  cancellation, permissions and resources on your cluster.

### Kubernetes

- **An explicit kubeconfig and shared paths are required.** No default
  kubeconfig is silently loaded. Nodes stage tasks through matching host paths;
  the executor creates Jobs, not Argo Workflow objects. Configure the namespace,
  kubeconfig and paths using [Kubernetes setup](executors.md#kubernetes).
- **Fake API checks are not live qualification.** Current tests do not establish
  cluster execution, storage permissions or network isolation. Live-cluster
  qualification remains outstanding. Validate those behaviors in your cluster.

## Storage, cache and disk use

- **Disk use grows beyond published results.** Store contents, retained scratch,
  downloads, pipeline archives and engine images occupy separate space.
  Small test inputs can still need large images. Monitor all affected filesystems;
  `check-machine` reports free store space but reserves none for future work.
- **Cleanup needs retention rules.** Bare `iunctus clean` is a usage error.
  Preview with `iunctus clean -keep-days 30`; use `-keep-run RUN_ID` for runs
  that must remain repeatable. Add `-remove` only after reviewing the preview.
- **Cleanup is not a complete disk purge.** It removes unretained store entries
  and eligible completed scratch, not engine images, published results, pipeline
  archives or every download cache. Clean those separately when no longer needed.
  Retained run rows can refer to removed files.
- **Shared-store retention sees one database.** Cleanup protects active leases
  but cannot know every old run in another machine's database.
  Coordinate retention across hosts. Preserve unresolved remote attempt journals
  until ownership is reconciled.
- **Resume and repeat need retained identities.** Run records alone cannot
  recreate missing local inputs, output bytes or images. Preserve their bytes,
  immutable image identities and configuration alongside the database.
  Simulated runs keep no source archive; repeat needs their unchanged source files.
- **Writable remote storage needs configuration.** Public reads do not imply
  write access. HTTPS and FTP providers cannot host a writable content store.
  Configure a writable S3 or other supported provider; verify credentials and paths.
- **Some remote inputs are unsupported.** Remote directory addresses, FTPS and
  SFTP are unavailable through the native storage path. Recognized workflow
  syntax does not add a transfer implementation. Stage directories locally or
  use supported file addresses and providers.
- **Cached URLs can retain old bytes.** Downloads reuse complete bundles keyed
  by provider and address rather than fetching that address every run.
  A changed remote file can therefore leave an old local copy in use.
  Use immutable versioned addresses for reproducible inputs.
- **TES staging objects persist.** S3 inputs and attempt outputs remain for
  replay and diagnostics after import. Local cleanup does not apply bucket
  retention. Set bucket lifecycle rules that preserve needed recovery data.
- **Automatic credential discovery is not qualified here.** S3 and Google
  defaults defer to OpenDAL's credential providers. Their discovery chains were
  not exercised for the storage guide. Use explicit credentials or verify the
  intended discovery chain; anonymous access must be deliberate.
- **Exact S3 versions have limits.** Mutable `null` version selectors and
  unsupported query parameters are refused. They cannot pin immutable bytes.
  Use a real immutable object version or retain the required bytes locally.
- **Upgrade rollback needs a matching backup.** Database opening applies forward
  migrations and refuses newer schemas. Replacing only the binary cannot undo
  them. Stop writers, back up SQLite consistently with its store and retained
  scratch, and test upgrades on copies. No downgrade migration is guaranteed.

See [storage settings](storage.md) and [cleanup details](troubleshooting.md#clean-the-store).

## Security

- **The run page exposes run data.** It binds to loopback by default; `-address`
  can expose commands, logs and outputs elsewhere. A non-loopback bind requires
  a token, generated if none is configured. Keep the complete printed URL private
  and set `[page] token` for stable access. It is access control, not tenant isolation.
  The page uses HTTP. Use a private network or HTTPS proxy for remote access.
- **Secrets are not workflow values.** Nextflow `secrets` reads and required
  CWL secret extensions are unsupported. Plain parameters and task logs can be
  recorded. Use configuration environment references for supported service
  credentials, and keep sensitive values out of parameters, scripts and logs.
- **Registry login can depend on unavailable helpers.** Docker or Podman login
  may store credentials only through `credsStore` or `credHelpers`.
  iunctus never runs those programs. Configure direct `[registry."HOST"]`
  credentials or inline Docker/Podman auth, including on remote workers.
- **Network policy is not an HTTP proxy.** `-no-network` requests task isolation
  and fails when the executor cannot enforce it. `[network] proxy` configures
  iunctus requests rather than task access control. Choose an executor that
  supports the required isolation.
- **Workflow interpretation needs trust.** Native workflow code can read files
  during planning; Snakemake hooks can write the mounted workflow folder.
  Task containers do not isolate the interpreter. Run trusted workflows with
  an account whose files and credentials they may access.

See [page and registry setup](executors.md) and [secret configuration](configuration.md).

## Embedding

- **A returned run can contain a failure.** Setup failures return an error, but
  workflow failure and cancellation remain in `execution.error` with a partial
  graph. Use `execution.into_result()` when the host requires successful execution.
- **Dropping a future does not finish cleanup.** The async request uses a
  blocking worker that cannot be forcibly aborted. Dropping requests cancellation
  while cleanup continues. Cancel the host token and await completion.
  Use the async API inside Tokio rather than blocking the runtime thread.
- **Task limits do not bound the interpreter.** CPU, memory and pending-task
  settings constrain task requests. They do not impose a process-wide budget or
  hard native interpretation timeout. Apply host isolation and resource budgets
  when accepting work from others.
- **Configuration can be shared across requests.** Without explicit `Setup`,
  requests load CLI configuration. Some cache, proxy and environment settings
  remain process-wide. Supply a host-owned setup and provide authorization and
  tenant cache isolation outside this API.
- **Forms are contracts, not a hosted UI.** Typed validation does not insert
  defaults, coerce values, fetch remote bytes or authorize object access.
  Discovery can read resources and need helper containers. Render the form in
  the host application, submit explicit typed values and validate again for execution.
- **Strict admission can reject a CLI-compatible request.** `RunRequest::new`
  requires an authoritative schema without simulated assumptions. Missing or
  advisory schemas can be refused. Supply the schema or deliberately choose
  relaxed policy when that weaker contract fits the host application.

See [embedding contracts](../design/embedding.md).

## Testing and qualification status

- **Nextflow output comparisons are selective.** Recorded demo, datasync,
  fastqrepair, phyloplace and methylseq comparisons exclude pipeline metadata
  and normalize specified report fields. Datasync uses its dry-run test profile.
  Compare the exact files and scientific values you need; these receipts do not
  establish every pipeline, profile or byte-for-byte report equivalence.
- **Large pipeline comparisons remain incomplete.** rnaseq was skipped for
  disk reserve; sarek's reference run stopped at the disk guard. Fetchngs has
  partial comparisons and no full rerun after the UUID fix. Nanoseq has no complete
  reference comparison; viralrecon completed with altered database parameters but
  no file-by-file comparison. Taxprofiler was not run. Treat these as unqualified.
- **Snakemake counts check planning.** Recorded simulation succeeds for 639 of
  892 test workflows. Of 531 real Snakemake dry-run tables, 494 match job counts
  and 37 have [documented differences](../../snakemake-runtime/tests/fixtures/job_differences.txt).
  Another 108 successful simulations have no comparable table. Groups, checkpoints,
  top-level commands and dynamic replanning explain count differences.
  These numbers do not compare real tool outputs; validate your workflow's results.
- **CWL conformance is simulated.** The
  [recorded fixture](../../cwl-runtime/tests/fixtures/conformance.txt) has 623 passes
  out of 828 tests, or 75%, with 205 marked `ran` and none unsupported.
  `ran` means the output needs real tools. A real conformance suite is not recorded.
  Run the relevant real cases before claiming conformance.
- **Argo records check simulated execution and admission.** The
  [recorded documents](../../argo-runtime/tests/fixtures/test-workflows/outcomes.txt)
  contain accepted plans, documents without entrypoints and explicit refusals.
  They do not qualify Argo controller equivalence. Verify real artifacts and
  required control behavior for the documents you use.
- **GitHub CI remains unconfirmed.** Committed CI definitions and local checks
  do not prove a successful remote run. The support notes record no confirmed
  green GitHub execution. Require the source checks and explicit qualification
  against the exact revision before treating a candidate as qualified.
- **Telemetry is best effort.** Live export can drop batches after failures or
  drain deadlines; successful exports are not a durable delivery receipt.
  Default tracing shows warnings, while `-v` enables debug request logging.
  Check the collector or export the recorded run with `iunctus telemetry RUN_ID`.

Qualification receipts are in [support](../support.md) and the
[dated progress notes](../universal-workflow-progress.md). Green Rust tests,
parser acceptance and simulated outputs do not replace real backend validation.

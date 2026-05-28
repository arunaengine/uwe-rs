<!-- Research notes on the Argo Workflows language, its Kubernetes coupling, an inventory of test workflows and a Rust parser evaluation. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Argo Workflows: language, Kubernetes coupling, and what a native Rust implementation needs

Research date: 2026-09-27. Main evidence: a blobless sparse clone of `argoproj/argo-workflows` at tag v4.1.4 (commit b5b4d665, 2026-09-18), with its examples, docs, CRDs, JSON schema and the Go source of the controller, the executor and the template code. I built a set of test workflows: 1,466 unique Argo documents from 19 repositories (Q6). I ran a feature inventory over it (Q7). I generated Rust types with kopium and typify and loaded every document of the test workflows with serde-saphyr (Q8). I compiled every expression of the test workflows with the Rust `expr-lang` crate (Q3). Line numbers refer to files at v4.1.4. "Own measurement" marks numbers I computed myself. Everything under "Inferences" is my own reasoning, not a sourced fact.

The test workflows, the scripts and the raw results are in `~/.cache/iunctus/test-workflows/argo/`. Sources and commits are listed in `~/.cache/iunctus/test-workflows/argo/revisions.txt`. Scripts and outputs are in `~/.cache/iunctus/test-workflows/argo/_analysis/`.

## Q1. Current version, resource kinds and recent changes

### Takeaway
The current release is v4.1.4 (2026-09-18). The 4.x line started with v4.0.0 on 2026-02-04. v4.0 removed several v3 fields (`schedule` in CronWorkflows, singular `mutex` and `semaphore`, `podPriority`). Real corpora still use the removed fields, so iunctus must accept both the v3 and the v4 forms. The four document kinds that matter are `Workflow`, `WorkflowTemplate`, `ClusterWorkflowTemplate` and `CronWorkflow`, all in API group `argoproj.io/v1alpha1`.

### Cited Findings
- Latest release v4.1.4, published 2026-09-18. v4.0.12 was released the same day. v3.7.18 (2026-08-14) is the latest 3.x release. Minor releases: v4.1.0 (2026-08-11), v4.0.0 (2026-02-04), v3.7.0 (2025-07-23), v3.6.0 (2024-11-14), v3.5.0 (2023-10-13) ([GitHub releases](https://github.com/argoproj/argo-workflows/releases)).
- "Argo Workflows is an open source container-native workflow engine for orchestrating parallel jobs on Kubernetes." "Argo is a Cloud Native Computing Foundation (CNCF) graduated project." License Apache-2.0, 17,006 GitHub stars on 2026-09-27 ([README.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/README.md)).
- v4.0 removed `schedule` (replaced by the list `schedules`), `mutex` (replaced by `mutexes`), `semaphore` (replaced by `semaphores`) and `podPriority`. The `argo convert` command rewrites old documents to the new syntax ([docs/upgrading.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/upgrading.md), [docs/deprecations.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/deprecations.md)).
- v4.0 removed the Python SDK and recommends Hera instead. v4.0 release manifests default to "full" CRDs with validation schemas ([docs/upgrading.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/upgrading.md)).
- Patch releases can change semantics. v4.0.7 and v3.7.16 changed how outputs of skipped and omitted steps resolve: they now resolve to `valueFrom.default`, or to an "absent" value, or the node fails with a terminal error. An expression that evaluates to `nil` in `outputs.parameters` now fails the node unless a default is declared. "There is no configuration flag or environment variable to opt out" ([docs/upgrading.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/upgrading.md), [docs/variables.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/variables.md)).
- v4.1 added, among others: `pendingTimeout` on templates, pod-level `podResources`, `resourceClaims` for Dynamic Resource Allocation, workflow-level executor plugin configuration, S3 virtual-hosted-style addressing, streaming artifact saves, and an opt-in "init-less pod" layout ([docs/new-features.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/new-features.md)).
- The JSON schema `api/jsonschema/schema.json` (draft 2020-12, 425 definitions, 168 of them `io.argoproj.workflow.*`) has a top-level `oneOf` over ClusterWorkflowTemplate, CronWorkflow, Workflow, WorkflowEventBinding and WorkflowTemplate ([schema.json](https://github.com/argoproj/argo-workflows/blob/v4.1.4/api/jsonschema/schema.json)). Own measurement.
- The full CRDs are in `manifests/base/crds/full/` (about 2.7 to 2.9 MB each for the four main kinds). The minimal CRDs in `manifests/base/crds/minimal/` are about 2 KB each and carry no field schema ([manifests/base/crds](https://github.com/argoproj/argo-workflows/tree/v4.1.4/manifests/base/crds)). Own measurement.

### Inferences
- iunctus should pin one reference version (v4.1.x) and add a normalization step that turns v3 fields into v4 fields, as `argo convert` does.
- Patch-level semantic changes (v4.0.7) mean the reference version must be stated per test fixture.

### Gaps
- I did not compare v3.7 and v4.1 schemas field by field.

## Q2. The workflow model

### Takeaway
An Argo document is a list of named templates plus an entrypoint. A template is either a leaf that does work (`container`, `script`, `resource`, `http`, `plugin`, `containerSet`, `data`), a pause (`suspend`), or an orchestrator (`steps` or `dag`) that calls other templates. Values flow as string parameters and as file artifacts. Control flow uses `when`, `depends`, three loop kinds, retries, timeouts, exit handlers and lifecycle hooks. Templates can live in other documents (`templateRef`, `workflowTemplateRef`). The model maps well onto a task DAG, but it is dynamic: `withParam` fans out over runtime outputs, and templates can call themselves recursively.

### Cited Findings
- `Template` properties in the schema: `activeDeadlineSeconds`, `affinity`, `annotations`, `archiveLocation`, `automountServiceAccountToken`, `container`, `containerSet`, `daemon`, `dag`, `data`, `executor`, `failFast`, `hostAliases`, `http`, `initContainers`, `inputs`, `memoize`, `metadata`, `metrics`, `name`, `nodeSelector`, `outputs`, `parallelism`, `pendingTimeout`, `plugin`, `podResources`, `podSpecPatch`, `priorityClassName`, `resource`, `resourceClaims`, `retryStrategy`, `schedulerName`, `script`, `securityContext`, `serviceAccountName`, `sidecars`, `steps`, `suspend`, `synchronization`, `timeout`, `tolerations`, `volumes` ([schema.json](https://github.com/argoproj/argo-workflows/blob/v4.1.4/api/jsonschema/schema.json)). Own extraction.
- `WorkflowSpec` properties include `entrypoint`, `arguments`, `templates`, `onExit`, `hooks`, `workflowTemplateRef`, `templateDefaults`, `volumes`, `volumeClaimTemplates`, `parallelism`, `activeDeadlineSeconds`, `retryStrategy`, `synchronization`, `artifactRepositoryRef`, `artifactGC`, `podGC`, `ttlStrategy`, `serviceAccountName`, `podSpecPatch`, `nodeSelector`, `tolerations`, `affinity`, `securityContext`, `imagePullSecrets`, `executorPlugins` and more ([schema.json](https://github.com/argoproj/argo-workflows/blob/v4.1.4/api/jsonschema/schema.json)). Own extraction.
- A step (`WorkflowStep`) and a DAG task (`DAGTask`) share `arguments`, `continueOn`, `hooks`, `inline`, `name`, `onExit`, `template`, `templateRef`, `when`, `withItems`, `withParam`, `withSequence`. DAG tasks add `dependencies` and `depends` ([schema.json](https://github.com/argoproj/argo-workflows/blob/v4.1.4/api/jsonschema/schema.json)).
- `steps` is a list of lists. Each inner list runs in parallel. The outer list runs in sequence. The Go type `ParallelSteps` has a custom unmarshaller that re-implements "DisallowUnknownFields" by hand (workflow_types.go line 656) ([workflow_types.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/pkg/apis/workflow/v1alpha1/workflow_types.go)).
- `dependencies: [A, B]` is shorthand for `depends: (A.Succeeded || A.Skipped || A.Daemoned) && (B...)`, extended with `Errored` and `Failed` when the dependency has `continueOn` (ancestry.go lines 110-135). Task results in `depends` are `Succeeded`, `Failed`, `Errored`, `Skipped`, `Omitted`, `Daemoned`, `AnySucceeded`, `AllFailed` ([ancestry.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/common/ancestry.go)).
- DAG `failFast` defaults to true: after one node fails, no new nodes are scheduled, and the DAG fails once running nodes finish ([docs/fields.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/fields.md)).
- Loops: `withItems` (literal list, items may be maps and are read with `item.<key>`), `withParam` (a JSON list, usually from an earlier output), `withSequence` (`count` or `start`/`end`, optional `format`). When a looped step has outputs, `steps.<NAME>.outputs.parameters.<P>` is a JSON array of the per-iteration values ([docs/variables.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/variables.md), [docs/walk-through/loops.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/walk-through/loops.md)).
- Parameters are strings. The Go type `AnyString` accepts JSON numbers and booleans and turns them into strings ([anystring.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/pkg/apis/workflow/v1alpha1/anystring.go)). Output parameters come from `valueFrom.path`, `jsonPath`, `jqFilter`, `expression`, `parameter`, `supplied`, `configMapKeyRef`, `event` or `default` ([schema.json](https://github.com/argoproj/argo-workflows/blob/v4.1.4/api/jsonschema/schema.json)). "For script and container templates, the `result` output parameter captures up to 256 kb of the standard output" ([output-parameters.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/walk-through/output-parameters.md)).
- Artifact locations: `s3`, `gcs`, `azure`, `git`, `http`, `artifactory`, `hdfs`, `oss`, `raw`, `plugin`, plus `from` and `fromExpression` for passing between nodes. Other fields: `archive`, `mode`, `optional`, `subPath`, `globalName`, `artifactGC`, `recurseMode` ([schema.json](https://github.com/argoproj/argo-workflows/blob/v4.1.4/api/jsonschema/schema.json)). Output artifacts are "packaged as Tarballs and gzipped by default" ([artifacts.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/walk-through/artifacts.md)). On input, the executor untars a file if it detects a gzip tarball, unless `archive` says otherwise (executor.go lines 364-400) ([executor.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/executor/executor.go)). Artifacts without a location use the default artifact repository, configured in the controller ConfigMap or referenced by `artifactRepositoryRef` ([docs/artifact-repository-ref.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/artifact-repository-ref.md), [docs/key-only-artifacts.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/key-only-artifacts.md)).
- `retryStrategy` has `limit`, `retryPolicy` (`Always`, `OnFailure`, `OnError`, `OnTransientError`), `backoff`, `expression` and `affinity`. "The default `retryPolicy` is `OnFailure`, except in version 3.5 or later when an expression is also supplied, when it is `Always`" ([docs/retries.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/retries.md)).
- Timeouts: workflow and template `activeDeadlineSeconds`; template `timeout` is "the total node execution timeout duration counting from the node's start time", and "may not be applied to Step or DAG templates"; v4.1 adds `pendingTimeout` ([docs/fields.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/fields.md), [docs/new-features.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/new-features.md)).
- Exit handlers: `spec.onExit` and step or task `onExit` run a template after completion. They see `workflow.status` and `workflow.failures`. A `LifecycleHook` runs a template once "when a configured expression is met"; a hook named `exit` is an exit handler ([docs/lifecyclehook.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/lifecyclehook.md), [docs/variables.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/variables.md)).
- Memoization stores outputs under a user-written `key` "in config-maps", with optional `maxAge`. Since v3.5 all steps can be memoized ([docs/memoization.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/memoization.md)). See also the cache discussion in [ir-design-models.md](ir-design-models.md).
- Synchronization: `mutexes` and `semaphores` at workflow or template level. Local locks live in the controller; semaphore sizes come from a ConfigMap. "Multiple controller locks" use a database ([docs/synchronization.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/synchronization.md)).
- `suspend` pauses until `argo resume` or until an optional `duration` ends ([suspending.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/walk-through/suspending.md)).
- `containerSet` runs several containers in one pod, with dependencies between them, sharing `emptyDir` volumes ([docs/container-set-template.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/container-set-template.md)).
- `data` templates list artifact paths from a repository and then apply expression transformations. The docs call the feature "bare-bones" ([docs/data-sourcing-and-transformation.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/data-sourcing-and-transformation.md)).
- Recursion is allowed: `examples/coinflip-recursive.yaml` and `examples/recursive-for-loop.yaml` call their own template from a step ([examples](https://github.com/argoproj/argo-workflows/tree/v4.1.4/examples)).

### Inferences
- Argo's node states (`Succeeded`, `Failed`, `Errored`, `Skipped`, `Omitted`, `Daemoned`) must be first-class in the iunctus IR, because `depends` and exit handlers branch on them.
- The IR must support fan-out whose size is known only at run time (`withParam`), and unbounded recursion. This is the same kind of dynamic expansion that Nextflow channels and Snakemake checkpoints need.
- Parameter values are always strings. That keeps the value model simple. The complexity sits in substitution (Q3).

### Gaps
- I did not trace every controller rule for node naming, retry child nodes and aggregation of looped outputs. These need test-driven work against `workflow/controller/*_test.go`.

## Q3. Expression languages and variable substitution

### Takeaway
Argo uses three kinds of expressions. Simple tags (`{{inputs.parameters.x}}`) are string lookups. Expression tags (`{{=...}}`) and most expression fields (`depends`, hook and retry expressions, HTTP `successCondition`, `valueFrom.expression`, `fromExpression`, `data` transformations) use expr-lang (Go `github.com/expr-lang/expr` v1.17.8) plus Argo helpers and the Sprig function map. `when` clauses use govaluate after simple-tag substitution, with a trick that turns bare words into strings. The Rust crate `expr-lang` 2.1.0 compiles 1,427 of 1,431 unique expressions of the test workflows. It needs work for `sprig.fn(...)` calls, and it panics on one real expression. No Rust govaluate exists; iunctus needs its own small evaluator.

### Cited Findings
- Tag kinds: "simple" (`{{workflow.name}}`) and "expression", "where `{{` is immediately followed by `=`". Hyphenated names must be indexed: `inputs.parameters['my-param']`. Argo adds `asInt`, `asFloat`, `toJson`, `jsonpath` and `sprig.*` ([docs/variables.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/variables.md)).
- The expression environment: `GetFuncMap` expands the flat variable map into nested maps (`expand.Expand`), aliases `asInt` and `asFloat` to expr builtins, adds `jsonpath` (using `github.com/evilmonkeyinc/jsonpath`) and `toJson`, and exposes `sprig.GenericFuncMap()` without `env` and `expandenv` (env.go line 20) ([env.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/util/expr/env/env.go)). Library versions: expr v1.17.8, sprig v3.3.0, govaluate at a 2025-03-25 commit (pseudo-version `v3.0.1-0.20250325060307-7625b7f8c03d`), evilmonkeyinc/jsonpath v0.8.1, gojq v0.12.19 ([go.mod](https://github.com/argoproj/argo-workflows/blob/v4.1.4/go.mod)).
- Substitution works on JSON text. `SubstituteParams` marshals the whole template to JSON, replaces global and local tags, unmarshals, adds input parameters, input and output artifact paths and output parameter paths, then replaces again (util.go line 273) ([util.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/common/util.go)). `Replace` rejects input or output that is not valid JSON (replace.go line 41) ([replace.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/util/template/replace.go)). Simple values are escaped with `strconv.Quote` before insertion ([simple_template.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/util/template/simple_template.go)). Tags are found with `fasttemplate` using `{{` and `}}` ([template.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/util/template/template.go)).
- Unresolved tags can stay in place for a later phase ("allowUnresolved"). Expression tags that reference late variables (`item`, `retries`, `lastRetry.*`, `workflow.status`, `workflow.failures`) are left unevaluated until those exist. Results are JSON-marshaled, and surrounding quotes are trimmed (expression_template.go lines 31-41 and 205-280) ([expression_template.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/util/template/expression_template.go)).
- `when` uses govaluate. Variables are rewritten into strings "to have expressions like: `foo != bar` without requiring foo and bar to be quoted" (steps.go lines 384-420) ([steps.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/controller/steps.go)). CronWorkflow `when` also uses govaluate ([cron/operator.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/cron/operator.go)).
- `depends` is evaluated with expr after replacing `-` with `_` in task names, over a map of task result booleans (dag.go lines 1003-1058) ([dag.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/controller/dag.go)).
- expr is also used for exit and lifecycle hook expressions, `retryStrategy.expression`, `valueFrom.expression`, `fromExpression`, `data` transformations, HTTP `successCondition` and CronWorkflow `stopStrategy.expression` (own grep for `expr.Compile` and `argoexpr.EvalBool` over the v4.1.4 source).
- Rust `expr-lang` 2.1.0 (jdx/expr.rs, MIT, 2026-08-21, 304,316 downloads): "implements expr syntax and built-ins with Go-compatible runtime semantics". It ships a Go parity test (`tests/go_compat.rs`, 255 cases in `compat/cases.tsv`). "Go-specific reflection and static type checking are not implemented" ([crates.io](https://crates.io/crates/expr-lang), [GitHub](https://github.com/jdx/expr.rs)). Method calls are evaluated only for its date and time values (`src/functions/temporal.rs`, `eval_method`). Own reading of the 2.1.0 source.
- Own measurement over the test workflows (`_analysis/results/exprcheck.txt`): 1,431 unique expressions (497 `{{=...}}` bodies, 904 `depends` strings after Argo's rewrite, 11 `valueFrom.expression`, 7 hook, 5 HTTP, 4 `data`, 2 `fromExpression`, 1 retry). `expr::compile` accepts 1,427. Of the 4 failures, 2 are panics ("entered unreachable code: Unexpected rule: ident sprig") on one Argo example (`examples/expression-destructure-json-complex.yaml`) and its Hera copy. The other 2 are artifacts of my test (a retry expression that still holds a simple tag, and a Hera task named `map-`).
- Own evaluation test: `sprig.osBase(x)` fails with "unknown variable: sprig". The same function registered as `sprig_osBase` works. `filter`, `map`, `toJSON`, `??`, `contains`, `int` and `string` evaluate as documented.
- Own measurement of `when` clauses: 505 occurrences, 135 unique. 58 compare with a bare word (for example `== Succeeded`, `== tails`), 54 use quoted strings, 30 use parentheses, 29 use `&&` or `||`, 17 embed an expression tag, 3 use `<`, `>`, `<=` or `>=`, 2 use `=~`.
- Sprig functions used in the test workflows (by number of documents): `osBase` 103, `osDir` 73, `base` 15, `trimSuffix` 12, `int` 11, `ext` 6, `min` 6, `default` 4, and 1 to 3 documents each for `max`, `add`, `b64dec`, `date`, `replace`, `join`, `last`, `osClean`, `splitList`, `empty`, `lower`, `toString`, `trimPrefix`. Own measurement.
- A crates.io search on 2026-09-27 for "govaluate" returned no crate. Searches for "sprig" returned only partial ports for other template engines (`tera-sprig`, `handlebars_sprig`, `lithos-sprig`) ([crates.io search](https://crates.io/search?q=sprig)).

### Inferences
- Simple tags: implement substitution on string leaves (and map keys) of the parsed document, with phases that match Argo: globals, then inputs, then late variables. Doing it on string leaves avoids the JSON-text round trip but must reproduce its escaping results.
- expr: use `expr-lang` as the base. Add a thin rewrite for `sprig.<fn>(` calls (or contribute member-function support upstream), register about 25 Sprig functions, `asInt`, `asFloat`, `toJson` and `jsonpath`. Run it with `catch_unwind` or fix the panic upstream before using it on untrusted input.
- govaluate: write a small evaluator (tokenizer, precedence parser, strings, numbers, booleans, `==`, `!=`, `<`, `>`, `=~`, `!~`, `&&`, `||`, `!`, parentheses, `in`). The bare-word rule is essential. The grammar is small; a few hundred lines plus tests.
- `jsonpath()` should use a Rust JSONPath crate (`serde_json_path` 0.7.2 or `jsonpath-rust` 1.0.11). Differences to evilmonkeyinc/jsonpath must be tested. `jqFilter` (resource templates only) could use `jaq-core` 3.1.1.

### Gaps
- I only compiled the expressions. I did not evaluate them against real runtime values, so Go-versus-Rust result differences are unverified.
- I did not test `jsonpath` semantics differences between the Go library and Rust crates.

## Q4. What depends on Kubernetes, and how iunctus could run it without Kubernetes

### Takeaway
The workflow logic (templates, steps, DAG, loops, conditions, retries, parameters, expressions) lives in the controller and does not need Kubernetes in principle. What needs Kubernetes is the execution layer: each leaf runs as a pod with helper containers, data moves through volumes and object stores, and many fields are pod settings. Most of it can be mapped to a local container runtime or to TES. The main conflict is storage: real bioinformatics users treat PVCs and host paths as a shared file system, which fits local and HPC runs but not TES object staging. Resource templates and executor plugins need a Kubernetes API and are out of scope for a cluster-free runner.

### Cited Findings
- Each step or task becomes a pod with a `main` container, an `init` container that fetches artifacts and parameters, and a `wait` container that "performs tasks that are needed for clean up, including saving off parameters and artifacts" ([docs/architecture.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/architecture.md)). Since 3.4 the only executor is `emissary`, which reads and writes the container disk ([docs/workflow-executors.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/workflow-executors.md)).
- `steps`, `dag` and `suspend` run inside the controller. `container`, `script`, `containerSet`, `resource` and `data` create pods; `data` runs `argoexec data` in a pod (operator.go lines 3434-4060) ([operator.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/controller/operator.go)). "HTTP and Plugin Templates use the Argo Agent", which talks to the controller through the `WorkflowTaskSet` CRD ([docs/http-template.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/http-template.md)).
- Executor plugins are disabled by default and are enabled per controller or per workflow ([docs/executor_plugins.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/executor_plugins.md)). Artifact plugins implement a gRPC service over a Unix socket ([docs/artifact-plugin.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/artifact-plugin.md)).
- Kubernetes-backed state: memoization caches in ConfigMaps, semaphore sizes in ConfigMaps, parameters from `configMapKeyRef`, environment from `secretKeyRef`, artifact credentials from Secrets, WorkflowTemplates looked up in the cluster (Q2 sources).
- TES has no GPU or wall-time field, per-task `volumes` only, optional inline `content` for inputs, and "no guarantees" for captured stdout. Aruna's TES route does not support inline content or volumes. See [tes-and-executors.md](tes-and-executors.md).
- Real bioinformatics repositories use volumes as a shared file system. gennext-workflows passes file paths as parameters and mounts `hostPath` directories derived with `sprig.osDir` (`~/.cache/iunctus/test-workflows/argo/gennext-workflows/tasks/base_tasks/samtools.yaml`). Ensembl creates a PVC with a resource template, passes `pvc-name` through parameters and mounts it in each step (`~/.cache/iunctus/test-workflows/argo/regulation-workflows/workflow-templates/samtools/samtools-sort-pvc-callable-v-0.2.yaml`, `helpers/create-pvc-v-0.2.yaml`).

### Mapping table (inference)

| Argo feature | On Kubernetes | Local container runtime | TES | Suggested iunctus behavior |
|---|---|---|---|---|
| `container`, `script` | Pod with emissary | `docker`/`podman run` | One executor; script as inline `content` or uploaded input | Supported |
| `outputs.result`, exit code | `wait` reads stdout (256 KB) | Capture stdout | Redirect stdout to a file and upload it; exit code from `ExecutorLog` | Supported, with a size limit |
| Output parameter `valueFrom.path` | `wait` reads the file | Read from the task directory | Upload as output, read back | Supported; costs one round trip on TES |
| Artifacts (S3, key-only, default repo, `from`) | `init`/`wait` download and upload, tar.gz | iunctus staging | TES inputs and outputs by URL | Supported; store in iunctus storage; untar inputs like Argo |
| `steps`, `dag`, loops, `when`, `depends`, retries, timeouts, `onExit`, hooks | Controller | iunctus scheduler | iunctus scheduler | Supported natively |
| `emptyDir`, `volumeClaimTemplates` | Pod or workflow volume | Scratch directory per task or per run | TES `volumes` (per task only) | Supported locally; per task only on TES |
| `persistentVolumeClaim`, `hostPath` | Shared cluster storage | Bind mount of a run directory | Not possible without a shared file system | Local and HPC only; clear error on TES |
| `configMap`, `secret` volumes and env refs | Kubernetes objects | Files and values from an iunctus config or secret store | Same, passed as inputs or env | Supported through a stand-in store |
| `resources` (cpu, memory, `nvidia.com/gpu`) | Pod requests and limits | Runtime limits | `cpu_cores`, `ram_gb`; GPU only via `backend_parameters` | Supported; GPU per backend |
| `containerSet` | One pod, parallel containers | Several containers with a shared directory | Several executors, run in sequence | Supported; sequential on TES |
| `daemon`, `sidecars` | Long-running pods with an IP | Background containers on a network | Not possible | Local only |
| `http` | Agent pod | Native HTTP client | Native HTTP client | Supported |
| `data` | `argoexec data` pod | In-process | In-process | Supported (small) |
| `suspend`, resume | Controller and API | iunctus CLI | iunctus CLI | Supported |
| `memoize` | ConfigMap cache | iunctus cache | iunctus cache | Map `key` onto iunctus cache |
| `synchronization` | Controller locks, ConfigMap or database | In-process or file locks | Same | Supported within one iunctus instance |
| `templateRef`, `workflowTemplateRef` | Cluster lookup | Local template library (files, directories, kustomization lists) | Same | Supported |
| `CronWorkflow` | Controller cron | Not a batch concept | Not a batch concept | Run `workflowSpec` once; scheduling later if needed |
| `resource` template | kubectl-like actions against the API | Needs a cluster | Needs a cluster | Out of scope, or an optional Kubernetes backend |
| `plugin` template, artifact plugins | Agent pod, sidecar services, gRPC | Only if an endpoint is configured | Same | Out of scope at first |
| `podSpecPatch`, `serviceAccountName`, `securityContext`, `nodeSelector`, `affinity`, `tolerations`, `podGC`, `imagePullSecrets`, `priorityClassName`, `dnsConfig`, `hostAliases`, `schedulerName` | Pod settings | Ignore, map resources and registry credentials | Ignore, map resources | Warn and ignore; map the useful subset |
| `workflow.uid`, `workflow.namespace`, `pod.name` | Cluster metadata | Synthesized | Synthesized | Synthesize stable values |
| Events, metrics, artifact GC, archive, Argo Server API | Cluster services | n/a | n/a | Out of scope |

### Inferences
- Retry policies need a mapping from iunctus task outcomes to Argo's classes: a failing main command is `Failed` (`OnFailure`), an infrastructure problem is `Error` (`OnError`). TES `EXECUTOR_ERROR` and `SYSTEM_ERROR` map onto these two.
- Workflows that use PVCs or host paths as shared storage can run locally or on HPC with a shared run directory. On TES they need a shared file system, the same limitation Nextflow's TES executor has.
- The Ensembl pattern "create PVC with a resource template, then mount it" could be recognized and replaced by "create a run directory". That is a special case, not general resource-template support.

### Gaps
- I did not test any mapping in practice. Output parameter round trips on TES and the stdout size limit are design assumptions.

## Q5. Existing implementations outside Argo's controller, and Rust crates

### Takeaway
I found one independent runner: argo-junit, a Java library that runs Argo YAML locally with real containers. It uses Apache JEXL instead of expr-lang, so it is not a semantic reference. Most other projects produce Argo YAML (Hera, Couler, KFP, cargo-athena) or lint it. In Rust, no maintained crate provides complete Argo Workflow types. The well-known generated CRD crate covers Argo CD only. The older Argo Workflow crates are generated from 3.x schemas and unmaintained. `cargo-athena-api` is maintained, but it is a small hand-written subset for emitting YAML.

### Cited Findings
- argo-junit (vilmosnagy/argo-junit, Java, Apache-2.0, created 2026-06-02, last push 2026-09-25): "executes Argo Workflows locally against real containers, without a Kubernetes cluster". It supports `script` and `container`, `steps` and `dag` with `depends` and `dependencies`, daemons, artifact passing through a host temp directory, S3/MinIO with tar.gz, `when`, `retryStrategy`, output parameters and artifacts, WorkflowTemplate resolution through kwok (a fake Kubernetes API), and `emptyDir`, `configMap` and `secret` volumes. Types are generated from the Argo OpenAPI spec with openapi-generator. Expressions use "Apache JEXL3" ("No custom parser"). Its lint tests call `argo lint --offline` ([README](https://github.com/vilmosnagy/argo-junit), [DESIGN.md](https://github.com/vilmosnagy/argo-junit/blob/main/DESIGN.md)).
- `argo lint --offline`: "perform offline linting. For resources referencing other resources, the references will be resolved from the provided args" ([docs/cli/argo_lint.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/cli/argo_lint.md)).
- Argo parses user files with `sigs.k8s.io/yaml` into JSON, then decodes strictly (`kjson.UnmarshalStrict`) when `strict` is set (parse.go lines 84-120) ([parse.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/workflow/common/parse.go)).
- Hera (argoproj-labs/hera, 939 stars) is the recommended Python SDK after v4.0 ([docs/upgrading.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/docs/upgrading.md), [Hera](https://github.com/argoproj-labs/hera)).
- Kubeflow Pipelines compiles pipelines to Argo Workflows. The compiled test workflows use `kfp-driver` and `kfp-launcher` images and call `ml-pipeline.kubeflow.svc.cluster.local` (`~/.cache/iunctus/test-workflows/argo/pipelines/test_data/compiled-workflows/two_step_pipeline.yaml`). See also [ir-design-models.md](ir-design-models.md).
- Couler (couler-proj/couler, 943 stars, last push 2024-10-08) is a "Unified Interface for Constructing and Managing Workflows" with an Argo backend ([Couler](https://github.com/couler-proj/couler)).
- tugboat (tzing/tugboat) is "A linter to streamline your Argo Workflows" ([tugboat](https://github.com/tzing/tugboat)).
- `kcr_argoproj_io` 3.20260618.213614 (metio/kube-custom-resources-rs, 0BSD) is kopium-generated, but its modules are Argo CD only (`applications`, `appprojects`, `applicationsets`, `argocds`, `argocdexports`). It has no Workflow types. Own reading of the crate source.
- `argo-crds` 0.3.1 (2023-12-15, MIT) runs typify in `build.rs` on the v3.5.2 JSON schema, downloaded over the network at build time. `argo-workflow` 0.1.0 (2024-02-28, Apache-2.0, dashbook/dashtool) holds 397 generated structs, including Argo Events types. `argo-workflows-crds` 0.1.0 (2022) and `argo-workflows-rs` 0.0.2 (2023) are older ([crates.io search](https://crates.io/search?q=argo-workflows)). Own reading of the crate sources.
- `cargo-athena` and `cargo-athena-api` 0.8.0 (2026-08-14, MIT OR Apache-2.0) "Compile regular Rust into Argo Workflow YAML". The API crate is "a hand-owned, curated subset" covering WorkflowTemplate and Workflow with container, dag and steps templates, artifacts, volumes, parameters, nodeSelector and service accounts. It uses `serde_norway` ([cargo-athena](https://github.com/mostlymaxi/cargo-athena), [crates.io](https://crates.io/crates/cargo-athena-api)).
- Other Rust projects: Shark/wasm-workflows-plugin (an executor plugin that runs WebAssembly), gauravgahlot/argoflows (a client) ([GitHub search](https://github.com/search?q=argo+workflows+language%3ARust&type=repositories)). I did not inspect them.
- Name clash: "ICGC ARGO" is a cancer genomics project whose workflows are written in Nextflow, not Argo Workflows ([icgc-argo-workflows/dna-seq-processing-wfs](https://github.com/icgc-argo-workflows/dna-seq-processing-wfs)).

### Inferences
- iunctus would be the first cluster-free Argo runner that aims at Argo's own expression semantics. argo-junit is still useful as a list of features that others found necessary for local runs.
- `argo lint --offline` and Argo's Go test fixtures can serve as a differential oracle in CI, without bundling Argo.

### Gaps
- I did not run argo-junit. Its feature claims come from its README and DESIGN.md.
- I did not check Metaflow, ZenML or Kedro exports to Argo.

## Q6. The test workflows

### Takeaway
The test workflows have 2,845 text files (34 MB) from 19 repositories. It holds 1,466 unique Argo documents (by content hash; 1,465 with the Rust loader). Only 546 documents are hand-written real-world workflows. Public bioinformatics use of Argo is thin: two sizable repositories (gennext-workflows, 167 documents; Ensembl regulation-workflows, 132 documents) and a few small ones.

### Cited Findings
Own measurement. All clones are blobless sparse clones of YAML and Markdown (and, for Argo itself, Go source and docs). Commits are in `~/.cache/iunctus/test-workflows/argo/revisions.txt`.

| Group | Repositories (commit date) | Unique Argo documents |
|---|---|---|
| Argo examples | argoproj/argo-workflows `examples/` at v4.1.4 | 223 |
| Argo e2e fixtures | argoproj/argo-workflows `test/e2e/**/*.yaml` at v4.1.4 | 285 |
| Generated | argoproj-labs/hera `examples/` (2026-09-08; `*.upstream.yaml` copies excluded), kubeflow/pipelines `test_data/compiled-workflows` (2026-09-27), couler-proj/couler tests (2024-10-07) | 245 + 148 + 19 = 412 |
| Bioinformatics | GenivaInformatics/gennext-workflows (2026-09-27, no license file), Ensembl/regulation-workflows v1.0.1 (Apache-2.0), DataCloud-project/SIM-PIPE, PGpalt/Hybrid-Argo-Workflows, wwood/singlem-sra-processing, DonaldKellett/snakemake-tutorial-argo-workflows, uc-cdis/argo-wrapper | 167 + 132 + 30 = 329 |
| Community | codefresh-io/argo-hub, argoproj-labs/argo-workflows-catalog, pipekit/argo-workflows-ci-example, pipekit/talk-demos, devops-ws/argo-workflows-guide, terrytangyuan/distributed-ml-patterns, thoth-station/workflows, AliyunContainerService/argo-workflow-examples | 217 |

- I found the bioinformatics repositories with GitHub code search for `"argoproj.io/v1alpha1"` together with `samtools`, `bwa`, `gatk`, `fastq`, `vcf`, `bcftools` or `biocontainers`. gennext-workflows and Ensembl dominated every query ([GitHub code search](https://github.com/search?type=code&q=%22argoproj.io%2Fv1alpha1%22+samtools)).
- USERS.md lists about 214 organizations. Three names look like life-science companies (BioBox Analytics, Cyrus Biotechnology, Quantibio) ([USERS.md](https://github.com/argoproj/argo-workflows/blob/v4.1.4/USERS.md)).
- Real-world documents are mostly libraries: 440 of 546 are WorkflowTemplates. gennext has 161 WorkflowTemplates and 6 Workflows; Ensembl 122 and 10; argo-hub 105 WorkflowTemplates only. Own measurement.

### Inferences
- A usable Argo runner must load a template library (directories of WorkflowTemplates, and kustomization lists as in Ensembl) and resolve `templateRef` against it. Loading single files is not enough.
- gennext-workflows has no license file. It can be used as local research test workflows but not redistributed as test data.
- KFP-compiled workflows depend on KFP services and are not a realistic target, even though they parse.

### Gaps
- GitHub code search returns at most 100 results per query and is rate-limited. I may have missed public Argo pipelines.
- I could not query WorkflowHub for Argo entries (its API returned no data). Unverified.

## Q7. Feature inventory

### Takeaway
Real-world documents use a small core: container and script templates, DAGs with `depends`, `templateRef`, simple tags, output parameters from files, retries, and volumes. Expression tags appear in 29% of real-world documents, almost all in the two bioinformatics repositories, mostly for Sprig path functions. `steps` templates and `withItems` loops are much less common than in the official examples. Kubernetes fields are frequent: volume mounts in 45% of real-world documents, `hostPath` or PVC volumes in 33%, secrets in environment variables in 18%.

### Cited Findings
Own measurement (`_analysis/inventory.py`, `_analysis/results/matrix.txt`). Counts are documents that use the feature at least once. "Bio + community" is the real-world subset (546 documents).

| Feature | Argo examples | Argo e2e | Generated | Bio | Community | All | Bio + community |
|---|---|---|---|---|---|---|---|
| Documents | 223 | 285 | 412 | 329 | 217 | 1466 | 546 |
| Kind Workflow | 199 | 232 | 400 | 44 | 55 | 930 | 18% |
| Kind WorkflowTemplate | 13 | 31 | 7 | 284 | 156 | 491 | 81% |
| Kind ClusterWorkflowTemplate | 6 | 7 | 0 | 1 | 4 | 18 | 1% |
| Kind CronWorkflow | 5 | 15 | 5 | 0 | 2 | 27 | 0% |
| container template | 168 | 232 | 310 | 97 | 136 | 943 | 43% |
| script template | 31 | 18 | 89 | 195 | 47 | 380 | 44% |
| dag template | 38 | 54 | 220 | 135 | 54 | 501 | 35% |
| steps template | 99 | 91 | 121 | 28 | 28 | 367 | 10% |
| resource template | 15 | 6 | 18 | 8 | 24 | 71 | 6% |
| suspend template | 3 | 3 | 5 | 0 | 4 | 15 | 1% |
| http template | 4 | 1 | 5 | 2 | 4 | 16 | 1% |
| plugin template | 1 | 3 | 0 | 0 | 4 | 8 | 1% |
| containerSet template | 6 | 2 | 6 | 0 | 3 | 17 | 1% |
| data template | 1 | 2 | 2 | 0 | 0 | 5 | 0% |
| `daemon: true` | 7 | 2 | 7 | 0 | 2 | 18 | 0% |
| `depends` | 30 | 12 | 203 | 110 | 21 | 376 | 24% |
| `dependencies` | 2 | 30 | 6 | 8 | 5 | 51 | 2% |
| `when` | 21 | 15 | 174 | 60 | 4 | 274 | 12% |
| `withItems` | 14 | 11 | 15 | 0 | 4 | 44 | 1% |
| `withParam` | 16 | 11 | 26 | 38 | 5 | 96 | 8% |
| `withSequence` | 8 | 5 | 32 | 5 | 8 | 58 | 2% |
| `continueOn` | 6 | 3 | 7 | 6 | 1 | 23 | 1% |
| `templateRef` (namespaced) | 8 | 12 | 2 | 110 | 20 | 152 | 24% |
| `templateRef` (`clusterScope`) | 4 | 1 | 1 | 5 | 2 | 13 | 1% |
| `workflowTemplateRef` | 6 | 15 | 1 | 3 | 7 | 32 | 2% |
| `inline` template | 4 | 0 | 2 | 0 | 1 | 7 | 0% |
| `retryStrategy` (any level) | 14 | 14 | 14 | 126 | 60 | 228 | 34% |
| `retryStrategy.expression` | 1 | 0 | 1 | 0 | 0 | 2 | 0% |
| template `timeout` | 2 | 0 | 2 | 0 | 0 | 4 | 0% |
| template `activeDeadlineSeconds` | 1 | 1 | 2 | 1 | 16 | 21 | 3% |
| workflow `activeDeadlineSeconds` | 1 | 1 | 1 | 1 | 2 | 6 | 1% |
| workflow `onExit` | 7 | 9 | 9 | 8 | 5 | 38 | 2% |
| step or task `onExit` | 1 | 1 | 2 | 0 | 1 | 5 | 0% |
| `hooks` (workflow level) | 1 | 2 | 1 | 0 | 0 | 4 | 0% |
| `hooks` (step or task level) | 5 | 2 | 14 | 0 | 0 | 21 | 0% |
| `memoize` | 2 | 0 | 1 | 0 | 2 | 5 | 0% |
| `synchronization` (any) | 10 | 15 | 13 | 8 | 1 | 47 | 2% |
| workflow `parallelism` | 3 | 3 | 2 | 2 | 1 | 11 | 1% |
| `templateDefaults` | 1 | 2 | 1 | 0 | 0 | 4 | 0% |
| simple tags `{{...}}` | 123 | 95 | 323 | 319 | 166 | 1026 | 89% |
| expression tags `{{=...}}` | 6 | 5 | 5 | 152 | 4 | 172 | 29% |
| Sprig functions in expressions | 2 | 1 | 3 | 127 | 2 | 135 | 24% |
| `jsonpath()` in expressions | 4 | 2 | 4 | 55 | 0 | 65 | 10% |
| input artifacts | 31 | 39 | 39 | 60 | 29 | 198 | 16% |
| output artifacts | 29 | 60 | 38 | 60 | 20 | 207 | 15% |
| S3 artifact location | 9 | 19 | 8 | 64 | 8 | 108 | 13% |
| key-only S3 artifact | 5 | 4 | 4 | 53 | 6 | 72 | 11% |
| output artifact to default repository | 16 | 36 | 26 | 15 | 14 | 107 | 5% |
| git artifact | 4 | 1 | 4 | 4 | 5 | 18 | 2% |
| http artifact | 3 | 11 | 2 | 2 | 0 | 18 | 0% |
| raw artifact | 1 | 4 | 1 | 4 | 1 | 11 | 1% |
| GCS, Azure, OSS, HDFS or Artifactory | 7 | 1 | 8 | 0 | 0 | 16 | 0% |
| artifact `archive` setting | 5 | 22 | 13 | 49 | 7 | 96 | 10% |
| output parameter `valueFrom.path` | 17 | 22 | 170 | 66 | 62 | 337 | 23% |
| output parameter from `expression`, `jsonPath` or `jqFilter` | 7 | 2 | 9 | 2 | 2 | 22 | 1% |
| `globalName` on outputs | 3 | 8 | 2 | 3 | 10 | 26 | 2% |
| template `volumes` | 3 | 4 | 152 | 167 | 46 | 372 | 39% |
| workflow `volumes` | 4 | 5 | 10 | 15 | 17 | 51 | 6% |
| `volumeClaimTemplates` | 7 | 13 | 17 | 0 | 6 | 43 | 1% |
| `volumeMounts` in containers | 11 | 21 | 172 | 176 | 71 | 451 | 45% |
| `hostPath` volume | 0 | 0 | 0 | 106 | 1 | 107 | 20% |
| `persistentVolumeClaim` volume | 1 | 0 | 3 | 64 | 9 | 77 | 13% |
| `secret` volume | 2 | 0 | 4 | 1 | 44 | 51 | 8% |
| `emptyDir` volume | 4 | 8 | 156 | 10 | 8 | 186 | 3% |
| `configMap` volume | 0 | 1 | 147 | 4 | 2 | 154 | 1% |
| template `podSpecPatch` | 3 | 0 | 151 | 3 | 2 | 159 | 1% |
| workflow `serviceAccountName` | 3 | 7 | 153 | 50 | 25 | 238 | 14% |
| workflow `nodeSelector` | 0 | 1 | 0 | 28 | 0 | 29 | 5% |
| `imagePullSecrets` | 1 | 0 | 1 | 65 | 0 | 67 | 12% |
| `securityContext` in containers | 0 | 1 | 147 | 39 | 5 | 192 | 8% |
| `sidecars` | 4 | 4 | 4 | 0 | 0 | 12 | 0% |
| `initContainers` | 1 | 1 | 150 | 2 | 1 | 155 | 1% |
| env from `secretKeyRef` | 1 | 0 | 5 | 27 | 74 | 107 | 18% |
| container `resources` | 4 | 5 | 156 | 87 | 38 | 290 | 23% |
| `podGC` | 2 | 14 | 2 | 64 | 5 | 87 | 13% |

Further own measurements:
- The bioinformatics column is dominated by two repositories. Expression tags: 121 of 167 gennext documents and 31 of 132 Ensembl documents. Retries: 117 Ensembl documents (102 with `OnError`, 91 with `backoff`). `hostPath`: 105 gennext documents. PVC: 63 Ensembl documents.
- Script interpreters (`script.command`) across all documents: `python` 222, `sh` 113, `bash` 33, plus a few variants (`python3`, `sh -x`, `bash -c`, `node`).
- Resource template actions: `create` 55, `delete` 12, `patch` 10, `get` 8, `apply` 3 documents.
- Retry policies: `OnError` 105, `Always` 89, `OnFailure` 1 document.
- Variables in simple tags (documents): `inputs.parameters` 837, `tasks.<n>.outputs.parameters` 268, `workflow.name` 209, `item` 168, `workflow.uid` 168, `workflow.parameters` 143, `tasks.<n>.outputs.result` 72, `steps.<n>.outputs.result` 52, `steps.<n>.outputs.parameters` 47, `steps.<n>.outputs.artifacts` 41.
- `nvidia.com/gpu` resources appear in 13 gennext files and 2 AliyunContainerService examples, and nowhere else in the real-world subset.
- 13 analysed files (plus 2 Hera copies) use YAML 1.1 octal file modes such as `mode: 0755` or `defaultMode: 0600` (Argo examples and e2e fixtures, argo-workflows-catalog, argo-workflows-guide, SIM-PIPE, Hybrid-Argo-Workflows, AliyunContainerService examples).
- Removed v3 fields still in use: `schedule` in 5 CronWorkflows and singular `mutex` or `semaphore` in 25 places (Argo's conversion fixtures, Hera examples, gennext, talk-demos).

### Inferences
- A first release can skip `data`, `plugin`, `containerSet`, hooks, memoization and CronWorkflow scheduling. Together they appear in about 3% of real-world documents.
- `templateRef` support and a template library are as important as `dag` support.
- Expression tags cannot be deferred: the bioinformatics repositories need them from day one, but only a small function set (Sprig path helpers, `jsonpath`, `asInt`).
- Volumes cannot be ignored. A clear mapping for `hostPath`, PVC and `emptyDir` is needed in the first release.

### Gaps
- The feature detection is regex- and key-based. It may slightly over- or under-count, for example for comparison operators in `when`.
- Counts are per document, not per template.

## Q8. Parser evaluation: generated Rust types and YAML loading

### Takeaway
Both kopium (from the full CRD) and typify (from the JSON schema) produce compilable Rust types. With serde-saphyr set to YAML 1.1 octal numbers, kopium types load 1,449 of 1,465 unique documents (98.9%). The rest are Argo's own intentionally invalid fixtures, removed v3 fields, and a few type edge cases. typify types load only 1,353 (92.4%) because the JSON schema models int-or-string and quantity fields as plain strings. The generated code is large and slow to compile. Generated types are a good reference, but iunctus should hand-write its Argo types.

### Cited Findings
Own measurements (`_analysis/results/kopium-final.txt`, `typify-final.txt`).
- Tools: kopium 0.24.1 (`--hide-kube --no-condition`, input `manifests/base/crds/full/*.yaml`), cargo-typify 0.8.0 (input `api/jsonschema/schema.json`), serde-saphyr 1.3.0, serde_ignored 0.1 to record unknown fields, k8s-openapi 0.28 (feature `v1_35`) for `IntOrString`.
- schemars was not used. It derives JSON Schema from Rust types, which is the opposite direction. kopium uses it only when asked to emit a schema back (`--schema=derived`) (kopium `--help`).
- kopium output with default flags: 27,014 lines for the Workflow CRD (2,426 structs, 57 enums); 98,680 lines for the four CRDs. The CRD schema has no shared definitions, so every nested position gets its own struct. typify output: 424 structs and 1 enum, one per schema definition, including Argo Events and API request types; 61,024 lines with builders and 10,811 without.
- Compile cost: one crate with all four kopium files and typify with builders did not finish; I stopped it after more than 11 minutes when rustc used 12.6 GB of resident memory. A reduced crate (one kopium file without status types, 23,871 lines, 2,155 structs, and a hand-written CronWorkflow wrapper) built in about 6 minutes, and rebuilt in about 3 minutes after editing `main.rs`. The typify crate without builders built in 1 minute 14 seconds.
- kopium's `--relaxed` flag "allows certain invalid openapi specs to be interpreted as arbitrary objects as used by argo workflows" (kopium `--help`). With the full v4 CRDs it was not needed.
- The schema cannot express int-or-string. Argo's example validator skips such errors and explains: "due swagger 2.0 limitation, we can only specify one data type (we use string, same as k8s api swagger)" (validator.go line 59) ([validator.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/examples/validator.go)). In the schema, `IntOrString` and `Quantity` are `{"type": "string"}` and `Item` is `{}` ([schema.json](https://github.com/argoproj/argo-workflows/blob/v4.1.4/api/jsonschema/schema.json)).
- YAML level, 1,953 YAML files. With serde-saphyr defaults, 9 files fail. With `emit_comments = false`, 6 fail. Three gennext files fail only by default, with "too many consecutive comments before resolving collection entry"; granit-parser 1.3.0 buffers at most 96 comment events by default. Of the remaining 6: one Argo example has a duplicate mapping key (`examples/k8s-patch-json-workflow.yaml`, `annotations`), one gennext file is invalid YAML (PyYAML also rejects it), one e2e fixture is invalid on purpose, and three are not Argo files (a Kubernetes Deployment with an unfilled placeholder, a Helm template, a GitHub config file).
- Scalars: with serde-saphyr defaults `mode: 0755` becomes the float `755.0`, and kopium's `i32` field rejects it. With `legacy_octal_numbers = true` it becomes `493`, the YAML 1.1 value that Go's YAML library produces. `yes` becomes `true` by default (`strict_booleans = false`).
- Typed loading of the 1,465 unique documents:

| Types | As written | With Argo's `AnyString` rule emulated | Loaded without unknown fields |
|---|---|---|---|
| kopium (full CRD) | 1,431 (97.7%) | 1,449 (98.9%) | 1,398 |
| typify (JSON schema) | 1,335 (91.1%) | 1,353 (92.4%) | 1,305 |

- "Argo's `AnyString` rule emulated" means that numbers and booleans in `value`, `default` and `enum` are turned into strings before typing, as Argo's Go type does. Without it, 18 documents fail on values such as `value: 1` or `default: false`.
- The 16 kopium failures: 7 Argo e2e fixtures that are invalid on purpose (`malformed/`, `lintfail/`, `workflow-template-invalid-*`); 5 CronWorkflows with the removed `schedule` field; 2 documents with `cpu: 4.7` (a float quantity, which kopium models as `IntOrString`); 1 `null` for a string (gennext `multicaller.yaml`); 1 `memoize` without `maxAge` (required in the CRD, while the docs treat it as optional).
- Real-world subset: 540 of 546 documents load with kopium types.
- Unknown fields found by serde_ignored in documents that did load: `spec.workflowMetadata.generateName` (14), removed `synchronization.mutex` and `semaphore` (25), `podSpecPatch` placed inside `container` (6), removed `schedule` (5), and misplaced keys such as `outputs` under a DAG task, `arguments` at template level, `commands` in a container, or `parameters` at spec level. Argo rejects such files in strict parsing, which is the default for `argo lint`, `argo submit` and `argo template create` (`--strict`, "default true") ([docs/cli](https://github.com/argoproj/argo-workflows/tree/v4.1.4/docs/cli)). A `kubectl apply` of them would be accepted by the API server, since the older minimal CRDs keep unknown fields; I did not verify what the full v4 CRDs do with them.
- typify failures are dominated by numbers in int-or-string and quantity fields (`limit: 1`, `cpu: 1`, `port: 8080`) and by `required` fields that Go does not enforce (`script.source`, container `image` supplied by `templateDefaults`).

### Inferences
- Use hand-written serde types for the Argo layer. The needed surface is small: about 60 to 100 structs for Workflow, templates, steps, tasks, parameters, artifacts, retry, hooks and the subset of Kubernetes Container, Volume and ResourceRequirements that iunctus maps. Keep everything else as `serde_json::Value` and warn about it.
- Write custom deserializers for Argo's lenient types: `AnyString`, `IntOrString`, `Quantity` (including floats), `Item`, `ParallelSteps`, and the removed v3 fields (normalize them to v4).
- Record unknown fields with `serde_ignored`. Report them as warnings by default and as errors in a strict mode, similar to `argo lint`.
- Configure serde-saphyr with `legacy_octal_numbers = true` and `emit_comments = false`. Keep duplicate keys as an error, but test what Argo does.
- The kopium output is still useful as a checked reference for field names, and as a test: every document of the test workflows that loads with kopium types must also load with the hand-written types.

### Gaps
- I did not measure the peak memory of the reduced kopium build.
- I did not check what `sigs.k8s.io/yaml` does with duplicate keys in non-strict mode.
- I did not try openapi-generator or progenitor on `swagger.json`.

## Q9. What a native Rust Argo implementation needs: checklist, effort and risk

### Takeaway
Argo is the easiest of the four workflow languages in scope for iunctus. The syntax is plain YAML with a published schema, there is no general-purpose language inside the workflow, and a Rust implementation of its main expression language exists. A useful subset for local and TES execution looks like 3 to 4 person-months. The main risks are not parsing: they are the exact substitution and node-state semantics defined only by Go code, workflows that rely on Kubernetes storage and secrets, and the small number of public bioinformatics users.

### Cited Findings
- The facts behind this checklist are cited in Q1 to Q8. The checklist and estimates below are my own inferences.
- Size of the reference: the Go code under `workflow/` has 38,013 lines outside tests and 65,616 lines of tests. The central files are `operator.go` (4,869 lines), `workflowpod.go` (2,281), `validate.go` (1,743), `executor.go` (1,539), `dag.go` (1,059), `steps.go` (733) and the template code under `util/template` (about 700 lines) (own measurement at v4.1.4).
- Test material: 222 examples; 400 YAML files under `test/e2e` (124 functional, 32 expected failures, 153 testdata, 14 synchronization, 13 smoke, 9 cron, 3 lint failures); 37 Go e2e test files hold the expectations ([test/e2e](https://github.com/argoproj/argo-workflows/tree/v4.1.4/test/e2e)). `examples/validation_test.go` validates the examples against the JSON schema ([validation_test.go](https://github.com/argoproj/argo-workflows/blob/v4.1.4/examples/validation_test.go)).

### Inferences

Checklist:

1. Loading
   - [ ] Hand-written serde types for Workflow, WorkflowTemplate, ClusterWorkflowTemplate and CronWorkflow (the `workflowSpec` part), with Argo's lenient types and v3 to v4 normalization.
   - [ ] serde-saphyr with YAML 1.1 octal numbers; multi-document files; unknown-field warnings; source positions in errors.
   - [ ] Template library: load directories and kustomization lists; resolve `templateRef` (namespaced and `clusterScope`) and `workflowTemplateRef`; merge `templateDefaults`.
   - [ ] Validation that matches `argo lint --offline` for the supported subset (names, references, cycles, parameter declarations).
2. Substitution and expressions
   - [ ] Simple tags with Argo's phases (globals, inputs, late variables) and variable set (`workflow.*`, `inputs.*`, `steps.*`, `tasks.*`, `item`, `retries`, `lastRetry.*`, `pod.name`), including JSON-list aggregation for looped outputs and the v4.0.7 rules for skipped and omitted outputs.
   - [ ] expr-lang environment: nested variable maps, `asInt`, `asFloat`, `toJson`, `jsonpath`, a Sprig subset (start with the 21 functions found in the test workflows), `sprig.` call rewriting, panic isolation.
   - [ ] govaluate-compatible `when` evaluator with the bare-word rule.
   - [ ] `depends` translation (bare names, `-` to `_`, task result booleans, `AnySucceeded` and `AllFailed` for loops).
3. Orchestration
   - [ ] Steps (sequential groups of parallel steps) and DAG (`depends`, `dependencies`, `failFast`, `target`, `continueOn`).
   - [ ] Loops: `withItems`, `withParam`, `withSequence`; dynamic fan-out; recursion with a depth guard.
   - [ ] Node states and `workflow.status`/`workflow.failures`; exit handlers; later lifecycle hooks.
   - [ ] `retryStrategy` (limit, policies, backoff, expression); `timeout`, `activeDeadlineSeconds`, later `pendingTimeout`; `parallelism`; `suspend` with CLI resume.
   - [ ] Later: `memoize` on the iunctus cache, `synchronization`, `containerSet`, `daemon` and `sidecars` (local only), `http`, `data`.
4. Execution and data
   - [ ] Container and script tasks on a local runtime and on TES (script body as inline content or uploaded input).
   - [ ] Outputs: stdout `result` with a size limit, exit code, parameters from files, artifacts from files and directories.
   - [ ] Artifacts: S3 and key-only, a configured default repository, `from` passing, tar.gz and untar rules, `optional`, later git, HTTP, raw, GCS and Azure.
   - [ ] Volumes: `emptyDir` and `volumeClaimTemplates` as scratch; PVC and `hostPath` as a shared run directory (local and HPC); clear errors on TES.
   - [ ] ConfigMap and Secret stand-ins for `configMapKeyRef`, `secretKeyRef`, and secret and configMap volumes.
   - [ ] Resource requests to TES resources; GPU through backend parameters; warnings for ignored pod settings.
5. Verification
   - [ ] Load test over all test workflows (the kopium result above is the baseline).
   - [ ] Differential validation against `argo lint --offline` in CI.
   - [ ] Port selected e2e expectations from Argo's Go tests; run the Argo examples that need no cluster.
   - [ ] End-to-end runs of selected gennext and Ensembl pipelines with small test data on a local runtime, and a subset on a TES test server.

Effort and risk estimate (rough, for engineers experienced in Rust and workflow engines):

| Scope | Effort | Risk | Notes |
|---|---|---|---|
| Tier 1: loading, template library, simple tags, expression tags with the small function set the test workflows use (Sprig path helpers, `jsonpath`, `asInt`), `when`, `depends`, container and script, steps and DAG, loops, outputs, retries, timeouts, `onExit`, S3 and default-repository artifacts, volume mapping, local and TES execution | about 3 to 4 person-months | Low to medium | Well defined by schema and docs. Corner cases in substitution phases and node states need test-driven work. |
| Tier 2: broader Sprig coverage and Go compatibility tests for expressions, hooks, memoization, synchronization, suspend and resume, containerSet, daemons and sidecars, HTTP and data templates, more artifact drivers, CronWorkflow scheduling | about 3 to 5 more person-months | Medium | Expression compatibility (Go versus Rust expr, Sprig edge cases) is the main uncertainty. |
| Tier 3: resource templates through an optional Kubernetes backend (for example the `kube` crate), executor and artifact plugins, `podSpecPatch` beyond resources, metrics and events | about 3 to 6 more person-months | High | Needs a cluster. It works against the goal of running without Kubernetes. |
| Full parity (Argo Server API, UI-compatible node status, archive, artifact GC) | not a goal | Very high | These are cluster services, not language features. |

Main risks:
- Semantics are defined by Go code and tests, and they change in patch releases (v4.0.7). Pin a version and grow a fixture suite.
- Real workflows use Kubernetes storage as a shared file system (template volumes in 39% of real-world documents). This works locally and on HPC, but TES needs a shared file system, like Nextflow's TES executor.
- Secrets and ConfigMaps (18% of real-world documents read secrets into the environment) need a safe stand-in. This is a security design task, not just a feature.
- Small public bioinformatics user base: two sizable repositories, one without a license. The value of Argo support for iunctus is portability of existing Argo pipelines off Kubernetes, not a large pipeline catalog.
- Expression libraries: `expr-lang` is young (2.0.0 on 2026-08-19) and panicked on one real expression; no Rust govaluate or full Sprig exists.

Comparison with the other languages (inference): Argo needs less work than Nextflow and Snakemake, because it has no embedded Groovy or Python to evaluate at DAG-build time. It is closer to CWL in shape (YAML plus an expression language), but with a smaller expression surface than CWL's JavaScript.

### Gaps
- The estimates are not based on a prototype. They assume the shared iunctus scheduler, storage and TES layer exist.
- I did not measure how many real-world pipelines would run end to end with Tier 1 only. The next step is to pick a few gennext and Ensembl pipelines and trace every feature they need.

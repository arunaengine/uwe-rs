<!-- The design of argo-runtime, which runs Argo Workflows documents as language-neutral tasks. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Argo runtime design

Status: implemented in the crate `argo-runtime`, 2026-09-28. The reference is the controller of Argo Workflows v4.1.4. The [research note](../../research/argo-workflows.md) describes the language, the test workflows and the feature inventory that this design builds on.

## 1. The design in short

1. **The runtime repeats Argo's controller, not its API server.** Argo walks a tree of nodes again and again until nothing changes. `argo-runtime` does the same: every pass runs the templates whose inputs are ready, and every leaf template becomes one or more `workflow_graph::Task`s. The node names, the node IDs and the pod names follow Argo's rules, so warnings and errors name what `argo get` would show.
2. **Templates come from a library of documents.** The workflow file, the files given as configs and the repository of the workflow file are searched, in that order, for `templateRef` and `workflowTemplateRef`.
3. **Substitution follows Argo's phases.** Globals and the local scope are substituted first, inputs second. Tags that cannot be resolved yet stay in place for a later phase, as in Argo.
4. **The expression languages are implemented in the crate.** expr-lang with Argo's helpers and a Sprig subset, govaluate for `when`, and JSONPath. Values keep Go's model: JSON numbers are float64, maps print with sorted keys, and results print the way Go prints them.
5. **A simulated run always makes a full graph when Argo would.** Data that is not known in a simulated run (empty stdout, placeholder parameters) never stops the run. Where Argo would read that data, the runtime takes the path that keeps the graph whole and logs a warning.
6. **No Kubernetes, the common scientific subset (decision 18).** Templates that need a cluster, like `resource` and `data` templates, sidecars and daemons, stop the run before its first task. Fields that only tell Kubernetes how to run a pod are left out with a warning. ConfigMaps and volumes map onto task inputs, outputs and files; secret values never enter a task (section 11).
7. **Unsupported features fail loudly.** They stop the run with exit code 33 (`DriverError::unsupported`) and an error that names the decision.
8. **Executor plugins run in their own containers.** A plugin template becomes a task in the image of the plugin, which calls the plugin's server as Argo's agent does (section 8.1).
9. **Workflows are checked as Argo checks them when they are submitted** (section 4.1), and so are documents without an entrypoint, like libraries of WorkflowTemplates.

## 2. Crates and the entry point

`argo-parser` reads YAML and gives the typed model. `argo-runtime` holds everything else.

The entry point is `argo_runtime::driver(launch: &workflow_graph::Launch) -> Result<Box<dyn Driver>, DriverError>`:

| `Launch` field | Meaning for Argo |
|---|---|
| `target` | The workflow file. The first `Workflow` with an entrypoint runs; without one, the first runnable document (a `WorkflowTemplate`, `ClusterWorkflowTemplate` or `CronWorkflow`). |
| `inputs` | A parameter file, like `argo submit --parameter-file`: a YAML or JSON mapping. Text values stay as they are; other values become JSON. |
| `params` | Workflow parameters, like `argo submit -p`. They win over the parameter file. |
| `targets` | The first one is the entrypoint, like `argo submit --entrypoint`. |
| `configs` | Files or directories of more templates for the library. |
| `scratch` | Where the run keeps files of its own. |

`ArgoDriver::new(document, library, options)` builds the same thing from a loaded document, for callers that load their own library, like the test-workflows test.

The modules of `argo-runtime`:

| Module | What it does |
|---|---|
| `library` | Loads documents and executor plugins, finds templates by kind and name |
| `start` | How a run starts: the joined spec, workflow parameters and placeholders, globals |
| `run` | The operator: passes over the nodes, template resolution, node bookkeeping |
| `node` | Node kinds, phases, outputs and the artifacts nodes pass on |
| `inputs` | Arguments into inputs and the two substitution phases, artifact locations |
| `steps`, `dag` | Step groups, DAG tasks, `depends`, the assessment of a DAG |
| `items` | `withItems`, `withParam` and `withSequence` |
| `retry` | Retry nodes and their attempts |
| `hooks` | Lifecycle hooks and exit handlers |
| `scope` | Scopes of steps and tasks, outputs of steps and DAG templates |
| `pod` | Leaf templates as tasks, and the outcome of a task as outputs |
| `http`, `plugin` | HTTP templates, and plugin templates as calls of executor plugins |
| `resources` | Resources of a task from Kubernetes quantities |
| `validate`, `checks` | The checks Argo makes when a workflow is submitted, and their single rules |
| `tags` | `{{...}}` substitution |
| `expr`, `sprig`, `jsonpath`, `when` | The expression languages |
| `value` | Go's value model and formatting |
| `merge`, `time` | Merges by key (`templateDefaults`, `workflowTemplateRef`) and Go time formats |

`workflow-graph` is not changed. `argo-parser` gained these parts for the runtime:

- `parse_documents` reads every document of a YAML stream.
- The typed model (`argo_parser::model`) covers the fields that the runtime reads. It keeps locations of artifacts and other open-ended fields as YAML nodes.
- `normalize` rewrites the fields that v4 removed, as `argo convert` does: `schedule` of a CronWorkflow becomes `schedules`, and a single `mutex` or `semaphore` becomes a list.
- YAML merge keys follow go-yaml: an explicit key replaces a merged one.
- Two indentation errors that go-yaml accepts are repaired and the document is parsed again: a quoted scalar whose next lines are indented like its key, and a flow collection whose closing bracket is indented like its key.

## 3. The template library

A `Library` holds documents, and the executor plugin ConfigMaps of section 8.1, with a priority: 0 for the documents of the workflow file, 1 for the configs, 2 for the repository of the workflow file. The repository is the nearest directory above the file that holds `.git`, or the file's own directory. Directories are read lazily, on the first lookup that misses. Only files with `argoproj.io/` documents are kept.

A lookup names a kind (`WorkflowTemplate` or `ClusterWorkflowTemplate`, from `clusterScope`) and a name. The document with the lowest priority wins. Among documents with the same priority, the one that shares the longest path prefix with the referring file wins, which picks the right copy when a repository has several versions of a template.

`workflowTemplateRef` merges the referenced spec under the workflow's spec by key, like the merge patches of Kubernetes. Lists of templates, parameters and similar entries are merged by `name`, volume mounts by `mountPath`. `templateDefaults` is merged under every template, as Argo's `SetType` has it: the defaults do not add a second template type.

## 4. The operator

The run keeps a list of nodes with Argo's node kinds (Pod, Steps, StepGroup, DAG, TaskGroup, Retry, Skipped, Suspend, Plugin) and phases. `operate` repeats passes over the node tree until no node changes, then waits for task outcomes. `complete` records an outcome, turns it into outputs and runs the next pass.

Each template call goes through the same stages as `executeTemplate`: resolve the template (a name, `templateRef` or `inline`), apply `templateDefaults`, process the arguments into inputs, substitute, and start the node. Calls nest at most 100 levels deep, as Argo's `maxStackDepth`. A deeper recursion stops the run with the error `templates call each other more than 100 levels deep`. Recursion that ends by its own outputs, like the coin flip examples, runs normally.

Node IDs are FNV-1a hashes of the node name, and pod names use Argo's version 2 naming (`<workflow>-<template>-<hash>`). A workflow with `generateName` gets a five character suffix that is made from the kind, name and position of the document, so it does not depend on where the file is.

### 4.1 Checks at submission

Before the first pass, the run checks the workflow as Argo's `validate.Workflow` does when a workflow is submitted (workflow/validate/validate.go), with the parameters and artifacts that the run got. The messages are Argo's, like `templates.main.steps[0].a templates.print inputs.parameters.message was not supplied`. `argo_runtime::validate` checks a document on its own, as Argo does when a WorkflowTemplate is created: every template with any inputs, and the entrypoint when there is one.

The checks cover what decides whether a workflow can run: the names of templates, steps, tasks, parameters and artifacts; the entrypoint; template types; references by name, `templateRef` and `workflowTemplateRef`; the inputs that each call supplies; the arguments of the workflow, of steps and of tasks, including `enum`; hook expressions; retry policies; images; resource actions; and the order of DAG tasks (unknown dependencies, cycles, `depends` beside `dependencies`, targets). As in Argo, a template of another document is checked only where it is called.

Left out: the length of Kubernetes object names, volume mounts, metrics, pod garbage collection, cron schedules, and whether each tag names a known variable. A run finds tags that do not resolve when it substitutes them.

## 5. Substitution

Argo substitutes the JSON text of a template. `argo-runtime` substitutes every string and mapping key of the YAML tree, which gives the same result because Argo escapes the inserted values and unescapes the text again.

- A simple tag like `{{inputs.parameters.x}}` is a lookup. A tag like `{{a{{b}}` holds a nested tag, which resolves on its own, as fasttemplate has it.
- An expression tag `{{=...}}` runs as expr in an environment that nests the dotted names into maps. A name that a longer name continues (`item` beside `item.a`) is dropped, as Argo's `expand.Expand` does.
- Strings from an expression are inserted as they are. Other results are inserted as JSON, as Argo v4 writes them (v3 used `%v`).
- Some variables only exist later: `item`, `retries`, `lastRetry.*`, `workflow.status` and `workflow.failures`. An expression that reads one of them before it exists stays in place.
- A loop item is substituted with the item's values beside the globals and the scope, as `processItem` has it. A simple tag that stays unresolved there is an error, unless the step's `when` is false.

The phases are the same as in Argo: first the globals and the local scope (tags may stay), then the inputs of the template (inputs must resolve). `workflow.failures` holds a Go quoted JSON list of all failed nodes, as Argo sets it.

## 6. Expression languages

### 6.1 expr-lang

The research proposed the Rust crate `expr-lang`. The runtime has its own evaluator instead, for these reasons:

- Argo calls Sprig as `sprig.name(...)`, a method call on a map of functions. The crate evaluates method calls only on its date and time values.
- The crate's value model differs from what Argo gets from Go: JSON numbers in Argo are float64, maps from JSON have sorted keys, and `%v` prints `map[a:1]`. These differences change task commands without an error.
- Argo's own helpers (`asInt`, `asFloat`, `toJson`, `jsonpath`) and the rules for unknown names (a top-level name that is not in the environment is an error, a missing map key is nil) need hooks in the evaluator.
- An evaluator in the crate never panics on the input of the test workflows and has no dependency.

The evaluator has a lexer, a Pratt parser with expr's precedence table, and a tree walker. It covers literals, arithmetic (`/` always gives a float), comparison, `in`, `not in`, `contains`, `startsWith`, `endsWith`, `matches`, ranges, slices, the ternary operator, `??`, `let`, optional chaining, closures with `#` and `.field` for the predicate functions (`all`, `any`, `none`, `one`, `filter`, `map`, `count`, `sum`, `find`, `groupBy`, `sortBy` and others), and expr's built-in functions. The unit tests use expressions from the test workflows.

### 6.2 Sprig

The Sprig subset covers what the test workflows use and what is cheap to add alongside it: strings, paths (`base`, `dir`, `ext`, `clean` and the `os` forms), numbers with Sprig's lenient conversion, `default`, `empty`, `coalesce`, lists, dicts, base64, JSON, regular expressions and dates. An unknown Sprig function is an error that names it.

### 6.3 govaluate for `when`

`when` runs after simple tags are substituted. The evaluator follows govaluate as Argo uses it: a bare word is a string (`heads == heads` holds), a dotted word is an error, and `-` is minus. A comparison with a missing operand (`== heads`, as an empty output leaves it) is false, not an error.

### 6.4 JSONPath

`jsonpath(text, path)` follows evilmonkeyinc/jsonpath: a definite path gives the value, other paths give a list, and a missing key is an error. Filters with `@` work; `$` inside a filter is an error.

## 7. Control flow

| Feature | Behaviour |
|---|---|
| `steps` | Step groups run one after another, the steps of a group in parallel. A group waits for every step. |
| `dag` | Tasks start when their `dependencies` or `depends` hold. `depends` is translated to an expression over task results, as Argo does. The phase of the DAG follows `assessDAGPhase`: the phases of the branches that end the DAG. |
| `when` | Decided after substitution. A condition that still holds a tag, as a simulated run leaves some, cannot be decided: the step is skipped with a warning. |
| Loops | `withItems`, `withParam` and `withSequence` give one node per item, named `name(index:description)`. An empty loop gives a skipped node. A `withParam` that does not parse is ignored when `when` is false. |
| `continueOn` | `failed` and `error` let the run go on after a failed step or task. |
| `failFast` | Only changes the assessment of the DAG, as in Argo. `-keep-going` turns it off where a DAG does not set it. |
| Retries | A retry node holds attempts named `name(n)`. Policy, limit and expression decide each retry. Backoff uses asynchronous engine wakeups with duration, integer factor and cap. `maxDuration` bounds the retry sequence from its first attempt. Machine failures are transient; other executor failures are errors. Simulated delays advance virtual time. |
| Exit handlers | `onExit` of the workflow and the `exit` hook run after the entrypoint, with `workflow.status` and `workflow.failures`. The exit hooks of steps and tasks run after their node. |
| Lifecycle hooks | Hooks of the workflow and of steps and tasks run when their `expression` holds. |
| `suspend` | A duration creates a timed wait node without a container. Other tasks can run while it waits. Manual suspension remains refused because Argo node resume has no CLI implementation. Supplied output parameters take defaults or enum values. Simulated waits advance virtual time. |

Outputs of steps and DAG templates read the scope (`valueFrom.parameter`, `expression`, `supplied`, artifacts by `from` and `fromExpression`). Outputs of skipped steps get their defaults, as `addSkippedNodeOutputsToScope` does in v4.

## 8. Leaf templates as tasks

| Template | Task |
|---|---|
| `container` | One task with the image, command, arguments, working directory and environment. |
| `script` | The command runs the file `/argo/staging/script`, which the task gets as an `Input` with the source as its content, as Argo's executor writes it, with mode 0755 (`Input.executable`), so that `command: [sh, -c]` may run it directly. |
| `containerSet` | One task of several containers, as one pod (section 8.2). The main container (named `main`, or the last one) gets the outputs. |
| `resource` | Not supported (decision 18), except for a PersistentVolumeClaim, which becomes a volume of the run (section 11.4). |
| `http` | The image `curlimages/curl` runs curl, which writes the response body and status code to files. The status code decides `successCondition`. When the status is not known, as in a simulated run, the node succeeds with a warning. |
| `data` | Not supported (decision 18). |
| `plugin` | One call of an executor plugin per task, in the plugin's image (section 8.1). |

Containers run as Kubernetes runs them (decision 28, `Task.run_as`): as the user of the image, or the `runAsUser` and `runAsGroup` of the `securityContext` of the container, else of the template, else of the workflow; in the working directory of the image, or `workingDir`; with the `HOME` of that user and a file system that they may write. So a step may write `/result.txt` or `/data` where its user may, as in Argo. The work directory of the task stays where the executor puts it; outputs and inputs at absolute paths are where Argo has them. The container has no Linux capabilities but `CHOWN` when it runs as root on an engine with root, to give its files back to the user who runs iunctus ([executors](../executors.md), section 6). Init containers and the containers of a set run the same way. HTTP and plugin templates are no containers of the workflow; they run as the user who runs iunctus.

Resources come from the container's requests, or its limits where requests say nothing; a container set asks for the sum over its containers, as Argo does. `activeDeadlineSeconds` or `timeout` sets the time limit. Environment variables that read a field of the pod with `fieldRef` get the run's values: the pod name, the namespace, a stable uid, the service account, a node name and an IP address. Variables from ConfigMaps and Secrets follow section 11.

Workflow `activeDeadlineSeconds` starts with the run and cancels outstanding work with
`Step exceeded its deadline`. Exit handlers remain eligible to run. Template `timeout`
includes pending time and reports `timeout`; template `activeDeadlineSeconds` covers all
init and main tasks together and reports the pod deadline message. The engine cancels
individual attempts and waits for their cleanup before the node ends. After the first
failed attempt, retry `maxDuration` overrides the template active deadline.

### 8.1 Executor plugins

Argo runs a plugin template in its agent pod, whose sidecars are the servers of the executor plugins. The agent posts the template to `/api/v1/template.execute` of one plugin after the other, until one replies with a node (workflow/executor/agent.go).

iunctus has no agent pod. Each call of a plugin is a task in the plugin's own image, so it follows decision 11:

- The command is a POSIX shell script. It starts the plugin's server from the command and arguments of the sidecar, and posts `/argo/plugin/request.json`: the name, namespace and uid of the workflow and the template, as the agent sends them. It sends the token of `/var/run/argo/token`, which Argo also mounts for the plugin. It writes the reply to `/argo/plugin/reply.json` and its HTTP status to `/argo/plugin/status`. It uses `curl`, or `wget` where the image has no curl, as in `python:alpine`.
- The plugins are those of `spec.executorPlugins`, which replace the installed plugins as in Argo. Otherwise they are the plugin ConfigMaps of the library (label `workflows.argoproj.io/configmap-type: ExecutorPlugin`, as `argo executor-plugin build` makes them). Plugins whose name holds a key of the template's `plugin` field are tried first.
- The reply gives the node its phase, message and output parameters. A plugin that answers 404 or replies without a node passes the template on to the next plugin. When no plugin is left, the node fails with Argo's error `no plugin executed the template`.
- In a simulated run the reply is not known. The first plugin runs the template, and the node succeeds with a warning. Declared output parameters get their value or default.

Limits: the image of a plugin needs `sh` and `curl` or `wget`. A reply with the phase `Running` or `Pending` ends the node with an error, because iunctus does not call a plugin again later.

### 8.2 Container sets

Argo runs the containers of a container set in one pod, where they share its volumes, and its executor starts each container when those in its `dependencies` ended (docs/container-set-template.md; `executeContainerSet` in workflow/controller/container_set_template.go). iunctus runs a container set as one task of several containers (`Task.containers`, decision 11 still holds: every container runs in its own image):

- The task holds every container with its image, command and arguments, working directory, environment and `dependencies`. The main container comes first; its image and command are also those of the task, so the simulator and the graph read it as before.
- Each container runs in a wrapper of its own (`executor::containers`). It waits until the containers in its `dependencies` recorded their exit codes, and runs its command only when all of them ended with 0. Otherwise it does not run and ends with exit code 1, as Argo's executor ends a container whose dependency failed.
- `containerSet.retryStrategy` runs a failed command again in the same container, up to `retries` times in all, as Argo's executor does with its `Steps: retries`; the logs of the tries follow each other. A command that the shell cannot find (exit code 127) is not tried again. As with other retries (section 12), `duration` is not waited for.
- The containers share the work directory, the inputs and the volumes of the set: the `volumeMounts` of the set and those of every container. An `emptyDir` is a folder of the task, so containers that run at the same time see each other's files at once, as in the pod.
- The main container collects the outputs when it ended and every container that does not come after it ended too, so that files that other containers write into a volume are there. Its standard output is `outputs.result`, and `outputs.exitCode` is its own exit code.
- The node fails when any container failed. Its message names the first failed container by name, like `b: Error (exit code 2)`, as Argo's controller reports a failed pod of several main containers.
- Each container keeps its own logs: the run database has them as the streams `<container>/stdout` and `<container>/stderr` of the task; the main container's are the task's.
- The cache key holds every container, its pinned image included, so a change to any container runs the task again.

Where the containers run:

| Executor | How |
|---|---|
| Local | One engine container for each image, all at once, on the same scratch folder and mounts, each within the share of the task. Containers of the same image run in one engine container, whose wrapper starts theirs at once and waits for all. When one engine container ends without its wrapper, like one killed for lack of memory, the others stop. |
| Kubernetes | One pod with a container for each image, as on the local executor. The first container asks for the CPUs and memory of the task; each may use all of its memory. |
| SLURM | One job whose script starts a container for each image at once and waits for all of them. |
| TES | One TES task with one executor per container. TES runs executors one after the other, so the containers run in an order where each comes after those it waits for, and the main container as early as it can. Containers without dependencies do not run at the same time there. |
| Simulator | Simulates the main container, as for one container; the TES document of the task lists every container as an executor. |

Init containers stay tasks of their own before the task of the set (section 11.8), and sidecars stay unsupported (decision 18).

## 9. Inputs and outputs

Input artifacts become `Input`s, which the task gets as copies that it may change (`Writable::Copy`), as Argo's executor writes each artifact into the pod. The source is the artifact's location: `s3://bucket/key`, an HTTP URL, a git repository (with `#revision`), `gs://`, Azure, Artifactory, HDFS or OSS. Raw artifacts become an `Input` with content. An S3 artifact with only a key uses the default artifact repository, named `s3://default-artifact-repository` here.

Output artifacts and output parameters with a `path` become the task's outputs. Deeper paths come first, so that a directory output does not hide a file inside it. When a task completes, the runtime reads the files it wrote:

Explicit output `archive.none` exports files or directory trees unchanged. `archive.tar`
exports gzip tar with compression levels -1 and 0 through 9; `archive.zip` exports zip.
Huffman-only gzip (-2) remains refused. Native downstream steps retain the original files,
including inputs with an explicit archive strategy. External archive extraction remains refused.
S3 output locations use the configured storage delivery path and default to gzip tar when
no archive strategy is given. Archives are staged in a run-owned temporary folder that is
removed when the driver is dropped. Endpoint and credential overrides remain refused:
the language driver has no storage endpoint identity to verify a declared `S3.endpoint`.

- An output parameter is the file's text without its last newline, or its `default`.
- `outputs.result` is the standard output without its last newline.
- `outputs.exitCode` is the exit code of the main container.
- An output artifact is the file or directory. A later step that reads it, by `from` or by the same S3 key, gets that file.
- `globalName` makes an output a workflow output (`workflow.outputs.parameters.<name>`).

## 10. Simulated runs

A simulated run gives empty stdout and no files. Real workflows read that data, so the runtime has rules that keep the graph whole where Argo would stop or requeue. The placeholders of the first two rules apply only when `Launch.simulated` is set, as the binary does for the simulator (`Options.assume_inputs`); a real run needs the values, as `argo submit` does, and stops with Argo's message, like `spec.arguments.sample.value or spec.arguments.sample.valueFrom is required`.

- A workflow parameter without a value takes the first `enum` value, or a placeholder like `placeholder_sample_fastq`, with a warning. The same holds for the inputs of the entrypoint. A placeholder is one word, so that `when` reads it as a string.
- Only what a user gives when submitting gets a placeholder: workflow parameters, and the inputs and input artifacts of the entrypoint. A step or task that does not supply an input of the template it calls stays an error, because Argo rejects such a workflow when it is submitted (section 4.1). This matches the other drivers: a simulated run assumes the data a user gives (`Launch.simulated`, the placeholder inputs of the CWL graph test), not what the workflow itself must pass on.
- A `withParam` that is empty, still holds a tag or is a placeholder gives a loop without items, with a warning. So does a `withSequence` bound that is not known.
- An item that is empty text leaves the tags that read its fields in place.
- An output of a steps or DAG template that reads a skipped step, or a step whose output is empty, is empty, with a warning.
- A `when` that still holds a tag skips the step, with a warning.
- An output parameter whose file is empty takes its `default`, if it has one: the simulator makes up empty files for outputs that it cannot simulate. KFP-compiled workflows need this: their driver writes `cached-decision`, and the executor step runs only when that is not `true`. This rule too applies only with `Options.assume_inputs`.

A real executor gives real data, and these rules then do not apply.

## 11. Kubernetes objects and fields (decision 18)

iunctus runs Argo workflows without a Kubernetes cluster. It supports what scientific workflows commonly use, and maps Kubernetes objects onto task inputs, outputs and files where that keeps what a task computes. Warnings are printed to standard error once each, and `ArgoDriver::warnings` returns them.

### 11.1 Templates that need a cluster

These stop the run before its first task, with an error that names the template and decision 18, and exit code 33, in simulated runs too:

- `resource` templates, which create or change Kubernetes objects like Deployments, Services or Jobs. The exceptions are a PersistentVolumeClaim (section 11.4) and an Argo Workflow, which runs as part of the same run (section 11.7).
- `data` templates, which list an artifact repository from inside the cluster.
- `sidecars` and `daemon: true`, which are services that run beside tasks or across them and are reached over the pod network.
- Artifact plugins.

The check is part of the checks at submission (section 4.1), so it covers every template of the workflow and every template that it calls. A template that a `templateRef` with a tag names is checked when it is called.

### 11.2 Fields that are left out

Fields that only tell Kubernetes where and how to run a pod do not change what a task computes. They are left out with a warning: `podSpecPatch`, `nodeSelector`, `tolerations`, `affinity`, the fields of a `securityContext` other than `runAsUser` and `runAsGroup` (decision 28: no container gets more than the least permissions), `serviceAccountName`, `automountServiceAccountToken`, `priorityClassName`, `schedulerName`, `hostAliases`, `dnsConfig`, `dnsPolicy`, `hostNetwork`, `imagePullSecrets`, `imagePullPolicy`, `podGC`, `podMetadata`, `podDisruptionBudget`, `ttlStrategy`, `metrics`, `archiveLocation`, `artifactGC`, `volumeClaimGC`, `executor` and `memoize`.

Init containers are no such field: they run as tasks (section 11.8).

Container `readinessProbe` settings are accepted as a no-op wherever they appear, including main containers, scripts, init containers, sidecars and container sets: readiness only reports whether a container can receive traffic, and never stops or restarts it. Sidecars remain unsupported under decision 18. Container `livenessProbe`, `startupProbe`, lifecycle hooks, restart policies and rules, and resize policies remain refused because they can run, stop or restart commands.

### 11.3 ConfigMaps

Workflow and template `synchronization` use mutexes and semaphores shared by the run and
its child workflows. All locks of a node are acquired together, in arrival order, and
released when the node ends; retry nodes hold their locks across attempts and backoff.
Lock namespaces remain distinct. Database synchronization remains refused.
Semaphore `configMapKeyRef` reads a positive integer from a loaded ConfigMap's `data`.
Supply the ConfigMap in the workflow file or with `-config`. For an inline iunctus limit,
add `limit: 2` beside `name` and `key` in the reference. Missing sources fail with the
ConfigMap name, namespace and key; no Kubernetes request or guessed limit is used.

ConfigMaps come from the same documents as templates: the workflow file, the configs and the repository (section 3). They give:

- parameters with `valueFrom.configMapKeyRef`, of the workflow and of template inputs;
- environment variables with `valueFrom.configMapKeyRef`, and every key of an `envFrom` `configMapRef`;
- volumes of type `configMap`: each key, or each entry of `items`, becomes a file input with its text.

When no loaded document holds the ConfigMap or the key, a parameter takes its `valueFrom.default`, as Argo does, and an environment variable or volume marked `optional` is left out. Otherwise a simulated run uses a placeholder with a warning, and a real run stops with an error that names the ConfigMap and the key.

### 11.4 Volumes as shared files

Volumes that several steps mount act as a shared file system. The run keeps the files that tasks write into a volume and gives them to the tasks that mount it later:

- A volume is a `persistentVolumeClaim` (known by its claim name), a claim of `volumeClaimTemplates` (by its name), a `hostPath` (by its path), or an `emptyDir`, which only the containers of one pod share, like those of a container set, which run in one task (section 8.2).
- A task that mounts a volume at a path gets the files that earlier tasks wrote there as file inputs below the mount path, or an empty directory. `subPath` selects a folder of the volume. Unless the mount is `readOnly`, the files are copies that the task may change, like `echo more >> /work/log.txt`; the files of a host path stay read-only.
- Unless the mount is `readOnly`, the mount path is also a directory output of the task. When the task ends, its files replace the files of the same name in the volume.
- Tasks that run at the same time see the volume as it was when they started. Their files are merged when they end; for a file that both wrote, the one that ended last wins.
- A `resource` template that creates or applies a PersistentVolumeClaim, like Ensembl's `create-pvc`, makes a new empty volume of the claim's name and runs no task. Its output parameters read the manifest with `jsonPath`. A `delete` of the claim drops the volume.
- A claim that the workflow does not create starts empty, with a warning. Other volume types, like NFS, act like a claim of their name, with a warning.

`hostPath` names a folder on the Kubernetes node that runs the pod. Without a cluster there is no such node, so a host path is a path on the machine that runs iunctus (decision 18):

- A task that mounts it gets the host folder (or file, for `type: File`) as an input, and on top of it the files that earlier tasks of the run wrote there.
- Unless the mount is `readOnly` or of `type: File`, the files that the task writes there go into the volume, as for other volumes. In a real run the driver also writes them back to the host path when the task ends, into the folders of `subPath`, over files of the same name. A file that cannot be written back ends the node with an error.
- A simulated run never writes to a host path, and a host path that does not exist is no error there.

The driver writes back from the local paths of the task's output files, which the local executor and the simulator give it. An executor that keeps outputs elsewhere, like TES with S3, needs the Files service of the execution layer (docs/design/engine.md): the driver then asks it for each file and writes the bytes to the host path. Writing back stays in the driver, because only the driver knows which output belongs to a host path.

### 11.5 Secrets

Secret values never enter a task, because a task is written into TES documents, logs and cache keys. This holds for environment variables and HTTP headers from `secretKeyRef`, for `envFrom` with `secretRef`, and for volumes of type `secret`:

- A simulated run gives a placeholder like `placeholder_secret_<name>_<key>`, with a warning. A secret volume holds a placeholder file for each of its `items`.
- A real run stops before its first task with an error that names the Secret, because iunctus has no secret store yet.

The execution layer must supply that store. It maps a Secret's name and key to a value from the user's own store, like a local file or a vault. It gives the value to the container only when the container runs, as an environment variable or a file, and keeps it out of task documents, logs and cache keys, which name only the Secret and the key. The credentials of artifact repositories, like `accessKeySecret` of S3 artifacts, belong to the execution layer's storage, not to tasks.

### 11.6 Commands

A container's `command` and `args` form one command line, which the executor runs as given (decision 18). A container without either runs the default command of its image.

### 11.7 Workflows that start other workflows

A `resource` template whose manifest is an Argo Workflow, with its own templates or with a `workflowTemplateRef`, runs that workflow as part of the same run (decision 18):

- The action `create` (or `apply`) starts the child workflow when the resource node starts. The child is a run of its own, with its own name, globals, templates, parameters and exit handlers. Its arguments come from the manifest, after substitution. A `generateName` gets a suffix from the name of the resource node, so that children of a loop get different names.
- The tasks of the child join the execution graph of the run. Its first tasks wait for the tasks that the resource node waits for, and the tasks after the resource node wait for the last tasks of the child when the node waits for the child. Task ids are unique across parent and children.
- The resource node reads the child as Argo reads the Workflow object: `metadata.name`, the manifest's `spec`, and `status.phase` (`Running`, `Succeeded`, `Failed` or `Error`), `status.message`, `status.startedAt`, `status.finishedAt` and the workflow outputs in `status.outputs.parameters`.
- With a `successCondition` or `failureCondition`, the node waits for the child to end and then applies them, as Argo's executor does: requirements like `status.phase == Succeeded` or `status.phase in (Failed, Error)`, separated by commas, on paths of the Workflow object. The failure condition is checked first. A child that fails therefore fails the node. When the child has ended and neither condition holds, the node ends with an error, where Argo would wait forever. Without conditions, the node succeeds as soon as the child starts, as in Argo, and the child runs on.
- A `get` of a Workflow that this run started, by name, waits for that child in the same way, like `k8s-wait-wf`. Other actions on Workflows, like `patch` or `delete`, and Workflows that this run did not start, need a cluster and stay unsupported.
- Output parameters read the Workflow object with `jsonPath`.
- Workflows that start workflows may nest at most 10 levels deep. Deeper nesting stops the run with the error `workflows start other workflows more than 10 levels deep`, which catches a workflow that starts itself without end.
- A child that Argo would reject at submission, or that uses a feature iunctus does not support, ends the resource node with an error, or stops the run with exit code 33, like any template.

### 11.8 Init containers

Init containers run as their own tasks before the task they belong to, one after the other, as Kubernetes runs them (decision 18):

- Each init container is a task with its image, command, arguments, environment and resources, named after the node and the container, like `wf.step (init launcher)`. Its mounts are its own `volumeMounts`, and with `mirrorVolumeMounts` also those of the main container.
- The next task starts when the one before ends; the main task, or the task of a container set, comes last and has the init tasks in `after`. The files that an init container writes to a mounted volume, like an `emptyDir` of the pod, are inputs of the tasks that mount it later, as for any volume (section 11.4). KFP-compiled workflows use this: an init container copies the KFP launcher into an `emptyDir` that the main container runs.
- An init container that fails fails the node, as a failed pod does in Argo. In a simulated run an init task gets pretended outputs like any task.

## 12. Deliberate differences from Argo

- Tags of `tasks.` and `steps.` that stay unresolved in a simulated run stay literal. Argo would requeue the node until they resolve, or fail it.
- A `when` that cannot be decided skips the step instead of failing it.
- Manual suspension requires an Argo resume controller and remains refused.
- The retries inside a container set do not wait for their `duration`.
- The containers of a container set see all volumes that any of them mounts, at the same paths, and the work directory of the task. In Argo each container sees only its own mounts. A volume that containers of one set mount at different paths is one folder at each path, not one shared folder.
- On TES the containers of a set run one after the other (section 8.2).
- The containers of a set run as the user of the main container. Apptainer runs every container as the user who calls it, so on SLURM with Apptainer a `runAsUser` has no effect and a step may not write where only root may (decision 28).
- `workflow.failures` lists failed nodes in the order the run made them. Argo iterates a Go map, so its order varies.

## 13. The test workflows

`just argo-test-workflows` fetches the 19 repositories of research question Q6 at the commits in `argo-parser/tests/fixtures/test-workflows/revisions.txt` into `~/.cache/iunctus/test-workflows/argo`, and argoproj-labs/argo-workflows-hello-executor-plugin, whose plugin the talk-demos examples of plugin templates install. gennext-workflows has no license: it is fetched for research only and never copied into this repository.

`just argo-test-workflows-test` runs the ignored test `argo-runtime/tests/test_workflows.rs`. It reads every Argo document of the test workflows. A document with an entrypoint runs through the simulator; the others, like libraries of WorkflowTemplates and executor plugin ConfigMaps, get the checks of section 4.1. The outcomes are compared with `argo-runtime/tests/fixtures/test-workflows/outcomes.txt`: `ok` and the number of tasks, `valid`, or the exit code and the error (its end, when it is long). Every document that does not pass needs a record in `argo-runtime/tests/fixtures/test-workflows/rejected.txt` that names its kind and why, in one sentence: `invalid` (the workflow itself is broken) or `missing` (it calls a WorkflowTemplate that is in no repository of the test workflows), each with the output of `argo lint`, which rejects it the same way (or accepts a workflow that would wait forever in Argo, section 11.7), or `unsupported (decision 18)`. The test fails on a document without a record, on a record of the wrong kind (exit code 33 is `unsupported`), and on a record for a document that passes. `rejected.py` next to the records writes them; it runs `argo lint` and never runs as part of iunctus.

Some documents get directories as configs, as `-c` gives them: Hera's copies of Argo's examples get Argo's `examples`, because Hera does not copy all the WorkflowTemplates they refer to; the talk-demos plugin examples get the hello executor plugin; and the talk-demos copy of pipekit's CI workflow gets the WorkflowTemplates of pipekit/argo-workflows-ci-example. `ARGO_TEST_WORKFLOWS_UPDATE=1` records new outcomes, `ARGO_TEST_WORKFLOWS_FILTER` runs only matching paths, `ARGO_TEST_WORKFLOWS_VERBOSE=1` prints the warnings, and `ARGO_TEST_WORKFLOWS_ORDER=shuffled:<seed>` lets up to 8 tasks be pending and finish in a random order. With seeds 1 and 2 the outcomes stay the same, so the driver does not depend on the order of outcomes.

Results on 2026-09-29: 1390 of 1573 documents pass: 1182 run through with a full graph and 208 documents without an entrypoint pass Argo's checks. 104 documents are unsupported by decision 18, `argo lint` rejects 78 others the same way, and one would wait forever in Argo.

| Repository | Pass | Documents |
|---|---:|---:|
| argoproj/argo-workflows | 511 | 571 |
| argoproj-labs/hera | 250 | 278 |
| GenivaInformatics/gennext-workflows | 147 | 168 |
| kubeflow/pipelines | 153 | 153 |
| Ensembl/regulation-workflows | 107 | 132 |
| codefresh-io/argo-hub | 104 | 106 |
| pipekit/talk-demos | 18 | 36 |
| AliyunContainerService/argo-workflow-examples | 19 | 20 |
| pipekit/argo-workflows-ci-example | 5 | 20 |
| couler-proj/couler | 18 | 19 |
| argoproj-labs/argo-workflows-catalog | 16 | 17 |
| DataCloud-project/SIM-PIPE | 11 | 11 |
| thoth-station/workflows | 4 | 11 |
| wwood/singlem-sra-processing | 9 | 9 |
| PGpalt/Hybrid-Argo-Workflows | 6 | 6 |
| terrytangyuan/distributed-ml-patterns | 4 | 6 |
| devops-ws/argo-workflows-guide | 3 | 3 |
| uc-cdis/argo-wrapper | 1 | 3 |
| argoproj-labs/argo-workflows-hello-executor-plugin | 2 | 2 |
| DonaldKellett/snakemake-tutorial-argo-workflows | 2 | 2 |

The 183 documents that do not pass:

| Kind | Reason | Documents |
|---|---|---:|
| unsupported (decision 18) | A resource template that manages Kubernetes objects other than a PersistentVolumeClaim or a Workflow that the run starts, like Jobs, Services, Pods or Argo CD Applications, or that patches a Workflow | 61 |
| unsupported (decision 18) | A daemon template | 23 |
| unsupported (decision 18) | Sidecars | 15 |
| unsupported (decision 18) | A data template | 5 |
| missing | A WorkflowTemplate that no repository of the test workflows defines, like gennext's `collect-qc-data-templates` or Ensembl's deprecated `*-submittable` templates | 29 |
| invalid | A template that is not there, or that has no type | 18 |
| invalid | A step or task that does not supply an input of the template it calls, among them Argo's expected failures | 11 |
| invalid | Names that are missing, invalid or not unique | 10 |
| invalid | Documents that are not valid YAML or do not fit Argo's types, among them Argo's malformed test files; one has a duplicate key, which `argo lint` and `argo submit` reject in their default strict mode | 9 |
| invalid | A tag that does not resolve (`{{steps.A...}}` inside a DAG) | 1 |
| invalid | A resource template that waits for a Workflow whose phase never meets its conditions (`status.phase == Test`, an Argo test of stopping a run); Argo waits forever, and `argo lint` accepts it | 1 |

Some documents that pass are rejected by `argo lint --strict=false`, where iunctus is more lenient without a change to what runs: tags that name variables no template sets (19 documents; section 4.1 leaves this check out), CronWorkflows with the v3 field `schedule`, which iunctus converts as `argo convert` does (6), and references to WorkflowTemplates that Argo would refuse to create, like gennext's `copy-inputs-to-scratch` with its unfinished template that no workflow calls. With `--strict`, its default, `argo lint` also rejects unknown fields, which iunctus leaves out.

## File descriptions: argo-runtime

### `argo-runtime/src/checks.rs`

The single rules of Argo's checks of a submitted workflow (workflow/validate/validate.go of
v4.1.4): names, arguments, the inputs a call supplies, leaf templates, and the order of DAG
tasks. The module `validate` applies them while it walks the templates.

### `argo-runtime/src/child.rs`

Resource templates that start other Argo workflows (decision 18). A `create` or `apply` of
a Workflow runs that workflow, the child, as a run of its own inside this run: its tasks join
the execution graph, and the resource node reads the child as Argo reads the Workflow object,
with `successCondition`, `failureCondition` and `jsonPath`. A `get` of a child by name waits
for it in the same way.

### `argo-runtime/src/conditions.rs`

The `successCondition` and `failureCondition` of resource templates: Kubernetes label
selectors (k8s.io/apimachinery/pkg/labels) whose keys are paths into the JSON of the
resource, as Argo's executor reads them with gjson (workflow/executor/resource.go).

### `argo-runtime/src/expr.rs`

An evaluator for expr-lang (github.com/expr-lang/expr v1.17), the language of Argo's
expression tags `{{=...}}`, of `depends` and of the other expression fields. Argo adds
`asInt`, `asFloat`, `toJson`, `jsonpath` and the Sprig functions as `sprig.<name>(...)`.

The evaluator follows expr's runtime rules: `/` always gives a float, `&&` and `||` take
only booleans, a missing map key reads as nil, and an unknown top-level name is an error,
which makes Argo leave the tag for a later substitution phase.

### `argo-runtime/src/hooks.rs`

Hooks and exit handlers (workflow/controller/hooks.go and exit_handler.go): the hooks of the
workflow and of steps and tasks that run when their expression holds, and the exit handlers
that run after the entrypoint or after a node, with `workflow.status` and `workflow.failures`.

### `argo-runtime/src/http.rs`

HTTP templates. Argo's agent sends the request itself; here the image `curlimages/curl` runs
curl, which writes the response body and status code to files. The status code and the body
decide the success condition, as in Argo.

### `argo-runtime/src/jsonpath.rs`

JSONPath for Argo's `jsonpath(json, path)` function, after github.com/evilmonkeyinc/jsonpath
v0.8.1: a path that names one value gives that value, and a path with wildcards, slices,
unions, filters or `..` gives a list. A missing key or index is an error.

### `argo-runtime/src/lib.rs`

Runs Argo Workflows documents as language-neutral tasks, following the controller of Argo
v4.1.4: template resolution through `templateRef` and `workflowTemplateRef`, Argo's
substitution phases, expr-lang and govaluate expressions, steps, DAGs, loops, retries, exit
handlers and hooks. Each leaf template becomes a [`workflow_graph::Task`].

[`driver`] is the entry point for a [`Launch`]: the workflow file, a parameter file
(`Launch.inputs`, like `argo submit --parameter-file`), parameters (`Launch.params`, like
`-p`), an entrypoint (the first of `Launch.targets`), and extra files or directories of
templates (`Launch.configs`). Templates are also looked up in the repository of the workflow
file: the nearest directory above it that holds `.git`.

### `argo-runtime/src/library.rs`

The documents a workflow can reference by name: the WorkflowTemplates and
ClusterWorkflowTemplates of its own file, of extra files, and of its repository, which
stand in for the templates a cluster would hold.

### `argo-runtime/src/marks.rs`

The markers of an Argo run: a `withParam`, a `withSequence` or a `when` that reads the
outputs of earlier steps or tasks decides how many tasks follow from those outputs, so a run
with real outputs can start more or fewer tasks there than a simulated run.

### `argo-runtime/src/merge.rs`

A merge of value trees by key, like the merge patches of Kubernetes, as Argo joins a
Workflow with the WorkflowTemplate of its `workflowTemplateRef` and merges
`templateDefaults` into templates: mappings merge key by key, lists of named items merge by
name, and anything else in the overlay wins.

### `argo-runtime/src/objects.rs`

The Kubernetes objects that tasks read, without a cluster (decision 18). ConfigMaps come from
the loaded documents: the workflow file, the configs and the repository. Secrets have no
store: their values never enter a task, a simulated run gives placeholders, and a real run
stops.

### `argo-runtime/src/plugin.rs`

Executor plugin templates. Argo runs them in its agent pod, whose sidecars are the servers of
the plugins: the agent posts the template to `/api/v1/template.execute` of one plugin after
the other, until one replies with a node. Here each call is a task in the image of the
plugin: a shell script starts the plugin's server, posts the template as the agent does, and
keeps the reply and its HTTP status as output files. The reply gives the node its phase,
message and outputs; a plugin that does not take the template passes it to the next one.

### `argo-runtime/src/pod.rs`

Leaf templates as tasks: what a pod would run, and what its outcome makes.

A container template is one task with the container's image, command and arguments. A
script template runs its command on the source, which the task gets as the file
`/argo/staging/script`, as Argo's executor writes it. A container set is one task of several
containers, which run in one place in the order of their dependencies and share the volumes
of the set. Init containers are tasks before these, one after
the other, and the files they write into volumes are inputs of the next. An HTTP template
runs curl, which writes the response body and status code to files that decide the success
condition. Environment variables and volumes come from the modules `objects` and `volumes`
(decision 18).

### `argo-runtime/src/retry.rs`

Retries, after `processNodeRetries` of Argo's operator (workflow/controller/operator.go): a
retry node makes attempts of its call until one succeeds, the policy or the expression stops
it, or the limit is reached.

### `argo-runtime/src/run.rs`

The state of a run, after Argo's workflow controller (workflow/controller/operator.go): a
tree of nodes that each pass walks from the root. A pass starts the nodes whose turn has
come; leaf templates become tasks, and a run goes on when their outcomes arrive.

### `argo-runtime/src/schema.rs`

The inputs of an Argo workflow as one JSON Schema (decision 24): the parameters of
`spec.arguments`, which are texts, with their `enum`, their value as the default and their
`description`, and the input parameters of the entrypoint that no argument supplies. Input
artifacts are paths that the workflow gives itself, with their location as the default. The
values of a run are its parameters after the command line and the parameter file. The
outputs are those of the entrypoint, parameters as texts and artifacts as paths, and the
global outputs of all templates, by their `globalName`.

### `argo-runtime/src/scope.rs`

The local scope of steps and DAG templates (workflow/controller/scope.go): the inputs of
the template and what earlier steps and tasks made, as `steps.<name>.*` and
`tasks.<name>.*`, and the outputs that steps and DAG templates take from it.

### `argo-runtime/src/sprig.rs`

The Sprig functions (github.com/Masterminds/sprig v3.3.0) that Argo offers in expressions as
`sprig.<name>(...)`. Argo leaves out `env` and `expandenv`. Conversions follow Sprig's use
of `cast`: a text that is not a number converts to 0 rather than failing.

### `argo-runtime/src/start.rs`

How a run starts: the spec of the workflow joined with the WorkflowTemplate it runs, its
parameters, the name of the workflow, its global variables, and warnings about fields that
need Kubernetes.

### `argo-runtime/src/tags.rs`

Argo's `{{...}}` tags. A simple tag like `{{inputs.parameters.x}}` is looked up by name; an
expression tag `{{=...}}` runs as expr in an environment that nests the dotted names.

Argo substitutes the JSON text of a whole template (util/template/replace.go); substituting
every string of the value tree gives the same result, because Argo escapes the inserted
values and unescapes the text again. Tags that cannot be resolved yet stay in place when the
phase allows it, for a later phase to resolve.

### `argo-runtime/src/validate.rs`

The checks Argo makes when a workflow is submitted or a template is created
(workflow/validate/validate.go of v4.1.4), for what decides whether a workflow can run: the
names of templates, steps, tasks and parameters, the entrypoint, template types, references
to templates, the inputs that calls supply, the arguments of steps and tasks, hooks, and the
dependencies of DAG tasks. The messages are Argo's. Like Argo, a template is checked where
it is called and on its own, and templates of other documents only where they are called.

The single rules are in the module `checks`.

Left out: checks of Kubernetes fields (object name lengths, volume mounts, metrics, pod
garbage collection), cron schedules, and whether every tag names a known variable; a run
finds tags that do not resolve when it substitutes them.

### `argo-runtime/src/volumes.rs`

Volumes as shared files (decision 18). Volumes that several steps mount act as a shared file
system: the run keeps the files that tasks write into a volume, and gives them to the tasks
that mount it later. A volume is a claim (`persistentVolumeClaim` or `volumeClaimTemplates`),
a host path, or the `emptyDir` of one pod. In a real run, the files that tasks write into a
host path go back to that path on this machine. ConfigMaps become files with their text, and
Secrets placeholders in a simulated run.

### `argo-runtime/src/when.rs`

`when` conditions: govaluate (github.com/Knetic/govaluate v3) after simple-tag substitution,
with Argo's rule that a bare word is a string, so `heads == heads` holds. A word with a dot,
which govaluate reads as an accessor, stays an error, as in Argo.

One rule is iunctus's own: a missing operand, as a tag that substitution left empty makes
it (` == heads`), is not known, and every comparison with it is false, `!=` too. govaluate
fails on such conditions; the rule lets runs whose outputs are empty, like simulated runs,
decide their conditions instead of failing, and ends recursions that wait for an output.

### `argo-runtime/tests/run.rs`

Runs small Argo workflows through the engine and the simulator, and checks the
execution graphs: steps, DAGs, loops, conditions, outputs, artifacts, templates of other
documents, exit handlers, retries and hooks.

### `argo-runtime/tests/test_workflows.rs`

Reads every Argo document of the test workflows from `just argo-test-workflows`. Documents
with an entrypoint run with the simulator; the others, like libraries of
WorkflowTemplates, get the checks Argo makes when they are created. The outcomes are compared
with tests/fixtures/test-workflows/outcomes.txt: `ok` and the number of tasks, `valid`, or the
exit code (33 for unsupported features) and the first error. Each document that does not pass
needs a record in tests/fixtures/test-workflows/rejected.txt that names why it cannot run:
`invalid` or `missing`, with the evidence that `argo lint` rejects it too, or `unsupported`
by a decision. Before each run, the test describes the inputs of the workflow, as the binary
does (decision 24), and compares the counts of its schema and of a check of the simulated run
with tests/fixtures/test-workflows/inputs.txt, with the totals of their outputs as the line
`(outputs)`; every schema must be valid JSON Schema.
`ARGO_TEST_WORKFLOWS_UPDATE=1` rewrites the outcomes and the inputs;
`ARGO_TEST_WORKFLOWS_FILTER` runs only the documents whose path contains it, and
`ARGO_TEST_WORKFLOWS_ORDER=shuffled:<seed>` lets tasks finish in a random order.

## Source contracts: argo-runtime

### `argo-runtime/src/checks.rs:94`

Why a template cannot run without a Kubernetes cluster, if it cannot (decision 18):
`resource` templates, except those that manage a PersistentVolumeClaim, which become volumes
of the run, and those that start an Argo Workflow, which runs inside the run; `data`
templates, sidecars and daemons.

### `argo-runtime/src/dag.rs:201`

The phase of a DAG, as Argo's `assessDAGPhase` finds it: running while any of its
nodes, hooks included, runs; then by the branch phases of its target tasks. A task that
completed gives its branch its own phase; a skipped or omitted task inherits the phase
of its dependencies, a failure over success. A target that failed without continuing
fails the DAG; under fail-fast the first such target decides at once.

### `argo-runtime/src/library.rs:289`

The document that a `templateRef` names: like [`Library::find`], but among several
documents of the kind and name, those that hold the template win. A repository may keep
several versions of a WorkflowTemplate, while a cluster holds the one that the workflow
was written for.

### `argo-runtime/src/plugin.rs:118`

The plugins that a plugin template goes to, in the order they are tried: those of
`spec.executorPlugins`, which replace the installed plugins as in Argo, or else the
plugin ConfigMaps of the library. Plugins whose name holds a key of the template's
`plugin` field come first, as they are the ones most likely to take it.

### `argo-runtime/src/pod.rs:534`

Warns once about fields of a template and its containers that only matter to
Kubernetes, which a run without Kubernetes leaves out (decision 18).
The user and group that a container runs as, as Kubernetes picks them: from the
`securityContext` of the container, else of the template, else of the workflow; what
none of them names, the image decides.

### `argo-runtime/src/schema.rs:106`

The outputs of the workflow that a document runs: the output parameters of its entrypoint,
which are texts, and its output artifacts, which are paths, then the global outputs of its
templates by their `globalName`, as `workflow.outputs` gives them. An artifact that is
`optional` may be missing.

### `argo-runtime/src/schema.rs:313`

The parameters that the workflow reads: `workflow.parameters.<name>` anywhere in its spec
and in the templates it refers to with `templateRef`, and `inputs.parameters.<name>` in its
entrypoint, whose inputs the arguments supply. An input artifact of the entrypoint is read,
as Argo places it in the container. `{{workflow.parameters}}` reads every parameter.

### `argo-runtime/src/start.rs:211`

The workflow parameters of a run: those of the spec, overridden by the command line and the
parameter file, and the inputs of the entrypoint (`entry`), which `-p` supplies too. In a
simulated run, parameters without a value get placeholders, and the input artifacts of the
entrypoint that nothing supplies are listed for placeholders too.

### `argo-runtime/src/validate.rs:202`

Checks a workflow spec whose templates are those of `base`. `entry` is the entrypoint
and what the workflow supplies to it; without it the templates are only checked on
their own, as for a WorkflowTemplate without an entrypoint. `allow_empty` lets workflow
parameters be without a value, as when Argo lints a document or creates a template.

### `argo-runtime/src/volumes.rs:249`

The files of a volume that the task may write are copies that it may
change, as a task changes the files of a volume in Argo; its changes go
into the volume when it ends. The files of a host path stay read-only,
since the host folder itself is shown there.

### `argo-runtime/tests/run.rs:508`

Argo schedules the tasks whose dependencies are done before it assesses the DAG, and a
DAG runs while any of its tasks runs; so `c` starts after `b` although `a` failed, also
with `failFast`, the default (workflow/controller/dag.go of Argo v4.1.4). `-keep-going`
turns `failFast` off where a DAG does not set it.

### `argo-runtime/tests/test_workflows.rs:26`

Directories of templates that documents under a path get as configs, as `Launch.configs`
gives them. Hera's upstream examples are copies of Argo's examples, but Hera does not copy
the WorkflowTemplates that they refer to in full. The talk-demos examples of plugin templates
install the hello executor plugin from its own repository, and the talk-demos copy of the CI
workflow of pipekit/argo-workflows-ci-example refers to that repository's WorkflowTemplates.

### `argo-runtime/tests/test_workflows.rs:450`

The records of rejected.txt: for each document, its kind and why it cannot run. A record is
a block of lines between empty lines: the document, then `= <kind>: <why>` with one plain
sentence, then for `invalid` and `missing` the command (`$`) and the output (`>`) of `argo
lint`. The kind is `invalid` (the workflow itself is broken), `missing` (it needs a
WorkflowTemplate that is in no repository of the test workflows), or `unsupported (decision
N)` (iunctus leaves the feature out on purpose, as decision N of docs/decisions.md says).
Panics on a record without a valid kind or without its evidence.

## Rejection evidence regeneration

Records why each Argo test workflow that does not pass cannot run, into rejected.txt next to
this script: one block per document with its kind and one plain sentence, and for `invalid`
and `missing` the output of `argo lint` (the argo CLI v4.1.4 in the PATH). iunctus never runs
this script; it only records the evidence.

The document is linted with `argo lint --offline --strict=false` together with the files of
its repository that define the WorkflowTemplates it refers to (the copy that holds the
referenced template, cut down to the templates that are called, as a cluster would hold a
valid copy), with namespaces removed so that the references resolve, and with values for the
inputs of a Workflow's entrypoint, as `argo submit -p` gives them. A document that only the
strict mode rejects, the default of `argo lint` and `argo submit`, is linted in that mode too.

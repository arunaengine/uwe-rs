<!-- The design of argo-runtime, which runs Argo Workflows documents as language-neutral tasks. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Argo runtime design

Status: implemented in the crate `argo-runtime`, 2026-09-28. The reference is the controller of Argo Workflows v4.1.4. The [research note](../../research/argo-workflows.md) describes the language, the corpus and the feature inventory that this design builds on.

## 1. The design in short

1. **The runtime repeats Argo's controller, not its API server.** Argo walks a tree of nodes again and again until nothing changes. `argo-runtime` does the same: every pass runs the templates whose inputs are ready, and every leaf template becomes one or more `workflow_graph::Task`s. The node names, the node IDs and the pod names follow Argo's rules, so warnings and errors name what `argo get` would show.
2. **Templates come from a library of documents.** The workflow file, the files given as configs and the repository of the workflow file are searched, in that order, for `templateRef` and `workflowTemplateRef`.
3. **Substitution follows Argo's phases.** Globals and the local scope are substituted first, inputs second. Tags that cannot be resolved yet stay in place for a later phase, as in Argo.
4. **The expression languages are implemented in the crate.** expr-lang with Argo's helpers and a Sprig subset, govaluate for `when`, and JSONPath. Values keep Go's model: JSON numbers are float64, maps print with sorted keys, and results print the way Go prints them.
5. **A mock run always makes a full graph when Argo would.** Data that is not known in a mock run (empty stdout, placeholder parameters) never stops the run. Where Argo would read that data, the runtime takes the path that keeps the graph whole and logs a warning.
6. **Kubernetes fields are left out with a warning.** Volumes, sidecars, init containers, secrets and similar fields do not exist without a cluster. The run goes on without them.
7. **Unsupported features fail loudly.** Artifact plugins stop the run with exit code 33 (`DriverError::unsupported`).
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

`ArgoDriver::new(document, library, options)` builds the same thing from a loaded document, for callers that load their own library, like the corpus test.

The modules of `argo-runtime`:

| Module | What it does |
|---|---|
| `library` | Loads documents, finds templates by kind and name |
| `run` | The operator: nodes, phases, template resolution, arguments, retries, hooks, exit handlers |
| `steps`, `dag` | Step groups, DAG tasks, `depends`, the assessment of a DAG |
| `items` | `withItems`, `withParam` and `withSequence` |
| `scope` | Scopes of steps and tasks, outputs of steps and DAG templates |
| `pod` | Leaf templates as tasks, and the outcome of a task as outputs |
| `plugin` | Plugin templates as calls of executor plugins |
| `validate` | The checks Argo makes when a workflow is submitted |
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
- An evaluator in the crate never panics on corpus input and has no dependency.

The evaluator has a lexer, a Pratt parser with expr's precedence table, and a tree walker. It covers literals, arithmetic (`/` always gives a float), comparison, `in`, `not in`, `contains`, `startsWith`, `endsWith`, `matches`, ranges, slices, the ternary operator, `??`, `let`, optional chaining, closures with `#` and `.field` for the predicate functions (`all`, `any`, `none`, `one`, `filter`, `map`, `count`, `sum`, `find`, `groupBy`, `sortBy` and others), and expr's built-in functions. The unit tests use expressions from the corpus.

### 6.2 Sprig

The Sprig subset covers what the corpus uses and what is cheap to add alongside it: strings, paths (`base`, `dir`, `ext`, `clean` and the `os` forms), numbers with Sprig's lenient conversion, `default`, `empty`, `coalesce`, lists, dicts, base64, JSON, regular expressions and dates. An unknown Sprig function is an error that names it.

### 6.3 govaluate for `when`

`when` runs after simple tags are substituted. The evaluator follows govaluate as Argo uses it: a bare word is a string (`heads == heads` holds), a dotted word is an error, and `-` is minus. A comparison with a missing operand (`== heads`, as an empty output leaves it) is false, not an error.

### 6.4 JSONPath

`jsonpath(text, path)` follows evilmonkeyinc/jsonpath: a definite path gives the value, other paths give a list, and a missing key is an error. Filters with `@` work; `$` inside a filter is an error.

## 7. Control flow

| Feature | Behaviour |
|---|---|
| `steps` | Step groups run one after another, the steps of a group in parallel. A group waits for every step. |
| `dag` | Tasks start when their `dependencies` or `depends` hold. `depends` is translated to an expression over task results, as Argo does. The phase of the DAG follows `assessDAGPhase`: the phases of the branches that end the DAG. |
| `when` | Decided after substitution. A condition that still holds a tag, as a mock run leaves some, cannot be decided: the step is skipped with a warning. |
| Loops | `withItems`, `withParam` and `withSequence` give one node per item, named `name(index:description)`. An empty loop gives a skipped node. A `withParam` that does not parse is ignored when `when` is false. |
| `continueOn` | `failed` and `error` let the run go on after a failed step or task. |
| `failFast` | Only changes the assessment of the DAG, as in Argo. |
| Retries | A retry node holds attempts named `name(n)`. The policy, the limit and the retry expression decide whether another attempt runs. Backoff is not waited for. With `MockTes` every task succeeds, so each retry node has one attempt. |
| Exit handlers | `onExit` of the workflow and the `exit` hook run after the entrypoint, with `workflow.status` and `workflow.failures`. The exit hooks of steps and tasks run after their node. |
| Lifecycle hooks | Hooks of the workflow and of steps and tasks run when their `expression` holds. |
| `suspend` | Completes at once, as if `argo resume` came. Supplied output parameters take their default, or the first `enum` value, or stay empty with a warning. |

Outputs of steps and DAG templates read the scope (`valueFrom.parameter`, `expression`, `supplied`, artifacts by `from` and `fromExpression`). Outputs of skipped steps get their defaults, as `addSkippedNodeOutputsToScope` does in v4.

## 8. Leaf templates as tasks

| Template | Task |
|---|---|
| `container` | One task with the image, command, arguments, working directory and environment. |
| `script` | The command runs the file `/argo/staging/script`, which the task gets as an `Input` with the source as its content, as Argo's executor writes it. |
| `containerSet` | One task per container, ordered by their `dependencies` with `Order` edges. The main container (named `main`, or the last one) gets the outputs. |
| `resource` | Argo's executor image runs `argoexec resource <action>` with the manifest as `/tmp/manifest.yaml`, as Argo does in a pod. It needs a Kubernetes cluster when it really runs. Output parameters from `jsonPath` or `jqFilter` are empty, with a warning. |
| `http` | The image `curlimages/curl` runs curl, which writes the response body and status code to files. The status code decides `successCondition`. When the status is not known, as in a mock run, the node succeeds with a warning. |
| `data` | Argo's executor image runs `argoexec data` with the source and the transformations as JSON. |
| `plugin` | One call of an executor plugin per task, in the plugin's image (section 8.1). |

Resources come from the container's requests, or its limits where requests say nothing. `activeDeadlineSeconds` or `timeout` sets the time limit. Environment variables that read a field of the pod with `fieldRef` get the run's values: the pod name, the namespace, a stable uid, the service account, a node name and an IP address. Variables from Secrets or ConfigMaps are left out with a warning.

### 8.1 Executor plugins

Argo runs a plugin template in its agent pod, whose sidecars are the servers of the executor plugins. The agent posts the template to `/api/v1/template.execute` of one plugin after the other, until one replies with a node (workflow/executor/agent.go).

iunctus has no agent pod. Each call of a plugin is a task in the plugin's own image, so it follows decision 11:

- The command is a POSIX shell script. It starts the plugin's server from the command and arguments of the sidecar, and posts `/argo/plugin/request.json`: the name, namespace and uid of the workflow and the template, as the agent sends them. It sends the token of `/var/run/argo/token`, which Argo also mounts for the plugin. It writes the reply to `/argo/plugin/reply.json` and its HTTP status to `/argo/plugin/status`. It uses `curl`, or `wget` where the image has no curl, as in `python:alpine`.
- The plugins are those of `spec.executorPlugins`, which replace the installed plugins as in Argo. Otherwise they are the plugin ConfigMaps of the library (label `workflows.argoproj.io/configmap-type: ExecutorPlugin`, as `argo executor-plugin build` makes them). Plugins whose name holds a key of the template's `plugin` field are tried first.
- The reply gives the node its phase, message and output parameters. A plugin that answers 404 or replies without a node passes the template on to the next plugin. When no plugin is left, the node fails with Argo's error `no plugin executed the template`.
- In a mock run the reply is not known. The first plugin runs the template, and the node succeeds with a warning. Declared output parameters get their value or default.

Limits: the image of a plugin needs `sh` and `curl` or `wget`. A reply with the phase `Running` or `Pending` ends the node with an error, because iunctus does not call a plugin again later.

## 9. Inputs and outputs

Input artifacts become `Input`s. The source is the artifact's location: `s3://bucket/key`, an HTTP URL, a git repository (with `#revision`), `gs://`, Azure, Artifactory, HDFS or OSS. Raw artifacts become an `Input` with content. An S3 artifact with only a key uses the default artifact repository, named `s3://default-artifact-repository` here.

Output artifacts and output parameters with a `path` become the task's outputs. Deeper paths come first, so that a directory output does not hide a file inside it. When a task completes, the runtime reads the files it wrote:

- An output parameter is the file's text without its last newline, or its `default`.
- `outputs.result` is the standard output without its last newline.
- `outputs.exitCode` is the exit code of the main container.
- An output artifact is the file or directory. A later step that reads it, by `from` or by the same S3 key, gets that file.
- `globalName` makes an output a workflow output (`workflow.outputs.parameters.<name>`).

## 10. Mock runs

A mock run gives empty stdout and no files. Real workflows read that data, so the runtime has rules that keep the graph whole where Argo would stop or requeue. The placeholders of the first two rules apply only when `Launch.assume_inputs` is set, as the binary does for the mock executors (`Options.assume_inputs`); a real run needs the values, as `argo submit` does, and stops with Argo's message, like `spec.arguments.sample.value or spec.arguments.sample.valueFrom is required`.

- A workflow parameter without a value takes the first `enum` value, or a placeholder like `placeholder_sample_fastq`, with a warning. The same holds for the inputs of the entrypoint. A placeholder is one word, so that `when` reads it as a string.
- Only what a user gives when submitting gets a placeholder: workflow parameters, and the inputs and input artifacts of the entrypoint. A step or task that does not supply an input of the template it calls stays an error, because Argo rejects such a workflow when it is submitted (section 4.1). This matches the other drivers: a mock run assumes the data a user gives (`Launch.assume_inputs`, the placeholder inputs of the CWL graph test), not what the workflow itself must pass on.
- A `withParam` that is empty, still holds a tag or is a placeholder gives a loop without items, with a warning. So does a `withSequence` bound that is not known.
- An item that is empty text leaves the tags that read its fields in place.
- An output of a steps or DAG template that reads a skipped step, or a step whose output is empty, is empty, with a warning.
- A `when` that still holds a tag skips the step, with a warning.

A real executor gives real data, and these rules then do not apply.

## 11. Kubernetes fields

These fields are read, and left out with a warning: `volumes` and volume mounts (tasks share files only as artifacts), sidecars, init containers, `memoize`, `synchronization`, `podSpecPatch`, `envFrom`, and environment variables from Secrets or ConfigMaps. Warnings are printed to standard error once each, and `ArgoDriver::warnings` returns them.

In the corpus, the most common warnings are volumes that are not mounted (360 templates), environment variables from Secrets or ConfigMaps (141), resource templates that need a cluster (134) and workflow parameters that get a placeholder.

## 12. Deliberate differences from Argo

- Tags of `tasks.` and `steps.` that stay unresolved in a mock run stay literal. Argo would requeue the node until they resolve, or fail it.
- A `when` that cannot be decided skips the step instead of failing it.
- `suspend` never waits.
- Retries do not wait for their backoff.
- `workflow.failures` lists failed nodes in the order the run made them. Argo iterates a Go map, so its order varies.

## 13. The corpus

`just argo-corpus` fetches the 19 repositories of research question Q6 at the commits in `argo-parser/tests/fixtures/corpus/revisions.txt` into `~/.cache/iunctus/corpus/argo`, and argoproj-labs/argo-workflows-hello-executor-plugin, whose plugin the talk-demos examples of plugin templates install. gennext-workflows has no license: it is fetched for research only and never copied into this repository.

`just argo-corpus-test` runs the ignored test `argo-runtime/tests/corpus.rs`. It reads every Argo document of the corpus. A document with an entrypoint runs through `MockTes`; the others, like libraries of WorkflowTemplates and executor plugin ConfigMaps, get the checks of section 4.1. The outcomes are compared with `argo-runtime/tests/fixtures/corpus/outcomes.txt`: `ok` and the number of tasks, `valid`, or the exit code and the error (its end, when it is long). Every document that does not pass needs a line in `argo-runtime/tests/fixtures/corpus/rejected.txt` with the evidence that Argo rejects or fails it for the same reason, and the test fails on a document without evidence or on evidence for a document that passes.

Some documents get directories as configs, as `-c` gives them: Hera's copies of Argo's examples get Argo's `examples`, because Hera does not copy all the WorkflowTemplates they refer to; the talk-demos plugin examples get the hello executor plugin; and the talk-demos copy of pipekit's CI workflow gets the WorkflowTemplates of pipekit/argo-workflows-ci-example. `ARGO_CORPUS_UPDATE=1` records new outcomes, `ARGO_CORPUS_FILTER` runs only matching paths and `ARGO_CORPUS_VERBOSE=1` prints the warnings.

Results on 2026-09-29: 1493 of 1573 documents pass: 1285 run through with a full graph and 208 documents without an entrypoint pass Argo's checks. No document stops as unsupported.

| Repository | Pass | Documents |
|---|---:|---:|
| argoproj/argo-workflows | 555 | 571 |
| argoproj-labs/hera | 276 | 278 |
| GenivaInformatics/gennext-workflows | 147 | 168 |
| kubeflow/pipelines | 153 | 153 |
| Ensembl/regulation-workflows | 107 | 132 |
| codefresh-io/argo-hub | 104 | 106 |
| pipekit/talk-demos | 30 | 36 |
| AliyunContainerService/argo-workflow-examples | 20 | 20 |
| pipekit/argo-workflows-ci-example | 17 | 20 |
| couler-proj/couler | 18 | 19 |
| argoproj-labs/argo-workflows-catalog | 17 | 17 |
| DataCloud-project/SIM-PIPE | 11 | 11 |
| thoth-station/workflows | 9 | 11 |
| wwood/singlem-sra-processing | 9 | 9 |
| PGpalt/Hybrid-Argo-Workflows | 6 | 6 |
| terrytangyuan/distributed-ml-patterns | 6 | 6 |
| devops-ws/argo-workflows-guide | 3 | 3 |
| uc-cdis/argo-wrapper | 1 | 3 |
| argoproj-labs/argo-workflows-hello-executor-plugin | 2 | 2 |
| DonaldKellett/snakemake-tutorial-argo-workflows | 2 | 2 |

`argo lint` of Argo v4.1.4 rejects each of the 80 documents that do not pass, for the same reason:

| Reason | Documents |
|---|---:|
| A WorkflowTemplate that no document of the corpus defines, like gennext's `collect-qc-data-templates` or Ensembl's deprecated `*-submittable` templates | 29 |
| A template that is not there, or that has no type | 20 |
| A step or task that does not supply an input of the template it calls, among them Argo's expected failures | 11 |
| Names that are missing, invalid or not unique | 10 |
| Documents that are not valid YAML or do not fit Argo's types, among them Argo's malformed test files; one has a duplicate key, which `argo lint` and `argo submit` reject in their default strict mode | 9 |
| A tag that does not resolve (`{{steps.A...}}` inside a DAG) | 1 |

Some documents that pass are rejected by `argo lint --strict=false`, where iunctus is more lenient without a change to what runs: tags that name variables no template sets (19 documents; section 4.1 leaves this check out), CronWorkflows with the v3 field `schedule`, which iunctus converts as `argo convert` does (6), and references to WorkflowTemplates that Argo would refuse to create, like gennext's `copy-inputs-to-scratch` with its unfinished template that no workflow calls. With `--strict`, its default, `argo lint` also rejects unknown fields, which iunctus leaves out.

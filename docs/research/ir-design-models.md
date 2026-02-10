# IR and execution model designs in workflow systems (research notes for iunctus)

Research date: 2026-09-27. Method: source code read from shallow clones of each project's default branch (commit dates 2026-09-04 to 2026-09-27), plus official docs and papers. "Cited Findings" are checked facts with a source. "Inferences" are my own reasoning. "(search snippet only)" means I saw the claim only in a search result summary, not in the full page.

## 1. Cromwell WOM (Workflow Object Model): how it worked, how successful it was, its limits

### Takeaway
WOM was a real, working, language-neutral graph model. It had typed ports, call nodes, expression nodes, scatter and conditional nodes with inner graphs, and an opaque expression interface. It hosted several WDL versions well. Hosting CWL next to WDL did not last: CWL was removed after Cromwell 79 (2022), and Broad moved to running each language on its own native engine.

### Cited Findings
- WOM "is a directed acyclic graph that captures workflow inputs, outputs, calls, and the dependencies between them". Graph nodes inherit from `wom.graph.GraphNode`. WDL is first parsed into a WDL-specific object model (WDLOM). Then `WorkflowDefinitionElementToWomWorkflowDefinition` turns WDLOM elements into WOM nodes, links them and emits a `wom.graph.Graph`. - [Cromwell docs: What is WOM?](https://cromwell.readthedocs.io/en/latest/developers/bitesize/workflowParsing/whatIsWom/)
- Every `GraphNode` has `inputPorts`, `outputPorts` and a `completionPort`. Edges connect output ports to input ports. - [GraphNode.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/graph/GraphNode.scala)
- Call node kinds are `CommandCallNode`, `ExpressionCallNode` and `WorkflowCallNode`. - [CallNode.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/graph/CallNode.scala)
- `ScatterNode` holds an `innerGraph` (source comment: "Imagine that the contents of a WDL scatter block were a self-contained workflow. That's this Graph"), scatter variable nodes ("WDL uses exactly one, CWL >= 1"), gather output ports, a scatter processing function and a collection function builder. `ConditionalNode` also holds an inner graph. - [ScatterNode.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/graph/ScatterNode.scala), [ConditionalNode.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/graph/ConditionalNode.scala)
- Expressions are opaque to WOM. The `WomExpression` trait only requires `sourceString`, `cacheString` ("should not include references to memory locations or ephemeral, UUID-containing file paths"), `inputs`, `evaluateValue(inputValues, ioFunctionSet)`, `evaluateType(inputTypes)` and `evaluateFiles(...)`. Each language supplies its own implementation. - [WomExpression.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/expression/WomExpression.scala)
- The shared command task trait has fields that exist for one language's rules: `prefixSeparator`, `commandPartSeparator`, `adHocFileCreation`, `dockerOutputDirectory`, `additionalGlob`, and a custom output evaluation function. - [CommandTaskDefinition.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/callable/CommandTaskDefinition.scala)
- WOM source contains a re-implementation of cwltool's shell quoting ("`cwltool` invokes `shellescape.quote` to escape its arguments. This should reimplement that algorithm."). - [wom/package.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/package.scala)
- The file value model has several variants: `WomSingleFile`, `WomGlobFile`, `WomUnlistedDirectory`, `WomMaybePopulatedFile` (can carry secondary files) and `WomMaybeListedDirectory`. - [WomFile.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/values/WomFile.scala)
- Cromwell 30 ran WDL through WOM, supported several WDL versions and CWL 1.0, and allowed calling CWL tools from WDL and the reverse (search snippet only). - [WDL blog, Cromwell 30](https://software.broadinstitute.org/wdl/blog?id=11109)
- A 2021 preprint says WOM let users stitch "meta-workflows written in more than one language" (search snippet only). - [bioRxiv 2021.04.03.437906](https://www.biorxiv.org/content/10.1101/2021.04.03.437906v1.full)
- "Cromwell 79 is the last release with CWL. Support will be removed in Cromwell 80 and above." Cromwell 85 "removes the `cwl` top-level artifact". - [Cromwell CHANGELOG](https://github.com/broadinstitute/cromwell/blob/develop/CHANGELOG.md)
- Broad's stated reason (Terra blog, 2022-05-03): "we now believe it will be more productive for us to have Cromwell focus on supporting WDL as it continues to evolve and to adopt tools that are already widely used by the community to run workflows in those other languages." The replacement was an "app model" where each language runs on its own native engine. - [Terra blog](https://terra.bio/terras-roadmap-to-supporting-more-workflow-languages/)
- The request to document WOM and its WDL/CWL bindings (issue #3188) was closed without docs and labelled "CWL Discontinued". - [cromwell issue 3188](https://github.com/broadinstitute/cromwell/issues/3188)
- Current state: the latest Cromwell release tag is 92 ("Cromwell now fully supports WDL 1.1"). Notes for 93 are in progress on `develop` (last commit 2026-09-15). - [Cromwell CHANGELOG](https://github.com/broadinstitute/cromwell/blob/develop/CHANGELOG.md), [tags](https://github.com/broadinstitute/cromwell/tags)

### Inferences
- WOM worked well as the internal IR for one language family (WDL draft-2, 1.0, 1.1). These versions share scoping and evaluation rules, so one graph fits.
- CWL did not fit as cleanly. The evidence is that CWL-only concepts leaked into the shared task trait. Each CWL feature meant a WOM change plus backend work. For a small team, this cost was higher than the benefit.
- Worth copying: typed ports, inner graphs for scatter and conditional, gather ports, and the opaque expression interface (inputs, evaluate value, evaluate type, evaluate files, cache string).
- To avoid: one static DAG as the only execution model, and a shared task type that grows a field for each language.
- WOM was static. It could not host Nextflow (graph built at run time) or Snakemake (graph built backwards from target files).

### Gaps
- I found no talk or design document where Broad explains WOM trade-offs in its own words. Issue 3188 shows the docs were never written.
- I found no CWL conformance numbers for Cromwell.
- The full Cromwell 30 blog post could not be fetched (it redirects to a Terra support page).

## 2. Flyte IR (flyteidl for Flyte 1) and the Flyte 2 shift (flyteidl2)

### Takeaway
Flyte 1 has the most complete protobuf workflow IR in this survey. It has typed interfaces, literals with hashes, bindings with promises, several node kinds, and dynamic workflows that return a new graph at run time. Flyte 2 (GA announced 2026-08-04) dropped the static DAG for user code. It moved to a durable-execution model of "actions" with stored state and events.

### Cited Findings
- Flyte 1 `WorkflowTemplate` = id, metadata, `TypedInterface`, `repeated Node nodes`, output `Binding`s, an optional `failure_node`, and defaults. The comment calls it a "statically analyzable, directed acyclic graph". - [workflow.proto (master)](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto)
- `Node` = id, `NodeMetadata` (timeout, retries, interruptible, cacheable, cache_version, cache_serializable), input `Binding`s, `upstream_node_ids`, output aliases, and one target: `TaskNode`, `WorkflowNode`, `BranchNode`, `GateNode` or `ArrayNode`. - [workflow.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto)
- `BranchNode` is an if / else-if / else chain of `BooleanExpression` conditions that ends in an else node or an error. `TaskNodeOverrides` can change resources, image and pod template at execution time. - [workflow.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto)
- `ArrayNode` runs one sub-node for each list element, with `parallelism`, `min_successes` or `min_success_ratio`, and `run_all_sub_nodes`. - [workflow.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto)
- Failure policy: `FAIL_IMMEDIATELY` or `FAIL_AFTER_EXECUTABLE_NODES_COMPLETE`. - [workflow.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto)
- Dynamic workflows: a running task can return a `DynamicJobSpec` with `nodes`, `min_successes`, output bindings (which "should have the generated id for the subtask"), and the task and sub-workflow templates it uses. - [dynamic_job.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/dynamic_job.proto)
- Values: `Literal` = scalar, collection, map, or offloaded metadata, plus a `string hash` that "is used for caching purposes". `Scalar` covers primitive, blob (file or directory), binary, schema, none, error, structured dataset and union. `BindingData` is a scalar, collection, map, or `OutputReference promise`. - [literals.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/literals.proto)
- Tasks: `TaskTemplate` targets a `Container`, `K8sPod` or `Sql`. `Container` has image, command, args, resources, env and a data loading config. `TaskMetadata` has `discoverable` (cache on), `discovery_version`, `cache_ignore_input_vars`, retries and interruptible. - [tasks.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/tasks.proto)
- The `main` branch README says "Flyte 2 is now generally available". Flyte 1 is maintained on `master`. The latest tag is v2.0.50. - [Flyte README](https://github.com/flyteorg/flyte/blob/main/README.md), [tags](https://github.com/flyteorg/flyte/tags)
- Flyte 2 blog (2026-08-04): Flyte 1 had "Static workflows, compiled and type checked before they ran". Users pushed "toward dynamism". "Promises, dynamic workflows, our own conditionals: conceptually heavy abstractions users had to internalize." In Flyte 2, "Tasks simply call tasks. Loops, conditionals, try/except". On infrastructure failure, Flyte 2 "changes the resources it asks for on the retry, not just the code path it replays". - [Union blog](https://www.union.ai/blog-post/flyte-2-is-generally-available-the-durable-open-source-ai-runtime)
- flyteidl2 has no workflow graph message. It defines `Run` and `Action`. An action is a `TaskAction` (task spec, `cache_key`, queue), a `TraceAction` (tracks a step run by the local worker "to bring determinism to code that's otherwise not deterministic"), or a `ConditionAction` (waits for a typed external signal). - [run_definition.proto](https://github.com/flyteorg/flyte/blob/main/flyteidl2/workflow/run_definition.proto)
- A `StateService` stores per-action state and offers a `Watch` stream with "at-least-once delivery semantics". - [state_service.proto](https://github.com/flyteorg/flyte/blob/main/flyteidl2/workflow/state_service.proto)

### Inferences
- Flyte 1's IR is a strong template for the value layer (typed literals, blobs, hashes, offloading) and the task layer (retries, cache version, ignored cache inputs, overrides).
- Flyte's history shows the cost of forcing a static DAG onto dynamic user code. The dynamic parts became special node types that users found hard. Flyte 2 accepts a dynamic program and records its effects. iunctus has the same problem with Nextflow.
- `DynamicJobSpec` is a useful pattern: a running node returns a small new graph, and the engine merges it into the running execution.
- `TraceAction` is the Flyte 2 version of Temporal's recorded side effect (section 4). It is the tool for making a dynamic program replayable.

### Gaps
- The Flyte 1 dynamic workflow user docs could not be fetched (the docs site redirects to Flyte 2 migration pages).
- I found no numbers on Flyte 2 event volume, limits or replay cost.

## 3. Kubeflow Pipelines PipelineSpec and Argo Workflows

### Takeaway
KFP separates "what" (components and DAGs) from "how" (a deployment spec with executors). It also defines a per-task runtime contract (`ExecutorInput` and `ExecutorOutput`). It is a static DAG with iterators and trigger conditions. The KFP backend compiles it to Argo Workflows. Argo is itself a YAML IR with DAG templates, loops, memoization and retries.

### Cited Findings
- "The IR YAML is an intermediate representation of a compiled pipeline or component. It is an instance of the `PipelineSpec` protocol buffer message type." Sections: components, deployment_spec, root, pipeline_info, sdk_version, schema_version. The KFP backend compiles the IR YAML to Argo Workflow YAML. The same IR can be submitted to other backends such as Vertex AI Pipelines. - [KFP IR YAML docs](https://www.kubeflow.org/docs/components/pipelines/concepts/ir-yaml/)
- `ComponentSpec` has input and output definitions and is "Either a DAG or a single execution": `oneof implementation { DagSpec dag; string executor_label; }`. - [pipeline_spec.proto](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/pipeline_spec.proto)
- Inputs and outputs are split into parameters (JSON-like values) and artifacts (typed, with URIs and metadata). - [pipeline_spec.proto](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/pipeline_spec.proto)
- `PipelineTaskSpec` has `CachingOptions` (enable_cache, custom cache_key), `TriggerPolicy` (condition expression plus strategy), an iterator (`ArtifactIteratorSpec` or `ParameterIteratorSpec`) for fan-out, `RetryPolicy` ("Maps directly to Argo Workflows retryStrategy.retryPolicy") and `IteratorPolicy` (limit on concurrent sub-tasks). - [pipeline_spec.proto](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/pipeline_spec.proto)
- Runtime contract: `ExecutorInput` holds resolved input parameter values and artifacts plus the file paths where outputs must be written. The container writes an `ExecutorOutput` file. - [pipeline_spec.proto](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/pipeline_spec.proto)
- KFP cache key fields: input artifact names, input parameter values, output specs, and a container spec (image, cmdArgs, pvcNames, env). - [cache_key.proto](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/cache_key.proto)
- Latest KFP release tag: 2.17.2. - [KFP tags](https://github.com/kubeflow/pipelines/tags)
- Argo memoization: the key is a template expression (for example `{{inputs.parameters.message}}`). The cache is stored in Kubernetes ConfigMaps (1 MB limit). `maxAge` is optional. Since v3.5 "all steps can be memoized, whether or not they have outputs". - [Argo memoization docs](https://argo-workflows.readthedocs.io/en/latest/memoization/)
- Argo `retryStrategy`: `limit`, `retryPolicy` (Always, OnFailure, OnError, OnTransientError), `backoff`, and an `expression` over `lastRetry.exitCode`, `lastRetry.status`, `lastRetry.duration` and `lastRetry.message`. "If `expression` evaluates to false, the step will not be retried." - [Argo retries docs](https://argo-workflows.readthedocs.io/en/latest/retries/)

### Inferences
- The KFP split between the component graph and the deployment executors is the same split iunctus needs between the workflow layer and the task layer.
- `ExecutorInput` / `ExecutorOutput` is a clean per-task contract. TES has no such contract for values, so iunctus must define its own (section 11).
- Argo memoization (user-written keys, 1 MB ConfigMap store) is a weak cache model. It is a warning, not a model.
- The KFP IR shows that a static DAG with bounded fan-out (iterators) and trigger conditions covers CWL-like workflows well.

### Gaps
- I did not confirm whether Argo can change pod resources on retry. The retries page does not cover it.

## 4. Dynamic orchestrators and durable execution: Airflow, Prefect, Dagster, Temporal, Ray, Dask, Parsl

### Takeaway
Most modern orchestrators build the graph at run time. There are two styles. Style 1: a static graph with a bounded "map" expansion point (Airflow, Dagster). Style 2: no static graph at all, only futures plus a record of what happened (Prefect 3, Parsl, Ray, Dask, Temporal, Flyte 2). Temporal's event history is the clearest model for durable resume in style 2.

### Cited Findings
- Airflow 3.3.2: "Dynamic Task Mapping allows a way for a workflow to create a number of tasks at runtime based upon current data". `partial()` sets fixed arguments and `expand()` sets mapped arguments. "Right before a mapped task is executed the scheduler will create _n_ copies of the task". `[core] max_map_length` defaults to 1024. "Task-mapping in a mapped task group is not permitted". Each copy has a `map_index`. - [Airflow docs](https://airflow.apache.org/docs/apache-airflow/stable/authoring-and-scheduling/dynamic-task-mapping.html)
- Dagster: `DynamicOut` and `DynamicOutput` carry a `mapping_key`. "The ops downstream of a dynamic output will be cloned for each dynamic output, and identified using the associated mapping_key". `collect` creates a fan-in. - [Dagster docs](https://docs.dagster.io/guides/build/ops/dynamic-graphs)
- Prefect 3: flows are decorated Python functions with native control flow and no pre-built DAG. Tasks are "cacheable, retryable, have transactional semantics". - [Prefect docs](https://docs.prefect.io/v3/concepts/flows)
- Temporal: events are created "in response to external occurrences and Commands generated by a Workflow Execution". The history "enables the Workflow Execution to recover from a crash and continue making progress". A Side Effect "does not re-execute upon replay, but instead returns the recorded result". Warning after 10,240 events. Termination above 51,200 events. Continue-As-New closes the execution and starts a new one. - [Temporal docs](https://docs.temporal.io/workflow-execution/event)
- Ray (Wang et al., NSDI 2021): distributed futures with "ownership"; each object has an owner; lost objects are recovered by lineage reconstruction; about 1 ms per task. - [USENIX NSDI 2021](https://www.usenix.org/conference/nsdi21/presentation/cheng), [paper PDF](https://www.usenix.org/system/files/nsdi21-wang.pdf)
- Dask: "A Dask graph is a dictionary mapping keys to computation". The newer `Task` and `TaskRef` classes replace tuples because a tuple task "is not self-describing and heavily context dependent". The scheduler replaces references with computed values before calling the function. - [Dask graph spec](https://docs.dask.org/en/stable/spec.html)
- Parsl (Babuji et al., arXiv 1905.02158, 2019): apps return futures. Parsl builds "a dynamic dependency graph" at run time. Executors target low latency, high throughput or extreme scale. Reported scale: 250,000 workers on 8,000 nodes and about 1,200 tasks per second. - [arXiv](https://arxiv.org/abs/1905.02158)

### Inferences
- The key idea for iunctus comes from Temporal and Flyte 2: record every non-deterministic decision in an append-only log, and rebuild state from the log on resume. This is the general way to resume a dynamic program without re-running finished tasks.
- A map with a stable key per copy (Airflow `map_index`, Dagster `mapping_key`) is the right primitive for scatter. An index is fine for ordered collections (CWL, WDL). For unordered streams (Nextflow channels) the key should come from content, not from arrival order.
- Limits matter. Airflow caps a map at 1024 by default and Temporal caps history at 51,200 events. Genomics runs can have 100,000 or more tasks. iunctus needs event logs that scale (for example one log per sub-workflow, or snapshots).
- Ray's lineage idea (recompute lost results from the recorded producer) is the same as re-running a cached task whose outputs were deleted. Nextflow already requires both cache and work directory to resume (section 8).

### Gaps
- I did not verify the current versions of Prefect, Dagster, Ray, Dask or Parsl. The pages I read did not show them.
- Parsl memoization and checkpointing are not covered in the abstract I read.

## 5. Scientific engines: Pegasus, Makeflow/JX, Toil, miniwdl, sprocket

### Takeaway
Pegasus has the clearest two-level design. An abstract, resource-independent workflow is "planned" into an executable workflow by a fixed list of passes. Toil shows a generic dynamic job graph with promises, checkpoint jobs and a job store for resume. miniwdl and sprocket show how a WDL engine turns a static workflow into a run-time state machine that expands scatter and conditional sub-graphs. Sprocket is a Rust engine that already sends tasks to TES.

### Cited Findings
Pegasus
- Pegasus 5.1.3 docs: users give "a description of the abstract workflow in a YAML format", independent of where it runs. Three catalogs: replica, transformation and site. - [Pegasus intro](https://pegasus.isi.edu/documentation/user-guide/introduction.html)
- Planner passes: data reuse ("prune all the nodes in the abstract workflow for which the output files exist in the Replica Catalog", turned off with `--force`); site selection; job clustering ("usually done on short running jobs in order to reduce the remote execution overheads"); adding stage-in, inter-site, stage-out and registration nodes; directory creation and cleanup; and code generation (HTCondor DAG, shell script, or PMC). - [Pegasus mapping reference](https://pegasus.isi.edu/documentation/reference-guide/mapping.html)
- Paper: Deelman et al., "Pegasus, a workflow management system for science automation", Future Generation Computer Systems 46 (2015) 17-35. - [ScienceDirect](https://www.sciencedirect.com/science/article/abs/pii/S0167739X14002015)

Makeflow / JX
- A JX workflow is pure JSON, or JX that "can be evaluated to produce pure JSON". Rules have a `command` or a `workflow`, plus inputs, outputs and resources (cores, memory, disk, gpus, wall-time). Categories group rules with shared resources. - [cctools JX docs](https://cctools.readthedocs.io/en/latest/jx-workflow/jx/)

Toil
- Job graph: parent, child and follow-on jobs. Follow-ons run after the children and their successors finish. Jobs can create jobs at run time ("Static and dynamic creation of jobs can be mixed"). `rv()` returns a promise, "essentially a pointer" to a value filled in later. `PromisedRequirement` lets a job's resources depend on earlier results. - [Toil docs, developing.rst](https://github.com/DataBiosphere/toil/blob/master/docs/python/developing.rst)
- Checkpoint jobs: "A checkpoint job is rerun if one or more of its successors fails its retry attempts". Its successors are deleted and re-created. - [developing.rst](https://github.com/DataBiosphere/toil/blob/master/docs/python/developing.rst)
- The leader walks the job graph. "Each job is backed by a file in the job store, and atomic updates to this state are used to ensure the workflow can always be resumed upon failure." - [architecture.rst](https://github.com/DataBiosphere/toil/blob/master/docs/appendices/architecture.rst)
- Toil runs CWL and WDL by importing cwltool and miniwdl and mapping language constructs to its own `Job` subclasses: `CWLJob`, `CWLScatter`, `CWLGather`, `CWLLoop`, `CWLWorkflow` in cwltoil.py (5,442 lines), and `WDLTaskJob`, `WDLScatterJob`, `WDLConditionalJob`, `WDLWorkflowJob` in wdltoil.py (6,283 lines). Toil "inherits most of the functionality of MiniWDL". - [cwltoil.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/cwl/cwltoil.py), [wdltoil.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/wdl/wdltoil.py), [docs/wdl/developing.rst](https://github.com/DataBiosphere/toil/blob/master/docs/wdl/developing.rst)
- Latest Toil release tag 9.5.0; development version 9.6.0a1. - [Toil tags](https://github.com/DataBiosphere/toil/tags)
- Paper: Vivian et al., "Toil enables reproducible, open source, big biomedical data analyses", Nature Biotechnology 35:314-316 (2017). - [Nature](https://www.nature.com/articles/nbt.3772)

miniwdl
- Design notes (quoted from source): each Decl, Call, Scatter, Conditional and implicit Gather is a node that "advertises its dependencies". "Scatter sections contain a body, which provides a template for the job subgraph to be scheduled for each element of the runtime-evaluated scatter array." Nodes outside the scatter depend on Gather nodes, not on the body. A conditional launches zero or one instance of its body. - [WDL/runtime/workflow.py](https://github.com/chanzuckerberg/miniwdl/blob/main/WDL/runtime/workflow.py)
- `StateMachine`: "evaluates WDL expressions locally, while instructing the driver when to call tasks/subworkflows. It's agnostic to how/where the driver actually executes each call, just requiring asynchronous notification of call completion along with the outputs." - [_workflow_state.py](https://github.com/chanzuckerberg/miniwdl/blob/main/WDL/runtime/_workflow_state.py)
- Latest miniwdl tag: v1.15.0rc1. - [miniwdl tags](https://github.com/chanzuckerberg/miniwdl/tags)

sprocket (St. Jude, Rust)
- Workspace version 0.31.0, released 2026-09-16. Rust edition 2024. License "MIT OR Apache-2.0". Crates include wdl-grammar, wdl-ast, wdl-analysis, wdl-engine and wdl-lsp. - [Cargo.toml](https://github.com/stjude-rust-labs/sprocket/blob/main/Cargo.toml), [CHANGELOG](https://github.com/stjude-rust-labs/sprocket/blob/main/CHANGELOG.md)
- Workflow evaluation uses a petgraph `DiGraph` built by `wdl-analysis`. "Scatter and conditional statements introduce new subgraphs for evaluation", tracked by in-degree counts. - [eval/v1/workflow.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/eval/v1/workflow.rs)
- Backends: local, Docker, Apptainer, Slurm with Apptainer, LSF with Apptainer, and TES. The TES backend uses the `crankshaft` crate (version 0.11.0). - [backend directory](https://github.com/stjude-rust-labs/sprocket/tree/main/crates/wdl-engine/src/backend), [tes.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend/tes.rs)
- The backend request carries the command string, original inputs, backend inputs, requirements, hints, env, constraints and an attempt directory. The result carries the image actually used, exit code, work directory, stdout and stderr. - [backend.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend.rs)
- Call cache: the key is built from the document URI, backend name, task name and evaluated inputs. The evaluated command (with inputs replaced by content digests), requirements and hints are stored as digests to validate an entry. Bumping the cache entry version changes key derivation. - [cache.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/cache.rs)

### Inferences
- Pegasus's passes are a good template for the planning step between the workflow layer and the task layer in iunctus: reuse, placement, clustering, staging, cleanup.
- Clustering matters for TES. Nextflow and Snakemake workflows often have many short tasks. Each TES task pays for container start and file staging.
- miniwdl's StateMachine interface is the right shape for a per-language driver in iunctus: evaluate expressions locally, emit calls, receive completion events with outputs.
- sprocket and crankshaft can be reused or studied directly in Rust. crankshaft's own license should be checked.
- Toil shows that one runtime can host several languages if each language keeps its own evaluator. It also shows the cost: two translators of 5,000 to 6,000 lines, on top of reusing the reference parsers and evaluators.

### Gaps
- Pegasus failure recovery (rescue DAGs, retries) was not fetched.
- The Toil paper abstract (scale and cost numbers) could not be fetched (paywall and captcha).

## 6. Apache Beam model and portability framework

### Takeaway
Beam is a working multi-language IR. Several SDKs build one language-neutral protobuf pipeline. User code runs in a separate SDK harness per language. The cost is a second API (the Fn API) and one harness per language.

### Cited Findings
- Before portability, "each SDK-runner combination required non-trivial work on both sides". The Runner API is "a language-neutral pipeline representation with transformations specifying the execution environment as a docker container image". The SDK harness is "an SDK-provided program responsible for executing user code and is run separately from the runner". The Fn API "defines an execution-time binary contract between the SDK harness and the runner". Java, Python and Go SDKs support it. Runner support varies, for example "Python streaming mode is not yet supported on Spark". - [Beam portability](https://beam.apache.org/roadmap/portability/)

### Inferences
- Beam's split fits iunctus's problem with Nextflow Groovy closures and Snakemake Python input functions. The graph is shared. User code is opaque and runs in a language-specific evaluator. In iunctus that evaluator would run inside the Rust process, not in a container, but the boundary is the same.

### Gaps
- I found no cost figures (people, time) for Beam's portability work.

## 7. Build systems as models: Bazel, Buck2, REAPI, Nix

### Takeaway
Build systems give the best model for the task layer: a fully resolved, hashable action with content-addressed inputs and outputs, an action cache keyed by that hash, and a shared protocol (REAPI) used by many front ends. Their weak point for iunctus is dynamism. Bazel needs outputs declared before execution, and Buck2's dynamic dependencies are deliberately limited.

### Cited Findings
- Bazel phases: loading (evaluate BUILD files and extensions), analysis (rule implementations create the action graph; authors "must list explicitly which files will be generated before executing the actual commands"), and execution ("actions are executed, when at least one of their outputs is required"). An action describes "how to generate a set of outputs from a set of inputs". - [Bazel docs](https://bazel.build/extending/concepts)
- Buck2 dynamic dependencies use `ctx.actions.dynamic_output()` and "are restricted in their power compared to fully generic dynamic dependencies". The callback can read "dynamic" artifacts but must bind outputs declared up front: "The function must call `ctx.actions` (probably `ctx.actions.run`) to bind all outputs." - [Buck2 docs](https://buck2.build/docs/rule_authors/dynamic_dependencies/)
- REAPI `Action` = `command_digest`, `input_root_digest`, `timeout`, `do_not_cache`, `salt`, `platform`. `Command` = arguments, environment variables, `output_paths`, `working_directory`, output directory format. `ActionResult` = output files, symlinks, directories, exit code, stdout and stderr (raw or digest), and `ExecutedActionMetadata` (worker, queued, input fetch, execution and output upload timestamps). `salt` places an action "into a separate cache namespace", which "allows disowning an entire set of ActionResults that might have been poisoned". - [remote_execution.proto](https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto)
- REAPI "is intended primarily for use by build systems, such as Bazel, to distribute build and test actions through a worker pool". Clients include Bazel, Buck2, BuildStream, Pants, Please, recc and reclient. Servers include Buildbarn, Buildfarm, BuildGrid, BuildBuddy, EngFlow and NativeLink. The newest API version implemented by servers is 2.11.0. - [remote-apis README](https://github.com/bazelbuild/remote-apis)
- Nix 2.35.2 manual: a derivation is "a specification for running an executable on precisely defined input to produce one or more store objects". It has a name, inputs (deriving paths), outputs, system, builder, arguments and environment. Derivations can be input-addressed or content-addressed. The experimental `dynamic-derivations` feature lets a derivation output another derivation. - [Nix manual](https://nix.dev/manual/nix/latest/store/derivation/)
- Guix derivations have the same parts (outputs, inputs, system, builder and arguments, environment) and are stored as `.drv` files. "Derivations allow clients of the daemon to communicate build actions to the store." - [Guix manual](https://guix.gnu.org/manual/en/html_node/Derivations.html)

### Inferences
- The iunctus task layer should look like a REAPI `Action` plus `Command`, but with TES-style storage URLs instead of a content-addressed input tree only. The cache key should be a digest of the canonical task (command, environment, image digest, input content digests, output declarations), like the REAPI action digest.
- Nix dynamic derivations and Buck2 `dynamic_output` are the build-system answer to checkpoints: a finished action produces the next piece of graph. This answer is narrow (outputs declared up front) and still experimental in Nix. Snakemake checkpoints and Nextflow need more, because the number and names of downstream tasks are unknown until the data is seen.
- Early cutoff: if a re-run task produces byte-identical outputs, downstream cache keys (built from content digests) do not change, so downstream tasks are not re-run. This follows from content addressing. It is my inference; I did not verify it in a source here.
- REAPI is the strongest example in this survey of many front ends sharing one protocol, and that protocol is at the task level. This supports putting the "unified" part of iunctus at the task level.

### Gaps
- I did not study Bazel's Skyframe restart behaviour or newer REAPI features (the repo contains chunking test vectors, which suggests chunked CAS work).
- The Guix manual page did not say whether Guix's `.drv` format is identical to Nix's.

## 8. Dataflow theory: Kahn process networks, Nextflow channels, and program IR versus task DAG

### Takeaway
Nextflow is close to a Kahn process network: processes and operators connected by FIFO channels, where each process invocation produces one task. Some Nextflow operators (for example `mix`) are not Kahn-determinate, so item order depends on timing. A program IR (a network of processes and channels) and a task DAG (concrete invocations) are different objects. The task DAG is an unfolding of the program over time.

### Cited Findings
- Kahn (1974): a process may "wait until something on an input line" or "send"; "processes communicate via first-in first-out (fifo) queues"; a process "cannot be waiting on data coming from one or another of its input lines", and no two processes send on the same channel; a process is "a function from the histories of its input line into the histories of its output lines"; the meaning of a network is the least fixpoint of continuous functions; the model "can produce only determinate programs". Published in IFIP Congress 1974, pp. 471-475. - [Kahn 1974 PDF](https://perso.ensta.fr/~chapoutot/various/kahn_networks.pdf), [dblp](https://dblp.org/rec/conf/ifip/Kahn74.html)
- Nextflow: "Data flows from process to process through asynchronous dataflow structures, known as channels and values. The data dependencies between these processes implicitly determines the flow of execution." A channel is "an asynchronous sequence of values". - [overview.mdx](https://github.com/nextflow-io/nextflow/blob/master/docs/overview.mdx), [channel.mdx](https://github.com/nextflow-io/nextflow/blob/master/docs/reference/stdlib-types/channel.mdx)
- Nextflow `mix`: "The items in the mixed output channel may appear in any order". - [operator.mdx](https://github.com/nextflow-io/nextflow/blob/master/docs/reference/operator.mdx)
- Nextflow's internal DAG has vertex types PROCESS, OPERATOR, ORIGIN and NODE, with channels as edges. It is a graph of the program, not of tasks. - [DAG.groovy](https://github.com/nextflow-io/nextflow/blob/master/modules/nextflow/src/main/groovy/nextflow/dag/DAG.groovy)
- Nextflow still depends on the GPars 1.2.1 dataflow library. - [build.gradle](https://github.com/nextflow-io/nextflow/blob/master/modules/nextflow/build.gradle)
- Nextflow 26.04 adds static typing and records. Only a core subset of operators is recommended for typed workflows. Latest tag: v26.09.1-edge. - [static-types-operators.mdx](https://github.com/nextflow-io/nextflow/blob/master/docs/tutorials/static-types-operators.mdx), [tags](https://github.com/nextflow-io/nextflow/tags)
- Nextflow's cache docs warn that non-deterministic inputs (race conditions in operator closures, merging inputs without matching keys) invalidate the cache. - [cache-and-resume](https://docs.seqera.io/nextflow/cache-and-resume)
- CWL v1.2: "A workflow step can only begin execution when every data link connected to a step has been fulfilled." Scatter methods are dotproduct, nested_crossproduct and flat_crossproduct. Conditionals use `when`, and `pickValue` (first_non_null, the_only_non_null, all_non_null) merges branch results. - [CWL v1.2 Workflow spec](https://www.commonwl.org/v1.2/Workflow.html)
- Snakemake (docs for 9.27.0): checkpoints let Snakemake "re-evaluate the DAG after the successful execution of every job spawned from a checkpoint". `checkpoints.<name>.get(...)` "throws `snakemake.exceptions.IncompleteCheckpointException` if the checkpoint has not yet been executed for these particular wildcard value(s)". Inside an input function this "leads to a re-evaluation after the checkpoint has been successfully passed". Available "From Snakemake 5.4 on". - [rules.rst](https://github.com/snakemake/snakemake/blob/main/docs/snakefiles/rules.rst)

### Inferences
- The three models are three ways of unfolding into a task DAG:
  - CWL (and WDL) is a dataflow graph where each edge carries exactly one value per run (a "dataflow value" in Nextflow terms). Scatter turns one node into N copies. It is a Kahn network where every channel carries one item.
  - Nextflow is a Kahn-style network where edges carry streams. Each complete input tuple that reaches a process makes one task. Operators are cheap in-engine nodes.
  - Snakemake is not forward dataflow. It is goal-directed: a set of rule templates plus target files. The job DAG is derived by matching file patterns backwards from targets. Checkpoints are points where derivation must pause until data exists.
- So a static DAG IR cannot express all three. A "dataflow program" IR can express CWL and Nextflow. Snakemake needs a planner node (rule set plus targets) that emits jobs and re-plans when a checkpoint finishes.
- To resume Nextflow runs correctly, iunctus should key each task by its input content (as Nextflow already does), so that arrival order does not matter. Logging channel order is the fallback for operators whose output depends on order.

### Gaps
- I found no study that classifies every Nextflow operator as determinate or not in the Kahn sense. This needs an operator-by-operator review.

## 9. How systems handle the cross-cutting concerns

### Takeaway
Every system surveyed solves the same eight problems in a few recurring ways. The best patterns for iunctus are: map with stable keys, conditionals as zero-or-one inner graphs, typed values with file references that carry digests, content-derived cache keys that exclude resources, attempt-aware resource expressions, an append-only event record, and export of provenance to Workflow Run RO-Crate.

### Cited Findings
Dynamic graph expansion
- A running task returns a new graph: Flyte 1 `DynamicJobSpec`. - [dynamic_job.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/dynamic_job.proto)
- Copies created at run time from upstream data: Airflow `expand()`, Dagster `DynamicOutput`. - [Airflow](https://airflow.apache.org/docs/apache-airflow/stable/authoring-and-scheduling/dynamic-task-mapping.html), [Dagster](https://docs.dagster.io/guides/build/ops/dynamic-graphs)
- Jobs create jobs: Toil. Template sub-graphs per scatter element: miniwdl. Disjoint sub-graphs with in-degree tracking: sprocket. - [Toil](https://github.com/DataBiosphere/toil/blob/master/docs/python/developing.rst), [miniwdl](https://github.com/chanzuckerberg/miniwdl/blob/main/WDL/runtime/workflow.py), [sprocket](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/eval/v1/workflow.rs)
- Re-planning after data exists: Snakemake checkpoints, Buck2 `dynamic_output`, Nix dynamic derivations (experimental). - [Snakemake](https://github.com/snakemake/snakemake/blob/main/docs/snakefiles/rules.rst), [Buck2](https://buck2.build/docs/rule_authors/dynamic_dependencies/), [Nix](https://nix.dev/manual/nix/latest/store/derivation/)
- No graph, only a recorded program: Temporal, Flyte 2, Prefect 3, Parsl, Ray. - [Temporal](https://docs.temporal.io/workflow-execution/event), [Flyte 2](https://github.com/flyteorg/flyte/blob/main/flyteidl2/workflow/run_definition.proto), [Prefect](https://docs.prefect.io/v3/concepts/flows), [Parsl](https://arxiv.org/abs/1905.02158), [Ray](https://www.usenix.org/conference/nsdi21/presentation/cheng)

Scatter and gather
- WOM `ScatterNode` (inner graph, several scatter variables, gather ports). miniwdl Gather nodes. Flyte `ArrayNode` (parallelism, min_success_ratio). KFP iterators with `IteratorPolicy`. CWL scatter methods. Toil `CWLScatter` and `CWLGather`. - [ScatterNode.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/graph/ScatterNode.scala), [miniwdl](https://github.com/chanzuckerberg/miniwdl/blob/main/WDL/runtime/workflow.py), [Flyte workflow.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto), [KFP](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/pipeline_spec.proto), [CWL](https://www.commonwl.org/v1.2/Workflow.html), [cwltoil.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/cwl/cwltoil.py)

Conditionals
- WOM `ConditionalNode` (inner graph). miniwdl: zero or one body instance. Flyte `BranchNode` (if / else-if / else, else may be an error). KFP `TriggerPolicy` condition. CWL `when` plus `pickValue`. Snakemake: data-dependent choice through input functions and checkpoints. - [ConditionalNode.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/graph/ConditionalNode.scala), [miniwdl](https://github.com/chanzuckerberg/miniwdl/blob/main/WDL/runtime/workflow.py), [Flyte](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto), [KFP](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/pipeline_spec.proto), [CWL](https://www.commonwl.org/v1.2/Workflow.html), [Snakemake](https://github.com/snakemake/snakemake/blob/main/docs/snakefiles/rules.rst)

Typed values versus files
- Flyte `Literal` with `Blob` and a hash, plus offloading of large literals. KFP parameters versus artifacts. WOM `WomValue` with five `WomFile` variants. Nextflow channels carry values and paths. TES moves only files and directories (an input may also carry inline `content`). - [literals.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/literals.proto), [KFP](https://github.com/kubeflow/pipelines/blob/master/api/v2alpha1/pipeline_spec.proto), [WomFile.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/values/WomFile.scala), [Nextflow overview](https://github.com/nextflow-io/nextflow/blob/master/docs/overview.mdx), [TES OpenAPI](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)

Content-addressed caching and resume
- Nextflow task hash inputs: session ID, task name, container image, environment modules, Conda, Spack and CPU architecture, task inputs, eval commands (new in 25.10), script, global variables used in the script, `ext` properties used in the script (new in 23.10), bundled scripts, and stub-run flag. It also has "an incrementing component" to find an unused work directory. An input file is hashed by "full path, last modified timestamp, and file size"; `lenient` mode drops the timestamp. Resume needs both the task cache (`.nextflow/cache`, LevelDB per session) and the work directory. - [Nextflow cache-and-resume](https://docs.seqera.io/nextflow/cache-and-resume), [source mdx](https://github.com/nextflow-io/nextflow/blob/master/docs/cache-and-resume.mdx)
- sprocket, KFP and Flyte cache keys: see sections 2, 3 and 5. REAPI action cache with `salt` namespaces: section 7. Pegasus data reuse through the replica catalog: section 5. Argo memoization with user keys in ConfigMaps: section 3.
- Toil resumes from its job store with atomic per-job state files. Temporal resumes by replaying event history. - [Toil architecture](https://github.com/DataBiosphere/toil/blob/master/docs/appendices/architecture.rst), [Temporal](https://docs.temporal.io/workflow-execution/event)

Retries with changed resources
- Nextflow: `memory { 2.GB * task.attempt }` with `errorStrategy` retry on exit codes 137 to 140; also `task.previousTrace.memory * 2`. - [process.mdx](https://github.com/nextflow-io/nextflow/blob/master/docs/process.mdx)
- Snakemake: resource callables may take `attempt` ("adjust resources based on how often the job has been restarted"). - [rules.rst](https://github.com/snakemake/snakemake/blob/main/docs/snakefiles/rules.rst)
- Toil `PromisedRequirement`; Flyte 2 "changes the resources it asks for on the retry"; Flyte 1 `TaskNodeOverrides`; Argo retry `expression` over the last attempt. - [Toil](https://github.com/DataBiosphere/toil/blob/master/docs/python/developing.rst), [Flyte 2 blog](https://www.union.ai/blog-post/flyte-2-is-generally-available-the-durable-open-source-ai-runtime), [Flyte workflow.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/workflow.proto), [Argo](https://argo-workflows.readthedocs.io/en/latest/retries/)

Provenance
- REAPI `ExecutedActionMetadata` timestamps per phase. Temporal event history. Flyte 2 action events and phase transitions. Pegasus "keeps track of what has been done (provenance)". - [REAPI proto](https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto), [Temporal](https://docs.temporal.io/workflow-execution/event), [Flyte 2](https://github.com/flyteorg/flyte/blob/main/flyteidl2/workflow/run_definition.proto), [Pegasus](https://pegasus.isi.edu/documentation/user-guide/introduction.html)
- Workflow Run RO-Crate (on RO-Crate 1.3) captures "the provenance of the execution of computational workflows at different levels of granularity". Implementations listed: Galaxy (23.1.1+), Nextflow via nf-prov (1.4.0+), COMPSs (3.4+), StreamFlow, WfExS, Sapporo, Autosubmit and runcrate. Snakemake is not listed. - [Workflow Run RO-Crate](https://www.researchobject.org/workflow-run-crate/)

Failure recovery
- Flyte fail-fast versus finish-what-you-can policy, and partial success for arrays. Toil checkpoint jobs re-create their sub-tree. Ray lineage reconstruction. Temporal replay. Nextflow resume from cache plus work directory. Pegasus prunes jobs whose outputs exist when re-planning. - sources as cited in sections 2, 4, 5 and 8.

### Inferences
- A shared failure policy vocabulary for iunctus can be small: fail fast, finish independent work, and allow partial success for maps (with a threshold).
- Nextflow's default file hash (path, size, mtime) is cheap but not content-addressed. A global cross-run cache in iunctus needs real content digests, with the Nextflow-style metadata hash as an optional fast path.

### Gaps
- Snakemake's rerun triggers (mtime, code, params, input changes) and its metadata store were not researched here.

## 10. Which systems host several front-end languages on one IR, and what did it cost?

### Takeaway
Success came in two cases. Case 1: the front ends were designed for the IR (Beam SDKs, KFP SDK, Flyte SDKs). Case 2: the shared layer is at the task or action level (REAPI, Nix and Guix derivations, and TES itself). Hosting independently specified workflow languages on one workflow-level graph (Cromwell WOM with WDL and CWL) was abandoned. Toil hosts CWL and WDL, but through two separate translators onto a shared job runtime, not through a shared language-neutral workflow IR.

### Cited Findings
- Cromwell hosted WDL and CWL on WOM. CWL was removed after release 79. Broad chose to run other languages on "tools that are already widely used by the community". - [Cromwell CHANGELOG](https://github.com/broadinstitute/cromwell/blob/develop/CHANGELOG.md), [Terra blog](https://terra.bio/terras-roadmap-to-supporting-more-workflow-languages/)
- WOM's shared task trait carries fields for one language's rules. - [CommandTaskDefinition.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/callable/CommandTaskDefinition.scala)
- Toil maps CWL (5,442-line module on top of cwltool) and WDL (6,283-line module on top of miniwdl) to its own job classes. - [cwltoil.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/cwl/cwltoil.py), [wdltoil.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/wdl/wdltoil.py)
- Beam: Java, Python and Go SDKs share the Runner API; this needed the Fn API and an SDK harness per language; runner support still varies. - [Beam portability](https://beam.apache.org/roadmap/portability/)
- KFP IR runs on the KFP backend (through Argo) and on other backends such as Vertex AI Pipelines. - [KFP IR YAML](https://www.kubeflow.org/docs/components/pipelines/concepts/ir-yaml/)
- REAPI: at least seven client build tools and six server implementations share one action protocol. - [remote-apis README](https://github.com/bazelbuild/remote-apis)
- Nix and Guix both use derivations (builder, arguments, environment, inputs, outputs, system). - [Nix manual](https://nix.dev/manual/nix/latest/store/derivation/), [Guix manual](https://guix.gnu.org/manual/en/html_node/Derivations.html)
- Flyte (single front-end language family) dropped its static workflow IR for user code in Flyte 2 because it was "too hard to learn". - [Union blog](https://www.union.ai/blog-post/flyte-2-is-generally-available-the-durable-open-source-ai-runtime)

### Inferences
- The cost of a language-neutral workflow-level IR grows with the semantic distance between languages. WDL and CWL are both static, typed DAG languages, and they still did not live together in WOM for long. Nextflow and Snakemake are much further apart.
- Lesson for iunctus: do not aim for one workflow-level graph that all three languages compile into. Share four things instead: (1) a value model, (2) a task model, (3) a runtime (scheduler, event log, cache, executors), and (4) a thin interface for expanding work that each language driver implements. Keep language semantics inside the driver.
- Cross-language calls (for example, a CWL tool called from a Nextflow pipeline) remain possible at the task layer, if each driver can import a callable with a typed interface. WOM showed this is attractive. Broad still dropped it, which suggests it was not a main user need.

### Gaps
- I found no usage data on how many Cromwell users mixed WDL and CWL. I found no team-size or effort numbers for WOM, Toil's translators, or Beam portability.

## 11. Design recommendations for iunctus: a two-layer IR

### Takeaway
Workflow layer (a): one "workflow program" per language, held behind a common driver interface, with a small shared vocabulary (typed ports, callables, map, conditional, gather, sub-workflow, stream operator, planner with checkpoints, opaque expressions). Task layer (b): a fully resolved, hashable task record that maps one-to-one to a TES task, plus iunctus-side identity, cache key, attempt and provenance. Dynamic expansion: an event-driven loop. The driver consumes completion events and emits new concrete tasks. Every decision goes into an append-only log so a run can resume.

### Cited Findings
- TES 1.1.0 (OpenAPI on `develop`): a `tesTask` has name, description, `inputs`, `outputs`, `resources`, `executors`, `volumes`, `tags`, and read-only id, state, logs and creation time. `tesInput` = name, description, url, path, type, content, streamable. `tesOutput` = name, description, url, path, path_prefix, type. `tesResources` = cpu_cores, preemptible, ram_gb, disk_gb, zones, backend_parameters, backend_parameters_strict. `tesExecutor` = image, command, workdir, stdin, stdout, stderr, env, ignore_error. Output file logs return url, path and size_bytes, with no checksum. There is no retry field, no GPU field and no time limit field. - [TES OpenAPI](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- TES v1.1 release notes: added `backend_parameters`, "preempted" and CANCELING states, wildcards in output paths, `ignore_error`, `streamable`, and task list filters. The releases page summary gives v1.1 as 2024-11-15 and v1.0 as 2024-02-12 (dates not cross-checked). - [TES releases](https://github.com/ga4gh/task-execution-schemas/releases)
- The driver interface shape: miniwdl's `StateMachine` "evaluates WDL expressions locally, while instructing the driver when to call tasks/subworkflows", needing only "asynchronous notification of call completion along with the outputs". - [_workflow_state.py](https://github.com/chanzuckerberg/miniwdl/blob/main/WDL/runtime/_workflow_state.py)
- Opaque expressions: WOM `WomExpression` (inputs, evaluate value, evaluate type, evaluate files, cache string). - [WomExpression.scala](https://github.com/broadinstitute/cromwell/blob/develop/wom/src/main/scala/wom/expression/WomExpression.scala)
- Hashable action: REAPI `Action` and `Command`, `salt` for cache namespaces, `ExecutedActionMetadata` for timings. - [remote_execution.proto](https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto)
- Planning passes: Pegasus data reuse, site selection, clustering, staging, cleanup. - [Pegasus mapping](https://pegasus.isi.edu/documentation/reference-guide/mapping.html)
- Recording non-determinism: Temporal side effects "return the recorded result" on replay; Flyte 2 `TraceAction`. - [Temporal](https://docs.temporal.io/workflow-execution/event), [run_definition.proto](https://github.com/flyteorg/flyte/blob/main/flyteidl2/workflow/run_definition.proto)
- Resources excluded from the key: sprocket keeps requirements and hints out of the key and uses them only to validate entries; Flyte has `cache_ignore_input_vars`. - [sprocket cache.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/cache.rs), [Flyte tasks.proto](https://github.com/flyteorg/flyte/blob/master/flyteidl/protos/flyteidl/core/tasks.proto)

### Inferences
The recommendations below are my own design reasoning, based on the findings above.

(a) Workflow layer: what it should contain
- Shape. One program object per language, plus a common driver trait. Do not compile all three languages into one static graph. Each driver owns its language rules. The shared runtime only sees the driver trait, values, and task records.
- Shared vocabulary (used by all drivers where it fits, so tools like visualisation, validation and planning can be shared):
  - Callable (task template): typed input and output ports; a command template (opaque, language-specific); resource expressions that may read inputs and the attempt number; container and environment expressions; output collection rules (paths, globs, optional outputs, secondary files, stdout or stderr as values).
  - Node kinds: Call; Expression (opaque evaluator with the WOM-style interface: declared inputs, evaluate value, evaluate type, list files, cache string); Map or Scatter (inner body plus a method: dot product, nested cross product, flat cross product, or "one per stream item"); Conditional (inner body, zero or one instance, plus a merge rule like CWL `pickValue`); Gather or Collect (with an explicit ordering rule); Sub-workflow (a nested driver); Stream operator (Nextflow operators, each flagged determinate or not); Planner (Snakemake rule set plus targets, with checkpoint nodes that force re-planning).
  - Edges: typed, and of two kinds. A single-value edge (a dataflow value) is used by CWL and WDL. A stream edge (a channel) is used by Nextflow.
- Values: one value model for all languages. JSON-like typed values (null, bool, int, float, string, list, map or record, optional, union), plus File and Directory references. A file reference holds a location URI, a content digest when known, size, basename, and optional secondary files (CWL). Large values can be stored out of line and referenced by digest (as in Flyte's offloaded literals).
- How each language maps (sketch):
  - CWL: static program. The whole graph is known at load time, so iunctus can type-check and show a plan before running. Uses Call, Expression, Map, Conditional, Gather and Sub-workflow with single-value edges only.
  - Nextflow: only the network of processes, operators and channels is known at load time. Tasks appear at run time. Uses Call (process), Stream operator, and stream and value edges. Operators run inside the driver, in-process.
  - Snakemake: a Planner node holds rules and targets. The initial job DAG is known after backward planning, until the first checkpoint. Each rule is a Callable. Jobs are identified by rule plus wildcard values.

(b) Task layer: what it should contain, and how it maps to TES
- Split each concrete task into three parts:
  - Hashed specification (defines "what is computed"): container image (resolved to a digest where possible); one or more executor steps (argv, working directory, environment, stdin, stdout and stderr paths); inputs (container path, file or directory, content digest and size, source URI, or inline content); outputs (container path or path prefix for globs, file or directory, required or optional, and the output port it feeds); volumes; and the language driver name and version.
  - Placement, not hashed (defines "where and how big"): cpu_cores, ram_gb, disk_gb, preemptible, zones, backend_parameters (for GPUs, time limits, queues), output destination URLs, tags, and the chosen backend.
  - Attempt record, not hashed (defines "what happened"): stable task identity, cache key, attempt number, TES task id, state, executor logs and exit codes, output digests and sizes, timings.
- TES mapping is almost one-to-one: inputs become `tesInput` (url, path, type, content, streamable); outputs become `tesOutput` (url, path, path_prefix, type); placement becomes `tesResources`; executor steps become `tesExecutor` entries; volumes map directly. Put the iunctus task identity, cache key and attempt number into `tags`.
- iunctus must do what TES does not: retries (submit a new TES task for each attempt), output checksums (for example an extra final executor step that writes a digest manifest, or reading storage metadata), time limits and GPUs (through `backend_parameters`), and value outputs (read small output files such as stdout or a JSON result back into the value model, similar to KFP's `ExecutorOutput`).
- Cache key: a digest of the canonical hashed specification only. Placement and attempt number stay out, so a retry with more memory keeps the same key. Add a salt or namespace field (as REAPI does) to invalidate poisoned results. Hash file inputs by content. Offer the Nextflow-style metadata hash (path, size, mtime) as an optional fast path. Leave the session ID out of the key so the cache works across runs. Note that this differs from Nextflow's own resume behaviour.
- Planning passes between the layers (after Pegasus): cache lookup (data reuse), storage placement and staging, clustering of very short tasks into one TES task with several executors (TES executors run one after another and share the task's resources), and cleanup of intermediate files that no pending task needs.

How dynamic graph expansion should work
- Event-driven loop. The runtime holds the current set of pending, running and finished tasks. Each driver implements two operations: "start" returns the first task requests, and "on event" takes a completion event (outputs as values, or a failure) and returns new task requests or "done". This follows miniwdl's state machine design.
- Map and scatter: the driver creates one copy of the body when the collection value is known. Each copy gets a stable identity: the index for ordered collections (CWL, WDL), and a content-derived key for stream items (Nextflow).
- Nextflow: each process node keeps input queues. When a complete input tuple is available, the driver emits one task. Stream operators run in the driver. Task identity comes from the task's content hash, so arrival order does not change identity.
- Snakemake: the planner works backwards from targets over rule templates and produces a job DAG. Jobs whose inputs depend on checkpoint outputs are marked as waiting. When a checkpoint job finishes, the planner runs again for the affected targets. New jobs are matched to existing ones by rule plus wildcards. Unchanged jobs are kept.
- Sub-workflows run as nested driver instances with their own identity prefix (like Flyte `DynamicJobSpec` or WOM `WorkflowCallNode`).
- Append-only event log: record task submitted, task finished (outputs with digests), expansion decisions (scatter length, checkpoint results, planner output), and any other non-deterministic choice a driver makes (the Temporal side-effect and Flyte `TraceAction` idea). On resume, replay the log to rebuild driver state, reuse finished tasks through their cache keys, and resubmit the rest. Plan for large logs from the start (one log per sub-workflow, or periodic snapshots), since Temporal stops at 51,200 events.
- Retries: a failure event goes back to the driver. The driver re-evaluates resource expressions with attempt plus one (Nextflow `task.attempt`, Snakemake `attempt`) and emits a new attempt with the same cache key.
- Failure policy: support fail fast, finish independent work, and partial success for maps with a threshold (as in Flyte).

Provenance
- Store per task: hashed specification digest, inputs and outputs with digests, image digest, TES task id, executor logs, and timings (like REAPI `ExecutedActionMetadata`). Export runs as Workflow Run RO-Crate (Provenance Run Crate), which Nextflow (nf-prov) and several CWL engines already produce.

What not to do
- Do not build a WOM-style single static DAG as the only IR.
- Do not add language-specific fields to the shared task specification. Keep them in the driver, or in opaque extension data that does not change execution.
- Do not rely on TES for retries, caching, output digests or time limits.

### Gaps
- Evaluating Nextflow Groovy code and Snakemake Python code inside a Rust process is outside this note, but it decides how much of the workflow layer is feasible.
- A cross-run global cache differs from Nextflow's session-scoped resume. User expectations for this should be checked.
- Support for TES 1.1 features (output wildcards, `streamable`, `backend_parameters`) in real TES servers (for example Funnel, TESK) was not tested here.
- The TES release dates above come from a page summary and were not cross-checked.

<!-- Implementer specification of Nextflow configuration semantics (strict syntax) for the iunctus config evaluator, checked against the Nextflow source. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Nextflow configuration semantics

This document says how Nextflow reads, merges and applies its configuration. It is written for the people who build the config evaluator of iunctus. It covers the strict syntax only (the v2 parser), because iunctus supports only that syntax ([decisions](../decisions.md), item 5).

It resolves an open point of the [Nextflow research note](nextflow.md): "The exact selector priority and merge semantics were not verified in `ConfigBuilder`/`ProcessConfig` source code." They are verified here.

## Sources and method

- **Nextflow source.** Checkout at commit `bbf6ff0` (25 Sep 2026). Its `VERSION` file says `26.09.1-edge`. All source paths are relative to the root of that checkout, and line numbers refer to that commit.
- **Nextflow docs.** The `docs/` folder of the same checkout.
- **Experiments.** Small configs and pipelines were run with the Nextflow 26.04.6 binary on the research machine (build 12646, OpenJDK 21), with `NXF_OFFLINE=true`. Experiments 1 to 18 used an empty `NXF_HOME`. Experiments 19 to 33 were added in a review pass. They used an `NXF_HOME` that held only the framework jar and no `config` file. All are listed in [Appendix A](#appendix-a-experiments). This binary is older than the checkout. Where the two could differ, the text says so.
- **Groovy library code.** Groovy itself is not in the checkout. Where a rule depends on Groovy library code (`ConfigObject.merge`, `String.isInteger()`, `String.isFloat()`, `String as int`), it was checked by running small Groovy scripts with the Groovy 4.0.31 runtime that ships inside the 26.04.6 jar (experiment 32).
- **Corpus.** The text-only nf-core corpus: 144 pipelines at the commits in `revisions.txt`, plus the nf-core/modules repository. Counts come from `grep`, a small scanning script and `nextflow lint` (26.04.6).
- **Strict parser** means the v2 parser. It is the default: `NF.getSyntaxParserVersion()` returns `NXF_SYNTAX_PARSER` or else `v2` (`modules/nextflow/src/main/groovy/nextflow/NF.groovy:31-37`).
- Anything that was not confirmed in source code or by an experiment is marked **unverified**. Statements marked "derived from code" were read in the source but not run.

### Source files

The rest of the document cites files by name. This table gives the full paths.

| File name used below | Path in the checkout |
| --- | --- |
| `ConfigParser.g4`, `ConfigLexer.g4` | `modules/nf-lang/src/main/antlr/` |
| `ConfigAstBuilder.java` | `modules/nf-lang/src/main/java/nextflow/config/parser/ConfigAstBuilder.java` |
| config `VariableScopeVisitor.java` | `modules/nf-lang/src/main/java/nextflow/config/control/VariableScopeVisitor.java` |
| `ConfigResolveVisitor.java`, `ResolveIncludeVisitor.java`, `ConfigToGroovyVisitor.java` | `modules/nf-lang/src/main/java/nextflow/config/control/` |
| nf-lang `ConfigDsl.java` | `modules/nf-lang/src/main/java/nextflow/config/dsl/ConfigDsl.java` |
| nf-lang `PluginsDsl.java` | `modules/nf-lang/src/main/java/nextflow/config/scopes/PluginsDsl.java` |
| `ScriptDsl.java`, `ProcessDsl.java` | `modules/nf-lang/src/main/java/nextflow/script/dsl/` |
| `ResolveVisitor.java`, `GStringToStringVisitor.java`, `ScriptToGroovyVisitor.java` | `modules/nf-lang/src/main/java/nextflow/script/control/` |
| `ConfigCompiler.java`, `ConfigParserV2.groovy`, `ConfigDsl.groovy` | `modules/nextflow/src/main/groovy/nextflow/config/parser/v2/` |
| `ConfigBuilder.groovy`, `ConfigValidator.groovy`, `ConfigMap.groovy`, `Manifest.groovy` | `modules/nextflow/src/main/groovy/nextflow/config/` |
| `ConfigHelper.groovy` | `modules/nextflow/src/main/groovy/nextflow/util/ConfigHelper.groovy` |
| `ConfigCmdAdapter.groovy` | `modules/nf-cli-v1/src/main/groovy/nextflow/config/ConfigCmdAdapter.groovy` |
| `CliOptions.groovy`, `CmdRun.groovy`, `CmdConfig.groovy`, `Launcher.groovy` | `modules/nf-cli-v1/src/main/groovy/nextflow/cli/` |
| `Bolts.groovy` | `modules/nf-commons/src/main/nextflow/extension/Bolts.groovy` |
| `Const.groovy` | `modules/nf-commons/src/main/nextflow/Const.groovy` |
| `MemoryUnit.groovy`, `Duration.groovy` | `modules/nf-commons/src/main/nextflow/util/` |
| `NumberDelegatingMetaClass.java` | `modules/nextflow/src/main/java/groovy/runtime/metaclass/NumberDelegatingMetaClass.java` |
| `Session.groovy` | `modules/nextflow/src/main/groovy/nextflow/Session.groovy` |
| `ScriptBinding.groovy`, `BaseScript.groovy`, `ParamsDsl.groovy`, `ParamsHelper.groovy`, `ProcessDef.groovy`, `BindableDef.groovy`, `ProcessConfig.groovy`, `WorkflowMetadata.groovy` | `modules/nextflow/src/main/groovy/nextflow/script/` |
| `ProcessBuilder.groovy`, `ProcessConfigBuilder.groovy`, `ConfigSelectorResolver.groovy` | `modules/nextflow/src/main/groovy/nextflow/script/dsl/` |
| `TaskConfig.groovy` (also holds `LazyMap`), `TaskContext.groovy`, `TaskProcessor.groovy`, `TaskRun.groovy`, `PublishDir.groovy` | `modules/nextflow/src/main/groovy/nextflow/processor/` |
| `ExecutorConfig.groovy`, `ExecutorFactory.groovy`, `BashWrapperBuilder.groovy` | `modules/nextflow/src/main/groovy/nextflow/executor/` |
| `ContainerHandler.groovy` | `modules/nextflow/src/main/groovy/nextflow/container/ContainerHandler.groovy` |
| `TraceHelper.groovy` | `modules/nextflow/src/main/groovy/nextflow/trace/TraceHelper.groovy` |
| `AssetManager.groovy` | `modules/nextflow/src/main/groovy/nextflow/scm/AssetManager.groovy` |
| `ConfigBuilderTest.groovy`, `TaskConfigTest.groovy` | `modules/nextflow/src/test/groovy/nextflow/config/`, `.../processor/` |

## 1. Rules at a glance

1. Config files are read in this order, lowest priority first: `$NXF_HOME/config`, `nextflow.config` in the project directory, `nextflow.config` in the launch directory, then every `-c` file. `-C` replaces this whole list.
2. Each file is evaluated on its own into a tree of nested maps. The trees are merged in file order. Nested maps merge key by key. Any other value (number, string, list, set, closure) is replaced.
3. Inside one file, statements run from top to bottom. A later assignment replaces an earlier one. A map value assigned in the same file replaces the old map. It does not merge.
4. `includeConfig` evaluates another file in the context of the current one. The included file sees the params defined so far. Its tree is deep-merged into the current tree at that point.
5. A `profiles` block runs the selected profiles at its own position, in the order given by `-profile`. Statements after the `profiles` block override values that profiles set.
6. A top-level config file sees the command-line params (including the params file) and its own earlier params. It does not see params from other top-level config files.
7. A config assignment `params.x = v` is replaced by the command-line or params-file value of `x`, if there is one. For use inside the config, a command-line string is converted to the type of `v` (boolean or number). Any decimal number becomes a 32-bit `Float`.
8. At run time, legacy params are the merged config params, overlaid with the raw command-line values (strings). A script assignment `params.x = v` only fills names that are still missing. A typed `params {}` block converts values to the declared types.
9. Process settings are applied when a process is invoked. The order is: matching `withLabel` selectors, then matching `withName` selectors for the definition name, the alias and the fully qualified name, then the plain `process` settings. A plain setting is used when the process has no value for it, or when the current value equals the built-in default of that directive. The current value includes values set by selectors.
10. Selector patterns are Java regular expressions that must match the whole name. A leading `!` negates the pattern.
11. Closures in the `process` scope are stored unevaluated. They are evaluated per task, each time the directive is read, with the task inputs, `task` and the script variables in scope.
12. `cpus`, `memory`, `time` and `disk` are reduced to `resourceLimits` whenever they are read.
13. When a task fails and the error strategy is `retry`, a copy of the task runs with `task.attempt` increased, and all dynamic directives are evaluated again.
14. A `plugins { }` block only has an effect at the top level of a file. Inside a profile or any other block it is dropped without a message.

## 2. Configuration sources

### 2.1 Which files are read, and in what order

`ConfigCmdAdapter.resolveConfigFiles` builds the list (`ConfigCmdAdapter.groovy:165-222`). Lowest priority first:

| Order | Source | When it is read |
| --- | --- | --- |
| 1 | `$NXF_HOME/config`. `NXF_HOME` defaults to `~/.nextflow` (`Const.groovy:47-66`). | If the file exists (`ConfigCmdAdapter.groovy:178-182`). |
| 2 | `nextflow.config` in the project directory, which is the directory of the main script. The name is fixed. | If it exists and the project directory is not the launch directory (`ConfigCmdAdapter.groovy:189-195`). |
| 3 | `nextflow.config` in the launch directory. `NXF_CONFIG_FILE` can change the name. | If it exists and is not the same path as file 2 (`ConfigCmdAdapter.groovy:200-206`). |
| 4 | Files given with the launcher option `-c`/`-config`, then files given with `-c` after `run` (a hidden option). | Always. Relative paths are resolved against the launch directory (`ConfigCmdAdapter.groovy:207-219`; `CliOptions.groovy:45-46`; `CmdRun.groovy:234-235`). |
| 5 | Text snippets built from `-cluster.<name>=<value>` and `-executor.<name>=<value>`. | If such options are given (`ConfigCmdAdapter.groovy:273-297`). |

More rules:

- `-C <files>` replaces entries 1 to 4. The `-c` files are then ignored without a message (`ConfigCmdAdapter.groovy:167-171`; experiment 1). `-C` exists only as a launcher option, before the command name (`CliOptions.groovy:51-52`).
- `-c` and `-C` accept comma-separated lists (experiment 1) and can be repeated. The docs say the same (`docs/cli.mdx:272-294`). The comma split is done by the JCommander library, whose code is not in the checkout.
- A `-c` or `-C` file that does not exist aborts the run with "The specified configuration file does not exist" (`ConfigCmdAdapter.groovy:230-238`).
- The docs give the same order (`docs/config.mdx:10-21`). Experiment 1 confirms that the home config has the lowest priority.
- The snippets of entry 5 trim each value and quote it with single quotes, unless it is `true`, `false` or a number. An empty value gives an empty right-hand side (`ConfigCmdAdapter.groovy:306-317`). The snippets go through the same strict parser.

### 2.2 How the file trees are merged

`ConfigBuilder.build` creates one parser and evaluates each entry in turn (`ConfigBuilder.groovy:174-237`). Each entry produces its own tree. The tree is merged into the result with Groovy's `ConfigObject.merge` (`ConfigBuilder.groovy:248-262`). Groovy's library code is not in the checkout. The behaviour below was confirmed by experiments 13 and 18 and by running `ConfigObject.merge` directly with Groovy 4.0.31 (experiment 32):

- If both sides hold a map under the same key, the maps are merged recursively.
- Otherwise the later value replaces the earlier one. This applies to strings, numbers, lists, sets and closures.
- One detail: if the earlier map is empty, it is replaced by the later map object instead of being merged into. The resulting values are the same.
- A key that already exists keeps its position in the map. New keys are appended at the end.

Before the merge, `Bolts.toConfigObject` turns every nested `Map` into a nested config map (`ConfigParserV2.groovy:157`; `Bolts.groovy:585-598`). This includes map literals such as `[cpus: 4]`. Maps inside lists are not converted. So map-valued options deep-merge across files:

- File A sets `process.resourceLimits = [cpus: 8, memory: 1.GB]` and file B sets `process.resourceLimits = [cpus: 4]`. The result is `[cpus: 4, memory: 1.GB]` (experiment 13).
- Lists replace. `process.shell` from B replaces the one from A (experiment 13).
- `plugins` is a set, not a map (`ConfigDsl.groovy:330-345`), so a later file replaces it. This is derived from code only. It was not run, because plugin loading needs the network (**unverified** in practice).

Key order matters for selectors (section 6.5, experiment 18).

### 2.3 Each top-level file is evaluated in isolation

For every top-level entry, the parser starts the file's `params` map with the command-line params only. These include the params file (`ConfigParserV2.groovy:138-157`; `CmdRun.groovy:727-750`). The `configParams` field that would carry earlier params is set only for included files (`ConfigDsl.groovy:259-268`). So:

- A `-c` file that reads `params.outdir` gets `null` when `outdir` is defined only in the pipeline's `nextflow.config`. Experiment 1 shows `"${params.outdir}/x"` becoming `'null/x'`.
- If `--outdir` is given on the command line, every file sees it.

Included files are different. They see the params of the including file (section 3.5).

A quirk, derived from code and **unverified** by experiment: the parser deep-copies the command-line params once (`ConfigParserV2.groovy:104-110`), but each top-level file receives only a shallow copy of that map (`ConfigDsl.groovy:87-90`). A nested command-line param such as `--a.b 1` therefore puts the same inner map into every file. A config write below it, such as `params.a.c = 2`, changes that shared map, so later top-level files start with it. The final params are not affected, because they are rebuilt from the original command-line map (section 5.5). iunctus should not copy this.

### 2.4 Variables bound while parsing

`ConfigBuilder.getConfigVars` binds `baseDir`, `projectDir`, `launchDir`, `outputDir` and `secrets` (`ConfigBuilder.groovy:155-164`):

- `projectDir` and `baseDir` are the directory of the main script (`CmdRun.groovy:343-345`). For `nextflow config` without a project argument it is `.` (`CmdConfig.groovy:86-88`).
- `launchDir` is the real path of the current directory (`Path.of('.').toRealPath()`).
- `outputDir` is always `results` below the launch directory. It does not follow `-output-dir`.
- Only the legacy parser also binds all environment variables (`ConfigBuilder.groovy:200-203`). The strict parser does not.

The strict parser only lets a config refer to the names declared in nf-lang `ConfigDsl.java` (section 3.6). So `outputDir` is bound but cannot be referenced (experiment 11).

### 2.5 Environment variables given on the command line

`-e.NAME=value` and `-E` (export the whole launch environment) fill `env` of the result before any config file is merged (`ConfigCmdAdapter.groovy:260-271`; `ConfigBuilder.groovy:186-193`). `-e` values override `-E` values. The config files are merged over both. So `env.FOO = 'config'` in a config file wins over `-e.FOO=cli` (experiment 15).

### 2.6 Steps after all files are merged

1. **Profile check.** If `-profile` was given, every name in it must be declared in some parsed file or evaluated include. Otherwise the run aborts with "Unknown configuration profile" and suggestions (`ConfigBuilder.groovy:224-227, 343-363`; experiment 3). Nothing is checked when `-profile` is exactly `standard`. In a list, `standard` is checked like any other name, so `-profile standard,foo` fails if no `standard` profile exists (experiment 25).
2. **Top scopes.** `env`, `executor`, `params` and `process` always exist in the result (`ConfigBuilder.groovy:232-236`).
3. **Command-line overrides.** For `nextflow run` only, `ConfigCmdAdapter.configRunOptions` writes run options into the config (`ConfigCmdAdapter.groovy:328-560`). The ones that matter for nf-core:
   - `workDir`: `-w`, else the config value, else `NXF_WORK`, else `work` (`:355-359`).
   - `-output-dir` sets `outputDir` (`:342-343`). `-plugins` replaces `plugins` with the comma-split list (`:351-352`). `-resume` sets `resume` (`:403-404`).
   - `-process.<directive>=<value>` sets a plain `process` setting. It is only a default, so selectors and process definitions still win (`:372-374`; experiment 15). The value is converted by `ConfigHelper.parseValue`: `true`/`false` (any case) become booleans, then integer, long and double text become numbers, then duration text such as `2h` becomes a `Duration`. Anything else stays a string, so `-process.memory=8GB` stays the string `'8GB'` (`ConfigHelper.groovy:42-58`).
   - `-with-docker [image]`, `-with-singularity`, `-with-apptainer`, `-with-podman` and `-with-charliecloud` set `<engine>.enabled = true`. With an image name they also set `process.container`. Without an image name, `<engine>.image` from the config is used for `process.container` if it is set. If the engine scope exists but is not a map, the run aborts with "Invalid `<engine>` definition in the config file" (`:533-551, 571-591`). `-without-docker` sets `docker.enabled = false`, but only if a `docker` scope exists (`:527-531`).
   - `-with-trace`, `-with-report`, `-with-timeline` and `-with-dag` set `enabled = true` and optionally `file`.
   - `-queue-size` sets `executor.queueSize` (`:418-419`). `-with-conda` sets `conda.enabled = true`, and with a value also `process.conda` (`:382-387`).
   - The command-line params are merged into `params` (section 5.5).
4. **Validation.** Under the strict parser, `ConfigValidator` warns about unknown options. It never fails (`CmdRun.groovy:411-412`; `ConfigValidator.groovy:107-168`). `params` is not checked. Scopes of core plugins (`aws`, `azure`, `google`, `k8s`, `tower`, `wave`, `cloudcache`) are skipped when the plugin is not loaded. `env.NXF_*` gets a warning, except `NXF_DEBUG` (`:238-241`). The warning says the variable is ignored. `TaskProcessor.getProcessEnvironment` does not filter `NXF_` names (`TaskProcessor.groovy:1559-1566`), so whether such a variable still reaches tasks is **unverified**.
5. **Secrets.** The config is first built with a secrets provider that returns empty strings. If any secret was read, the config is built a second time after plugins are loaded (`CmdRun.groovy:347-405`).

### 2.7 The manifest read for finding the main script

When the pipeline argument is a directory or a repository, the main script comes from the manifest. `-main-script` overrides it (`CmdRun.groovy:270-271, 619-622`). When the argument is a script file, that file is used directly.

To read the manifest, `AssetManager.getManifest0` parses the project's `nextflow.config` text alone (`AssetManager.groovy:610-636`):

- includes are ignored and the missing-property error is turned off;
- no command-line params and no bound variables are given, so `projectDir` and similar names read as `null`;
- no profile list is given, so profiles are stored, not applied (see section 4.5);
- only the `manifest` scope is read. This gives `manifest.mainScript` (default `main.nf`, `Manifest.groovy:163`) and related values;
- if parsing fails, there is only a warning and an empty manifest is used.

## 3. Strict config syntax

### 3.1 Statements

The grammar is `ConfigParser.g4:98-176`:

- A file is a list of statements separated by newlines or `;` (`compilationUnit`, `sep` at `:579`).
- Top-level statements: `configInclude`, `configAssign`, `configBlock`, `configApplyBlock`. Two more rules only exist to give errors: `configIncomplete` and `invalidStatement`.
- Inside a block, `configSelector` is also allowed.
- A name part (`configPrimary`) is an identifier, a string literal or a built-in primitive type name such as `int` (`:128-132, 495-497`).
- Assignment: `a.b.c = expression` (`:120-126`). Newlines are allowed around `=`.
- Block: `name { statements }` (`:135-137`).
- Selector: `kind: target { statements }` (`:149-152`). `kind` must be an `Identifier` token, which cannot start with an upper-case letter (`ConfigLexer.g4:759-765`). `target` is a `configPrimary`. The docs say targets with special characters such as `-`, `:`, `!` or `*`, and targets that are keywords or built-in type names, must be quoted (`docs/config.mdx:223-225`). The grammar is a little looser: it accepts built-in primitive type names unquoted.
- Apply block: `name { method args ... }` (`:154-160`), for example `plugins { id 'nf-schema@2.5.1' }`.
- Values are expressions. The expression rules are copied from the script grammar (`ConfigParser.g4:180-580`). The config lexer is the script lexer without the script keywords, plus `includeConfig` (`ConfigLexer.g4:343`). `$` counts as a letter in identifiers, so `executor.$local.cpus` is valid (`ConfigLexer.g4:773-776`).

### 3.2 Evaluation model

The compiler turns the file into calls on a DSL object (`ConfigToGroovyVisitor.java`; `ConfigCompiler.java:161-184`). The calls run in file order:

| Syntax | Call | Behaviour (`ConfigDsl.groovy`) |
| --- | --- | --- |
| `a.b.c = v` | `assign(['a','b','c'], v)` | Creates missing maps for `a` and `a.b`, then puts `c = v`. A later write replaces the value (`:150-202`). |
| `a { ... }` | `block('a', closure)` | Runs the body with the scope prefix `a` (`:204-235`). |
| `withName: X { ... }` | `withName('X', closure)` | Runs the body with the scope `process` and `withName:X` (`:352-366`). |
| `includeConfig e` | `includeConfig(e)` | See section 3.5 (`:243-296`). |
| `plugins { id 'p' }` | `block('plugins', closure)` | Sets the top-level `plugins` to the set of ids (`:330-345`). Only at the top level (section 3.4). |

Consequences:

- Within one file, a map assignment replaces the previous map (experiment 13: `process.resourceLimits` assigned twice in one file keeps only the second map).
- The same block name can appear many times. The bodies write into the same map.
- The same selector key can appear many times. The bodies merge into one map.
- A block or selector with no assignment in it creates no key. So `withName: FOO { }` leaves no `withName:FOO` entry (experiment 24).
- Writing below a value that is not a map fails. For example `process.ext = 'x'` followed by `process.ext.args = 'y'` gives "Unable to parse config file" (`ConfigDsl.groovy:195-202`; experiment 31).
- `params` is special in `assign` (section 5.4).

### 3.3 Selectors

- Selectors are only accepted in the `process` scope (and the new `agent` scope, which nf-core does not use). `SELECTOR_SCOPES` is at `ConfigDsl.groovy:45`. A selector anywhere else raises "Config selectors are only allowed in the `process` and `agent` scopes (offending scope: ...)" (`ConfigDsl.groovy:314-320`; experiment 11 shows the older wording of 26.04.6).
- A selector inside a selector raises the same error, because the inner scope is no longer `process` (derived from `ConfigDsl.groovy:217-235`).
- Inside a profile, `process { withName: X { ... } }` works. During a run, the profile body runs at the top level of the file, so `process` is found directly (`ConfigDsl.groovy:394-402`). The rule that computes the scope relative to `profiles.<name>` (`ConfigDsl.groovy:221-226`) is only needed for `nextflow config -show-profiles` (section 4.5).
- The body is stored as a nested map under the key `withLabel:<target>` or `withName:<target>`. The target text is stored as written. It is trimmed only when it is matched (section 6.5).
- A kind other than `withName` or `withLabel` passes the compiler. It has no DSL method, so it fails when the file runs. The run aborts with "Unable to parse config file: '<path>'", caused by a `MissingMethodException` ("No signature of method: ... withFoo() is applicable for argument types: (String, ...)") (`ConfigToGroovyVisitor.java:110-111`; `ConfigBuilder.groovy:216-220`; experiment 19).

### 3.4 Apply blocks (`plugins`)

- Only `plugins` may be an apply block. Any other name gives "Config settings must be assigned with an equals sign (`=`)" (`ConfigAstBuilder.java:253-261`; experiment 11).
- The only method is `id(String)` (nf-lang `PluginsDsl.java`).
- The result is an ordered set of plugin ids under the top-level key `plugins`. Each top-level `plugins` block replaces the set. It does not add to it (`ConfigDsl.groovy:330-345`).
- A `plugins` block inside a profile, or inside any other block, is accepted by the parser and by the checker, but it is dropped without a message. `ConfigToGroovyVisitor.transformConfigBlock` only converts assignments, blocks and includes inside a block (`ConfigToGroovyVisitor.java:99-112`). Experiment 20 shows that `profiles { p { plugins { id '...' } } }` with `-profile p` gives no `plugins` entry. No nf-core Nextflow config puts `plugins` inside a block (the only indented `plugins` blocks in the corpus are in `nf-test.config` files, which are not Nextflow configs).

### 3.5 `includeConfig`

- **Syntax.** `includeConfig <expression>`. The expression is evaluated when the statement runs. It can be a string, an interpolated string or a ternary. It can also be an immediately invoked closure, `includeConfig ({ ... }())` (`docs/strict-syntax.mdx:652-666`). A bare closure that is not invoked is passed as a closure and fails, because the DSL method takes a string (derived from `ConfigToGroovyVisitor.java:119-121` and `ConfigDsl.groovy:243-247`, not run).
- **Where.** Only at the top level of a file or directly inside a named profile (config `VariableScopeVisitor.java:199-205`; `docs/config.mdx:94`; experiment 9). Anywhere else, including directly inside `profiles { }`, the compiler reports "Config includes are only allowed at the top-level or in a profile" (experiment 21). The run-time check in `ConfigDsl.groovy:388-391` is never reached for this case.
- **Path resolution.** The string becomes a path with `FileHelper.asPath`. A relative path is resolved against the directory of the including file (`resolveSibling`, `ConfigDsl.groovy:253-257`; `docs/config.mdx:91`; experiment 8). URLs such as `https://...` are used as they are. Remote includes were not run (**unverified** in practice).
- **Check before running.** If the argument is a constant string that points to a local file, the file must exist. Otherwise the compiler reports "Invalid include source: '<path>'". This happens even inside a profile that is not selected (`ResolveIncludeVisitor.java:65-90`; experiment 8). It also happens with `-config-ignore-includes`, because the check is part of compiling (`ConfigCompiler.java:165-171`; experiment 23). Computed paths are only checked when they run. A missing file then fails the run with "Unable to parse config file: '<top-level file>'" (`ConfigBuilder.groovy:216-220`). The cause is "Config file does not exist: <uri>" (`ConfigDsl.groovy:285-291`; experiment 10).
- **Including nothing.** `/dev/null` is the documented way to include nothing (`docs/strict-syntax.mdx:666`).
- **Evaluation.** The file text is read once per path and cached for the whole process (`@Memoized`, `ConfigDsl.groovy:283-295`). The text is evaluated again at every include. A new parser evaluates it (`ConfigDsl.groovy:259-269`) with:
  - the same bound variables, the command-line params and the same profile list;
  - a shallow copy of the including file's current `params` as the starting params. So the included file sees the params defined before the include, and not the ones after it (`docs/config.mdx:142`; experiment 8).
- **Merging.** The included tree is deep-merged into the current scope with `Bolts.deepMerge` (`ConfigDsl.groovy:271-274`; `Bolts.groovy:940-953`). During a run, the current scope is the top level, also for an include inside a selected profile. Maps merge. Other values are replaced by the included value. Existing keys keep their position and new keys are appended. Profiles and params declared in the included file are added to the declared sets.
- **Switching off.** `-config-ignore-includes` skips the evaluation of all includes (`CliOptions.groovy:48-49`; `ConfigDsl.groovy:250-251`). See the compile check above.
- An expression that evaluates to `null` or an empty string fails at `assert includeFile` (`ConfigDsl.groovy:248`). This runs before the ignore check, so it fails even with `-config-ignore-includes`. The error is a Groovy assertion error. It is not wrapped in "Unable to parse config file", because `ConfigBuilder` only wraps `Exception` (`ConfigBuilder.groovy:209-221`). The run prints "assert includeFile" with the value (experiment 22).

### 3.6 Names available in config expressions

Outside closures, a config expression may use only these names (nf-lang `ConfigDsl.java:30-72`):

| Name | Meaning |
| --- | --- |
| `params` | The live params map of the current file (`ConfigDsl.groovy:123-125`). A missing key gives `null` (experiment 1). |
| `projectDir` | Directory of the main script. |
| `baseDir` | Deprecated alias of `projectDir`. |
| `launchDir` | Directory where the run was launched. |
| `secrets` | Map of pipeline secrets. |
| `env(name)` | Value of a variable in the launch environment, or `null` (`ConfigDsl.groovy:239-241`). |

Classes can also be used:

- The standard types by simple name, for example `Path`, `List`, `Map`, `Set`, `String`, `Integer`, `Float`, `Boolean` (`ResolveVisitor.java:64-76, 149-172`).
- By simple name from `java.lang`, `java.util`, `java.io`, `java.net`, `groovy.lang` and `groovy.util`, plus `BigInteger` and `BigDecimal` (`ResolveVisitor.java:217-262`). For example, `System.getenv('X')` works (experiment 11).
- By fully qualified name, for example `new java.util.Date()`.
- `Duration` and `MemoryUnit` from `nextflow.util` are imported by default (`ConfigCompiler.java:58-61`).

Any other name is a compile error "`X` is not defined". Names that match `[A-Z_]+[A-Z0-9_]*` get the hint "(hint: use `env('...')` to access environment variable)" (`ConfigResolveVisitor.java:80-100`; experiment 11). The error hits:

- other config settings, such as `process.cpus` or `manifest.version` (`docs/strict-syntax.mdx:682-698`);
- environment variables written as bare names, such as `$HOME`;
- `workDir` and `outputDir`. Note that `docs/config.mdx:109` lists `workDir` as a config constant, but experiment 11 shows the error.

Inside closures, more names are allowed (config `VariableScopeVisitor.java:115-181, 311-348`):

- If the closure is the direct value of a config assignment, the script DSL names are known in it. If the assignment is also in the `process` or `agent` scope, or is `executor.jobName`, the process DSL names are known too. Together they include `task`, `workflow`, `nextflow`, `moduleDir`, `workDir`, `log` and `channel` (`ScriptDsl.java:42-107`; `ProcessDsl.java:36-45`).
- In those process-scope closures, in closures inside map values in that scope, and in `executor.jobName`, **any** undeclared name is accepted. It may be a task input such as `meta`.
- Other closures, for example a closure passed to `collect` in a `params` value, do not get these extra names (derived from code).

### 3.7 Values

- **Strings.** Interpolated strings are converted to plain strings when they are evaluated (`GStringToStringVisitor.java`, applied in `ConfigCompiler.java:183`). Outside closures, interpolation therefore happens while the config is parsed. A dynamic setting needs a closure (`docs/process.mdx:1221`; `docs/migrations/25-10.mdx:251`).
- **Unit literals.** `2.GB` and `4.h` come from a property hook on numbers (`NumberDelegatingMetaClass.java:37-53`). The property name is first lower-cased and looked up in the duration units. If that fails, it is upper-cased and looked up in the memory units. So `2.gb`, `2.Gb` and `2.GB` all work, `1.m` is one minute, not one megabyte, and `2.G` fails because `G` is not a memory unit.
  - Duration units are exactly `ms`, `milli`, `millis`, `s`, `sec`, `second`, `seconds`, `m`, `min`, `minute`, `minutes`, `h`, `hour`, `hours`, `d`, `day`, `days` (`Duration.groovy:47-66`). `mins`, `secs` and `hrs` are not units.
  - Memory units are `B`, `KB`, `MB`, `GB`, `TB`, `PB`, `EB`, `ZB` (`MemoryUnit.groovy:39`). They are powers of 1024.
- **Unit strings.**
  - Memory strings must match `([0-9\.]+)\s*(\S)?B?` as a whole (`MemoryUnit.groovy:37, 71-97`). The one-letter unit is upper-cased, but the trailing `B` must be upper case. So `'15.GB'`, `'16G'`, `'8g'` and `'8 GB'` work, while `'8 gb'`, `'8gb'` and `'8Gb'` fail. A value without a unit must be a whole number of bytes (experiment 32).
  - Duration strings accept one or more `<number><unit>` parts, such as `'1h 30m'` or `'1.5h'`, or the form `hh:mm:ss` (`Duration.groovy:43-45, 102-195`). The units are case-sensitive in strings: `'2H'` fails (experiment 32).
  - The tests use strings such as `'24h'` and `'16G'` in `resourceLimits` (`TaskConfigTest.groovy:230-300`).
- **Map literals** such as `[path: ..., mode: ...]` become nested maps when the file is finished (section 2.2).

### 3.8 What the strict parser rejects

| Construct | Result | Source |
| --- | --- | --- |
| `if`/`else` outside a closure | "If statements cannot be mixed with config statements" | `ConfigAstBuilder.java:348-359`; experiment 11 |
| `try`/`catch` outside a closure | "Try-catch blocks cannot be mixed with config statements" | same |
| `def x = ...` outside a closure | "Variable declarations cannot be mixed with config statements" | same; experiment 11 |
| Function definitions (`def check_max(obj, type) { ... }`) | Syntax error "Unexpected input: '('" | grammar has no rule; corpus lint (experiment 33) |
| `name.path` without `= value` | "Incomplete statement" | `ConfigAstBuilder.java:342-346` |
| `foo { bar 'x' }` (apply block other than `plugins`) | "Config settings must be assigned with an equals sign (`=`)" | `ConfigAstBuilder.java:253-261`; experiment 11 |
| `includeConfig` inside any block other than a named profile, including directly in `profiles { }` | "Config includes are only allowed at the top-level or in a profile" (compile time) | config `VariableScopeVisitor.java:199-205`; experiments 9 and 21 |
| Constant include path to a missing local file | "Invalid include source" (compile time, even with `-config-ignore-includes`) | `ResolveIncludeVisitor.java:65-76`; experiments 8 and 23 |
| Undeclared names (config settings, env vars, `workDir`, `outputDir`) | "`X` is not defined" | `ConfigResolveVisitor.java:80-100`; experiment 11 |
| Selector outside `process`/`agent` | error when the file runs | `ConfigDsl.groovy:314-320` |
| Selector kind other than `withName`/`withLabel` | error when the file runs | experiment 19 |
| Assignment directly in `profiles { }` | "Only profile blocks are allowed in the `profiles` scope" (when the file runs) | `ConfigDsl.groovy:377-380`; experiment 30 |
| `import` | not in the config lexer | `ConfigLexer.g4:302` (commented out) |
| `for`, `while`, `switch` | not in the statement grammar, even in closures | `ConfigParser.g4:180-191` |
| Implicit closure parameter `it` | warning only | config `VariableScopeVisitor.java:342-344` |
| `workflow.onComplete`/`onError` in config | warning only (deprecated) | config `VariableScopeVisitor.java:122-123` |

Statements such as `if`, `try` and local variables are fine inside closures.

## 4. Profiles

### 4.1 Definition

- A `profiles { }` block holds named blocks: `profiles { test { ... } docker { ... } }`. Anything else directly inside it is an error (`ConfigDsl.groovy:368-392`; section 3.8).
- A profile body may hold assignments, blocks, `process` blocks with selectors and `includeConfig`. A `plugins` block inside a profile is dropped (section 3.4).
- If the same name appears twice in one `profiles` block, the last body wins and the first is lost (`blocks[name] = closure`, `ConfigDsl.groovy:382-386`; experiment 3).
- Profiles may be defined in any config file, in `-c` files and in included files (experiment 3).

### 4.2 Activation

- `-profile a,b` (`CmdRun.groovy:240-241`) is split on commas with Groovy `tokenize(',')`. Empty items are dropped. Spaces are not trimmed, so `' a, b'` fails (`ConfigBuilder.groovy:255`; experiment 4).
- Without `-profile`, the list is `['standard']` (`ConfigBuilder.groovy:39, 47, 97-101`). A profile named `standard` is therefore applied by default if one exists (`docs/config.mdx:313`). It is not an error if none exists (`ConfigBuilder.groovy:344-347`).
- The check for unknown names runs after all files are parsed. It considers the profiles declared in all parsed files and all includes that actually ran (`ConfigBuilder.groovy:224-227`; `ConfigParserV2.groovy:154-156`). A profile declared only in a file that is included from an unselected profile is therefore unknown.
- `workflow.profile` in scripts is the `-profile` string, or `standard` (`WorkflowMetadata.groovy:274`).

### 4.3 Order of application (strict parser)

When the closing brace of a `profiles` block is reached, the parser walks the `-profile` list in command-line order. For each name that is defined in **this** block, it runs the body at the top level of the file (`ConfigDsl.groovy:394-402`). A name given twice in `-profile` is applied twice (derived from code). Consequences:

- `-profile b,a` applies `b` and then `a`, so `a` wins where they conflict (experiment 4). The docs agree (`docs/config.mdx:320-325`).
- Statements after the `profiles` block override values set by profiles. In experiment 3, `params.r = 'top-after'` after the block beats `r` set inside a profile.
- A profile body sees the params defined before the `profiles` block, the command-line params, and the params set by profiles applied earlier in the list (`ConfigBuilderTest.groovy:648-751`).
- Includes after the `profiles` block see params set by profiles (`ConfigBuilderTest.groovy:753-800`, "should merge profiles with conditions").
- A profile defined in several files, or in several `profiles` blocks, is applied once at each place (experiment 3).
- The legacy parser applied profiles in definition order (`docs/config.mdx:321`; experiment 4 run with `NXF_SYNTAX_PARSER=v1`). iunctus must not copy that.

### 4.4 Effect on the nf-core layout

A typical nf-core `nextflow.config` (for example rnaseq) has this order:

1. `params { }`
2. `process { }` defaults
3. `includeConfig 'conf/base.config'`
4. `profiles { }`
5. custom config includes
6. registries, the igenomes include, `env`, `process.shell`, reports, `manifest`, `plugins`, `validation`
7. the module config includes (`conf/modules/*.config`)

So module configs, which come after the profiles, override the same `withName` keys set by a test profile. The test profiles' `params` and `process.resourceLimits` stay in effect, unless a later statement sets the same key.

### 4.5 Showing all profiles

`nextflow config -show-profiles` calls the parser without a profile list. Every profile is then stored under `profiles.<name>` instead of being applied (`ConfigDsl.groovy:403-409`; `ConfigBuilder.groovy:253-256`; `CmdConfig.groovy:55-56`). Includes inside a profile are merged under `profiles.<name>` in this mode. A run never uses this mode.

## 5. Params

### 5.1 Precedence

Lowest to highest, as documented in `docs/cli.mdx:138-145` and confirmed by experiments 2, 12 and 16:

1. Script defaults: legacy `params.x = v` in a script, or the default in a typed `params { }` block.
2. Config params, in file order (section 2).
3. `-params-file`.
4. Command-line `--x`.

### 5.2 Command-line params

- **Values.** `--name value` and `--name=value` both work. The name must start with a letter or digit. A value is taken from the next argument if it does not start with `-`, or is a number, or contains a space, or is one character long. An empty next argument is not taken. Otherwise the value is `'true'` (`Launcher.groovy:337-341, 370-374`; `docs/cli.mdx:108-112`).
- **Nesting.** `--a.b 1` gives `[a: [b: '1']]`. `\.` keeps a literal dot (`CmdRun.groovy:753-780`). If `--a 1` and `--a.b 2` are both given, a warning says the first is overwritten.
- **Names.** kebab-case becomes camelCase: `--cli-kebab` is stored only as `cliKebab` (`CmdRun.groovy:782-794`; experiment 12). For a dotted name, only the last part is converted: `--my-group.sub-key` gives `[my-group: [subKey: ...]]` (derived from `CmdRun.groovy:756-786`).
- **Types.** Under the strict parser there is no type detection. Every command-line value is a string (`CmdRun.groovy:796-810`). The legacy parser, unless `NXF_DISABLE_PARAMS_TYPE_DETECTION` is set, converted `true`/`false` and numbers.

### 5.3 Params file

- `-params-file` or `NXF_PARAMS_FILE` names a `.json`, `.yml` or `.yaml` file (`CmdRun.groovy:70, 301-303, 727-740, 812-820`). The extension check itself is case-sensitive, so `.JSON` is rejected (derived from `CmdRun.groovy:815-817`).
- Before parsing, `${name}` placeholders are replaced with `projectDir`, `baseDir`, `launchDir`, `outputDir` and `secrets`. An unknown placeholder gives a warning and stays as it is (`CmdRun.groovy:822-844`). These values come from `ConfigBuilder.getConfigVars` (`CmdRun.groovy:345`), so `${outputDir}` is always `results` below the launch directory (derived from code).
- Top-level keys also go through the kebab-to-camel rule (experiment 12: `my-kebab` becomes `myKebab`). Nested keys do not.
- The JSON and YAML types are kept (experiment 12: `n: 3` stays an `Integer`).
- Command-line params override the params file. A nested command-line param is added into a nested params-file map (`CmdRun.groovy:742-749, 756-776`; experiment 12).

### 5.4 Params inside config files

`ConfigDsl.assign` treats `params.<name>` specially when the path has exactly two parts (`ConfigDsl.groovy:150-157`). That covers `params.x = v`, `params { x = v }` and the same forms inside profiles.

1. It records `x = v` as a declared config param. This record is used by typed params (section 5.6).
2. If the command-line map (params file plus `--x`) has `x`, the value is replaced by the command-line value (`ConfigDsl.groovy:169-193`). A string value is converted to the type of `v`:
   - boolean `v`: `'true'` or `'false'` (any case) become booleans;
   - number `v`: integer text becomes an `Integer`, a `Long` or a `BigInteger`, the first that fits. Any other text that Java can read as a number becomes a 32-bit `Float`, because Groovy's `isFloat()` accepts every such text (experiment 32). So `'0.7'` becomes a `Float` (experiment 2). Precision is lost: `'0.123456789'` becomes `0.12345679`, and `'1e40'` becomes `Infinity` (experiment 29). The `Double` and `BigDecimal` branches are not reached in practice;
   - text that does not convert stays a string;
   - a value that is not a string (for example a number or map from the params file) is used as it is.

This conversion only affects the value while the config is evaluated. Experiment 2 shows `params.n_is_int = params.n instanceof Integer` giving `true` for `--n 7`. The changelog records the conversion as "Fix type detection of CLI params in v2 config parser (#6765)" (`CHANGELOG.md:438`).

A map value is replaced as a whole. If the config has `params.a = [b: 1]` and the command line has `--a.c 2`, the command-line map `[c: '2']` replaces the config map. The config sees `[c: '2']`, and `b` is also missing from the final params (experiment 29).

Nested params written with three or more parts, such as `params.a.b = v`, are not replaced here. They are replaced after evaluation (section 5.5). So config expressions see the config value of such a nested param (derived from code).

What a config expression can see in `params`:

- params assigned earlier in the same file;
- the command-line params;
- in an included file, the including file's params at the point of the include;
- in a profile body, what section 4.3 lists.

It cannot see params from other top-level files (section 2.3).

### 5.5 The final params map

For `nextflow run`, after all files are merged, `ConfigCmdAdapter.mergeMaps` merges the raw command-line map into `config.params` (`ConfigCmdAdapter.groovy:523-526, 598-627`):

- Maps merge recursively.
- Any other value is replaced by the raw command-line value.
- If one side is a map and the other is not, there is a warning. It would be an error in strict mode, but strict mode is not active at this point under the strict parser (`CmdRun.groovy:516-525`).

So, for legacy params, the run-time value of `--n 7` is the string `'7'`, even though the config default is `5` and the config itself saw the Integer 7. `params.n + 1` then gives `'71'`. And `--flag false` gives the string `'false'`, which is true in a condition (experiment 2). Typed params do not have this problem. Whether the nf-schema plugin converts these values afterwards is **unverified**, because its source is not in the checkout.

### 5.6 Params in the script

- `Session.init0` puts `config.params` into the script binding (`Session.groovy:529-531`).
- **Legacy `params.x = v` in a script** only sets `x` if it is missing. A second assignment in the script gives the warning "defined multiple times" (`ScriptBinding.groovy:214-285`). Reading a missing param gives a warning and `null` (`ScriptBinding.groovy:242-252`; experiment 2).
- **Typed `params { }` block** (`ParamsDsl.groovy:44-96`; `docs/typed-parameters.mdx`):
  - A command-line or params-file name that is neither declared in the script nor declared in config is an error (experiment 16). "Declared in config" means a two-part `params.<name>` assignment that actually ran.
  - For each declared param, the value is taken from the first source that has it: command line and params file (`ParamsHelper.resolveFromCli`), then config (`resolveFromCode`), then the default.
  - A `Boolean` without a default defaults to `false`. A param with a nullable type (`Type?`) is optional and may stay `null` (`ParamsDsl.groovy:44-49, 76-78`; `ScriptToGroovyVisitor.java:178-185`). Any other param without a value is required. The value must fit the declared type.
  - Conversions (`ParamsHelper.groovy:49-200`): `Integer` and `Float` from any numeric text or number (an `Integer` rejects a fractional part; values too large are widened to `Long`, `BigInteger`, `Double` or `BigDecimal`); `Boolean` from `true`/`false` text on the command line only; `Path`, `Duration`, `MemoryUnit` and `VersionNumber` from strings; lists and maps with `asType`. Experiment 16 shows `--n 7` becoming Integer 7.
  - Params that exist only in config stay visible in the script (experiment 16: `extra`).

### 5.7 Params inside per-task closures

A closure in the `process` scope reads `params` when the task runs. It gets the params of the binding of the script that defines the process (`TaskContext.groovy:72, 132-146`). This is the run-time map: it includes script defaults and the raw command-line strings. Experiment 14 shows a config closure reading `params.only_script`, which is defined only in `main.nf`. A value that is not a closure was fixed while the config was parsed.

## 6. The `process` scope

### 6.1 How it is stored

After all files are merged and the `-process.` options are applied, `config.process` is a map with two kinds of keys (`ConfigDsl.groovy:352-366`):

- plain directive names (`cpus`, `memory`, `ext`, `publishDir`, ...). These are the defaults;
- `withLabel:<pattern>` and `withName:<pattern>`. Their values are maps of directive settings.

### 6.2 When it is applied

Each time a process is invoked, it is cloned under its invocation name. The config is applied to the clone when its task processor is created (`BindableDef.groovy:38-61`; `ProcessDef.groovy:113-120, 280-285`). The final session config is used. Selectors are not evaluated again per task. Only the closures inside them are (section 7).

### 6.3 The three names of a process

| Name | Meaning | Source |
| --- | --- | --- |
| base name | The name in the process definition. It never changes. | `ProcessDef.groovy:83-90` |
| simple name | The last part of the invocation name: the alias if the process was included with `as`. | `ProcessDef.groovy:100-120` |
| fully qualified name | The names of the enclosing workflow invocations and the process name, joined with `:`. The unnamed entry workflow is not part of it. Workflow aliases are used. | `BindableDef.groovy:41-45`; `docs/config.mdx:215-221`; experiment 6 gives `OUTER:SUB:BAR` |

### 6.4 The algorithm

`ProcessConfigBuilder.applyConfig` (`ProcessConfigBuilder.groovy:67-84`), as pseudocode:

```text
labels = labels of the process, or [""] if it has none
for each (key, body) in the process scope, in map order:
    if key starts with "withLabel:" and matchesLabels(labels, pattern of key):
        applySettings(body)
for each name in distinct([baseName, simpleName, fullyQualifiedName]), skipping empty names:
    for each (key, body) in the process scope, in map order:
        if key starts with "withName:" and matchesName(name, pattern of key):
            applySettings(body)
applyDefaults(process scope)
if scratch is set and stageInMode == "rellink": remove stageInMode, with a warning

applySettings(body):                              # lines 169-197
    for each (key, value) in body:
        skip keys that start with "withLabel:" or "withName:"
        warn "Unknown directive" if key is not a directive name
        skip "params"
        if key == "ext": copy each ext entry into the process ext map, overwriting
        else if key is label, module, pod or publishDir:
            remove the current value, then add value through the directive method
        else: set key = value

applyDefaults(scope):                             # lines 207-223
    for each (key, value) in scope:
        skip "params"
        if key == "ext": add only the ext entries the process does not have
        else if the process config has no value for key,
             or its current value equals the built-in default of key:
            set key = value (repeatable directives as above)
```

The built-in defaults are `debug: false`, `cacheable: true`, `shell: ['/bin/bash', '-ue']` (`['/bin/bash', '-uex']` when `NXF_DEBUG` is above 0), `maxRetries: 1`, `maxErrors: -1` and `errorStrategy: TERMINATE` (`ProcessConfig.groovy:41-48`; `BashWrapperBuilder.groovy:69-90`). Every process config starts with these values (`ProcessConfig.groovy:78-82`), so for these six keys only the "equals the built-in default" test decides.

`applyDefaults` does not skip the `withLabel:` and `withName:` keys. They are copied into the process config as extra keys, which has no known effect (derived from code).


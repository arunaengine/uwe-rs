<!-- Describes the owned native-run recorders and their evidence fields. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Native-run evidence

The scripts in `real_runs/` compare uwe with native tools. Their recorded `.txt`
outputs remain in this fixture directory. Moving the scripts does not regenerate
those outputs or change their historical provenance. The `slurm/` scripts require
an explicitly selected cluster environment; they are not ordinary local tests.

`real_runs/nextflow.py` cases identify a workflow repository and revision, fetched
under `<ROOT>/sources/<name>`. Commands substitute `{workflow}`, `{out}`, `{uwe}`
and `{limits}`. Results are compared under `results` when specified, otherwise
`{out}`. `notes` explains intentional differences. Native SLURM jobs are limited
to four concurrent jobs and receive proxy variables through Apptainer's whitelist.

## Failure evidence

`run_evidence.py` records why a native Nextflow pipeline cannot run. Ordered
`expect` patterns select successive output lines. The default kind is `invalid`:
the pipeline, its stubs or its test profile fails independently. A case may instead
declare `missing` for unavailable inputs or `unsupported (decision N)` for a
deliberately unsupported uwe feature.

`release` selects the pipeline's native Nextflow version. The launcher uses
`NXF_VER` and a separate `NXF_HOME` under `NXF_RELEASES`. Releases before 23.10
need Java 17 or earlier through `JAVA17_HOME`; releases before 21.10 need Java 11
through `JAVA11_HOME`. `parser: "v2"` selects the strict parser; legacy is the default.
`jars: True` installs the old Java libraries into the copied workflow's `lib/`.

`args` adds run arguments, substituting `{launch}` and `{pipeline}`. `profile:
False` omits the test profile. `config` becomes `-c extra.config`. `tools` supplies
commands with fixed output, such as version text used by a stub.

`stubs` replaces absent or broken stubs so the native run reaches the failure being
examined. The stand-in creates the process's expected files or the simulator's
placeholder files. A replacement containing three double quotes supplies the whole
stub section, including statements before the script. When missing native tools
prevent reaching the target failure, `reproduce` supplies a small workflow containing
the failing part instead. These controlled substitutions must remain explicit in
the evidence; they do not establish successful full native execution.

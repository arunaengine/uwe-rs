<!-- Named workflow presets and installed reference data in the uwe config file. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Workflow presets and installed references

A site or user config file of uwe can define named workflow presets and installed reference releases. `uwe presets` lists them as JSON. `uwe check -preset NAME`, `uwe inputs -preset NAME` and `uwe run -preset NAME` all use the same selection. Each submission checks the digests and read-only permissions of the installed references before the workflow is admitted. The normal schema and planning checks still apply.

```toml
[workflows.demo]
source = "nf-core/demo"
revision = "<full Git commit>"
language = "nextflow"
profiles = ["docker"]
params = "demo-inputs.json"
overrides = ["input", "outdir"]
executor = "local"
executors = ["local", "kubernetes"]

[workflows.demo.references]
reference = "genome_v1"

[references.genome_v1]
path = "/srv/references/genome-v1.fa"
digest = "sha256:<64 lowercase hexadecimal digits>"
version = "1"
files = []
```

Replace the commit, the digest and the parameter names with the values of your release and workflow.

## References

`uwe digest PATH` computes the digest of a file or a folder. Run it before you install the data read-only.

- A file reference uses the SHA-256 of the file.
- A folder reference uses the digest of its canonical `task_model::Tree` manifest. `files = ["genome.fa", "genome.fa.fai"]` also requires those files inside the folder. Required paths cannot leave the folder through `..` or links.

Installed data must be read-only, folders included. uwe checks installed data. It never installs or updates databases.

## Sources

Remote repository sources need a full Git commit in `revision`, not a branch or tag.

For a local workflow, leave out `revision` and give the `digest` of the whole read-only project folder. A folder `source` pins that folder. A file `source` pins its parent folder, including sibling modules and configuration. Keep the dependencies of the workflow inside that project. Whether the workflow may reach anything outside it at runtime is up to the authorization policy of the host that embeds uwe.

## Paths and parameters

`source`, `params` and reference `path` values are relative to the config file that set them, also in layered overrides. The `params` file must hold a JSON object.

Values apply in this order, later ones win: the `params` file, posted JSON inputs, then parameters on the command line in the normal syntax of the language. Only names listed in `overrides` may be set, and parameters bound to a reference can never be overridden. Use absolute local file paths in input JSON. Each input JSON file may be at most 1 MiB.

A preset supplies its source, language, native profiles, an optional `entry` and the workflow configuration. Any other launch configuration is rejected.

## Executors and requirements

The optional `executors` list restricts which executors a run may choose. The optional `requires = ["sidecars"]` checks the declared capabilities of the backend before any input is loaded. Supported names are `daemons`, `sidecars` and `offline`. A preset that sets `executors` or `requires` must also set a default `executor`.

`uwe presets` shows these capabilities, the native Nextflow coverage and its format version. Each workflow still needs its own checks and a real test run before production use.

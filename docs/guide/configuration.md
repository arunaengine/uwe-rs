<!-- Reference for every non-storage iunctus TOML section, value type and default. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Configuration reference

iunctus configuration is TOML. It is separate from workflow configuration.
Files merge key by key in this order; later values win:

1. `/etc/iunctus/config.toml`, if it exists. `IUNCTUS_SITE_CONFIG` replaces this
   path; an empty value disables the site file.
2. `$XDG_CONFIG_HOME/iunctus/config.toml`, or `~/.config/iunctus/config.toml`.
3. The file passed with `-iunctus-config FILE`, which must exist.

CLI options override matching configuration defaults. Specific environment
overrides are listed below. Unknown sections and keys normally fail on loading.
`[page]` is validated when the page starts.
Paths are literal. Use absolute paths where required; `~` is not shell-expanded
inside TOML strings.

The tables cover all sections accepted by
[setup.rs](../../iunctus/src/setup.rs), with storage in the
[storage reference](storage.md). Types are TOML types. `secret` means either a
string or `{ environment = "VARIABLE" }`. Environment secrets are resolved when
used and must be set and nonempty. Prefer environment references to written
credentials. `Required` means required when that section is present.

## Executor selection and local engine

| Section/key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `executor.default` | string | `"local"` for run and check-machine | `"slurm"`; local, tes, slurm, kubernetes or simulator. Inspection commands retain simulated planning. |
| `executor.local.engine` | string | Search Docker, then Podman | `"podman"` or `"/run/user/1000/podman/podman.sock"`. `IUNCTUS_ENGINE` wins. |

## TES

Keys belong to `[executor.tes]`.
The parser is [tes-executor/settings.rs](../../tes-executor/src/settings.rs).

| Key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `server` | string | `"http://localhost:8000"` for TES runs | `"https://tes.example.org"`; `-tes-server` wins. Readiness checks ask for an explicit server. |
| `token` | secret | none | `{ environment = "TES_TOKEN" }`; bearer auth. Nonempty `IUNCTUS_TES_TOKEN` wins. |
| `staging` | string | `"shared-path"` | `"object-store"`; select shared filesystem paths or S3 FILE staging. |
| `staging_root` | string | none | `"s3://bucket/tes"`; required for object-store, refused for shared-path. No credentials, query, fragment or empty path components. |
| `idempotency_tag` | string | none | `"example.org/request-key"`; requires server-side deduplication. Must be nonempty and differ from `iunctus-attempt`. |
| `user` | secret | none | `{ environment = "TES_USER" }`; HTTP Basic username, requires password. |
| `password` | secret | none | `{ environment = "TES_PASSWORD" }`; requires user. Bearer auth wins over Basic. |
| `max_inputs` | integer | `512` | `200`; accepted range 1 to 512 staged input files. |
| `max_outputs` | integer | `1024` | `500`; accepted range 1 to 1024 collected output files. |

`[executor.tes.sites.NAME]` adds a TES site.
Each site accepts all TES keys above except `sites` itself.
Its `server` is required. Its token and staging settings use their own defaults;
they do not inherit the main TES table. The global token override applies to the
main server. A site's token comes from its own table.

| Site key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `near` | array of strings | `[]` | `["s3://lab-data/", "/shared/lab/"]`; input prefixes used to choose nearby sites. |

## SLURM and Kubernetes

| Section/key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `executor.slurm.container` | string | `"apptainer"` | `"podman"`; also singularity or docker. `-slurm-container` wins. |
| `executor.slurm.options` | array of strings | `[]` | `["--account=lab"]`; extra sbatch arguments. `-slurm-options` replaces them. |
| `executor.slurm.pull.cpus` | positive integer | `2` | `4`; CPUs of the SIF acquisition job. |
| `executor.slurm.pull.memory_mb` | positive integer | `4096` | `8192`; memory of that job. |
| `executor.slurm.pull.minutes` | positive integer | `60` | `120`; time limit of that job. |
| `executor.kubernetes.kubeconfig` | string path | none, required for execution | `"/srv/workflows/kubeconfig"`; `-kubeconfig` wins. No implicit default kubeconfig. |
| `executor.kubernetes.namespace` | string | kubeconfig context namespace, otherwise `"default"` | `"workflows"`; `-kubernetes-namespace` wins. |

## Cache, network, reports and retries

| Section/key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `cache.folder` | absolute string path | `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus` | `"/scratch/iunctus"`; overrides the cache environment location. |
| `network.default` | string | `"allowed"` | `"none"`; deny task network unless allowed. `-no-network` also requests denial. |
| `network.allowed_steps` | array of strings | `[]` | `["DOWNLOAD_*"]`; wildcard step names allowed through the default denial. |
| `network.proxy` | string URL | environment proxy, otherwise none | `"http://proxy.example.org:3128"`; HTTP_PROXY, HTTPS_PROXY and ALL_PROXY (also lowercase) win. NO_PROXY is respected. |
| `report.file` | string path | none | `"report.html"`; automatic report after each run. Relative to the launch folder. `-report` wins. |
| `retry.machine_count` | integer, 0 to u32 maximum | `2` | `0`; retries for machine failures. `-machine-retries` wins. |
| `retry.base_delay_ms` | positive integer | `1000` | `250`; base for jittered retry backoff. `-machine-retry-delay-ms` wins. |

Network isolation must be supported by the chosen executor.
`network.proxy` configures iunctus HTTP requests; it is not general task-network
access control. Workflow retries and machine-failure retries are separate.

## Registry, telemetry and page

| Section/key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `registry."HOST".username` | secret | none | `{ environment = "REGISTRY_USER" }`; requires password and excludes token. |
| `registry."HOST".password` | secret | none | `{ environment = "REGISTRY_PASSWORD" }`; requires username. |
| `registry."HOST".token` | secret | none | `{ environment = "REGISTRY_TOKEN" }`; alternative bearer auth. |
| `telemetry.endpoint` | string URL | none, required in section | `"http://localhost:4318"`; OTLP HTTP endpoint, traces use `/v1/traces`. |
| `telemetry.headers.NAME` | secret, arbitrary header name | no headers | `{ environment = "OTLP_AUTHORIZATION" }`; complete value of the named header. |
| `page.token` | secret | none on loopback; generated beyond loopback | `{ environment = "IUNCTUS_PAGE_TOKEN" }`; letters, digits, hyphen and underscore only. |

Registry tables require exactly one complete authentication choice.
There are no TOML page address or port keys. Use `-address` and `-port`.
See [executor setup](executors.md) for registry credential-file lookup and traces.

## Conda

`[conda]` enables local environment preparation. Omit the whole section if it
is not needed. Its parser is [iunctus/conda.rs](../../iunctus/src/conda.rs).

| Key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `base` | string image reference | required | `"registry.example.org/base@sha256:MANIFEST_DIGEST"`; use a real platform manifest digest. |
| `platform` | string OCI platform | required | `"linux/amd64"`; target container platform. |
| `installer` | string | required, must be `"micromamba"` | `"micromamba"`; Conda-only bases must already provide it. |
| `offline` | boolean | `false` | `true`; use only cached metadata and packages. `-no-network` or `network.default = "none"` also forces offline acquisition. |
| `capabilities` | absolute string path | required | `"/srv/conda/target.json"`; exact base/platform and virtual packages. |
| `repodata` | absolute string path | required | `"/srv/conda/repodata"`; prepared JSON and downloaded metadata cache. |
| `packages` | absolute string path | required | `"/srv/conda/packages"`; verified package bytes indexed by SHA-256. |
| `registry` | string repository reference | none | `"registry.example.org/team/conda"`; required for remote Conda tasks. A local engine builds and pushes the image, and workers receive its manifest digest. |

The capabilities file has this shape. Replace the placeholders with data for
the pinned target. An empty package list asserts that no virtual packages exist:

```json
{
  "base": "registry.example.org/base@sha256:MANIFEST_DIGEST",
  "platform": "linux/amd64",
  "virtual_packages": [{"name": "__glibc", "version": "2.36", "build": "0"}]
}
```

Supported platform mappings are linux/amd64, linux/arm64 (also linux/arm64/v8),
linux/ppc64le, linux/s390x and linux/386.
This mapping is not a qualification claim for each host architecture.
Unknown capability fields fail. The file must match the configured base and
platform exactly. Do not derive target capabilities from the host.
See [Snakemake environments](snakemake.md#conda-and-pip) for combined rule images.

## Small local configuration

```toml
[executor]
default = "local"

[cache]
folder = "/scratch/iunctus"

[report]
file = "report.html"
```

Save it as `machine.toml`, choose a writable cache folder, and use
`iunctus run main.nf -iunctus-config machine.toml`.

All keys above were traced from parsing to their consumers.
Remote executors, Conda builds and telemetry delivery were not exercised for
this guide. Those checks require the configured service or target.
See [storage defaults that remain unconfirmed](storage.md#verification-boundaries).

Next: [Storage reference](storage.md) and [Troubleshooting](troubleshooting.md).

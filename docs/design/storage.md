<!-- The config file of iunctus and the storage providers it names: where the file is, its format, secrets, and how the store, remote inputs and results use the providers. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The config file and storage providers

Status: implemented, 2026-09-30. This applies [decision](../decisions.md) 20.

iunctus reads one config file, which may come in up to three parts: one for the site, one for the user and one for a run. It says which storage provider serves which bucket or address prefix, with its endpoint, region, credentials and settings, and where the cache store lives. A secret in the file may be the name of an environment variable instead of the secret itself, so the file can be shared. One run can read its inputs from one provider, keep its store on a second and deliver its results to a third.

Without any config file, everything works as before: public web addresses, public S3 and Google Cloud Storage buckets and local files need no provider, and the store is a folder in the cache. A laptop needs no config.

| Crate | What it is |
|---|---|
| `config-file` | Finds and reads the files, merges them, and gives each part of iunctus its section by type, with errors that name the file and the line |
| `storage` | The storage providers: which one serves an address, their clients, and reading, listing, downloading and uploading files |

## 1. The config file

**Where it is.** iunctus reads these files in this order; each one that exists overrides the ones before it, key by key:

1. `/etc/iunctus/config.toml`, for every user of the machine;
2. `$XDG_CONFIG_HOME/iunctus/config.toml`, else `~/.config/iunctus/config.toml`, for the user;
3. the file that `-iunctus-config <file>` names, for one run; it must exist.

Options on the command line win over all files: `-store` over `[store] location`.

**Another site file, or none.** The environment variable `IUNCTUS_SITE_CONFIG` names another file in place of `/etc/iunctus/config.toml`; an empty value means no site file. The tests of the `iunctus` binary use it together with an empty `XDG_CONFIG_HOME`, so that no file of the user or of the machine changes their results (section 7).

**Merging.** Tables merge key by key, and any other value, like a text or a list, replaces the one before it. So a user can add the keys of a provider that the site file names, and a run can point the store elsewhere.

**Format.** The file is TOML. Its sections are:

| Section | What it sets |
|---|---|
| `[store]` | `location`: where the cache store lives, an absolute folder or an address of a provider, like `s3://lab-cache/iunctus` |
| `[storage.<name>]` | one storage provider; the name is free, like `lab` or `aws` |
| `[executor.<name>]` | the settings of an executor ([executors](executors.md)) |
| `[telemetry]` | `endpoint`, an OTLP HTTP endpoint that the trace of every real run goes to, and `headers`, each value a secret ([runs](runs.md), section 6.4) |

A section of another name, and a key that its table does not know, is an error, so that a typing mistake does not go unnoticed.

**Secrets.** A key that holds a secret, like a password, a key or a token, takes the secret as text, or a reference to an environment variable:

```toml
secret_access_key = { environment = "LAB_S3_SECRET" }
```

iunctus reads the variable when the provider is first used, so a provider that a run does not use needs no variable. A variable that is not set, or is empty, is an error that names the variable, the file and the line.

**Errors** name the file, the line and what is wrong, in plain words:

```text
iunctus: /home/me/.config/iunctus/config.toml, line 12: `[storage.lab]` has no setting `regoin`; it knows `type`, `prefixes`, `endpoint`, `region`, `path_style`, `access_key_id`, `secret_access_key`, `session_token` and `anonymous`
iunctus: /home/me/.config/iunctus/config.toml, line 7: the environment variable LAB_S3_SECRET, which holds this secret, is not set
```

**For other parts of iunctus** (`config_file`): `ConfigFile::load(named)` reads the files; `config.section("executor")` gives a `Table` whose accessors `text`, `texts`, `flag`, `integer`, `number`, `secret`, `table`, `tables` and `keys` read values by type, `only(&[...])` rejects unknown keys, and `invalid` and `missing` make errors at the right line. `Secret::reveal()` gives the secret when it is needed; printing a `Secret` shows only where it comes from.

## 2. An example

```toml
# ~/.config/iunctus/config.toml

# The cache store lives in a bucket of the lab's MinIO.
[store]
location = "s3://lab-cache/iunctus"

# The lab's MinIO serves two buckets.
[storage.lab]
type = "s3"
prefixes = ["s3://lab-data/", "s3://lab-cache/"]
endpoint = "https://minio.lab.example.org"
access_key_id = "lab-user"
secret_access_key = { environment = "LAB_S3_SECRET" }

# The lab's archive is on AWS, with the credentials of the environment or ~/.aws. Other
# public buckets, like those of nf-core test data, need no provider.
[storage.aws]
type = "s3"
prefixes = ["s3://lab-archive/"]
region = "eu-central-1"

# Results go to Cloudflare R2.
[storage.results]
type = "s3"
prefixes = ["s3://lab-results/"]
endpoint = "https://0123456789abcdef.r2.cloudflarestorage.com"
region = "auto"
access_key_id = { environment = "R2_KEY_ID" }
secret_access_key = { environment = "R2_SECRET" }

# A private web server that wants a token.
[storage.portal]
type = "https"
prefixes = ["https://data.example.org/private/"]
token = { environment = "PORTAL_TOKEN" }

# A local copy of a public bucket, which saves downloads.
[storage.igenomes]
type = "folder"
prefixes = ["s3://ngi-igenomes/"]
path = "/data/igenomes"
```

With this file, `iunctus run main.nf -executor local --outdir s3://lab-results/run1` reads its samples from `s3://lab-data/`, keeps its store in `s3://lab-cache/iunctus`, takes the iGenomes references from `/data/igenomes`, and publishes its results to R2.

## 3. Storage providers

| `type` | Serves addresses like | Settings besides `type` and `prefixes` |
|---|---|---|
| `s3` | `s3://bucket/key`, or `https://endpoint/bucket/key` | `endpoint`, `region`, `path_style`, `access_key_id`, `secret_access_key`, `session_token`, `anonymous` |
| `google-cloud-storage` | `gs://bucket/key` | `endpoint`, `credential_file`, `token`, `anonymous` |
| `azure-blob-storage` | `az://container/blob`, or `https://account.blob.core.windows.net/container/blob` | `account`, `endpoint`, `key`, `sas_token` |
| `https` | `https://host/path` and `http://host/path` | `token`, `user`, `password`, `headers` |
| `ftp` | `ftp://host/path`, for inputs | `user`, `password` |
| `folder` | any address; the rest after the prefix is the path in the folder | `path` |

- **`s3`** is AWS S3 and every service that speaks its API: MinIO, Ceph, Cloudflare R2, RustFS and others. Without `endpoint` it is AWS, whose buckets are addressed by host name; with an `endpoint` buckets go into the path (`path_style`, which can be set either way). Without keys it takes the standard AWS sources: `AWS_ACCESS_KEY_ID` and the other variables, `~/.aws/credentials`, and the role of the machine. `anonymous = true` signs nothing, for public buckets. The region is `region`, else `AWS_REGION` or `AWS_DEFAULT_REGION`, else `us-east-1` for a service at its own `endpoint`; AWS itself needs one.
- **`google-cloud-storage`** takes a service account key file (`credential_file`), an OAuth 2.0 access token (`token`), or else the standard Google sources, like `GOOGLE_APPLICATION_CREDENTIALS`.
- **`azure-blob-storage`** takes the storage `account` with its access `key`, or a shared access signature (`sas_token`).
- **`https`** sends `token` as `Authorization: Bearer`, `user` and `password` as basic authentication, and each entry of `headers` as a header of its own, like `headers = { X-Api-Key = { environment = "API_KEY" } }`. Every header value counts as a secret.
- **`ftp`** logs in as `user`, or as `anonymous`, and reads files in passive mode.
- **`folder`** maps addresses to a folder on this machine, like a mirror of a bucket or a disk that the cluster shares.

**Which provider serves an address.** The provider with the longest prefix that the address starts with. A prefix ends at a name: `s3://lab` serves `s3://lab/a.txt`, but not `s3://lab-data/a.txt`; `s3://` serves every bucket. Two providers may not have the same prefix. An address that no provider of the config file serves goes to a built-in one:

| Address | Built-in provider |
|---|---|
| `http://`, `https://` | the web, without credentials; a file of a GitHub repository is read from `raw.githubusercontent.com` |
| `s3://`, `gs://` | public buckets, read over HTTPS without credentials (`https://<bucket>.s3.amazonaws.com/<key>`, `https://storage.googleapis.com/<bucket>/<key>`), which can also list them |
| `file://` | the local file system |
| `ftp://` | FTP servers, as Nextflow reads them (decision 27): as the user `anonymous`, or as the user and password that the address names (`ftp://user:password@server/path`); a password in an address shows as `[secret]` in messages |

Other addresses, like `az://`, need a provider of the config file. Without one, a run stops with `no storage provider serves az://container/blob; name one in the config file, like [storage.<name>] with its type and prefixes = ["az://container/"]`. A Nextflow workflow that reads such a file fails as before, with exit code 33.

**Clients.** Each provider makes its client once per process, when it is first used, and every request to it shares that client, so connections are reused and a host's address is looked up once. S3, Google Cloud Storage and Azure go through OpenDAL, with one operator per bucket. They, the web providers and the public buckets share one HTTP client of the storage crate (`storage::http_client`), which a web provider with credentials gives its headers with each request. The checks of images at registries and the requests to TES servers use the same client (`executor::http::client`), so the whole process has one. FTP opens a session per request, as FTP transfers one file per session at a time, and looks up each server's address once. Requests that drivers make wait on a small runtime of the storage crate (`Storage::blocking`), so they work from the driver's thread.

## 4. Where iunctus uses the providers

- **The store.** `-store` or `[store] location` is a folder, or an address of a provider that can write: `s3`, `google-cloud-storage`, `azure-blob-storage` or `folder`. The store then lives there with the same layout (`cas/`, `trees/`, `ac/`, `runs/`), and its outputs have addresses like `s3://lab-cache/iunctus/runs/...`, which the run database records.
- **Remote inputs.** A task input at an address comes through its provider (`task_runner::Settings::storage`), in all four languages: Nextflow `file('s3://...')`, Snakemake `storage("s3://...")`, CWL `File` locations and Argo artifacts. Nextflow workflow code that reads a remote file while it plans, like a sample sheet, or asks whether one exists or lists a bucket folder, goes through the same providers. So does CWL, when a real run needs the `size` of an input file or its content for `loadContents`: it asks the provider for the size first, and reads at most 64 KiB (a larger file is an error, as in CWL v1.2). A simulated CWL run takes such a file as present and empty and sends no request. Each file is downloaded once into `<cache>/downloads`, named by the SHA-256 of its public address as before the providers came, so earlier downloads stay valid; a download that stopped continues where it ended.
- **Delivered results.** A target may be an address: Nextflow `publishDir 's3://bucket/results'` and `outputDir`, and `-output-dir s3://bucket/results` for CWL and Argo. `task_runner::StorageFiles`, the `Files` service of every real executor, uploads local outputs to such an address (a directory file by file), copies outputs of a store elsewhere to this machine (a link becomes a copy), and gives a local copy of a remote output (`Files::local`).
- **Workflow files on the web.** Snakemake modules and includes and CWL `run` and `$import` documents from web addresses come through the providers, so a private server with a token works too. CWL keeps what it downloads in `<cache>/downloads`, like the other languages.

## 5. Secrets

Secrets live only in the environment, or in the config file, and in the memory of the provider that uses them. They never reach anything that iunctus writes:

- Tasks name remote inputs by address, never with credentials, so task descriptions, TES documents and cache keys hold none.
- The run database and the RO-Crate record addresses, the command line and parameters, never a provider's settings.
- Every message of a provider, like an error of a server or of OpenDAL, has each of the provider's secrets replaced by `[secret]`, and the headers of the web providers are marked as sensitive, so that no log shows them.
- Printing a `Secret` or the storage shows names and prefixes only.

`iunctus/tests/storage.rs` checks this: a Nextflow workflow reads its sample sheet and inputs from a local web server that wants a token, which only an environment variable holds. After a run, a run with a wrong token and a run without the variable, neither token is in the output, the run database, the exported RO-Crate, the task documents of the simulator or any file of the cache.

## 6. Not yet

- **The record of WorkflowHub workflow 1106**, which stopped at an FTP file before the built-in FTP provider, must be recorded again on the next run of the Nextflow test workflows with network.
- **Drivers that read task outputs** from a store elsewhere, like a Nextflow `.command.env` or a CWL `cwl.output.json`, still read them as local paths; they need to go through `Files::local` ([the engine](engine.md), section 8). Runs that only pass files on work with a store on S3 today.
- **CWL** lists no remote directory (`loadListing`), reads no remote file in a simulated run, and fetches `run` and `$import` documents only from `http://` and `https://` addresses.
- **Settings in workflows.** Argo artifacts may name an endpoint and Kubernetes secrets, and Snakemake's `storage:` directive may give provider settings; iunctus takes the bucket and key from the workflow and the provider from the config file (decision 20).
- Remote directories as task inputs, FTPS and SFTP, and TES servers that read a store on S3 with credentials of their own.

## 7. Checks

- `config-file/tests/config.rs`: typed settings, the merging of files, secrets from the environment, and errors with the file and the line. A unit test covers `IUNCTUS_SITE_CONFIG`.
- `iunctus/tests/common/mod.rs`: every test that runs the binary does so through `common::command()`, which sets `XDG_CONFIG_HOME` to an empty folder and `IUNCTUS_SITE_CONFIG` to nothing, so that tests never read the config files of the user or the site. `iunctus/tests/config_files.rs` checks that a site file comes only from the variable and that a file in the home folder is not read.
- `storage/tests/providers.rs`: settings and their errors, the longest prefix, folders, a local web server that wants a token or a header or a password (and a wrong token that never shows), downloads that happen once and continue after a stop, the blocking view, and a small FTP server of the tests. Unit tests cover addresses, public bucket listings and the hiding of secrets.
- `cwl-parser/tests/web.rs` and `cwl-runtime/src/files_tests.rs`: CWL documents, and the size and content of input files, from a local web server that wants a token, which only a provider of the config file sends. `iunctus/tests/storage.rs` runs a CWL workflow whose tool is on the private server.
- `task-runner/src/files.rs`: deliveries from this machine to addresses, from addresses to this machine, and between addresses.
- `iunctus/tests/storage.rs`: the secrets of a whole run, as above.
- `just storage-test` starts two RustFS servers from the local image, each with its own credentials, and runs `storage/tests/s3.rs` (inputs on one provider, the store on the other, a wrong secret that never shows) and the ignored test of `iunctus/tests/storage.rs`: a real Nextflow run with Docker that reads its sample sheet and inputs from the first server, keeps its store on the second, publishes its results to a third provider (a folder), and takes every task from the store in a second run.

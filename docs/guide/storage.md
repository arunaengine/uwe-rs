<!-- Reference for content-store locations and all configured storage provider keys. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Storage reference

These tables complete the [configuration reference](configuration.md).
They follow [storage/settings.rs](../../storage/src/settings.rs) and its provider
consumers. A `secret` is a string or `{ environment = "VARIABLE" }`.
No providers are configured by default. Built-in public HTTP, FTP, S3 and Google
bucket reads need no provider settings. Private access and writable remote stores
need a matching provider.

## Content store

| Section/key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `store.location` | string absolute path or address | `<cache>/store` | `"s3://results/iunctus"`; `-store` wins. Remote location needs a writable provider. |
| `store.local` | absolute string path | none | `"/scratch/iunctus/store"`; local front store. Reads try it first; writes go to both stores. |

`<cache>` is the effective cache folder from [configuration](configuration.md).
The run database normally lives separately at `<cache>/runs.sqlite`.

## Common provider keys

Use `[storage.NAME]` for each provider. Names are local labels.

| Key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `type` | string | required | `"s3"`; also google-cloud-storage, azure-blob-storage, https, ftp or folder. |
| `prefixes` | nonempty array of strings | required | `["s3://lab-data/"]`; addresses handled by this provider. |

The longest matching prefix selects the provider. Duplicate prefixes are refused.
Providers can be read-only. HTTPS and FTP do not implement writable stores.

## S3

Use `type = "s3"`. Prefixes can start with `s3://`, `http://` or `https://`.

| Key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `endpoint` | string HTTP(S) URL | AWS service endpoint | `"https://s3.example.org"`; compatible service endpoint. |
| `region` | string | AWS_REGION, then AWS_DEFAULT_REGION; `"us-east-1"` with custom endpoint | `"eu-central-1"`; otherwise required when connecting. |
| `path_style` | boolean | `true` with custom endpoint; `false` otherwise | `true`; put bucket in URL path instead of hostname. |
| `access_key_id` | secret | none; then the standard AWS sources | `{ environment = "S3_ACCESS_KEY" }`; requires secret_access_key. |
| `secret_access_key` | secret | none; then the standard AWS sources | `{ environment = "S3_SECRET_KEY" }`; requires access_key_id. |
| `session_token` | secret | none | `{ environment = "S3_SESSION_TOKEN" }`; passed with the explicit key pair. |
| `anonymous` | boolean | `false` | `true`; unsigned access, cannot combine with explicit access keys. |

Use a real bucket and choose prefixes that include your input, output or
[TES staging](executors.md#tes) locations.
Without a key pair, OpenDAL reads the standard AWS sources: the `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN` environment variables, the `~/.aws` config and credentials files, web identity tokens and instance metadata. An explicit key pair disables this discovery.

```toml
[storage.lab]
type = "s3"
prefixes = ["s3://lab-data/"]
region = "eu-central-1"
access_key_id = { environment = "S3_ACCESS_KEY" }
secret_access_key = { environment = "S3_SECRET_KEY" }

[store]
location = "s3://lab-data/iunctus"
local = "/scratch/iunctus/store"
```

## Google Cloud Storage

Use `type = "google-cloud-storage"` with `gs://`, HTTP or HTTPS prefixes.

| Key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `endpoint` | string HTTP(S) URL | `"https://storage.googleapis.com"` | `"https://storage.googleapis.com"`; provider endpoint. |
| `credential_file` | string path | none; then the standard Google sources | `"/srv/secrets/service-account.json"`; service-account key file passed to OpenDAL. |
| `token` | secret | none; then the standard Google sources | `{ environment = "GCS_TOKEN" }`; OAuth access token. |
| `anonymous` | boolean | `false` | `true`; unsigned access, disables credential and metadata discovery. |

Without `credential_file` or `token`, OpenDAL reads the standard Google sources: the file named by `GOOGLE_APPLICATION_CREDENTIALS`, the gcloud default credentials and VM metadata.

## Azure Blob Storage

Use `type = "azure-blob-storage"` with `az://`, HTTP or HTTPS prefixes.

| Key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `account` | string | required unless endpoint is supplied | `"labstorage"`; also required with key. |
| `endpoint` | string HTTP(S) URL | `https://<account>.blob.core.windows.net` | `"https://labstorage.blob.core.windows.net"`; service endpoint. |
| `key` | secret | none | `{ environment = "AZURE_KEY" }`; account access key. |
| `sas_token` | secret | none | `{ environment = "AZURE_SAS" }`; shared access signature. |

## HTTP and HTTPS

Use `type = "https"` with HTTP or HTTPS prefixes.
The provider adds credentials to requests under those prefixes.

| Key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `token` | secret | none | `{ environment = "WEB_TOKEN" }`; bearer token. |
| `user` | string | none | `"reader"`; Basic username. |
| `password` | secret | none | `{ environment = "WEB_PASSWORD" }`; requires user. |
| `headers.NAME` | secret, arbitrary valid HTTP header name | no headers | `{ environment = "WEB_API_KEY" }`; custom header value. |

Use one authentication method per provider. Custom Authorization headers can
replace the authorization value set earlier by token or Basic settings.

## FTP and folder providers

| Provider/key | Type | Default | Example and meaning |
| --- | --- | --- | --- |
| `type = "ftp"`, `user` | string | `"anonymous"` | `"reader"`; FTP login. Prefixes must use ftp://. |
| `type = "ftp"`, `password` | secret | `"anonymous@"` | `{ environment = "FTP_PASSWORD" }`; FTP password. |
| `type = "folder"`, `path` | absolute string path | required | `"/data/mirror"`; local files behind provider prefixes. |

Folder providers accept any address scheme. The part after the matching prefix
is resolved under `path`. They can serve a writable local mirror of a bucket.

## Verification boundaries

Every listed key has a confirmed parser and consumer. No remote storage access
was attempted for this guide. Automatic credential discovery remains unconfirmed
for omitted S3 `access_key_id`/`secret_access_key` and omitted GCS
`credential_file`/`token`. These paths defer to OpenDAL credential providers.
Use explicit credentials or anonymous access instead of assuming a working
default discovery chain on your machine.

Next: [Troubleshooting](troubleshooting.md). Return to [Configuration](configuration.md).

//! The storage settings of the config file: the providers of `[storage.<name>]`, each with
//! the address prefixes it serves, and the place of the cache store in `[store]`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::{ConfigError, ConfigFile, Origin, Secret, Table};

use crate::address;

/// The storage settings of the config file.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// The providers, in the order of their names.
    pub providers: Vec<ProviderSettings>,
    /// Where the cache store lives: a folder or an address (`[store] location`).
    pub store: Option<String>,
    /// A folder of this machine in front of a store at an address (`[store] local`): reads
    /// look there first, and new results go to both.
    pub local_store: Option<String>,
}

/// A storage provider: its kind with its settings, and the addresses it serves.
#[derive(Clone, Debug)]
pub struct ProviderSettings {
    /// The name of its table, like `lab` of `[storage.lab]`.
    pub name: String,
    /// The addresses it serves start with one of these, like `s3://lab-data/`.
    pub prefixes: Vec<String>,
    pub kind: Kind,
    /// Where the provider is written, for errors that come up when it is used.
    pub origin: Origin,
}

/// The kinds of providers, by the `type` of their table.
#[derive(Clone, Debug)]
pub enum Kind {
    S3(S3),
    GoogleCloudStorage(GoogleCloudStorage),
    AzureBlobStorage(AzureBlobStorage),
    Https(Https),
    Ftp(Ftp),
    Folder(Folder),
    Drs(Drs),
    Synapse(Synapse),
    Irods(Irods),
    Oci(Oci),
    Lakefs(Lakefs),
    Lamin(Lamin),
}

/// AWS S3, or a service that speaks its API: MinIO, Ceph, Cloudflare R2, RustFS and others.
#[derive(Clone, Debug, Default)]
pub struct S3 {
    /// The address of the service; AWS itself without one.
    pub endpoint: Option<String>,
    pub region: Option<String>,
    /// Buckets in the path (`endpoint/bucket/key`) instead of the host name
    /// (`bucket.endpoint/key`); by default when an endpoint is given.
    pub path_style: Option<bool>,
    pub access_key_id: Option<Secret>,
    pub secret_access_key: Option<Secret>,
    pub session_token: Option<Secret>,
    /// Requests without credentials, for public buckets.
    pub anonymous: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GoogleCloudStorage {
    pub endpoint: Option<String>,
    /// The JSON key file of a service account.
    pub credential_file: Option<PathBuf>,
    /// An OAuth 2.0 access token.
    pub token: Option<Secret>,
    pub anonymous: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AzureBlobStorage {
    /// The storage account.
    pub account: Option<String>,
    /// The address of the service; `https://<account>.blob.core.windows.net` without one.
    pub endpoint: Option<String>,
    /// The access key of the account.
    pub key: Option<Secret>,
    /// A shared access signature.
    pub sas_token: Option<Secret>,
}

/// Web servers, for example private ones that need a token.
#[derive(Clone, Debug, Default)]
pub struct Https {
    /// Sent as `Authorization: Bearer <token>`.
    pub token: Option<Secret>,
    /// Basic authentication.
    pub user: Option<String>,
    pub password: Option<Secret>,
    /// More headers for every request, each value a secret.
    pub headers: Vec<(String, Secret)>,
}

/// FTP servers, for inputs.
#[derive(Clone, Debug, Default)]
pub struct Ftp {
    /// The user, `anonymous` by default.
    pub user: Option<String>,
    pub password: Option<Secret>,
}

/// GA4GH DRS servers, for inputs. Unset values come from the environment.
#[derive(Clone, Debug, Default)]
pub struct Drs {
    /// `gen3`, `bearer` or `none`.
    pub authentication: Option<String>,
    /// The Gen3 API key or the bearer token; else `GEN3_API_KEY` of the environment.
    pub credential: Option<Secret>,
    /// For `gen3`: the path of the token exchange on the DRS server.
    pub token_path: Option<String>,
    /// The `type` of access method to prefer, like `s3` or `https`.
    pub access_protocol: Option<String>,
    /// Servers, as `host:port`, that serve plain HTTP instead of HTTPS.
    pub plain_http: Vec<String>,
}

/// Synapse, for inputs and outputs. Unset values come from the environment.
#[derive(Clone, Debug, Default)]
pub struct Synapse {
    /// The address of the REST API; Synapse itself without one.
    pub endpoint: Option<String>,
    /// A personal access token; else `SYNAPSE_AUTH_TOKEN` of the environment.
    pub token: Option<Secret>,
}

/// iRODS zones, over the iRODS protocol. Unset values come from the environment.
#[derive(Clone, Debug, Default)]
pub struct Irods {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub zone: Option<String>,
    pub password: Option<Secret>,
    /// `native`, or `pam_password` for a PAM password.
    pub authentication: Option<String>,
    /// The TLS policy: `CS_NEG_REQUIRE`, `CS_NEG_DONT_CARE` or `CS_NEG_REFUSE`.
    pub negotiation: Option<String>,
}

/// Oracle Cloud Object Storage, with the API key of a user. Unset values come from the
/// environment and the OCI config file.
#[derive(Clone, Debug, Default)]
pub struct Oci {
    pub profile: Option<String>,
    pub region: Option<String>,
    pub tenancy: Option<String>,
    pub user: Option<String>,
    pub fingerprint: Option<String>,
    /// The API key as PEM text.
    pub private_key: Option<Secret>,
    pub endpoint: Option<String>,
}

/// lakeFS repositories, through its REST API and presigned addresses.
#[derive(Clone, Debug, Default)]
pub struct Lakefs {
    /// The base of the API, like `https://lakefs.example.org/api/v1`.
    pub endpoint: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<Secret>,
    /// Whether writing to a missing branch makes it, from `branch_source`.
    pub create_branches: bool,
    pub branch_source: Option<String>,
}

/// LaminDB artifacts, found through LaminHub, for inputs. Unset values come from the
/// environment.
#[derive(Clone, Debug, Default)]
pub struct Lamin {
    pub api_key: Option<Secret>,
    /// The address of LaminHub, `https://hub.lamin.ai` by default.
    pub hub: Option<String>,
    /// The public anon key of LaminHub.
    pub anon_key: Option<Secret>,
    /// Whether S3 is read with the session credentials of LaminHub, true by default.
    pub hub_credentials: Option<bool>,
}

/// A folder on this machine, which holds the objects of the addresses it serves, like a mirror
/// of a bucket.
#[derive(Clone, Debug)]
pub struct Folder {
    pub path: PathBuf,
}

/// The `type` of each kind of provider.
const TYPES: [&str; 12] = [
    "s3",
    "google-cloud-storage",
    "azure-blob-storage",
    "https",
    "ftp",
    "folder",
    "drs",
    "synapse",
    "irods",
    "oci",
    "lakefs",
    "lamin",
];

impl Settings {
    /// The storage settings of a config file; none without `[storage]` and `[store]`.
    pub fn from_config(config: &ConfigFile) -> Result<Settings, ConfigError> {
        let mut providers = Vec::new();
        if let Some(storage) = config.section("storage")? {
            for table in storage.tables()? {
                providers.push(provider(&table)?);
            }
        }
        check_prefixes(&providers)?;
        let (store, local_store) = match config.section("store")? {
            Some(store) => {
                store.only(&["location", "local"])?;
                let location = store.text("location")?;
                let relative = location.as_deref().is_some_and(|location| {
                    address::scheme(location).is_none() && !Path::new(location).is_absolute()
                });
                if relative {
                    return Err(store.invalid(
                        "location",
                        "must be an absolute folder or an address, like \"s3://my-bucket/uwe\"",
                    ));
                }
                let local = store.text("local")?;
                if local
                    .as_deref()
                    .is_some_and(|local| !Path::new(local).is_absolute())
                {
                    return Err(store.invalid(
                        "local",
                        "must be an absolute folder of this machine, like \"/scratch/uwe\"",
                    ));
                }
                (location, local)
            }
            None => (None, None),
        };
        Ok(Settings {
            providers,
            store,
            local_store,
        })
    }
}

fn provider(table: &Table<'_>) -> Result<ProviderSettings, ConfigError> {
    let name = table.name().trim_start_matches("storage.").to_owned();
    let kind_name = table.text("type")?.ok_or_else(|| {
        table.missing(
            "type",
            format!("the kind of provider: one of {}", TYPES.join(", ")),
        )
    })?;
    let Some((known, schemes, build)) = kind_of(&kind_name) else {
        return Err(table.invalid(
            "type",
            format!(
                "is `{kind_name}`, which uwe does not know; use one of {}",
                TYPES.join(", ")
            ),
        ));
    };
    let mut keys = vec!["type", "prefixes"];
    keys.extend(known);
    table.only(&keys)?;
    let kind = build(table)?;
    let prefixes = table.texts("prefixes")?.unwrap_or_default();
    if prefixes.is_empty() {
        return Err(table.missing(
            "prefixes",
            "the addresses it serves, like prefixes = [\"s3://my-bucket/\"]",
        ));
    }
    for prefix in &prefixes {
        let scheme = address::scheme(prefix);
        let fits = match scheme {
            Some(scheme) => schemes.is_empty() || schemes.contains(&scheme),
            None => false,
        };
        if !fits {
            let wanted = match schemes {
                [] => "an address with a scheme, like \"s3://my-bucket/\"".to_owned(),
                schemes => format!("an address that starts with {}://", schemes.join("://, ")),
            };
            return Err(table.invalid(
                "prefixes",
                format!("holds \"{prefix}\"; a `{kind_name}` provider needs {wanted}"),
            ));
        }
    }
    // An account for a prefix without a host would go to every server, for files and Git alike.
    let signs_in =
        |https: &Https| https.token.is_some() || https.user.is_some() || !https.headers.is_empty();
    let hostless = |prefix: &&String| {
        let rest = prefix.split_once("://").map_or("", |(_, rest)| rest);
        rest.split('/').next().unwrap_or_default().is_empty()
    };
    if let Kind::Https(https) = &kind
        && signs_in(https)
        && let Some(prefix) = prefixes.iter().find(hostless)
    {
        return Err(table.invalid(
            "prefixes",
            format!(
                "holds \"{prefix}\", which serves every server, so its account would go to all \
                 of them; name the server, like \"https://git.example.org/\""
            ),
        ));
    }
    Ok(ProviderSettings {
        name,
        prefixes,
        kind,
        origin: table.origin().clone(),
    })
}

/// How a provider of a kind is read from its table.
type Build = fn(&Table<'_>) -> Result<Kind, ConfigError>;

/// The settings that a kind of provider takes besides `type` and `prefixes`, the schemes of
/// the addresses it serves (any for a folder), and how it is read.
fn kind_of(name: &str) -> Option<(&'static [&'static str], &'static [&'static str], Build)> {
    let kind: (&[&str], &[&str], Build) = match name {
        "s3" => (
            &[
                "endpoint",
                "region",
                "path_style",
                "access_key_id",
                "secret_access_key",
                "session_token",
                "anonymous",
            ],
            &["s3", "http", "https"],
            |table| Ok(Kind::S3(s3(table)?)),
        ),
        "google-cloud-storage" => (
            &["endpoint", "credential_file", "token", "anonymous"],
            &["gs", "http", "https"],
            |table| Ok(Kind::GoogleCloudStorage(google_cloud_storage(table)?)),
        ),
        "azure-blob-storage" => (
            &["account", "endpoint", "key", "sas_token"],
            &["az", "http", "https"],
            |table| Ok(Kind::AzureBlobStorage(azure_blob_storage(table)?)),
        ),
        "https" => (
            &["token", "user", "password", "headers"],
            &["http", "https"],
            |table| Ok(Kind::Https(https(table)?)),
        ),
        "ftp" => (&["user", "password"], &["ftp"], |table| {
            Ok(Kind::Ftp(Ftp {
                user: table.text("user")?,
                password: table.secret("password")?,
            }))
        }),
        "folder" => (&["path"], &[], |table| Ok(Kind::Folder(folder(table)?))),
        "drs" => (
            &[
                "authentication",
                "credential",
                "token_path",
                "access_protocol",
                "plain_http",
            ],
            &["drs"],
            |table| {
                Ok(Kind::Drs(Drs {
                    authentication: table.text("authentication")?,
                    credential: table.secret("credential")?,
                    token_path: table.text("token_path")?,
                    access_protocol: table.text("access_protocol")?,
                    plain_http: table.texts("plain_http")?.unwrap_or_default(),
                }))
            },
        ),
        "irods" => (
            &[
                "host",
                "port",
                "user",
                "zone",
                "password",
                "authentication",
                "negotiation",
            ],
            &["irods"],
            |table| Ok(Kind::Irods(irods(table)?)),
        ),
        "oci" => (
            &[
                "profile",
                "region",
                "tenancy",
                "user",
                "fingerprint",
                "private_key",
                "endpoint",
            ],
            &["oci"],
            |table| {
                Ok(Kind::Oci(Oci {
                    profile: table.text("profile")?,
                    region: table.text("region")?,
                    tenancy: table.text("tenancy")?,
                    user: table.text("user")?,
                    fingerprint: table.text("fingerprint")?,
                    private_key: table.secret("private_key")?,
                    endpoint: web_address(table, "endpoint")?,
                }))
            },
        ),
        "lakefs" => (
            &[
                "endpoint",
                "access_key",
                "secret_key",
                "create_branches",
                "branch_source",
            ],
            &["lakefs"],
            |table| {
                Ok(Kind::Lakefs(Lakefs {
                    endpoint: web_address(table, "endpoint")?,
                    access_key: table.text("access_key")?,
                    secret_key: table.secret("secret_key")?,
                    create_branches: table.flag("create_branches")?.unwrap_or(false),
                    branch_source: table.text("branch_source")?,
                }))
            },
        ),
        "lamin" => (
            &["api_key", "hub", "anon_key", "hub_credentials"],
            &["lamin"],
            |table| {
                Ok(Kind::Lamin(Lamin {
                    api_key: table.secret("api_key")?,
                    hub: web_address(table, "hub")?,
                    anon_key: table.secret("anon_key")?,
                    hub_credentials: table.flag("hub_credentials")?,
                }))
            },
        ),
        "synapse" => (&["endpoint", "token"], &["syn"], |table| {
            Ok(Kind::Synapse(Synapse {
                endpoint: web_address(table, "endpoint")?,
                token: table.secret("token")?,
            }))
        }),
        _ => return None,
    };
    Some(kind)
}

fn s3(table: &Table<'_>) -> Result<S3, ConfigError> {
    let settings = S3 {
        endpoint: web_address(table, "endpoint")?,
        region: table.text("region")?,
        path_style: table.flag("path_style")?,
        access_key_id: table.secret("access_key_id")?,
        secret_access_key: table.secret("secret_access_key")?,
        session_token: table.secret("session_token")?,
        anonymous: table.flag("anonymous")?.unwrap_or(false),
    };
    match (&settings.access_key_id, &settings.secret_access_key) {
        (Some(_), None) => {
            Err(table.missing("secret_access_key", "which goes with `access_key_id`"))
        }
        (None, Some(_)) => {
            Err(table.missing("access_key_id", "which goes with `secret_access_key`"))
        }
        (Some(_), Some(_)) if settings.anonymous => Err(table.invalid(
            "anonymous",
            "cannot be true for a provider with access keys",
        )),
        _ => Ok(settings),
    }
}

fn google_cloud_storage(table: &Table<'_>) -> Result<GoogleCloudStorage, ConfigError> {
    Ok(GoogleCloudStorage {
        endpoint: web_address(table, "endpoint")?,
        credential_file: table.text("credential_file")?.map(PathBuf::from),
        token: table.secret("token")?,
        anonymous: table.flag("anonymous")?.unwrap_or(false),
    })
}

fn azure_blob_storage(table: &Table<'_>) -> Result<AzureBlobStorage, ConfigError> {
    let settings = AzureBlobStorage {
        account: table.text("account")?,
        endpoint: web_address(table, "endpoint")?,
        key: table.secret("key")?,
        sas_token: table.secret("sas_token")?,
    };
    if settings.account.is_none() && settings.endpoint.is_none() {
        return Err(table.missing("account", "the storage account, like account = \"mylab\""));
    }
    if settings.key.is_some() && settings.account.is_none() {
        return Err(table.missing("account", "which goes with `key`"));
    }
    Ok(settings)
}

fn https(table: &Table<'_>) -> Result<Https, ConfigError> {
    let mut headers = Vec::new();
    if let Some(table) = table.table("headers")? {
        for name in table.keys() {
            let valid = !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte));
            if !valid {
                return Err(table.invalid(name, "is not a valid name of a header"));
            }
            if let Some(value) = table.secret(name)? {
                headers.push((name.to_owned(), value));
            }
        }
    }
    let settings = Https {
        token: table.secret("token")?,
        user: table.text("user")?,
        password: table.secret("password")?,
        headers,
    };
    if settings.password.is_some() && settings.user.is_none() {
        return Err(table.missing("user", "which goes with `password`"));
    }
    Ok(settings)
}

fn irods(table: &Table<'_>) -> Result<Irods, ConfigError> {
    let port = match table.integer("port")? {
        None => None,
        Some(port) => Some(
            u16::try_from(port)
                .map_err(|_| table.invalid("port", "must be a port number, like 1247"))?,
        ),
    };
    Ok(Irods {
        host: table.text("host")?,
        port,
        user: table.text("user")?,
        zone: table.text("zone")?,
        password: table.secret("password")?,
        authentication: table.text("authentication")?,
        negotiation: table.text("negotiation")?,
    })
}

fn folder(table: &Table<'_>) -> Result<Folder, ConfigError> {
    let path = table
        .text("path")?
        .ok_or_else(|| table.missing("path", "the folder, like path = \"/data/mirror\""))?;
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(table.invalid("path", "must be an absolute path, like \"/data/mirror\""));
    }
    Ok(Folder { path })
}

/// A setting that must be a web address, like an endpoint.
fn web_address(table: &Table<'_>, key: &str) -> Result<Option<String>, ConfigError> {
    let Some(text) = table.text(key)? else {
        return Ok(None);
    };
    match address::scheme(&text) {
        Some("http" | "https") => Ok(Some(text.trim_end_matches('/').to_owned())),
        _ => Err(table.invalid(
            key,
            "must be a web address that starts with https:// or http://",
        )),
    }
}

/// Two providers may not serve the same prefix, as then neither would be the longest.
fn check_prefixes(providers: &[ProviderSettings]) -> Result<(), ConfigError> {
    let mut seen: Vec<(&str, &ProviderSettings)> = Vec::new();
    for provider in providers {
        for prefix in &provider.prefixes {
            if let Some((_, other)) = seen.iter().find(|(seen, _)| *seen == prefix) {
                let message = format!(
                    "the storage providers `{}` and `{}` both serve \"{prefix}\"; give it to one \
                     of them",
                    other.name, provider.name
                );
                return Err(ConfigError::at(&provider.origin, message));
            }
            seen.push((prefix, provider));
        }
    }
    Ok(())
}

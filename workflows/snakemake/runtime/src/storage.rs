//! Maps storage addresses to job-local paths; planning assumes remote files are present.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Weak;

use python_evaluator::{Args, Error, Interpreter, Native, Value};

use crate::workflow::workflow_error;

/// Where Snakemake keeps local copies of files in storage.
const LOCAL_PREFIX: &str = ".snakemake/storage";

/// The tag of a file in storage, which holds its storage address.
pub const TAG: &str = "storage_object";

/// Checks the provider of a `storage:` directive. Its tag, or else the plugin name, names the
/// folder of its local copies, as `storage.<name>(query)` does without a directive.
pub fn check_provider(provider: Option<&str>) -> Result<(), Error> {
    match provider {
        None => Err(workflow_error(
            "Storage provider must be specified (provider=\"...\").",
        )),
        Some(provider) if provider != provider.to_lowercase() => {
            Err(workflow_error("Storage provider must be lowercase."))
        }
        Some("s3" | "gcs" | "azure" | "http" | "ftp" | "fs") => Ok(()),
        Some(provider) => Err(Error::unsupported(format!(
            "Snakemake storage plugin '{provider}'; mapped providers are s3, gcs, azure, http, ftp and fs; sftp, xrootd, zenodo and irods are unavailable"
        ))),
    }
}

/// Why a setting of a Snakemake storage plugin is refused, naming the setting of the uwe
/// config file that takes its place, since uwe reads providers from there.
pub fn plugin_setting(provider: &str, key: &str) -> String {
    let kind = match provider {
        "gcs" => "google-cloud-storage",
        "azure" => "azure-blob-storage",
        "http" => "https",
        "fs" => "folder",
        other => other,
    };
    let setting = match (provider, key) {
        ("s3" | "gcs" | "azure", "endpoint_url" | "endpoint") => Some("endpoint"),
        ("s3", "access_key" | "access_key_id") => Some("access_key_id"),
        ("s3", "secret_key" | "secret_access_key") => Some("secret_access_key"),
        ("s3", "token" | "session_token") => Some("session_token"),
        ("s3", "region" | "region_name") => Some("region"),
        ("s3", "addressing_style") => Some("path_style"),
        ("gcs" | "http", "token") => Some("token"),
        ("gcs", "credentials" | "credentials_file" | "credential_file") => Some("credential_file"),
        ("azure", "account_name" | "account") => Some("account"),
        ("azure", "account_key" | "access_key" | "key") => Some("key"),
        ("azure", "sas_token") => Some("sas_token"),
        ("http" | "ftp", "username" | "user") => Some("user"),
        ("http" | "ftp", "password") => Some("password"),
        _ => None,
    };
    let table = format!(
        "a [storage.<name>] table of the uwe config file with type = \"{kind}\" and the prefixes it serves"
    );
    match setting {
        Some(setting) => format!(
            "Snakemake storage setting '{key}' of provider {provider}; set `{setting}` in {table}"
        ),
        None => format!(
            "Snakemake storage setting '{key}' of provider {provider}; configure the provider in {table}"
        ),
    }
}

/// The storage plugin whose queries look like `query`, by its scheme.
fn infer_provider(query: &str) -> Result<&'static str, Error> {
    let scheme = query.split_once("://").map_or("", |(scheme, _)| scheme);
    Ok(match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => "http",
        "s3" => "s3",
        "gs" | "gcs" => "gcs",
        "ftp" | "ftps" => "ftp",
        "sftp" => "sftp",
        "az" => "azure",
        "root" | "roots" | "xroot" => "xrootd",
        "zenodo" => "zenodo",
        "irods" => "irods",
        "" | "file" => "fs",
        _ => {
            return Err(workflow_error(format!(
                "No storage provider found for query {query}. Either install the required storage plugin or check your query. Also consider to explicitly specify the storage provider to get a more informative error message."
            )));
        }
    })
}

/// The path of a file in storage below the folder of its provider: the address without its
/// scheme, query and fragment, as the plugins for HTTP, S3 and the file system make it.
fn local_suffix(query: &str) -> String {
    let rest = query.split_once("://").map_or(query, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    rest.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| if part == ".." { "_" } else { part })
        .collect::<Vec<_>>()
        .join("/")
}

/// A file in storage: its local path, with the storage address as a tag, and the flags of
/// the query.
pub fn storage_object(
    interpreter: &mut Interpreter,
    query: &Value,
    provider: Option<&str>,
) -> Result<Value, Error> {
    if let Value::List(_) = query.plain() {
        let items = interpreter.iterate(query)?;
        let objects = items
            .iter()
            .map(|item| storage_object(interpreter, item, provider))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Value::list(objects));
    }
    let text = interpreter.to_str(query)?;
    let name = match provider {
        Some(name) => name.to_owned(),
        None => infer_provider(&text)?.to_owned(),
    };
    check_provider(Some(&name))?;
    let address = match (name.as_str(), text.split_once("://")) {
        ("fs", None) => format!("file://{}", interpreter.host.resolve(&text).display()),
        ("gcs", Some(("gcs", rest))) => format!("gs://{rest}"),
        ("s3" | "gcs" | "azure" | "ftp", None) => {
            format!("{}://{text}", if name == "gcs" { "gs" } else { &name })
        }
        ("http", None) => {
            return Err(workflow_error(
                "HTTP storage needs an http:// or https:// query",
            ));
        }
        _ => text.clone(),
    };
    let local = format!("{LOCAL_PREFIX}/{name}/{}", local_suffix(&text));
    let object = crate::rule::retag(query, Value::str(local));
    Ok(object.with_tag(TAG, Value::str(address)))
}

pub fn default_file(
    interpreter: &mut Interpreter,
    value: &Value,
    provider: &str,
    prefix: &str,
) -> Result<Value, Error> {
    if value.tag(TAG).is_some() || value.tag("local").is_some() {
        return Ok(value.clone());
    }
    if let Value::List(_) = value.plain() {
        let items = interpreter.iterate(value)?;
        return items
            .iter()
            .map(|item| default_file(interpreter, item, provider, prefix))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::list);
    }
    let query = interpreter.to_str(value)?;
    let query = if prefix.is_empty() || query.contains("://") {
        query
    } else {
        format!("{}/{query}", prefix.trim_end_matches('/'))
    };
    storage_object(
        interpreter,
        &crate::rule::retag(value, Value::str(query)),
        Some(provider),
    )
}

/// The `storage` global: `storage(query)` and `storage.<name>(query)`.
pub struct StorageRegistry {
    pub state: Weak<RefCell<crate::workflow::State>>,
}

impl Native for StorageRegistry {
    fn type_name(&self) -> &str {
        "StorageRegistry"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        mut args: Args,
    ) -> Result<Option<Value>, Error> {
        let query = args
            .take(0, "query")
            .ok_or_else(|| Error::type_error("storage() missing 1 required argument: 'query'"))?;
        let state = self.state.upgrade();
        let profile = state.as_ref().map(|state| state.borrow().profile.clone());
        match profile.and_then(|profile| {
            profile
                .storage_provider
                .map(|provider| (provider, profile.storage_prefix.unwrap_or_default()))
        }) {
            Some((provider, prefix)) => {
                default_file(interpreter, &query, &provider, &prefix).map(Some)
            }
            None => storage_object(interpreter, &query, None).map(Some),
        }
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let name = self
            .state
            .upgrade()
            .and_then(|state| state.borrow().storage_providers.get(name).cloned())
            .unwrap_or_else(|| name.to_owned());
        check_provider(Some(&name))?;
        Ok(Some(Value::builtin(
            &name.clone(),
            move |interpreter, mut args| {
                let query = args.take(0, "query").ok_or_else(|| {
                    Error::type_error("storage provider missing 1 required argument: 'query'")
                })?;
                storage_object(interpreter, &query, Some(&name))
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_storage_paths() {
        assert_eq!(
            local_suffix("https://github.com/owner/repo/raw/main/data.txt?raw=1"),
            "github.com/owner/repo/raw/main/data.txt"
        );
        assert_eq!(local_suffix("s3://bucket/a/../b.txt"), "bucket/a/_/b.txt");
        assert_eq!(local_suffix("/mnt/data/x.txt"), "mnt/data/x.txt");
        assert_eq!(infer_provider("https://example.org/x").ok(), Some("http"));
        assert_eq!(infer_provider("gs://bucket/x").ok(), Some("gcs"));
        assert_eq!(infer_provider("data/x.txt").ok(), Some("fs"));
        assert!(infer_provider("unknown://x").is_err());
    }
}

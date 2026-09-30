//! Snakemake's storage: files in remote storage, like `storage.http(url)` or
//! `storage("s3://bucket/key")`. Snakemake's storage plugins download such an input into
//! `.snakemake/storage/<provider>/...` before the job runs, and the job sees that local path.
//! iunctus gives the job the same local path, and its task gets the file from the storage
//! address, which the executor downloads. While the workflow is planned, a file in storage
//! counts as present, as iunctus does not look into the storage itself.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

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
        Some(_) => Ok(()),
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
        "" => "fs",
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
fn storage_object(
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
    let local = format!("{LOCAL_PREFIX}/{name}/{}", local_suffix(&text));
    let object = crate::rule::retag(query, Value::str(local));
    Ok(object.with_tag(TAG, Value::str(text)))
}

/// The `storage` global: `storage(query)` and `storage.<name>(query)`.
pub struct StorageRegistry;

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
        storage_object(interpreter, &query, None).map(Some)
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let name = name.to_owned();
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

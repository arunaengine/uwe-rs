//! Reads LaminDB artifacts of `lamin://<owner>/<instance>/artifact/<uid>[/<path>]` addresses: it
//! finds where the artifact lives through LaminHub and the instance API, and reads it there.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Value, json};

use crate::error::Problem;
use crate::objects::{Objects, Service};
use crate::web::{Web, network, origin_client};
use crate::{Downloaded, Entry, Metadata};

/// What a LaminDB client signs in with. Unset values come from the environment.
#[derive(Clone, Default)]
pub struct Options {
    /// The LaminHub API key; else `LAMIN_API_KEY`. Without one, only public instances work.
    pub api_key: Option<String>,
    /// The address of LaminHub; else `SUPABASE_API_URL`, else `https://hub.lamin.ai`.
    pub hub: Option<String>,
    /// The public anon key of LaminHub; else `SUPABASE_ANON_KEY`.
    pub anon_key: Option<String>,
    /// Whether S3 storage is read with the session credentials of LaminHub (the default)
    /// rather than the AWS credentials of the environment.
    pub hub_credentials: Option<bool>,
}

/// The options of the `lamin` scope of a Nextflow config, for `lamin://` addresses that no
/// provider of the config file serves.
static SCOPE: Mutex<Option<Options>> = Mutex::new(None);

/// Uses these options for `lamin://` addresses that no provider of the config file serves,
/// from the first request on.
pub fn use_scope(options: Options) {
    *SCOPE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(options);
}

/// The options of the built-in provider.
pub(crate) fn scope() -> Options {
    let scope = SCOPE.lock().unwrap_or_else(|poison| poison.into_inner());
    scope.clone().unwrap_or_default()
}

const HUB: &str = "https://hub.lamin.ai";
const CONNECT: Duration = Duration::from_secs(5);
const REQUEST: Duration = Duration::from_secs(15);

pub(crate) struct Lamin {
    hub: String,
    api_key: Option<String>,
    anon_key: Option<String>,
    hub_credentials: bool,
    /// The bearer token of the instance APIs, fetched once.
    token: tokio::sync::OnceCell<String>,
    /// The settings of each instance, by `owner/name`.
    instances: tokio::sync::Mutex<HashMap<String, Value>>,
    /// The client of each storage root.
    roots: Mutex<HashMap<String, Arc<Root>>>,
}

/// Where an artifact lives: the address of its file or folder, and its record.
struct Located {
    address: String,
    root: Arc<Root>,
    record: Value,
}

/// The client of a storage root.
enum Root {
    Objects(Objects),
    Web(Web),
}

/// The parts of a `lamin://` address.
#[derive(Debug, PartialEq, Eq)]
struct Address<'a> {
    owner: &'a str,
    name: &'a str,
    uid: &'a str,
    path: &'a str,
}

impl Lamin {
    pub fn new(options: Options) -> Result<Lamin, Problem> {
        let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let hub = (options.hub)
            .or_else(|| variable("SUPABASE_API_URL"))
            .unwrap_or_else(|| HUB.to_owned());
        Ok(Lamin {
            hub: hub.trim_end_matches('/').to_owned(),
            api_key: options.api_key.or_else(|| variable("LAMIN_API_KEY")),
            anon_key: options.anon_key.or_else(|| variable("SUPABASE_ANON_KEY")),
            hub_credentials: options.hub_credentials.unwrap_or(true),
            token: tokio::sync::OnceCell::new(),
            instances: tokio::sync::Mutex::new(HashMap::new()),
            roots: Mutex::new(HashMap::new()),
        })
    }

    /// The secrets that messages must not show.
    pub fn secrets(&self) -> Vec<String> {
        let mut secrets: Vec<String> = self.api_key.iter().cloned().collect();
        secrets.extend(self.token.get().cloned());
        secrets
    }

    /// The artifact by its record, or a file or folder inside a folder artifact.
    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let Some(located) = self.locate(target).await? else {
            return Ok(None);
        };
        if address(target)?.path.is_empty() {
            let record = &located.record;
            let folder = record["n_files"].as_u64().is_some_and(|files| files > 1)
                || record["is_dir"].as_bool() == Some(true)
                || record["suffix"].as_str() == Some("");
            return Ok(Some(Metadata {
                directory: folder,
                modified: None,
                size: if folder {
                    0
                } else {
                    record["size"].as_u64().unwrap_or(0)
                },
                version: record["hash"]
                    .as_str()
                    .map(str::to_owned)
                    .filter(|_| !folder),
            }));
        }
        match located.root.as_ref() {
            Root::Objects(objects) => objects.stat(&located.address).await,
            Root::Web(web) => web.stat(&located.address).await,
        }
    }

    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let located = self.locate(target).await?.ok_or_else(Problem::missing)?;
        match located.root.as_ref() {
            Root::Objects(objects) => objects.list(&located.address).await,
            Root::Web(_) => Err(Problem::fixed(
                "web storage cannot list the files of a folder",
            )),
        }
    }

    /// Downloads the file of an artifact, whose version is the hash of its record as in
    /// [`Lamin::stat`], or a file inside a folder artifact.
    pub async fn download(&self, target: &str, file: &Path) -> Result<Downloaded, Problem> {
        let located = self.locate(target).await?.ok_or_else(Problem::missing)?;
        let downloaded = match located.root.as_ref() {
            Root::Objects(objects) => objects.download(&located.address, file, 0).await?,
            Root::Web(web) => web.download(&located.address, file, 0).await?,
        };
        let hash = located.record["hash"].as_str().map(str::to_owned);
        Ok(match address(target)?.path.is_empty() {
            true => Downloaded { version: hash },
            false => downloaded,
        })
    }

    /// The storage address of an artifact, from its record and the record of its storage.
    async fn locate(&self, target: &str) -> Result<Option<Located>, Problem> {
        let parts = address(target)?;
        let instance = self.instance(parts.owner, parts.name).await?;
        let Some(record) = self.record(&instance, "artifact", parts.uid).await? else {
            return Ok(None);
        };
        let default = &instance["storage"];
        let storage = match (&record["storage_id"], &default["id"]) {
            (wanted, known) if wanted == known || wanted.is_null() => default.clone(),
            (wanted, _) => {
                let id = text(wanted).unwrap_or_default();
                (self.record(&instance, "storage", &id).await?)
                    .ok_or_else(|| Problem::fixed(format!("the storage {id} is not found")))?
            }
        };
        let root = (storage["root"].as_str())
            .ok_or_else(|| Problem::fixed("the storage of the artifact names no root"))?;
        let mut address = format!("{}/{}", root.trim_end_matches('/'), key(&record)?);
        if !parts.path.is_empty() {
            address = format!("{address}/{}", parts.path);
        }
        let region = storage["region"].as_str().unwrap_or("us-east-1");
        let root = self.root(root, region).await?;
        Ok(Some(Located {
            address,
            root,
            record,
        }))
    }

    /// The settings of an instance, asked once.
    async fn instance(&self, owner: &str, name: &str) -> Result<Value, Problem> {
        let slug = format!("{owner}/{name}");
        let mut instances = self.instances.lock().await;
        if let Some(found) = instances.get(&slug) {
            return Ok(found.clone());
        }
        let body = json!({ "owner": owner, "name": name });
        let settings = self.hub_call("get-instance-settings-v1", body).await?;
        if let (Some(code), Some(message)) = (settings.get("code"), settings["message"].as_str()) {
            return Err(Problem::fixed(format!(
                "Failed to get instance settings for {slug}: {message} (code: {code})"
            )));
        }
        if settings
            .as_object()
            .is_none_or(|settings| settings.is_empty())
        {
            return Err(Problem::fixed(format!(
                "LaminDB instance {slug} could not be found."
            )));
        }
        instances.insert(slug, settings.clone());
        Ok(settings)
    }

    /// One record of a model of the core module; `None` for 404.
    async fn record(
        &self,
        instance: &Value,
        model: &str,
        id: &str,
    ) -> Result<Option<Value>, Problem> {
        let api = instance["api_url"]
            .as_str()
            .unwrap_or_default()
            .trim_end_matches('/');
        let instance_id = text(&instance["id"]).unwrap_or_default();
        let schema = text(&instance["schema_id"]).unwrap_or_default();
        let url = format!(
            "{api}/instances/{instance_id}/modules/core/{model}/{id}?schema_id={schema}&\
             include_foreign_keys=true"
        );
        let client = origin_client().map_err(Problem::fixed)?;
        let request = (client.post(url))
            .bearer_auth(self.token().await?)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .timeout(REQUEST)
            .body("{}");
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        let body = response.text().await.map_err(network)?;
        match status {
            StatusCode::NOT_FOUND => Ok(None),
            status if status.is_success() => {
                serde_json::from_str(&body).map(Some).map_err(|error| {
                    Problem::fixed(format!("the {model} record is not JSON: {error}"))
                })
            }
            status => Err(refused(status, &body)),
        }
    }

    /// The token of the instance APIs: a JWT for the API key, else the anon key.
    async fn token(&self) -> Result<&str, Problem> {
        let token = self.token.get_or_try_init(|| async {
            let anon = self.anon()?.to_owned();
            let Some(key) = &self.api_key else {
                return Ok(anon);
            };
            let answer = self
                .hub_post("get-jwt-v1", &anon, json!({ "api_key": key }))
                .await?;
            if let Some(error) = answer.get("error") {
                return Err(Problem::fixed(format!(
                    "Failed to fetch access token: {error}"
                )));
            }
            (answer["accessToken"].as_str().map(str::to_owned))
                .ok_or_else(|| Problem::fixed("Failed to fetch access token: no accessToken"))
        });
        token.await.map(String::as_str)
    }

    fn anon(&self) -> Result<&str, Problem> {
        self.anon_key.as_deref().ok_or_else(|| {
            Problem::fixed(
                "LaminHub needs its public anon key: set lamin.api.supabase_anon_key, `anon_key` \
                 of the storage provider, or SUPABASE_ANON_KEY",
            )
        })
    }

    /// A call of a LaminHub function with the token of the instance APIs.
    async fn hub_call(&self, function: &str, body: Value) -> Result<Value, Problem> {
        let token = self.token().await?.to_owned();
        self.hub_post(function, &token, body).await
    }

    async fn hub_post(&self, function: &str, bearer: &str, body: Value) -> Result<Value, Problem> {
        let client = origin_client().map_err(Problem::fixed)?;
        let url = format!("{}/functions/v1/{function}", self.hub);
        let request = (client.post(url))
            .bearer_auth(bearer)
            .header(CONTENT_TYPE, "application/json; utf-8")
            .header(ACCEPT, "application/json")
            .timeout(CONNECT + REQUEST)
            .body(body.to_string());
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        let text = response.text().await.map_err(network)?;
        if !status.is_success() {
            return Err(refused(status, &text));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// The client of a storage root, made once: S3 with the session credentials of LaminHub,
    /// Google Cloud Storage with those of the environment, or the web.
    async fn root(&self, root: &str, region: &str) -> Result<Arc<Root>, Problem> {
        if let Some(found) = self.known(root) {
            return Ok(found);
        }
        let made = match crate::address::scheme(root) {
            Some("http" | "https") => Root::Web(Web::public()?),
            Some("gs") => Root::Objects(Objects::new(Service::GoogleCloudStorage {
                endpoint: None,
                credential_file: None,
                token: None,
                anonymous: false,
            })?),
            Some("s3") => {
                let keys = match self.hub_credentials {
                    true => Some(self.session(root).await?),
                    false => None,
                };
                Root::Objects(Objects::new(Service::S3 {
                    endpoint: None,
                    region: region.to_owned(),
                    virtual_host: true,
                    keys,
                    anonymous: false,
                })?)
            }
            _ => {
                return Err(Problem::fixed(format!(
                    "uwe cannot read the LaminDB storage {root}"
                )));
            }
        };
        let made = Arc::new(made);
        let mut roots = self
            .roots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        roots.insert(root.to_owned(), made.clone());
        Ok(made)
    }

    fn known(&self, root: &str) -> Option<Arc<Root>> {
        let roots = self
            .roots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        roots.get(root).cloned()
    }

    /// Session credentials of LaminHub for an S3 storage root.
    async fn session(&self, root: &str) -> Result<(String, String, Option<String>), Problem> {
        let answer =
            (self.hub_call("get-cloud-access-v1", json!({ "storage_root": root }))).await?;
        let credentials = &answer["Credentials"];
        match (
            credentials["AccessKeyId"].as_str(),
            credentials["SecretAccessKey"].as_str(),
        ) {
            (Some(id), Some(secret)) => Ok((
                id.to_owned(),
                secret.to_owned(),
                credentials["SessionToken"].as_str().map(str::to_owned),
            )),
            _ => Err(Problem::fixed(format!(
                "LaminHub gives no credentials for {root}"
            ))),
        }
    }
}

/// The key of an artifact below its storage root: its own key, or `.lamindb/<uid><suffix>`
/// when the key is virtual or missing.
fn key(record: &Value) -> Result<String, Problem> {
    let suffix = record["suffix"].as_str().unwrap_or_default();
    let virtual_key = record["_key_is_virtual"].as_bool().unwrap_or(false);
    match record["key"].as_str() {
        Some(key) if !virtual_key => Ok(key.trim_start_matches('/').to_owned()),
        _ => {
            let uid = (record["uid"].as_str())
                .ok_or_else(|| Problem::fixed("the artifact record has no uid"))?;
            Ok(format!(".lamindb/{uid}{suffix}"))
        }
    }
}

/// Parses `lamin://<owner>/<instance>/artifact/<uid>[/<path>]`.
fn address(target: &str) -> Result<Address<'_>, Problem> {
    let invalid = || {
        Problem::fixed(format!(
            "{target} is not a LaminDB address like lamin://<owner>/<instance>/artifact/<uid>"
        ))
    };
    let rest = target.strip_prefix("lamin://").ok_or_else(invalid)?;
    let mut parts = rest.splitn(5, '/');
    let (owner, name, kind, uid) = (parts.next(), parts.next(), parts.next(), parts.next());
    let (Some(owner), Some(name), Some("artifact"), Some(uid)) = (owner, name, kind, uid) else {
        return Err(invalid());
    };
    if owner.is_empty() || name.is_empty() || uid.is_empty() {
        return Err(invalid());
    }
    Ok(Address {
        owner,
        name,
        uid,
        path: parts.next().unwrap_or_default().trim_end_matches('/'),
    })
}

/// An id, which LaminDB writes as text or as a number.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// An answer that is not a success; errors of the server may go away.
fn refused(status: StatusCode, body: &str) -> Problem {
    let detail: Value = serde_json::from_str(body).unwrap_or_default();
    let said = (detail["message"].as_str())
        .or_else(|| detail["detail"].as_str())
        .map(|said| format!(": {said}"))
        .unwrap_or_default();
    let message = format!("LaminDB answered {status}{said}");
    match status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        true => Problem::passing(message),
        false => Problem::fixed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        let parsed = address("lamin://laminlabs/lamindata/artifact/s3rtK8wIzJNKvg5Q/a/b.txt");
        assert_eq!(
            parsed.ok(),
            Some(Address {
                owner: "laminlabs",
                name: "lamindata",
                uid: "s3rtK8wIzJNKvg5Q",
                path: "a/b.txt",
            })
        );
        let bare = address("lamin://laminlabs/lamindata/artifact/s3rtK8wIzJNKvg5Q").ok();
        assert_eq!(bare.map(|parsed| parsed.path), Some(""));
        assert!(address("lamin://laminlabs/lamindata/transform/x").is_err());
        assert!(address("lamin://laminlabs/lamindata/artifact/").is_err());
    }

    #[test]
    fn keys_follow_records() {
        let real = json!({ "uid": "AbC", "key": "raw/a.csv", "suffix": ".csv" });
        assert_eq!(key(&real).ok().as_deref(), Some("raw/a.csv"));
        let hidden = json!({ "uid": "AbC", "key": "a.csv", "suffix": ".csv",
            "_key_is_virtual": true });
        assert_eq!(key(&hidden).ok().as_deref(), Some(".lamindb/AbC.csv"));
        let unnamed = json!({ "uid": "AbC", "key": null, "suffix": ".h5ad" });
        assert_eq!(key(&unnamed).ok().as_deref(), Some(".lamindb/AbC.h5ad"));
    }
}

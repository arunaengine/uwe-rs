//! Resolves registry secrets from iunctus, Docker and Podman without running helpers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use base64::Engine;
use config_file::{ConfigError, ConfigFile, RegistryAuth};

static CONFIGURED: RwLock<BTreeMap<String, RegistryAuth>> = RwLock::new(BTreeMap::new());

pub(crate) enum Credential {
    Basic(String, String),
    Bearer(String),
    Identity(String),
}

#[derive(Default)]
pub(crate) struct Found {
    pub credential: Option<Credential>,
    pub helper: bool,
}

impl Found {
    pub fn hint(&self) -> &'static str {
        if self.credential.is_some() {
            "; configured registry credentials were rejected"
        } else if self.helper {
            "; credentials are configured only through a credential helper (credsStore or credHelpers); iunctus cannot run external helpers, configure registry credentials directly"
        } else {
            "; no registry credentials were found in iunctus, Docker or Podman settings"
        }
    }
}

/// Installs the registry section of the merged settings for registry requests.
pub fn configure(config: &ConfigFile) -> Result<(), ConfigError> {
    let auth = config_file::registry_auth(config)?;
    *CONFIGURED
        .write()
        .unwrap_or_else(|poison| poison.into_inner()) = auth;
    Ok(())
}

pub(crate) fn lookup(host: &str) -> Result<Found, String> {
    let configured = CONFIGURED
        .read()
        .unwrap_or_else(|poison| poison.into_inner());
    lookup_in(host, &configured, &paths())
}

fn paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let docker = std::env::var_os("DOCKER_CONFIG")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".docker")));
    let mut paths = Vec::new();
    paths.extend(docker.map(|path| path.join("config.json")));
    paths.extend(
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(path).join("containers/auth.json")),
    );
    paths.extend(home.map(|home| home.join(".config/containers/auth.json")));
    paths
}

fn host_key(host: &str) -> &str {
    let host = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(host);
    let host = host
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or(host.trim_end_matches('/'));
    match host {
        "docker.io" | "index.docker.io" => "registry-1.docker.io",
        host => host,
    }
}

fn lookup_in(
    host: &str,
    configured: &BTreeMap<String, RegistryAuth>,
    paths: &[PathBuf],
) -> Result<Found, String> {
    if let Some((_, auth)) = configured
        .iter()
        .find(|(key, _)| host_key(key) == host_key(host))
    {
        let reveal =
            |secret: &config_file::Secret| secret.reveal().map_err(|error| error.to_string());
        let credential = match auth {
            RegistryAuth::Basic { username, password } => {
                Credential::Basic(reveal(username)?, reveal(password)?)
            }
            RegistryAuth::Token(token) => Credential::Bearer(reveal(token)?),
        };
        return Ok(Found {
            credential: Some(credential),
            helper: false,
        });
    }
    let mut found = Found::default();
    for path in paths {
        let Some(file) = read_file(path)? else {
            continue;
        };
        let entries = file.get("auths").and_then(serde_json::Value::as_object);
        if let Some(entry) = entries.and_then(|entries| {
            entries
                .iter()
                .find(|(key, _)| host_key(key) == host_key(host))
                .map(|(_, entry)| entry)
        }) {
            if let Some(token) = entry["identitytoken"]
                .as_str()
                .filter(|text| !text.is_empty())
            {
                found.credential = Some(Credential::Identity(token.to_owned()));
                return Ok(found);
            }
            if let Some(auth) = entry["auth"].as_str().filter(|text| !text.is_empty()) {
                let invalid = || "invalid registry auth encoding (credentials redacted)".to_owned();
                let bytes = base64::prelude::BASE64_STANDARD
                    .decode(auth)
                    .map_err(|_| invalid())?;
                let text = String::from_utf8(bytes).map_err(|_| invalid())?;
                let (user, password) = text.split_once(':').ok_or_else(invalid)?;
                found.credential = Some(Credential::Basic(user.to_owned(), password.to_owned()));
                return Ok(found);
            }
        }
        found.helper |= file["credsStore"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
            || file["credHelpers"]
                .as_object()
                .is_some_and(|helpers| helpers.keys().any(|key| host_key(key) == host_key(host)));
    }
    Ok(found)
}

fn read_file(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "invalid registry credential file (contents redacted)".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("cannot read registry credential file".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_source_order() {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            ["docker.json", "runtime.json", "podman.json"].map(|name| directory.path().join(name));
        for (index, path) in paths.iter().enumerate() {
            let auth = base64::prelude::BASE64_STANDARD.encode(format!("user:password{index}"));
            std::fs::write(
                path,
                format!(r#"{{"auths":{{"https://index.docker.io/v1/":{{"auth":"{auth}"}}}}}}"#),
            )
            .unwrap();
        }
        let config = ConfigFile::parse(
            "[registry.\"docker.io\"]\ntoken = \"configured\"",
            Path::new("config.toml"),
        )
        .unwrap();
        let configured = config_file::registry_auth(&config).unwrap();
        assert!(
            matches!(lookup_in("registry-1.docker.io", &configured, &paths).unwrap().credential, Some(Credential::Bearer(token)) if token == "configured")
        );
        for index in 0..3 {
            let found = lookup_in("registry-1.docker.io", &BTreeMap::new(), &paths).unwrap();
            assert!(
                matches!(found.credential, Some(Credential::Basic(user, password)) if user == "user" && password == format!("password{index}"))
            );
            std::fs::remove_file(&paths[index]).unwrap();
        }
        assert!(
            lookup_in("registry-1.docker.io", &BTreeMap::new(), &paths)
                .unwrap()
                .credential
                .is_none()
        );
    }

    #[test]
    fn bypasses_external_helpers() {
        let directory = tempfile::tempdir().unwrap();
        let docker = directory.path().join("config.json");
        let podman = directory.path().join("auth.json");
        std::fs::write(&docker, r#"{"credHelpers":{"host":"external"}}"#).unwrap();
        let found = lookup_in("host", &BTreeMap::new(), std::slice::from_ref(&docker)).unwrap();
        assert!(found.helper && found.credential.is_none());
        std::fs::write(
            &podman,
            r#"{"auths":{"host":{"identitytoken":"identity"}}}"#,
        )
        .unwrap();
        let found = lookup_in("host", &BTreeMap::new(), &[docker.clone(), podman]).unwrap();
        assert!(
            matches!(found.credential, Some(Credential::Identity(token)) if token == "identity")
        );
        std::fs::write(&docker, r#"{"credsStore":"external"}"#).unwrap();
        assert!(
            lookup_in("host", &BTreeMap::new(), std::slice::from_ref(&docker))
                .unwrap()
                .helper
        );
        std::fs::write(&docker, r#"{"auths":{"host":{"auth":"private-value"}}}"#).unwrap();
        let error = lookup_in("host", &BTreeMap::new(), &[docker])
            .err()
            .unwrap();
        assert!(!error.contains("private-value"));
    }
}

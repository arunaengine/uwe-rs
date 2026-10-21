//! Reads per-host registry credentials without exposing secret values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use crate::{ConfigError, ConfigFile, Secret};

#[derive(Clone, Debug)]
pub enum RegistryAuth {
    Basic { username: Secret, password: Secret },
    Token(Secret),
}

/// Credentials in `[registry."host"]`, resolved only when authentication is needed.
pub fn registry_auth(config: &ConfigFile) -> Result<BTreeMap<String, RegistryAuth>, ConfigError> {
    let mut hosts = BTreeMap::new();
    if let Some(section) = config.section("registry")? {
        for host in section.keys() {
            let table = section.table(host)?.expect("a listed table");
            table.only(&["username", "password", "token"])?;
            let auth = match (
                table.secret("username")?,
                table.secret("password")?,
                table.secret("token")?,
            ) {
                (Some(username), Some(password), None) => {
                    RegistryAuth::Basic { username, password }
                }
                (None, None, Some(token)) => RegistryAuth::Token(token),
                _ => {
                    return Err(
                        table.invalid("token", "needs either username and password, or token")
                    );
                }
            };
            hosts.insert(host.to_owned(), auth);
        }
    }
    Ok(hosts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn reads_registry_secrets() {
        let config = ConfigFile::parse(
            "[registry.\"example.org\"]\nusername = { environment = \"REGISTRY_USER\" }\npassword = \"private-value\"",
            Path::new("config.toml"),
        ).unwrap();
        let auth = registry_auth(&config).unwrap();
        assert!(!format!("{auth:?}").contains("private-value"));
        let RegistryAuth::Basic { username, password } = &auth["example.org"] else {
            panic!("Basic")
        };
        assert_eq!(username.variable(), Some("REGISTRY_USER"));
        assert_eq!(password.reveal().unwrap(), "private-value");
        for text in ["username = \"u\"", "token = \"t\"\npassword = \"p\""] {
            let config = ConfigFile::parse(
                &format!("[registry.host]\n{text}"),
                Path::new("config.toml"),
            )
            .unwrap();
            assert!(registry_auth(&config).is_err());
        }
    }
}

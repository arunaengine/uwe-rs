//! Reads Nextflow's local JSON secret store without exposing values in diagnostics.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::exception::Exception;
use crate::interpreter::{Error, Host};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub fn get<H: Host + ?Sized>(host: &H, name: &str) -> Result<Option<String>, Error> {
    let home = host
        .environment("NXF_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(host.environment("HOME").unwrap_or_default()).join(".nextflow")
        });
    let path = PathBuf::from(
        host.environment("NXF_SECRETS_FILE")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "secrets/store.json".into()),
    );
    let path = if path.is_absolute() {
        path
    } else {
        home.join(path)
    };
    Ok(load(&path)?.remove(name))
}

pub fn load(path: &Path) -> Result<BTreeMap<String, String>, Error> {
    let failed = |message: String| Error::Thrown {
        exception: Rc::new(Exception::new("AbortOperationException", message)),
        span: Default::default(),
    };
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(_) => {
            return Err(failed(format!(
                "Cannot read secret store file: {}",
                path.display()
            )));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|_| failed("Cannot inspect secret store file".into()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(failed(format!(
            "Invalid permissions for secret store file: {} - It is required that your secret store file is NOT accessible by others.",
            path.display()
        )));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|_| failed("Cannot read secret store file".into()))?;
    let crate::value::Value::List(rows) = crate::interpreter::parse_json(&text)
        .map_err(|_| failed("Invalid secret store JSON".into()))?
    else {
        return Err(failed("Invalid secret store JSON".into()));
    };
    let mut values = BTreeMap::new();
    for row in &rows.borrow().items {
        let crate::value::Value::Map(row) = row else {
            return Err(failed("Invalid secret store entry".into()));
        };
        let row = row.borrow();
        let field = |name: &str| -> Result<String, Error> {
            match row
                .entries
                .get(&crate::value::Key(crate::value::Value::string(name)))
            {
                Some(crate::value::Value::String(value)) => Ok(value.to_string()),
                _ => Err(failed(format!("Missing secret {name}"))),
            }
        };
        values.insert(field("name")?, field("value")?);
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enforces_store_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store.json");
        std::fs::write(&path, r#"[{"name":"TOKEN","value":"fixture"}]"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let values = load(&path).unwrap();
        assert_eq!(values["TOKEN"], "fixture");
        assert!(!values.contains_key("MISSING"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let error = load(&path).unwrap_err().to_string();
        assert!(error.contains("Invalid permissions"));
        assert!(!error.contains("fixture"));
        assert!(load(&root.path().join("missing")).unwrap().is_empty());
    }
}

#[cfg(test)]
mod locations {
    use super::*;
    struct Located(PathBuf);
    impl Host for Located {
        fn print(&mut self, _: &str) {}
        fn environment(&self, name: &str) -> Option<String> {
            match name {
                "NXF_HOME" => Some(self.0.to_string_lossy().into_owned()),
                "NXF_SECRETS_FILE" => Some("alternate.json".into()),
                _ => None,
            }
        }
    }
    #[test]
    fn resolves_relative_store() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("alternate.json");
        std::fs::write(&path, r#"[{"name":"TOKEN","value":"fixture"}]"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut host = Located(root.path().to_owned());
        assert_eq!(host.secret("TOKEN").unwrap().as_deref(), Some("fixture"));
        assert_eq!(host.secret("MISSING").unwrap(), None);
    }
}

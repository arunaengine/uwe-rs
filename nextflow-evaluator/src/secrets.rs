//! Reads Nextflow's local JSON secret store without exposing values in diagnostics.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::exception::Exception;
use crate::interpreter::{Error, Host};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

thread_local! {
    /// The values that the scripts of the compile on this thread read from the environment,
    /// while it notes them; no compiled document may hold them, as they can be credentials.
    static READ: std::cell::RefCell<Option<std::collections::BTreeSet<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `work`, a compile, while noting the values that its scripts read from the environment,
/// which [`holds_read`] then finds; nothing of one compile reaches another. Work inside a
/// compile notes into the set of that compile.
pub fn noting<T>(work: impl FnOnce() -> T) -> T {
    if READ.with(|read| read.borrow().is_some()) {
        return work();
    }
    let earlier = READ.with(|read| read.replace(Some(Default::default())));
    let made = work();
    READ.with(|read| read.replace(earlier));
    made
}

/// Notes a value that a script read from the environment, while a compile notes them.
pub(crate) fn note_read(value: &str) {
    READ.with(|read| {
        if let Some(read) = read.borrow_mut().as_mut() {
            read.insert(value.to_owned());
        }
    });
}

/// Whether `text` holds a value that a script of the compile read from the environment; an
/// empty value holds nothing to keep.
pub fn holds_read(text: &str) -> bool {
    READ.with(|read| {
        let read = read.borrow();
        let mut values = read.iter().flatten().filter(|value| !value.is_empty());
        values.any(|value| text.contains(value.as_str()))
    })
}

pub fn get<H: Host + ?Sized>(host: &H, name: &str) -> Result<Option<String>, Error> {
    Ok(load(&path(host))?.remove(name))
}

pub fn path<H: Host + ?Sized>(host: &H) -> PathBuf {
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
    if path.is_absolute() {
        path
    } else {
        home.join(path)
    }
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

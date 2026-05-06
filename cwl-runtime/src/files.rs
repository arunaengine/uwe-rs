//! File and Directory objects: derived fields, contents, listings, sizes, checksums and the
//! names of secondary files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_parser::iri;
use sha1::{Digest, Sha1};

use crate::value::{Map, Value};

/// The most `loadContents` reads; larger files are an error since CWL v1.2.
pub const CONTENTS_LIMIT: u64 = 64 * 1024;

/// How files that are not on disk count. The mock executor makes empty outputs and job files
/// often name data that is not there, so a mock run takes every file as present and empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub assume_present: bool,
}

/// The local path of a `file:` location.
pub fn local_path(location: &str) -> Option<PathBuf> {
    iri::file_path(location)
}

/// A location for a path or URL written in a document or job, relative to `base`.
pub fn resolve_location(base: &str, reference: &str) -> String {
    if iri::has_scheme(reference) || reference.starts_with("_:") {
        return reference.to_owned();
    }
    if reference.starts_with('/') {
        return iri::file_url(Path::new(reference));
    }
    let encoded = iri::file_url(Path::new(reference));
    iri::join(base, encoded.trim_start_matches("file://"))
}

/// The last segment of a location, decoded.
pub fn basename_of(location: &str) -> String {
    let path = match local_path(location) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => {
            let (without, _) = iri::split_fragment(location);
            without.to_owned()
        }
    };
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_owned()
}

/// Splits a name as Python's `os.path.splitext` does: `a.tar.gz` gives `a.tar` and `.gz`,
/// and leading dots do not start an extension.
pub fn split_extension(name: &str) -> (&str, &str) {
    if let Some(dot) = name.rfind('.')
        && name[..dot].chars().any(|c| c != '.')
    {
        return (&name[..dot], &name[dot..]);
    }
    (name, "")
}

/// The name of a secondary file: each leading `^` of the pattern removes one extension.
pub fn substitute(name: &str, pattern: &str) -> String {
    match pattern.strip_prefix('^') {
        Some(rest) => match name.rfind('.') {
            Some(dot) => substitute(&name[..dot], rest),
            None => format!("{name}{}", rest.trim_start_matches('^')),
        },
        None => format!("{name}{pattern}"),
    }
}

/// Calls `visit` on every File and Directory map inside a value, outermost first, including
/// those in `secondaryFiles` and `listing`.
pub fn visit_files(value: &mut Value, visit: &mut dyn FnMut(&mut Map)) {
    match value {
        Value::List(items) => {
            for item in items {
                visit_files(item, visit);
            }
        }
        Value::Map(map) => {
            if matches!(
                map.get("class").and_then(Value::as_str),
                Some("File" | "Directory")
            ) {
                visit(map);
            }
            for (_, item) in map.iter_mut() {
                visit_files(item, visit);
            }
        }
        _ => {}
    }
}

/// Resolves relative `location` and `path` fields of the file objects in a value against
/// `base`, as cwltool does for job files; `path` becomes the `location` when there is none.
pub fn resolve_locations(value: &mut Value, base: &str) {
    visit_files(value, &mut |file| {
        if let Some(Value::String(location)) = file.get("location") {
            let resolved = resolve_location(base, location);
            file.insert("location".into(), Value::String(resolved));
        } else if let Some(Value::String(path)) = file.get("path") {
            let resolved = resolve_location(base, path);
            file.insert("location".into(), Value::String(resolved));
        }
        file.shift_remove("path");
    });
}

/// Fills `location`, `basename`, `nameroot` and `nameext` of every file object, as cwltool's
/// `normalizeFilesDirs` does; literals without a location get a `_:` one from their content.
pub fn normalize(value: &mut Value) -> Result<(), String> {
    let mut error = None;
    visit_files(value, &mut |file| {
        if let Err(message) = normalize_one(file) {
            error.get_or_insert(message);
        }
    });
    match error {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

fn normalize_one(file: &mut Map) -> Result<(), String> {
    let directory = file.get("class").and_then(Value::as_str) == Some("Directory");
    if !file.contains_key("location") {
        if let Some(Value::String(path)) = file.get("path") {
            let location = resolve_location("file:///", path);
            file.insert("location".into(), Value::String(location));
        } else if directory {
            if !file.contains_key("listing") && !file.contains_key("basename") {
                return Err(
                    "a Directory needs a location, a path, or a basename and a listing".into(),
                );
            }
            let location = format!("_:{}", hash_of(&Value::Map(file.clone())));
            file.insert("location".into(), Value::String(location));
        } else {
            if !file.contains_key("contents") {
                return Err("a File needs a location, a path or contents".into());
            }
            let location = format!("_:{}", hash_of(&Value::Map(file.clone())));
            file.insert("location".into(), Value::String(location));
        }
    }
    let location = file
        .get("location")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if location.ends_with('/') && location.contains("://") {
        let trimmed = location.trim_end_matches('/').to_owned();
        file.insert("location".into(), Value::String(trimmed));
    }
    let has_basename = file
        .get("basename")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.is_empty());
    if !has_basename {
        let name = match location.strip_prefix("_:") {
            Some(id) => id.to_owned(),
            None => basename_of(&location),
        };
        file.insert("basename".into(), Value::String(name));
    }
    if !directory {
        let name = file
            .get("basename")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (root, extension) = split_extension(&name);
        file.insert("nameroot".into(), Value::string(root));
        file.insert("nameext".into(), Value::string(extension));
    }
    Ok(())
}

/// A short digest that names literal files and directories.
fn hash_of(value: &Value) -> String {
    hex(&Sha1::digest(value.to_json().as_bytes()))[..16].to_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The `sha1$...` checksum of some bytes.
pub fn checksum_of(bytes: &[u8]) -> String {
    format!("sha1${}", hex(&Sha1::digest(bytes)))
}

impl Access {
    /// The bytes of a file: from disk, or its `contents` for a literal.
    pub fn read(&self, file: &Map, limit: Option<u64>) -> Result<Vec<u8>, String> {
        let location = file
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(path) = local_path(location) {
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) if self.assume_present => return Ok(Vec::new()),
                Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
            };
            if let Some(limit) = limit
                && metadata.len() > limit
            {
                return Err(format!(
                    "{} is larger than {limit} bytes, the limit of loadContents",
                    path.display()
                ));
            }
            return std::fs::read(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()));
        }
        if let Some(Value::String(contents)) = file.get("contents") {
            return Ok(contents.clone().into_bytes());
        }
        if self.assume_present {
            return Ok(Vec::new());
        }
        Err(format!("cannot read `{location}`"))
    }

    /// Sets `contents` from the file, at most 64 KiB of UTF-8.
    pub fn load_contents(&self, file: &mut Map) -> Result<(), String> {
        if file
            .get("contents")
            .is_some_and(|contents| !contents.is_null())
        {
            return Ok(());
        }
        let bytes = self.read(file, Some(CONTENTS_LIMIT))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        file.insert("contents".into(), Value::String(text));
        Ok(())
    }

    /// Sets `size` when it is missing: from disk, from the contents of a literal, or 0 for an
    /// absent file that counts as present.
    pub fn add_size(&self, file: &mut Map) {
        if file.contains_key("size") {
            return;
        }
        let location = file
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let size = if let Some(path) = local_path(location) {
            match std::fs::metadata(&path) {
                Ok(metadata) => Some(metadata.len() as i64),
                Err(_) => self.assume_present.then_some(0),
            }
        } else if let Some(Value::String(contents)) = file.get("contents") {
            Some(contents.len() as i64)
        } else {
            self.assume_present.then_some(0)
        };
        if let Some(size) = size {
            file.insert("size".into(), Value::Integer(size));
        }
    }

    /// Sets `size` and `checksum` from the bytes of a file, as cwltool does for outputs.
    pub fn add_checksum(&self, file: &mut Map) -> Result<(), String> {
        let bytes = self.read(file, None)?;
        file.insert("size".into(), Value::Integer(bytes.len() as i64));
        file.insert("checksum".into(), Value::String(checksum_of(&bytes)));
        Ok(())
    }

    /// Whether a location names an existing file or directory.
    pub fn exists(&self, location: &str) -> bool {
        match local_path(location) {
            Some(path) => path.exists() || self.assume_present,
            None => true,
        }
    }

    /// Whether a file next to a primary file, like a secondary file, exists. When the primary
    /// file is on disk, the data is at hand, so the other file must be on disk too; else as
    /// [`Access::exists`].
    pub fn exists_next_to(&self, location: &str, primary: &str) -> bool {
        let on_disk = |location: &str| local_path(location).is_some_and(|path| path.exists());
        if on_disk(primary) && local_path(location).is_some() {
            return on_disk(location);
        }
        self.exists(location)
    }

    /// Fills the listings of the directories in a value, as cwltool's `get_listing`: those
    /// the value holds, not the ones a new listing brings, unless `deep`.
    pub fn load_listings(&self, value: &mut Value, deep: bool) {
        match value {
            Value::List(items) => {
                for item in items {
                    self.load_listings(item, deep);
                }
            }
            Value::Map(map) if map.get("class").and_then(Value::as_str) == Some("Directory") => {
                let listed = map.contains_key("listing");
                self.load_listing(map, deep);
                if listed && let Some(listing) = map.get_mut("listing") {
                    self.load_listings(listing, deep);
                }
            }
            Value::Map(map) => {
                for (_, item) in map.iter_mut() {
                    self.load_listings(item, deep);
                }
            }
            _ => {}
        }
    }

    /// Fills the `listing` of a directory from disk, one level or all levels deep. A listing
    /// that is already there stays; a directory that is not on disk gets an empty one.
    pub fn load_listing(&self, directory: &mut Map, deep: bool) {
        if !directory.contains_key("listing") {
            let location = directory
                .get("location")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let listing = match local_path(location) {
                Some(path) => list_directory(&path),
                None => Vec::new(),
            };
            directory.insert("listing".into(), Value::List(listing));
        }
        if deep && let Some(Value::List(items)) = directory.get_mut("listing") {
            for item in items {
                if let Value::Map(child) = item
                    && child.get("class").and_then(Value::as_str) == Some("Directory")
                {
                    self.load_listing(child, true);
                }
            }
        }
    }
}

/// The entries of a directory on disk, sorted by name.
fn list_directory(path: &Path) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let class = if path.is_dir() { "Directory" } else { "File" };
            let mut entry = Map::new();
            entry.insert("class".into(), Value::string(class));
            entry.insert("location".into(), Value::String(iri::file_url(&path)));
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if class == "File" {
                let (root, extension) = split_extension(&name);
                entry.insert("nameroot".into(), Value::string(root));
                entry.insert("nameext".into(), Value::string(extension));
            }
            entry.insert("basename".into(), Value::String(name));
            Value::Map(entry)
        })
        .collect()
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

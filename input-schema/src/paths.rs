//! Checks nf-schema path formats and existence for local files and directories.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use jsonschema::{Draft, Keyword, ValidationError};
use serde_json::Value as Json;

use crate::{DIRECTORY_PATH, FILE_PATH, is_remote};

/// A validator of draft 2020-12 whose `format` keyword checks the kind of the paths that texts
/// name, and whose `exists` keyword checks that they exist, as nf-schema does. Other formats
/// are not checked. Relative paths start at `base`.
pub(crate) fn validator(schema: &Json, base: &Path) -> Result<jsonschema::Validator, String> {
    let base: Arc<Path> = Arc::from(base);
    let formats = base.clone();
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .with_keyword("format", move |_, value, _| {
            Ok(Box::new(PathFormat {
                format: value.as_str().unwrap_or_default().to_owned(),
                base: formats.clone(),
            }))
        })
        .with_keyword("exists", move |parent, value, _| {
            let format = parent.get("format").and_then(Json::as_str);
            Ok(Box::new(Exists {
                wanted: value.as_bool(),
                what: match format {
                    Some(FILE_PATH) => "file",
                    Some(DIRECTORY_PATH) => "directory",
                    _ => "file or directory",
                },
                base: base.clone(),
            }))
        })
        .build(schema)
        .map_err(|error| error.to_string())
}

/// The local path that a text names, from `base`, with `~/` as the home folder; `None` for
/// remote addresses, glob patterns and empty texts, which the check does not look at.
fn local_path(text: &str, base: &Path) -> Option<PathBuf> {
    if text.is_empty() || is_remote(text) || text.contains(['*', '?', '[', '{']) {
        return None;
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Some(PathBuf::from(home).join(rest));
    }
    Some(base.join(text))
}

/// The `format` keyword: a text of the format `file-path` must not name a directory, and one of
/// the format `directory-path` must not name a file.
struct PathFormat {
    format: String,
    base: Arc<Path>,
}

impl PathFormat {
    fn problem(&self, instance: &Json) -> Option<String> {
        let text = instance.as_str()?;
        let wanted = match self.format.as_str() {
            FILE_PATH => false,
            DIRECTORY_PATH => true,
            _ => return None,
        };
        let path = local_path(text, &self.base)?;
        let directory = std::fs::metadata(path).ok()?.is_dir();
        match (wanted, directory) {
            (false, true) => Some(format!("names `{text}`, which is a directory, not a file")),
            (true, false) => Some(format!("names `{text}`, which is a file, not a directory")),
            _ => None,
        }
    }
}

impl<'i> Keyword<'i> for PathFormat {
    fn validate(&self, instance: &'i Json) -> Result<(), ValidationError<'i>> {
        match self.problem(instance) {
            Some(message) => Err(ValidationError::custom(message)),
            None => Ok(()),
        }
    }

    fn is_valid(&self, instance: &'i Json) -> bool {
        self.problem(instance).is_none()
    }
}

/// The `exists` keyword: `true` when the file or directory must exist, `false` when it must not.
struct Exists {
    wanted: Option<bool>,
    /// What the text names, in words: `file`, `directory`, or `file or directory`.
    what: &'static str,
    base: Arc<Path>,
}

impl Exists {
    fn problem(&self, instance: &Json) -> Option<String> {
        let wanted = self.wanted?;
        let text = instance.as_str()?;
        let path = local_path(text, &self.base)?;
        let what = self.what;
        match (path.exists(), wanted) {
            (false, true) => Some(format!("names the {what} `{text}`, which does not exist")),
            (true, false) => Some(format!("names the {what} `{text}`, which exists already")),
            _ => None,
        }
    }
}

impl<'i> Keyword<'i> for Exists {
    fn validate(&self, instance: &'i Json) -> Result<(), ValidationError<'i>> {
        match self.problem(instance) {
            Some(message) => Err(ValidationError::custom(message)),
            None => Ok(()),
        }
    }

    fn is_valid(&self, instance: &'i Json) -> bool {
        self.problem(instance).is_none()
    }
}

//! Hides the secret values and server folders that the options of an export name.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::Value;

use crate::Options;
use crate::ids::encode;

/// Every string and key of `value` as [`text`] gives it.
pub(crate) fn json(value: &mut Value, options: &Options) {
    match value {
        Value::String(string) => *string = text(string, options),
        Value::Array(values) => values.iter_mut().for_each(|value| json(value, options)),
        Value::Object(object) => {
            for (key, mut value) in std::mem::take(object) {
                json(&mut value, options);
                object.insert(text(&key, options), value);
            }
        }
        _ => {}
    }
}

/// `text` with each secret value as `[secret]` and each path in a server folder relative to
/// that folder, the folder itself as `.`.
pub(crate) fn text(text: &str, options: &Options) -> String {
    let mut text = text.to_owned();
    for secret in options.secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret.as_str(), "[secret]");
    }
    let mut folders: Vec<String> = (options.folders.iter())
        .map(|folder| folder.to_string_lossy().trim_end_matches('/').to_owned())
        .filter(|folder| !folder.is_empty())
        .collect();
    // A folder inside another one goes first, so that its paths keep its own name.
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.len()));
    for folder in folders {
        for form in [folder.clone(), encode(&folder)] {
            text = text.replace(&format!("file://{form}/"), "");
            text = relative(&text, &form);
        }
    }
    text
}

/// Whether the file holds one of the secret values, so that the crate must not copy it.
pub(crate) fn holds_secret(path: &Path, options: &Options) -> std::io::Result<bool> {
    let secrets: Vec<&[u8]> = (options.secrets.iter())
        .filter(|secret| !secret.is_empty())
        .map(|secret| secret.as_bytes())
        .collect();
    if secrets.is_empty() {
        return Ok(false);
    }
    let bytes = std::fs::read(path)?;
    Ok(secrets
        .iter()
        .any(|secret| (bytes.windows(secret.len())).any(|window| window == *secret)))
}

/// `text` with the paths in `folder` relative to it: only where the folder starts a path and
/// ends at a `/` or at the end of the path.
fn relative(text: &str, folder: &str) -> String {
    let path = |byte: u8| byte.is_ascii_alphanumeric() || b"._-/~%".contains(&byte);
    let bytes = text.as_bytes();
    let mut result = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, _) in text.match_indices(folder) {
        let end = at + folder.len();
        let starts = at == 0 || !path(bytes[at - 1]);
        let next = bytes.get(end).copied();
        // A `.` that ends a sentence ends the path too.
        let longer = match next {
            Some(b'/') | None => false,
            Some(b'.') => bytes.get(end + 1).is_some_and(|byte| path(*byte)),
            Some(byte) => path(byte),
        };
        if at < copied || !starts || longer {
            continue;
        }
        result.push_str(&text[copied..at]);
        match next {
            Some(b'/') => copied = end + 1,
            _ => {
                result.push('.');
                copied = end;
            }
        }
    }
    result.push_str(&text[copied..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_paths_relative() {
        let options = Options {
            secrets: vec!["s3cret".into()],
            folders: vec!["/srv/run".into(), "/srv/run/data/".into()],
            ..Options::default()
        };
        let cases = [
            ("/srv/run/work/a.txt", "work/a.txt"),
            (
                "cd /srv/run && cat /srv/run/data/in.txt",
                "cd . && cat in.txt",
            ),
            ("file:///srv/run/main.nf", "main.nf"),
            (
                "/srv/run2/a.txt /x/srv/run/a.txt",
                "/srv/run2/a.txt /x/srv/run/a.txt",
            ),
            ("TOKEN=s3cret", "TOKEN=[secret]"),
            (
                "from /srv/run. Then /srv/run.old",
                "from .. Then /srv/run.old",
            ),
        ];
        for (given, expected) in cases {
            assert_eq!(text(given, &options), expected, "{given}");
        }
    }
}

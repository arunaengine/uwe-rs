//! The image map of nf-biffix: every process container resolves through the run's map file, by
//! its exact reference or its normalised form, and a container outside the map fails its task.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

/// The image map of a run, or why it could not be read.
pub struct ImageMap {
    path: String,
    entries: Result<HashMap<String, String>, String>,
}

impl ImageMap {
    /// Reads the map once; a map that cannot be read fails each task that needs it.
    pub(crate) fn load(path: &str) -> ImageMap {
        let entries = read(path);
        if let Ok(entries) = &entries {
            tracing::info!(
                "biffix: containers resolve through {path} ({} entries)",
                entries.len()
            );
        }
        ImageMap {
            path: path.to_owned(),
            entries,
        }
    }

    /// The image to run for the container of a process, given as written and after the
    /// registry of the container engine.
    pub fn resolve(&self, process: &str, image: &str, resolved: &str) -> Result<String, String> {
        let entries = self.entries.as_ref().map_err(|error| {
            format!(
                "BIFFIX_IMAGE_MAP_FILE={}: cannot read the run's image map ({error})",
                self.path
            )
        })?;
        let exact = entries.get(image);
        let found = match image.starts_with("job-definition://") {
            true => exact,
            false => exact
                .or_else(|| normalise(image).and_then(|key| entries.get(&key)))
                .or_else(|| normalise(resolved).and_then(|key| entries.get(&key))),
        };
        found.cloned().ok_or_else(|| {
            format!(
                "Process `{process}` uses container '{image}', which biffix did not import for \
                 this workflow version: declare it under `images:` in biffix.yaml and re-register"
            )
        })
    }
}

/// The entries of the map file, with the normalised form of each key that is a reference.
fn read(path: &str) -> Result<HashMap<String, String>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    let object = json
        .as_object()
        .ok_or_else(|| format!("{path}: an image map is a JSON object"))?;
    let mut entries = HashMap::new();
    for (key, value) in object {
        let value = (value.as_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{path}: entry `{key}` is not an image reference"))?;
        entries.insert(key.clone(), value.to_owned());
        let bare = !key.contains(['/', ':', '@']);
        if let Some(normalised) = normalise(key).filter(|_| !bare) {
            entries.insert(normalised, value.to_owned());
        }
    }
    Ok(entries)
}

/// The full form of an image reference, like `docker.io/library/ubuntu:latest` for `ubuntu`,
/// after the grammar of container image references; `None` when it is not one.
fn normalise(reference: &str) -> Option<String> {
    let reference = reference.trim();
    let (name, digest) = match reference.rsplit_once('@') {
        Some((name, digest)) => (name, Some(digest_text(digest)?)),
        None => (reference, None),
    };
    let last = name.rfind('/').map_or(0, |place| place + 1);
    let (name, tag) = match name[last..].rfind(':') {
        Some(place) => (&name[..last + place], Some(&name[last + place + 1..])),
        None => (name, None),
    };
    if tag.is_some_and(|tag| !valid_tag(tag)) || name.is_empty() {
        return None;
    }
    let (registry, path) = match name.split_once('/') {
        Some((first, rest)) if first.contains(['.', ':', '[']) || first == "localhost" => {
            (first.to_lowercase(), rest)
        }
        _ => ("docker.io".to_owned(), name),
    };
    if !valid_registry(&registry) || !path.split('/').all(valid_component) {
        return None;
    }
    let registry = match registry.as_str() {
        "index.docker.io" | "registry-1.docker.io" => "docker.io".to_owned(),
        _ => registry,
    };
    let path = match registry == "docker.io" && !path.contains('/') {
        true => format!("library/{path}"),
        false => path.to_owned(),
    };
    let mut text = format!("{registry}/{path}");
    // The full name, as normalised, may have at most 255 characters.
    if text.len() > 255 {
        return None;
    }
    match (tag, &digest) {
        (Some(tag), _) => text.push_str(&format!(":{tag}")),
        (None, None) => text.push_str(":latest"),
        (None, Some(_)) => {}
    }
    if let Some(digest) = digest {
        text.push_str(&format!("@{digest}"));
    }
    Some(text)
}

/// A digest like `sha256:<64 hex digits>`, lower-cased.
fn digest_text(digest: &str) -> Option<String> {
    let (algorithm, hex) = digest.split_once(':')?;
    let algorithm_ok = !algorithm.is_empty()
        && (algorithm.chars()).all(|c| c.is_ascii_alphanumeric() || "+._-".contains(c));
    let length_ok = match algorithm {
        "sha256" => hex.len() == 64,
        _ => hex.len() >= 32,
    };
    let hex_ok = hex.chars().all(|c| c.is_ascii_hexdigit());
    (algorithm_ok && length_ok && hex_ok).then(|| digest.to_lowercase())
}

/// A tag: a word character, then up to 127 word characters, dots and dashes.
fn valid_tag(tag: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut chars = tag.chars();
    chars.next().is_some_and(word)
        && tag.len() <= 128
        && chars.all(|c| word(c) || c == '.' || c == '-')
}

/// A host name or an IPv6 address in brackets, with an optional port.
fn valid_registry(registry: &str) -> bool {
    let (host, port) = match registry.strip_prefix('[') {
        Some(rest) => {
            let Some((address, port)) = rest.split_once(']') else {
                return false;
            };
            let hex = |c: char| c.is_ascii_hexdigit() || c == ':';
            if address.is_empty() || !address.chars().all(hex) {
                return false;
            }
            (None, port)
        }
        None => match registry.split_once(':') {
            Some((host, _)) => (Some(host), &registry[host.len()..]),
            None => (Some(registry), ""),
        },
    };
    let digits = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
    let port_ok = port.is_empty() || port.strip_prefix(':').is_some_and(digits);
    let label_ok = |label: &str| {
        !label.is_empty()
            && (label.chars()).all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    port_ok && host.is_none_or(|host| host.split('.').all(label_ok))
}

/// A path component: lower-case letters and digits, joined by `.`, `_`, `__` or dashes.
fn valid_component(component: &str) -> bool {
    let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let bytes: Vec<char> = component.chars().collect();
    if bytes.is_empty() || !alnum(bytes[0]) || !alnum(bytes[bytes.len() - 1]) {
        return false;
    }
    let mut place = 0;
    while place < bytes.len() {
        if alnum(bytes[place]) {
            place += 1;
            continue;
        }
        let start = place;
        while place < bytes.len() && !alnum(bytes[place]) {
            place += 1;
        }
        let separator: String = bytes[start..place].iter().collect();
        let allowed =
            matches!(separator.as_str(), "." | "_" | "__") || separator.chars().all(|c| c == '-');
        if !allowed {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_references() {
        // The cases are biffix's own contract file, schemas/image-ref-cases.json at 4a95e52 (MIT).
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/nf_biffix/image-ref-cases.json"
        ))
        .expect("the cases");
        for case in cases.as_array().into_iter().flatten() {
            let input = case["input"].as_str().unwrap_or_default();
            let expected = case["normalized"].as_str().map(str::to_owned);
            assert_eq!(normalise(input), expected, "{input}");
        }
    }

    #[test]
    fn resolves_through_map() {
        let folder = tempfile::tempdir().expect("a folder");
        let path = folder.path().join("image-map.json");
        let mirror = "123456789012.dkr.ecr.us-east-1.amazonaws.com/mirror/ubuntu:24.04";
        let text = format!("{{\"docker.io/library/ubuntu:24.04\": \"{mirror}\"}}");
        std::fs::write(&path, text).expect("writes");
        let map = ImageMap::load(&path.to_string_lossy());
        assert_eq!(
            map.resolve("MAPPED", "ubuntu:24.04", "ubuntu:24.04"),
            Ok(mirror.into())
        );
        let missing = "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0";
        let error = map
            .resolve("MISSING", missing, missing)
            .expect_err("not mapped");
        assert!(
            error.starts_with("Process `MISSING` uses container 'quay.io/"),
            "{error}"
        );
        std::fs::write(&path, "[]").expect("writes");
        let error = ImageMap::load(&path.to_string_lossy()).resolve("P", "x", "x");
        assert!(error.is_err_and(|error| error.ends_with("an image map is a JSON object)")));
    }
}

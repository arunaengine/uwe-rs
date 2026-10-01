//! Resolves tagged, local, and remote wrapper scripts, caching downloads for real runs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use super::script::Source;
use crate::remote::Mirrors;

/// Where wrappers are unless the workflow names an address, as Snakemake's
/// `DEFAULT_WRAPPER_PREFIX`.
pub const DEFAULT_PREFIX: &str = "https://github.com/snakemake/snakemake-wrappers/raw/";

/// The repository of wrappers named by a tag.
const REPOSITORY: &str = "snakemake/snakemake-wrappers";

/// The script files of a wrapper folder, in the order Snakemake tries them.
const SCRIPTS: &[&str] = &["wrapper.py", "wrapper.R", "wrapper.Rmd", "wrapper.jl"];

/// Whether a name is an address rather than a name in the repository of wrappers.
fn is_address(name: &str) -> bool {
    name.starts_with("http") || name.starts_with("file:") || name.starts_with("git+file")
}

/// A tag like `v8.1.1` or `8.1.1`, which names a release of the repository.
fn is_version(reference: &str) -> bool {
    let digits = reference.strip_prefix('v').unwrap_or(reference);
    let parts: Vec<&str> = digits.splitn(3, '.').collect();
    parts.len() == 3
        && parts[..2]
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        && parts[2].chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// The folder or script of a wrapper: a local path or a web address.
pub fn location(name: &str, workdir: &Path) -> String {
    let name = name.trim_end_matches('/');
    if let Some(path) = name
        .strip_prefix("file://")
        .or_else(|| name.strip_prefix("file:"))
    {
        let path = Path::new(path);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workdir.join(path)
        };
        return python_evaluator::host::normalize(&absolute)
            .to_string_lossy()
            .into_owned();
    }
    if is_address(name) {
        return name.to_owned();
    }
    match name.split_once('/') {
        Some((reference, path)) if is_version(reference) => {
            format!("https://raw.githubusercontent.com/{REPOSITORY}/{reference}/{path}")
        }
        _ => format!("{DEFAULT_PREFIX}{name}"),
    }
}

/// The folder of a wrapper, whose `environment.yaml` is its Conda environment.
pub fn folder(location: &str) -> &str {
    if SCRIPTS.iter().any(|script| location.ends_with(script)) {
        location
            .rsplit_once('/')
            .map_or(location, |(folder, _)| folder)
    } else {
        location
    }
}

/// The script of a wrapper and its text, read unless the run is simulated and the script is on
/// the web.
pub fn source(
    name: &str,
    workdir: &Path,
    mirrors: &Mirrors,
    simulated: bool,
) -> Result<Source, String> {
    let location = location(name, workdir);
    let candidates: Vec<String> = if SCRIPTS.iter().any(|script| location.ends_with(script)) {
        vec![location.clone()]
    } else {
        SCRIPTS
            .iter()
            .map(|script| format!("{location}/{script}"))
            .collect()
    };
    let mut errors = Vec::new();
    for candidate in &candidates {
        if !candidate.contains("://") {
            match std::fs::read_to_string(candidate) {
                Ok(text) => {
                    return Ok(Source {
                        location: candidate.clone(),
                        text: Some(text),
                    });
                }
                Err(error) => errors.push(format!("{}: {error}", file_name(candidate))),
            }
            continue;
        }
        if simulated {
            return Ok(Source {
                location: candidate.clone(),
                text: None,
            });
        }
        let Some(local) = mirrors.url(candidate) else {
            errors.push(format!("{}: not a web address", file_name(candidate)));
            continue;
        };
        match mirrors
            .fetch(&local)
            .and_then(|()| std::fs::read_to_string(&local))
        {
            Ok(text) => {
                return Ok(Source {
                    location: candidate.clone(),
                    text: Some(text),
                });
            }
            Err(error) => errors.push(format!("{}: {error}", file_name(candidate))),
        }
    }
    Err(format!(
        "Unable to locate wrapper script in wrapper git repository ({location}). This can be a \
         network issue or a mistake in the wrapper URL. Encountered errors:\n{}",
        errors.join("\n")
    ))
}

fn file_name(location: &str) -> &str {
    location.rsplit('/').next().unwrap_or(location)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{folder, location};

    #[test]
    fn resolves_wrapper_paths() {
        let workdir = Path::new("/w");
        assert_eq!(
            location("v8.1.1/bio/bgzip", workdir),
            "https://raw.githubusercontent.com/snakemake/snakemake-wrappers/v8.1.1/bio/bgzip"
        );
        assert_eq!(
            location("master/bio/bgzip", workdir),
            "https://github.com/snakemake/snakemake-wrappers/raw/master/bio/bgzip"
        );
        assert_eq!(location("file://wrappers/x", workdir), "/w/wrappers/x");
        assert_eq!(location("file:///opt/x/", workdir), "/opt/x");
        assert_eq!(folder("/opt/x/wrapper.py"), "/opt/x");
        assert_eq!(folder("/opt/x"), "/opt/x");
    }
}

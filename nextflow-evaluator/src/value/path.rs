//! Paths as Nextflow shows them: local paths, and remote paths like `s3://` and `https://`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// An absolute, normalised path. A remote path keeps the part of its URI before the path:
/// `s3://bucket/key` has the prefix `s3:/` and the path `/bucket/key`, and
/// `https://host/a` has the prefix `https://host` and the path `/a`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PathValue {
    pub prefix: Option<String>,
    pub path: String,
    /// For an input file of a task, the name it is staged under, which is what it prints.
    pub staged: Option<String>,
    /// For a `new File(text)` with relative text, that text, which it prints; `path` is the
    /// file in the launch directory, where Java finds a relative file.
    pub relative: Option<String>,
}

impl PathValue {
    /// A path from text, relative paths taken from `base`, as `file()` makes them.
    pub fn parse(text: &str, base: &str) -> PathValue {
        if let Some((scheme, rest)) = text.split_once("://")
            && !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            let scheme = scheme.to_ascii_lowercase();
            if scheme == "file" {
                return PathValue::local(rest, base);
            }
            // Object stores keep the bucket in the path; web paths drop the host.
            if matches!(scheme.as_str(), "http" | "https" | "ftp") {
                let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
                return PathValue {
                    prefix: Some(format!("{scheme}://{host}")),
                    path: normalise(&format!("/{path}")),
                    staged: None,
                    relative: None,
                };
            }
            return PathValue {
                prefix: Some(format!("{scheme}:/")),
                path: normalise(&format!("/{rest}")),
                staged: None,
                relative: None,
            };
        }
        PathValue::local(text, base)
    }

    fn local(text: &str, base: &str) -> PathValue {
        let absolute = if text.starts_with('/') {
            text.to_owned()
        } else {
            format!("{base}/{text}")
        };
        PathValue {
            prefix: None,
            path: normalise(&absolute),
            staged: None,
            relative: None,
        }
    }

    /// The scheme, `file` for a local path.
    pub fn scheme(&self) -> &str {
        match &self.prefix {
            Some(prefix) => prefix.split(':').next().unwrap_or("file"),
            None => "file",
        }
    }

    /// `toUriString()`: the path with its scheme for a remote path, else the plain path.
    pub fn uri(&self) -> String {
        format!("{}{}", self.prefix.as_deref().unwrap_or(""), self.path)
    }

    /// The text of the path: the staged name of a task input, the text of a relative file,
    /// else the path.
    pub fn text(&self) -> &str {
        self.staged
            .as_deref()
            .or(self.relative.as_deref())
            .unwrap_or(&self.path)
    }

    /// `isAbsolute()`: false only for a file made from relative text.
    pub fn is_absolute(&self) -> bool {
        self.relative.is_none()
    }

    /// The file name; empty for the root. A task input's name is its staged name.
    pub fn name(&self) -> &str {
        match &self.staged {
            Some(staged) => staged,
            None => self.path.rsplit('/').next().unwrap_or(""),
        }
    }

    /// The name without its last extension: `sample.fastq.gz` gives `sample.fastq`.
    pub fn base_name(&self) -> &str {
        let name = self.name();
        name.rfind('.').map_or(name, |dot| &name[..dot])
    }

    /// The name up to the first dot after the first character: `sample.fastq.gz` gives `sample`.
    pub fn simple_name(&self) -> &str {
        let name = self.name();
        match name.get(1..).and_then(|rest| rest.find('.')) {
            Some(dot) => &name[..dot + 1],
            None => name,
        }
    }

    /// The text after the last dot of the name, or nothing.
    pub fn extension(&self) -> &str {
        let name = self.name();
        name.rfind('.').map_or("", |dot| &name[dot + 1..])
    }

    /// The parent directory; `None` for the root and for a task input.
    pub fn parent(&self) -> Option<PathValue> {
        if self.path == "/" || self.staged.is_some() {
            return None;
        }
        let end = self.path.rfind('/').unwrap_or(0);
        let parent = if end == 0 { "/" } else { &self.path[..end] };
        Some(PathValue {
            prefix: self.prefix.clone(),
            path: parent.to_owned(),
            staged: None,
            relative: None,
        })
    }

    /// `resolve(other)`: an absolute `other` replaces the path, else it is appended.
    pub fn resolve(&self, other: &str) -> PathValue {
        if other.is_empty() {
            return self.clone();
        }
        let path = if other.starts_with('/') {
            other.to_owned()
        } else {
            format!("{}/{other}", self.path)
        };
        PathValue {
            prefix: self.prefix.clone(),
            path: normalise(&path),
            staged: None,
            relative: None,
        }
    }

    /// `resolveSibling(other)`: resolved against the parent.
    pub fn resolve_sibling(&self, other: &str) -> PathValue {
        self.parent().unwrap_or_else(|| self.clone()).resolve(other)
    }

    /// `path + text`: a sibling whose name has the text appended.
    pub fn with_suffix(&self, suffix: &str) -> PathValue {
        PathValue {
            prefix: self.prefix.clone(),
            path: format!("{}{suffix}", self.path),
            staged: None,
            relative: None,
        }
    }
}

/// Removes `.`, `..`, repeated slashes and a trailing slash from an absolute path.
fn normalise(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

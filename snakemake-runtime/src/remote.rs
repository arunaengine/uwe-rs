//! Mirrors remote repositories and downloads workflow files while preserving relative paths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use storage::{Blocking, Storage, StorageError};

/// A local folder that mirrors a place on the web.
struct Mirror {
    folder: PathBuf,
    /// The web address of the folder, without a slash at the end.
    url: String,
}

/// The mirrors of a run and the files that the web does not have.
pub struct Mirrors {
    downloads: PathBuf,
    /// The storage providers that the files come from.
    storage: Blocking,
    mirrors: RefCell<Vec<Mirror>>,
    missing: RefCell<HashSet<PathBuf>>,
    /// The files of mirrors that this run read, so noted in its storage.
    read: RefCell<HashSet<PathBuf>>,
    /// The Git client for the repositories on the web of `gitfile(...)`, and the ones that
    /// this run fetched already.
    pub git: RefCell<Option<crate::gitfile::GitClient>>,
    fetched: RefCell<std::collections::HashMap<String, PathBuf>>,
}

/// A folder name for each part of a web address: parts like `..` or empty ones become `_`.
fn folder_of(parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .flat_map(|part| part.split('/'))
        .map(|part| match part {
            "" | "." | ".." => "_",
            part => part,
        })
        .collect()
}

impl Mirrors {
    pub fn stat(&self, address: &str) -> Result<Option<storage::Metadata>, StorageError> {
        self.storage.current(address)
    }

    pub fn stored(&self, address: &str) -> Result<PathBuf, StorageError> {
        self.storage.fetch(address, &self.downloads)
    }

    pub fn new(downloads: PathBuf, storage: &Storage) -> Mirrors {
        // A repeat reads pinned bytes, which must not replace the copies that other runs read;
        // `@` starts no host name, so no mirror of an address is in this folder.
        let downloads = match storage.pinned_name() {
            Some(name) => downloads.join("@pinned").join(name),
            None => downloads,
        };
        Mirrors {
            downloads,
            storage: storage.blocking(),
            mirrors: RefCell::new(Vec::new()),
            missing: RefCell::new(HashSet::new()),
            read: RefCell::default(),
            git: RefCell::new(None),
            fetched: RefCell::default(),
        }
    }

    /// The folder with the files of the repository of `gitfile(...)` at a reference: one of
    /// this machine read with gix, or one on the web fetched once per run.
    pub fn git_folder(&self, repository: &str, reference: &str) -> Result<PathBuf, String> {
        if !crate::gitfile::on_web(repository) {
            return crate::gitfile::local(Path::new(repository), reference, &self.downloads);
        }
        let client = self.git.borrow().clone().ok_or_else(|| {
            format!("{repository} is on the web, and this run has no Git client to fetch it")
        })?;
        let key = format!("{repository}@{reference}");
        if let Some(folder) = self.fetched.borrow().get(&key) {
            return Ok(folder.clone());
        }
        let folder = crate::gitfile::web(&client, repository, reference, &self.downloads)?;
        self.fetched.borrow_mut().insert(key, folder.clone());
        Ok(folder)
    }

    fn add(&self, folder: PathBuf, url: String) -> PathBuf {
        let mut mirrors = self.mirrors.borrow_mut();
        if !mirrors.iter().any(|mirror| mirror.folder == folder) {
            mirrors.push(Mirror {
                folder: folder.clone(),
                url: url.trim_end_matches('/').to_owned(),
            });
        }
        folder
    }

    /// The local path of a file in a git repository on GitHub or GitLab at a tag, branch or
    /// commit, with the web addresses of its raw files as Snakemake builds them.
    pub fn hosted(
        &self,
        provider: &str,
        host: &str,
        repository: &str,
        reference: &str,
        path: &str,
    ) -> PathBuf {
        let url = match (provider, host) {
            ("github", "github.com") => {
                format!("https://raw.githubusercontent.com/{repository}/{reference}")
            }
            ("github", host) => format!("https://{host}/{repository}/raw/{reference}"),
            (_, host) => format!("https://{host}/{repository}/-/raw/{reference}"),
        };
        let folder = self
            .downloads
            .join(folder_of(&[host, repository, "@", reference]));
        self.add(folder, url).join(path.trim_matches('/'))
    }

    /// The local path of a file at a web address, or `None` for other paths.
    pub fn url(&self, address: &str) -> Option<PathBuf> {
        let (scheme, rest) = address.split_once("://")?;
        if !matches!(scheme, "http" | "https") {
            return None;
        }
        let (server, path) = rest.split_once('/').unwrap_or((rest, ""));
        let folder = self.downloads.join(folder_of(&[scheme, server]));
        Some(
            self.add(folder, format!("{scheme}://{server}"))
                .join(folder_of(&[path])),
        )
    }

    /// The local path of a file that Snakemake's short form names, like
    /// `gh:owner/repo@v1.0:workflow/Snakefile` or `gl:host:group/repo@main`; the path is
    /// `workflow/Snakefile` unless given. `None` for other texts.
    pub fn shorthand(&self, text: &str) -> Option<PathBuf> {
        if text.contains("://") {
            return None;
        }
        let (provider, remainder) = text.split_once(':')?;
        let provider = match provider {
            "gh" => "github",
            "gl" => "gitlab",
            _ => return None,
        };
        let slash = remainder.find('/')?;
        let (host, remainder) = match remainder.find(':') {
            Some(colon) if colon < slash => {
                let host = &remainder[..colon];
                if host.is_empty() {
                    return None;
                }
                (Some(host), &remainder[colon + 1..])
            }
            _ => (None, remainder),
        };
        let (repository, reference_and_path) = remainder.split_once('@')?;
        if repository.is_empty() || reference_and_path.is_empty() || !repository.contains('/') {
            return None;
        }
        let (reference, path) = match reference_and_path.rsplit_once(':') {
            Some((reference, path)) if !path.is_empty() => (reference, path),
            Some((reference, _)) => (reference, "workflow/Snakefile"),
            None => (reference_and_path, "workflow/Snakefile"),
        };
        if reference.is_empty() {
            return None;
        }
        let host = host.unwrap_or(if provider == "github" {
            "github.com"
        } else {
            "gitlab.com"
        });
        Some(self.hosted(provider, host, repository, reference, path))
    }

    /// The web address of a local path in a mirror.
    pub fn address(&self, path: &Path) -> Option<String> {
        let mirrors = self.mirrors.borrow();
        mirrors.iter().find_map(|mirror| {
            let rest = path.strip_prefix(&mirror.folder).ok()?;
            let parts: Vec<String> = rest
                .components()
                .map(|component| match component {
                    Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Option<_>>()?;
            Some(format!("{}/{}", mirror.url, parts.join("/")))
        })
    }

    /// A text with the local copies of files on the web replaced by their addresses.
    pub fn describe(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for mirror in self.mirrors.borrow().iter() {
            let folder = format!("{}/", mirror.folder.display());
            text = text.replace(&folder, &format!("{}/", mirror.url));
        }
        text
    }

    /// Downloads a file of a mirror that is not there yet, or notes the copy that an earlier run
    /// left there, once per run. Paths outside the mirrors, folders and files that the web does
    /// not have are left as they are.
    pub fn fetch(&self, path: &Path) -> std::io::Result<()> {
        let done = self.read.borrow().contains(path) || self.missing.borrow().contains(path);
        if done || path.is_dir() {
            return Ok(());
        }
        let Some(url) = self.address(path) else {
            return Ok(());
        };
        // A copy from an earlier run is read by this one too, which a repeat's pin replaces.
        let copied = if path.is_file() {
            self.storage.note_copy(&url, path)
        } else {
            std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
            // The storage renames a private copy into place and notes that path; its providers
            // share one client per process, so a host is looked up once and its connection reused.
            self.storage.copy_of(&url, path)
        };
        match copied {
            Ok(()) => {
                self.read.borrow_mut().insert(path.to_owned());
                Ok(())
            }
            Err(StorageError::Missing { .. }) => {
                self.missing.borrow_mut().insert(path.to_owned());
                Ok(())
            }
            Err(error) => Err(std::io::Error::other(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_storage_providers() {
        use std::io::{BufRead, BufReader, Write};
        // A web server that answers only requests with the token of its provider.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let server = format!("http://{}", listener.local_addr().expect("an address"));
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
                let (mut first, mut allowed) = (String::new(), false);
                let _ = reader.read_line(&mut first);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 2 {
                    allowed |= line
                        .trim()
                        .eq_ignore_ascii_case("authorization: Bearer t0k3n");
                    line.clear();
                }
                let found = first.contains(" /rules.smk ");
                let (status, body) = match (allowed, found) {
                    (false, _) => ("401 Unauthorized", ""),
                    (true, true) => ("200 OK", "rule a:\n    output: 'a'\n"),
                    (true, false) => ("404 Not Found", ""),
                };
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        let text = format!(
            "[storage.rules]\ntype = \"https\"\nprefixes = [\"{server}/\"]\ntoken = \"t0k3n\"\n"
        );
        let config = config_file::ConfigFile::parse(&text, Path::new("config.toml")).expect("TOML");
        let storage = Storage::from_config(&config).expect("valid settings");
        let downloads = tempfile::tempdir().expect("a folder");
        let mirrors = Mirrors::new(downloads.path().to_owned(), &storage);
        let rules = mirrors
            .url(&format!("{server}/rules.smk"))
            .expect("a web address");
        mirrors.fetch(&rules).expect("a download");
        assert_eq!(
            std::fs::read_to_string(&rules).unwrap(),
            "rule a:\n    output: 'a'\n"
        );
        // A file that the server does not have is left out, without an error.
        let missing = mirrors
            .url(&format!("{server}/other.smk"))
            .expect("a web address");
        mirrors.fetch(&missing).expect("no error");
        assert!(!missing.exists());
    }

    #[test]
    fn maps_remote_paths() {
        let mirrors = Mirrors::new(PathBuf::from("/downloads"), &Storage::default());
        let snakefile = mirrors.hosted(
            "github",
            "github.com",
            "owner/repo",
            "v1.0",
            "workflow/Snakefile",
        );
        assert_eq!(
            snakefile,
            Path::new("/downloads/github.com/owner/repo/@/v1.0/workflow/Snakefile")
        );
        assert_eq!(
            mirrors.address(&snakefile.with_file_name("rules/common.smk")),
            Some(
                "https://raw.githubusercontent.com/owner/repo/v1.0/workflow/rules/common.smk"
                    .into()
            )
        );
        let gitlab = mirrors.hosted("gitlab", "gitlab.com", "group/repo", "main", "Snakefile");
        assert_eq!(
            mirrors.address(&gitlab),
            Some("https://gitlab.com/group/repo/-/raw/main/Snakefile".into())
        );
        let included = mirrors
            .url("https://example.org/a/../Snakefile")
            .expect("a web address");
        assert_eq!(
            included,
            Path::new("/downloads/https/example.org/a/_/Snakefile")
        );
        assert_eq!(mirrors.url("/local/Snakefile"), None);
        assert_eq!(
            mirrors.shorthand("gh:owner/repo@v1.0"),
            Some(PathBuf::from(
                "/downloads/github.com/owner/repo/@/v1.0/workflow/Snakefile"
            ))
        );
        assert_eq!(
            mirrors.shorthand("gl:example.org:group/repo@main:Snakefile"),
            Some(PathBuf::from(
                "/downloads/example.org/group/repo/@/main/Snakefile"
            ))
        );
        assert_eq!(mirrors.shorthand("gh:repo@main"), None);
        assert_eq!(mirrors.shorthand("rules/gh:x.smk"), None);
        assert_eq!(mirrors.address(Path::new("/elsewhere/Snakefile")), None);
    }
}

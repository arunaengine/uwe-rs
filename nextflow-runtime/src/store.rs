//! Checks and copies persistent process outputs through the workflow storage host.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::glob::{FileType, GlobOptions};
use nextflow_evaluator::interpreter::{
    Error, Host, RenderedOutput, RenderedPart, TaskDirective, glob,
};
use nextflow_evaluator::value::{PathValue, Value};

pub(crate) fn directory(directives: &[TaskDirective], host: &mut dyn Host) -> Option<PathValue> {
    let value = directives
        .iter()
        .rev()
        .find(|directive| &*directive.name == "storeDir")?
        .arguments
        .first()?;
    value.truth().then(|| match value {
        Value::Path(path) => (**path).clone(),
        value => PathValue::parse(&value.to_string(), &host.launch_dir()),
    })
}

pub(crate) fn files(
    outputs: &[RenderedOutput],
    directory: &PathValue,
    host: &mut dyn Host,
) -> Result<Option<Vec<(String, PathValue)>>, Error> {
    if outputs
        .iter()
        .flat_map(|output| &output.parts)
        .any(|part| !matches!(part, RenderedPart::Value(_) | RenderedPart::Path { .. }))
    {
        return Err(Error::Unsupported {
            message: "storeDir with outputs other than path or val".into(),
            span: Default::default(),
        });
    }
    if host.metadata(directory)?.is_none()
        && (directory.prefix.is_none() || host.list(directory)?.is_empty())
    {
        return Ok(None);
    }
    let mut files = Vec::new();
    for part in outputs.iter().flat_map(|output| &output.parts) {
        match part {
            RenderedPart::Value(_) => {}
            RenderedPart::Path { patterns, .. } => {
                for pattern in patterns {
                    let options = GlobOptions {
                        file_type: FileType::Any,
                        ..GlobOptions::default()
                    };
                    for path in glob::glob(host, &directory.resolve(pattern).uri(), &options)? {
                        let name = path
                            .path
                            .strip_prefix(&format!("{}/", directory.path.trim_end_matches('/')))
                            .unwrap_or(&path.path)
                            .to_owned();
                        if !files.iter().any(|(known, _)| known == &name) {
                            files.push((name, path));
                        }
                    }
                }
            }
            _ => {
                return Err(Error::Unsupported {
                    message: "storeDir with outputs other than path or val".into(),
                    span: Default::default(),
                });
            }
        }
    }
    Ok(Some(files))
}

pub(crate) fn save(
    files: &mut [(String, PathValue)],
    directory: &PathValue,
    host: &mut dyn Host,
) -> Result<(), Error> {
    host.create_directories(directory)?;
    for (name, source) in files {
        let target = directory.resolve(name);
        if source != &target {
            copy(source, &target, host)?;
        }
        *source = target;
    }
    Ok(())
}

/// Copies a file or directory output into the store. A local directory is copied whole into
/// a hidden temporary sibling first and renamed into place, so that a crash never leaves a
/// partial entry that a later run takes as stored.
fn copy(source: &PathValue, target: &PathValue, host: &mut dyn Host) -> Result<(), Error> {
    let directory = host
        .metadata(source)?
        .is_some_and(|metadata| metadata.directory);
    if !directory || target.prefix.is_some() {
        return copy_tree(source, target, host);
    }
    let failed = |error: std::io::Error| {
        let message = format!("{}: {error}", target.path);
        Error::Thrown {
            exception: std::rc::Rc::new(Exception::new("IOException", message)),
            span: Default::default(),
        }
    };
    let path = std::path::Path::new(&target.path);
    let parent = path.parent().unwrap_or(std::path::Path::new("/"));
    std::fs::create_dir_all(parent).map_err(failed)?;
    let ready = tempfile::Builder::new()
        .prefix(".storing-")
        .tempdir_in(parent)
        .map_err(failed)?;
    let staged = ready.path().join("content");
    copy_tree(
        source,
        &PathValue::parse(&staged.to_string_lossy(), "/"),
        host,
    )?;
    // An older or partial entry moves aside, and goes with the temporary folder.
    match std::fs::rename(path, ready.path().join("old")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(failed(error)),
        _ => {}
    }
    std::fs::rename(&staged, path).map_err(failed)
}

fn copy_tree(source: &PathValue, target: &PathValue, host: &mut dyn Host) -> Result<(), Error> {
    if host
        .metadata(source)?
        .is_some_and(|metadata| metadata.directory)
    {
        let entries = host.list(source)?;
        if entries.is_empty() && target.prefix.is_some() {
            return Err(Error::Unsupported {
                message: "empty directory outputs in an object store storeDir".into(),
                span: Default::default(),
            });
        }
        host.create_directories(target)?;
        for (name, _) in entries {
            copy_tree(&source.resolve(&name), &target.resolve(&name), host)?;
        }
    } else {
        if let Some(parent) = target.parent() {
            host.create_directories(&parent)?;
        }
        let bytes = host.read(source)?;
        host.write(target, &bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nextflow_evaluator::interpreter::FileMetadata;
    use std::collections::BTreeMap;

    struct Remote(BTreeMap<String, Vec<u8>>);
    impl Host for Remote {
        fn print(&mut self, _: &str) {}
        fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
            let directory = self
                .0
                .keys()
                .any(|name| name.starts_with(&format!("{}/", path.uri().trim_end_matches('/'))));
            Ok(self
                .0
                .get(&path.uri())
                .map(|bytes| FileMetadata {
                    directory: false,
                    size: bytes.len() as u64,
                    modified: 0,
                    mode: 0o777,
                })
                .or(directory.then_some(FileMetadata {
                    directory: true,
                    size: 0,
                    modified: 0,
                    mode: 0o777,
                })))
        }
        fn read(&mut self, path: &PathValue) -> Result<Vec<u8>, Error> {
            Ok(self.0[&path.uri()].clone())
        }
        fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
            self.0.insert(path.uri(), bytes.to_vec());
            Ok(())
        }
        fn create_directories(&mut self, _: &PathValue) -> Result<(), Error> {
            Ok(())
        }
        fn list(&mut self, path: &PathValue) -> Result<Vec<(String, bool)>, Error> {
            let prefix = format!("{}/", path.uri().trim_end_matches('/'));
            Ok(self
                .0
                .keys()
                .filter_map(|name| {
                    name.strip_prefix(&prefix)
                        .filter(|name| !name.contains('/'))
                        .map(|name| (name.to_owned(), false))
                })
                .collect())
        }
    }

    #[test]
    fn copies_object_outputs() {
        let mut host = Remote(BTreeMap::from([(
            "s3://bucket/task/result.txt".into(),
            b"result".to_vec(),
        )]));
        let directory = PathValue::parse("s3://bucket/stored", "/");
        let mut produced = vec![(
            "result.txt".into(),
            PathValue::parse("s3://bucket/task/result.txt", "/"),
        )];
        save(&mut produced, &directory, &mut host).unwrap();
        assert_eq!(host.0["s3://bucket/stored/result.txt"], b"result");
        assert!(host.0.contains_key("s3://bucket/task/result.txt"));
        let outputs = vec![RenderedOutput {
            parts: vec![RenderedPart::Path {
                patterns: vec!["*.txt".into()],
                options: Value::map([]),
            }],
            tuple: false,
            emit: None,
            topic: None,
            optional: false,
            typed: None,
            hidden: false,
        }];
        assert_eq!(
            files(&outputs, &directory, &mut host).unwrap().unwrap()[0]
                .1
                .uri(),
            "s3://bucket/stored/result.txt"
        );
    }

    /// Reads local files, but fails for one name, as a crash partway through a copy would.
    struct Failing(&'static str);
    impl Host for Failing {
        fn print(&mut self, _: &str) {}
        fn read(&mut self, path: &PathValue) -> Result<Vec<u8>, Error> {
            if path.name() == self.0 {
                return Err(Error::Unsupported {
                    message: "stopped".into(),
                    span: Default::default(),
                });
            }
            std::fs::read(&path.path).map_err(|error| Error::Unsupported {
                message: error.to_string(),
                span: Default::default(),
            })
        }
    }

    /// A directory output appears in the store only whole: a copy that fails partway leaves
    /// no entry, and a later copy replaces a partial one.
    #[test]
    fn stores_whole_directories() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("task/out");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("a.txt"), "a").unwrap();
        std::fs::write(output.join("b.txt"), "b").unwrap();
        let text = |path: &std::path::Path| PathValue::parse(&path.to_string_lossy(), "/");
        let store = text(&root.path().join("store"));
        let mut produced = vec![("out".to_owned(), text(&output))];
        assert!(save(&mut produced, &store, &mut Failing("b.txt")).is_err());
        assert!(!root.path().join("store/out").exists());
        std::fs::create_dir_all(root.path().join("store/out")).unwrap();
        std::fs::write(root.path().join("store/out/a.txt"), "partial").unwrap();
        let mut produced = vec![("out".to_owned(), text(&output))];
        save(&mut produced, &store, &mut Failing("none")).unwrap();
        let stored = root.path().join("store/out");
        assert_eq!(std::fs::read_to_string(stored.join("a.txt")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(stored.join("b.txt")).unwrap(), "b");
        let names: Vec<_> = std::fs::read_dir(root.path().join("store"))
            .unwrap()
            .collect();
        assert_eq!(names.len(), 1, "no temporary folder is left");
    }
}

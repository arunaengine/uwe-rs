//! Checks and copies persistent process outputs through the workflow storage host.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

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

fn copy(source: &PathValue, target: &PathValue, host: &mut dyn Host) -> Result<(), Error> {
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
            copy(&source.resolve(&name), &target.resolve(&name), host)?;
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
        }];
        assert_eq!(
            files(&outputs, &directory, &mut host).unwrap().unwrap()[0]
                .1
                .uri(),
            "s3://bucket/stored/result.txt"
        );
    }
}

//! Arguments into inputs: where the artifacts of a template come from.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::Node;
use argo_parser::model::Artifact;

use crate::node::{ArtifactValue, Location};
use crate::run::{Failure, Result, Run, scalar_text};

impl Run {
    /// The artifact that a declared artifact names by its location, if it has one.
    pub(crate) fn artifact_value(&mut self, artifact: &Artifact) -> Result<ArtifactValue> {
        let location = self.location(artifact)?;
        Ok(ArtifactValue {
            name: artifact.name.clone(),
            path: artifact.path.clone(),
            location,
            optional: artifact.optional,
            directory: artifact.git.is_some(),
        })
    }

    fn location(&mut self, artifact: &Artifact) -> Result<Option<Location>> {
        let text = |node: &Node, key: &str| node.get(key).map(scalar_text).unwrap_or_default();
        if let Some(raw) = &artifact.raw {
            return Ok(Some(Location::Raw(text(raw, "data"))));
        }
        let url = if let Some(s3) = &artifact.s3 {
            let key = text(s3, "key");
            let bucket = text(s3, "bucket");
            if key.is_empty() && bucket.is_empty() {
                return Ok(None);
            }
            let bucket = if bucket.is_empty() {
                self.warn("artifacts with only a key use the default artifact repository, here s3://default-artifact-repository".to_owned());
                "default-artifact-repository".to_owned()
            } else {
                bucket
            };
            format!("s3://{bucket}/{}", key.trim_start_matches('/'))
        } else if let Some(http) = &artifact.http {
            text(http, "url")
        } else if let Some(git) = &artifact.git {
            let repository = text(git, "repo");
            match text(git, "revision") {
                revision if revision.is_empty() => repository,
                revision => format!("{repository}#{revision}"),
            }
        } else if let Some(gcs) = &artifact.gcs {
            format!("gs://{}/{}", text(gcs, "bucket"), text(gcs, "key"))
        } else if let Some(azure) = &artifact.azure {
            format!(
                "{}/{}/{}",
                text(azure, "endpoint").trim_end_matches('/'),
                text(azure, "container"),
                text(azure, "blob")
            )
        } else if let Some(artifactory) = &artifact.artifactory {
            text(artifactory, "url")
        } else if let Some(hdfs) = &artifact.hdfs {
            format!("hdfs://{}", text(hdfs, "path").trim_start_matches('/'))
        } else if let Some(oss) = &artifact.oss {
            format!("oss://{}/{}", text(oss, "bucket"), text(oss, "key"))
        } else if artifact.plugin.is_some() {
            return Err(Failure::unsupported(format!(
                "artifact {}: artifact plugins are not supported",
                artifact.name
            )));
        } else {
            return Ok(None);
        };
        // An artifact that a task of this run wrote is read from the task's output.
        if let Some(file) = self.store.get(&url) {
            return Ok(Some(Location::File(file.clone())));
        }
        Ok(Some(Location::Url(url)))
    }
}

//! Bounds hosted workflow submissions to server-approved tenants and presets.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::Failure;
use crate::application::Admission;

/// The embedding service authorizes submitted values before planning and normalized values
/// before execution.
pub type Authorization = dyn Fn(&serde_json::Value) -> Result<(), Failure> + Send + Sync;

/// Server-owned policy for an approved immutable catalogue preset.
/// The embedding service controls filesystem roots and executor configuration.
#[derive(Clone)]
pub struct Hosted {
    tenant: String,
    base: PathBuf,
    presets: BTreeSet<String>,
    callback: Arc<Authorization>,
    pub namespace: Option<String>,
    pub host_roots: Vec<PathBuf>,
    pub volumes: BTreeMap<String, PathBuf>,
    pub locks: argo_runtime::SharedLocks,
}

impl Hosted {
    /// The base directory must already exist and be controlled by the embedding service.
    pub fn new(
        tenant: impl Into<String>,
        base: impl Into<PathBuf>,
        presets: BTreeSet<String>,
        callback: Arc<Authorization>,
    ) -> Result<Self, Failure> {
        let tenant = tenant.into();
        if tenant.is_empty()
            || !tenant
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(Failure::setup(
                "hosted tenant must be a nonempty safe identifier",
            ));
        }
        if presets.is_empty() || presets.iter().any(|name| name.trim().is_empty()) {
            return Err(Failure::setup(
                "hosted policy requires approved preset names",
            ));
        }
        let base = base.into();
        let base = base.canonicalize().map_err(|error| failed(&base, error))?;
        if !base.is_dir() {
            return Err(Failure::setup("hosted base must be a directory"));
        }
        Ok(Self {
            tenant,
            base,
            presets,
            callback,
            namespace: None,
            host_roots: Vec::new(),
            volumes: BTreeMap::new(),
            locks: argo_runtime::SharedLocks::default(),
        })
    }

    /// Returns the tenant root for `cache`, `runs.sqlite` and `outputs`.
    /// These paths must remain under service control throughout execution.
    pub fn check(&self, preset: Option<&str>) -> Result<PathBuf, Failure> {
        if !preset.is_some_and(|name| self.presets.contains(name)) {
            return Err(Failure::new(
                "hosted submission requires an approved preset",
            ));
        }
        let base = self
            .base
            .canonicalize()
            .map_err(|error| failed(&self.base, error))?;
        if base != self.base {
            return Err(Failure::setup(
                "hosted base changed after policy construction",
            ));
        }
        let root = self.base.join(&self.tenant);
        for path in [
            root.clone(),
            root.join("cache"),
            root.join("runs.sqlite"),
            root.join("outputs"),
        ] {
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(Failure::setup(format!(
                        "hosted path must not be a symlink: {}",
                        path.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(failed(&path, error)),
            }
        }
        Ok(root)
    }

    /// Authorizes merged submitted values before driver construction can read input paths.
    pub fn authorize_values(&self, values: &serde_json::Value) -> Result<(), Failure> {
        (self.callback)(values)
    }

    /// Rechecks normalized values after strict typed admission succeeds.
    pub fn authorize(&self, admission: &Admission) -> Result<(), Failure> {
        self.authorize_values(
            admission
                .inputs
                .as_ref()
                .map_or(&serde_json::Value::Null, |inputs| &inputs.values),
        )
    }
}

/// What workflow code may reach while uwe interprets it, for a host that runs the workflows of
/// many users in one process. `Default` reaches nothing of the service user.
#[derive(Clone, Debug, Default)]
pub struct Isolation {
    /// The folders that workflow code may read and write besides those of the run, like the
    /// input and output folders of the host.
    pub roots: Vec<PathBuf>,
    /// The folders that workflow code may read and list but never change, like shared input
    /// data of the host. A folder of `roots` inside one stays writable.
    pub read_only: Vec<PathBuf>,
    /// The folder that the run starts in, which relative paths start from and workflow code
    /// may reach; `None` keeps the working directory of the process, which it may not. An
    /// embedded Nextflow run then starts in a folder of its own instead.
    pub launch: Option<PathBuf>,
    /// The variables of the process environment that a run may read besides its secret
    /// values, like `PATH`.
    pub variables: BTreeSet<String>,
    /// Reads the Nextflow secret store of the service user, as `NXF_SECRETS_FILE` names it.
    pub secret_store: bool,
    /// Reads the Docker and Podman registry credentials of the service user.
    pub registry_files: bool,
    /// The hosts of private networks that inputs may come from, like a data server of the
    /// lab; every other web, bucket or FTP address must be on a public network.
    pub allowed_hosts: Vec<String>,
}

fn failed(path: &Path, error: std::io::Error) -> Failure {
    Failure::setup(format!(
        "cannot check hosted path {}: {error}",
        path.display()
    ))
}

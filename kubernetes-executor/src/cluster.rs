//! How the backend reaches the cluster: a client of the Kubernetes API, made from the kubeconfig
//! that the run names. It never reads the default kubeconfig or its contexts, so that no run
//! reaches a cluster by accident, and it needs no program like `kubectl`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::path::Path;

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, Config};

/// The cluster of a kubeconfig, and the namespace of the Jobs.
#[derive(Clone)]
pub struct Cluster {
    client: Client,
    namespace: String,
}

impl fmt::Debug for Cluster {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Cluster {{ namespace: {:?} }}", self.namespace)
    }
}

impl Cluster {
    /// The cluster of the current context of the kubeconfig `file`, with the Jobs in
    /// `namespace`, else in the namespace of that context, else in `default`. The client runs
    /// on the Tokio runtime that calls this.
    pub async fn connect(file: &Path, namespace: Option<String>) -> Result<Cluster, String> {
        let failed = |error: &dyn fmt::Display| {
            format!("cannot use the kubeconfig {}: {error}", file.display())
        };
        let kubeconfig = Kubeconfig::read_from(file).map_err(|error| failed(&error))?;
        let options = KubeConfigOptions::default();
        let config = Config::from_custom_kubeconfig(kubeconfig, &options)
            .await
            .map_err(|error| failed(&error))?;
        let namespace = namespace.unwrap_or_else(|| config.default_namespace.clone());
        let client = Client::try_from(config).map_err(|error| failed(&error))?;
        Ok(Cluster { client, namespace })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn jobs(&self) -> Api<Job> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    pub fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }
}

//! Selects a Kubernetes API client from explicit host configuration or pod credentials.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::path::Path;

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Event, Pod};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, Config};

/// An authenticated cluster client, its server identity and the namespace of the Jobs.
#[derive(Clone)]
pub struct Cluster {
    client: Client,
    namespace: String,
    server: String,
}

impl fmt::Debug for Cluster {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Cluster {{ namespace: {:?} }}", self.namespace)
    }
}

impl Cluster {
    /// Uses the host's authenticated client and explicit server identity for journal fencing.
    pub fn new(client: Client, namespace: String, server: String) -> Cluster {
        Cluster {
            client,
            namespace,
            server,
        }
    }

    /// Uses the pod's service account without consulting a host kubeconfig.
    pub fn in_cluster(namespace: Option<String>) -> Result<Cluster, String> {
        let config = Config::incluster().map_err(|error| error.to_string())?;
        let namespace = namespace.unwrap_or_else(|| config.default_namespace.clone());
        let server = config.cluster_url.to_string();
        let client = Client::try_from(config).map_err(|error| error.to_string())?;
        Ok(Self::new(client, namespace, server))
    }

    /// The cluster of the current context of the kubeconfig `file`, with the Jobs in
    /// `namespace`, else in the namespace of that context, else in `default`. The client runs
    /// on the Tokio runtime that calls this.
    pub async fn connect(file: &Path, namespace: Option<String>) -> Result<Cluster, String> {
        Cluster::connect_context(file, namespace, None).await
    }

    /// The cluster of the context `context` of the kubeconfig `file`, or of its current
    /// context, as [`Cluster::connect`] finds it.
    pub async fn connect_context(
        file: &Path,
        namespace: Option<String>,
        context: Option<String>,
    ) -> Result<Cluster, String> {
        let failed = |error: &dyn fmt::Display| {
            format!("cannot use the kubeconfig {}: {error}", file.display())
        };
        let kubeconfig = Kubeconfig::read_from(file).map_err(|error| failed(&error))?;
        let options = KubeConfigOptions {
            context,
            ..KubeConfigOptions::default()
        };
        let config = Config::from_custom_kubeconfig(kubeconfig, &options)
            .await
            .map_err(|error| failed(&error))?;
        let namespace = namespace.unwrap_or_else(|| config.default_namespace.clone());
        let server = config.cluster_url.to_string();
        let client = Client::try_from(config).map_err(|error| failed(&error))?;
        Ok(Self::new(client, namespace, server))
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn jobs(&self) -> Api<Job> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    pub fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    pub fn events(&self) -> Api<Event> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }
}

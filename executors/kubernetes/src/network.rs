//! Cuts the pods of offline tasks off the network with a NetworkPolicy for each Job.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{DeleteParams, PostParams, Preconditions};
use serde_json::{Value, json};

use crate::cluster::Cluster;
use crate::manifest::{LABEL, RUN};
use executor::ExecutorError;
use executor::poll::REQUEST_GRACE;

/// The label of the pods of one Job, which its NetworkPolicy selects.
pub const JOB: &str = "uwe.dev/job";

/// How the backend cuts the pods of offline tasks off the network: a NetworkPolicy for each Job
/// that denies all egress but to these peers. The cluster must enforce NetworkPolicies, and
/// staging must run outside the pod, like kubedal or a shared volume, or reach only the peers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Isolation {
    /// Where the pods may still connect, like the object store of helper staging.
    pub peers: Vec<Peer>,
    /// Whether the pods may still ask the DNS service of the cluster for names.
    pub dns: bool,
}

/// An address block that the pods of offline tasks may reach, on some ports or all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub cidr: String,
    pub ports: Vec<u16>,
}

impl Peer {
    /// A peer as `<address>/<length>`, with `:<port>` or `:<port>,<port>` for some ports.
    pub fn parse(text: &str) -> Result<Peer, String> {
        let invalid = || format!("`{text}` is no address block like 10.0.0.0/8:443");
        let (address, rest) = text.split_once('/').ok_or_else(invalid)?;
        let (length, ports) = rest.split_once(':').unwrap_or((rest, ""));
        let length: u8 = length.parse().map_err(|_| invalid())?;
        let ip: std::net::IpAddr = address.parse().map_err(|_| invalid())?;
        if length > if ip.is_ipv4() { 32 } else { 128 } {
            return Err(invalid());
        }
        let ports = (ports.split(',').filter(|port| !port.is_empty()))
            .map(|port| port.parse::<u16>().ok().filter(|port| *port > 0))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)?;
        Ok(Peer {
            cidr: format!("{address}/{length}"),
            ports,
        })
    }
}

/// The namespace of the DNS service of a cluster, which the pods of offline tasks may ask.
const DNS_NAMESPACE: &str = "kube-system";

/// The NetworkPolicy of the Job `name` of the run `run`, whose pods carry the label [`JOB`]: it
/// denies their egress but to the peers and the DNS service, and leaves their ingress as is.
pub fn policy(name: &str, label: &str, run: &str, isolation: &Isolation) -> Value {
    let mut egress = Vec::new();
    for peer in &isolation.peers {
        let mut rule = json!({"to": [{"ipBlock": {"cidr": peer.cidr}}]});
        if !peer.ports.is_empty() {
            let ports: Vec<Value> = (peer.ports.iter())
                .map(|port| json!({"protocol": "TCP", "port": port}))
                .collect();
            rule["ports"] = json!(ports);
        }
        egress.push(rule);
    }
    if isolation.dns {
        // Only the DNS pods of the cluster; pods of that label in other namespaces stay closed.
        let namespace = json!({"matchLabels": {"kubernetes.io/metadata.name": DNS_NAMESPACE}});
        egress.push(json!({
            "to": [{
                "namespaceSelector": namespace,
                "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}},
            }],
            "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}],
        }));
    }
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": name,
            "labels": {"app.kubernetes.io/managed-by": "uwe", LABEL: label},
            "annotations": {RUN: run},
        },
        "spec": {
            "podSelector": {"matchLabels": {JOB: name}},
            "policyTypes": ["Egress"],
            "egress": egress,
        },
    })
}

/// Creates a NetworkPolicy and gives its uid. One of the same name counts only when it is the
/// one that an earlier try of this attempt made: of this backend and run, for the pods of this
/// Job, with this egress; another one is never taken, and never removed.
pub(crate) async fn create(cluster: &Cluster, manifest: Value) -> Result<String, ExecutorError> {
    let policy: NetworkPolicy = serde_json::from_value(manifest)
        .map_err(|error| failed(format!("the NetworkPolicy is not valid: {error}")))?;
    let name = policy.metadata.name.clone().unwrap_or_default();
    let (policies, parameters) = (cluster.policies(), PostParams::default());
    let created = policies.create(&parameters, &policy);
    let made = match tokio::time::timeout(REQUEST_GRACE, created).await {
        Ok(Ok(made)) => made,
        Ok(Err(kube::Error::Api(status))) if status.code == 409 => {
            let found = tokio::time::timeout(REQUEST_GRACE, policies.get_opt(&name)).await;
            let found =
                found.map_err(|_| failed(format!("reading the NetworkPolicy {name} timed out")))?;
            let found = found.map_err(|error| {
                failed(format!("cannot read the NetworkPolicy {name}: {error}"))
            })?;
            match found {
                Some(found) if same(&policy, &found) => found,
                Some(_) => {
                    return Err(failed(format!(
                        "the NetworkPolicy {name} belongs to another attempt"
                    )));
                }
                None => return Err(failed(format!("the NetworkPolicy {name} went away"))),
            }
        }
        Ok(Err(error)) => {
            return Err(failed(format!(
                "the cluster did not take the NetworkPolicy: {error}"
            )));
        }
        Err(_) => return Err(failed("the NetworkPolicy submission timed out".into())),
    };
    (made.metadata.uid).ok_or_else(|| failed(format!("the NetworkPolicy {name} has no uid")))
}

/// Whether a NetworkPolicy of the cluster is `ours`: the same owner labels and run, and the
/// same rules for the same pods.
fn same(ours: &NetworkPolicy, found: &NetworkPolicy) -> bool {
    let owner = |policy: &NetworkPolicy| {
        let labels = policy.metadata.labels.clone().unwrap_or_default();
        let annotations = policy.metadata.annotations.clone().unwrap_or_default();
        (labels.get(LABEL).cloned(), annotations.get(RUN).cloned())
    };
    let rules = |policy: &NetworkPolicy| {
        let spec = policy.spec.clone().unwrap_or_default();
        (spec.pod_selector, spec.policy_types, spec.egress)
    };
    owner(ours) == owner(found) && rules(ours) == rules(found)
}

/// Removes the NetworkPolicy `name` that has the uid `uid`, which may be gone already. Without
/// a uid, as after a crash before its creation was recorded, it must be of this backend and
/// select the pods of the Job `name`; a policy of another attempt stays.
pub(crate) async fn remove(
    cluster: &Cluster,
    name: &str,
    uid: Option<&str>,
    label: &str,
) -> Result<(), ExecutorError> {
    let policies = cluster.policies();
    let uid = match uid {
        Some(uid) => uid.to_owned(),
        None => {
            let found = tokio::time::timeout(REQUEST_GRACE, policies.get_opt(name)).await;
            let found =
                found.map_err(|_| failed(format!("reading the NetworkPolicy {name} timed out")))?;
            let found = found.map_err(|error| {
                failed(format!("cannot read the NetworkPolicy {name}: {error}"))
            })?;
            let Some(found) = found else { return Ok(()) };
            let labels = found.metadata.labels.clone().unwrap_or_default();
            let selector = (found.spec.as_ref())
                .and_then(|spec| spec.pod_selector.as_ref()?.match_labels.clone())
                .unwrap_or_default();
            if labels.get(LABEL).map(String::as_str) != Some(label)
                || selector.get(JOB).map(String::as_str) != Some(name)
            {
                return Ok(());
            }
            found.metadata.uid.unwrap_or_default()
        }
    };
    let parameters = DeleteParams {
        preconditions: Some(Preconditions {
            uid: Some(uid),
            ..Preconditions::default()
        }),
        ..DeleteParams::default()
    };
    let removed = policies.delete(name, &parameters);
    match tokio::time::timeout(REQUEST_GRACE, removed).await {
        Ok(Ok(_)) => Ok(()),
        // Gone, or replaced by a policy of another attempt, which stays.
        Ok(Err(kube::Error::Api(status))) if matches!(status.code, 404 | 409) => Ok(()),
        Ok(Err(error)) => Err(failed(format!(
            "cannot remove the NetworkPolicy {name}: {error}"
        ))),
        Err(_) => Err(failed(format!(
            "removing the NetworkPolicy {name} timed out"
        ))),
    }
}

fn failed(message: String) -> ExecutorError {
    crate::backend::machine(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_peers() {
        let peer = Peer::parse("10.0.0.0/8:443,9000").unwrap();
        assert_eq!(peer.cidr, "10.0.0.0/8");
        assert_eq!(peer.ports, [443, 9000]);
        assert_eq!(Peer::parse("fd00::/8").unwrap().ports, Vec::<u16>::new());
        for text in [
            "10.0.0.0",
            "10.0.0.0/33",
            "host/8",
            "10.0.0.0/8:0",
            "10.0.0.0/8:web",
        ] {
            assert!(Peer::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn denies_other_egress() {
        let isolation = Isolation {
            peers: vec![Peer::parse("192.0.2.0/24:443").unwrap()],
            dns: true,
        };
        let policy = policy("uwe-run-1", "7-9", "run", &isolation);
        assert_eq!(
            policy["spec"]["podSelector"]["matchLabels"][JOB],
            "uwe-run-1"
        );
        assert_eq!(policy["spec"]["policyTypes"], json!(["Egress"]));
        let egress = policy["spec"]["egress"].as_array().unwrap();
        assert_eq!(egress.len(), 2);
        assert_eq!(egress[0]["to"][0]["ipBlock"]["cidr"], "192.0.2.0/24");
        assert_eq!(egress[0]["ports"][0]["port"], 443);
        assert_eq!(egress[1]["ports"][0]["port"], 53);
        let dns = &egress[1]["to"][0]["namespaceSelector"]["matchLabels"];
        assert_eq!(dns["kubernetes.io/metadata.name"], "kube-system");
        assert_eq!(policy["metadata"]["annotations"][RUN], "run");
        let closed = super::policy("uwe-run-2", "7-9", "run", &Isolation::default());
        assert_eq!(closed["spec"]["egress"], json!([]), "no egress at all");
        assert!(serde_json::from_value::<NetworkPolicy>(closed).is_ok());
    }
}

//! Nextflow's pod options for the Jobs of tasks, read from a list of maps: environment, volumes,
//! labels, annotations, scheduling, image pulls and pod security, without privileged containers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use task_model::Placement;

use crate::manifest::LABEL;

/// Why uwe refuses privileged pods and pods that run as root.
pub const PRIVILEGED: &str = "uwe runs pods with the least privileges and never starts \
                              privileged containers or containers as root";

/// The longest name and value of a label, and the longest prefix of a key.
const NAME_LENGTH: usize = 63;
const PREFIX_LENGTH: usize = 253;

/// The pod options of tasks, from `[executor.kubernetes] pod`, Nextflow's `k8s.pod` and the
/// `pod` directive of a task, in that order.
#[derive(Clone, Debug, Default)]
pub struct PodOptions {
    pub service_account: Option<String>,
    env: Vec<Value>,
    /// Volumes without their names, and their mounts.
    volumes: Vec<(Value, Value)>,
    labels: BTreeMap<String, String>,
    annotations: BTreeMap<String, String>,
    node_selector: BTreeMap<String, String>,
    affinity: Option<Value>,
    tolerations: Vec<Value>,
    pull_policy: Option<String>,
    pull_secrets: Vec<String>,
    /// Plain fields of the pod, like `priorityClassName`.
    fields: Map<String, Value>,
    security: Map<String, Value>,
    ttl: Option<u64>,
}

impl PodOptions {
    /// Adds the options of `list`, a map or a list of maps, after these ones.
    pub fn add(&mut self, list: &Value) -> Result<(), String> {
        let items = match list {
            Value::Array(items) => items.clone(),
            Value::Object(_) => vec![list.clone()],
            Value::Null => Vec::new(),
            other => return Err(format!("Not a valid pod setting: {other}")),
        };
        for item in &items {
            let Value::Object(map) = item else {
                return Err(format!("Not a valid pod setting: {item}"));
            };
            self.entry(map)?;
        }
        Ok(())
    }

    fn entry(&mut self, map: &Map<String, Value>) -> Result<(), String> {
        let text = |key: &str| map.get(key).and_then(plain);
        let unknown = || format!("Unknown pod options: {}", Value::Object(map.clone()));
        if let Some(name) = text("env") {
            let from = |kind: &str, reference: String| {
                let (source, key) = split(&reference, &name);
                json!({kind: {"name": source, "key": key}})
            };
            let entry = match (
                text("value"),
                text("secret"),
                text("config"),
                text("fieldPath"),
            ) {
                (Some(value), ..) => json!({"name": name, "value": value}),
                (_, Some(secret), ..) => {
                    json!({"name": name, "valueFrom": from("secretKeyRef", secret)})
                }
                (_, _, Some(config), _) => {
                    json!({"name": name, "valueFrom": from("configMapKeyRef", config)})
                }
                (.., Some(path)) => {
                    json!({"name": name, "valueFrom": {"fieldRef": {"fieldPath": path}}})
                }
                _ => return Err(unknown()),
            };
            self.env.push(entry);
            return Ok(());
        }
        if let Some(path) = text("mountPath") {
            let mut target = path.clone();
            let volume = if let Some(secret) = text("secret") {
                keyed(json!({"secretName": secret}), "secret", &mut target)
            } else if let Some(config) = text("config") {
                keyed(json!({"name": config}), "configMap", &mut target)
            } else if let Some(claim) = text("volumeClaim") {
                json!({"persistentVolumeClaim": {"claimName": claim}})
            } else if let Some(empty) = map.get("emptyDir") {
                json!({"emptyDir": if empty.is_object() { empty.clone() } else { json!({}) }})
            } else if let Some(host) = text("hostPath") {
                json!({"hostPath": {"path": host}})
            } else if let Some(csi) = map.get("csi").filter(|csi| csi.is_object()) {
                json!({"csi": csi})
            } else {
                return Err(unknown());
            };
            let mut mount = json!({"mountPath": target});
            if map.get("readOnly").and_then(Value::as_bool) == Some(true) {
                mount["readOnly"] = json!(true);
            }
            if let Some(sub) = text("subPath") {
                mount["subPath"] = json!(sub);
            }
            self.volumes.push((volume, mount));
            return Ok(());
        }
        for (kind, slot) in [
            ("label", &mut self.labels),
            ("annotation", &mut self.annotations),
        ] {
            if let Some(name) = text(kind) {
                slot.insert(name, text("value").unwrap_or_default());
                return Ok(());
            }
        }
        let Some((key, value)) = map.iter().next().filter(|_| map.len() == 1) else {
            return Err(unknown());
        };
        match (key.as_str(), value) {
            ("nodeSelector", Value::Object(selector)) => {
                for (name, value) in selector {
                    self.node_selector
                        .insert(name.clone(), plain(value).unwrap_or_default());
                }
            }
            ("nodeSelector", Value::String(text)) => {
                for pair in text.split(',').filter(|pair| !pair.trim().is_empty()) {
                    let (name, value) = pair.split_once('=').ok_or_else(unknown)?;
                    self.node_selector
                        .insert(name.trim().into(), value.trim().into());
                }
            }
            ("affinity", Value::Object(_)) => self.affinity = Some(value.clone()),
            ("toleration", Value::Object(_)) => self.tolerations.push(value.clone()),
            ("imagePullPolicy", Value::String(policy)) => self.pull_policy = Some(policy.clone()),
            ("imagePullSecret" | "imagePullSecrets", Value::String(name)) => {
                self.pull_secrets.push(name.clone())
            }
            ("priorityClassName" | "schedulerName" | "runtimeClassName", Value::String(_))
            | ("automountServiceAccountToken", Value::Bool(_)) => {
                self.fields.insert(key.clone(), value.clone());
            }
            ("privileged", Value::Bool(true)) => return Err(PRIVILEGED.into()),
            ("privileged", Value::Bool(false)) => {}
            ("runAsUser", _) => {
                let user = value.as_u64().ok_or_else(unknown)?;
                self.secure(&Map::from_iter([("runAsUser".to_owned(), json!(user))]))?;
            }
            ("securityContext", Value::Object(context)) => self.secure(context)?,
            ("ttlSecondsAfterFinished", _) => self.ttl = Some(value.as_u64().ok_or_else(unknown)?),
            _ => return Err(unknown()),
        }
        Ok(())
    }

    /// Adds fields of the pod's `securityContext`, but none that runs a container as root or
    /// privileged.
    fn secure(&mut self, context: &Map<String, Value>) -> Result<(), String> {
        let root = |key: &str| context.get(key).and_then(Value::as_u64) == Some(0);
        let flag = |key: &str| context.get(key).and_then(Value::as_bool);
        if root("runAsUser") || root("runAsGroup") || flag("privileged") == Some(true) {
            return Err(PRIVILEGED.into());
        }
        if flag("runAsNonRoot") == Some(false) || flag("allowPrivilegeEscalation") == Some(true) {
            return Err(PRIVILEGED.into());
        }
        self.security.extend(context.clone());
        Ok(())
    }

    /// Adds the options to the Job `job` of a task with `placement`, and its resource labels,
    /// disk and accelerator type. uwe's own labels, node selectors and security stay.
    pub fn apply(&self, job: &mut Value, placement: &Placement) -> Result<(), String> {
        let mut labels = BTreeMap::new();
        for (name, value) in placement.labels.iter().chain(&self.labels) {
            labels.insert(key(name, "label")?, cleaned(value, NAME_LENGTH));
        }
        let mut annotations = BTreeMap::new();
        for (name, value) in &self.annotations {
            annotations.insert(key(name, "annotation")?, value.clone());
        }
        for path in [&["metadata"][..], &["spec", "template", "metadata"]] {
            let metadata = path.iter().fold(&mut *job, |value, key| &mut value[*key]);
            for (name, value) in &labels {
                if name != LABEL && name != "app.kubernetes.io/managed-by" {
                    metadata["labels"][name] = json!(value);
                }
            }
            for (name, value) in &annotations {
                metadata["annotations"][name] = json!(value);
            }
        }
        if let Some(ttl) = self.ttl {
            job["spec"]["ttlSecondsAfterFinished"] = json!(ttl);
        }
        let pod = &mut job["spec"]["template"]["spec"];
        if let Some(account) = &self.service_account {
            pod["serviceAccountName"] = json!(account);
        }
        for (name, value) in &self.node_selector {
            if pod["nodeSelector"].get(name).is_none() {
                pod["nodeSelector"][name] = json!(value);
            }
        }
        if let Some(affinity) = &self.affinity {
            pod["affinity"] = affinity.clone();
        }
        if !self.tolerations.is_empty() {
            pod["tolerations"] = json!(self.tolerations);
        }
        if !self.pull_secrets.is_empty() {
            let names: Vec<Value> = self
                .pull_secrets
                .iter()
                .map(|name| json!({"name": name}))
                .collect();
            pod["imagePullSecrets"] = json!(names);
        }
        for (name, value) in &self.fields {
            pod[name] = value.clone();
        }
        if !self.security.is_empty() {
            pod["securityContext"] = Value::Object(self.security.clone());
        }
        // Claims come first, as Nextflow names them.
        let mut volumes: Vec<&(Value, Value)> = self.volumes.iter().collect();
        volumes.sort_by_key(|(volume, _)| volume.get("persistentVolumeClaim").is_none());
        let mut mounts = Vec::new();
        for (index, (volume, mount)) in volumes.into_iter().enumerate() {
            let name = format!("vol-{}", index + 1);
            let mut volume = volume.clone();
            volume["name"] = json!(name);
            pod["volumes"]
                .as_array_mut()
                .ok_or("a Job without volumes")?
                .push(volume);
            let mut mount = mount.clone();
            mount["name"] = json!(name);
            mounts.push(mount);
        }
        let gpu = accelerator(placement);
        let disk = placement
            .disk_bytes
            .map(|bytes| format!("{}Mi", bytes.div_ceil(1 << 20)));
        let containers = pod["containers"]
            .as_array_mut()
            .ok_or("a Job without containers")?;
        for (index, container) in containers.iter_mut().enumerate() {
            if !self.env.is_empty() {
                let env = container["env"].as_array().cloned().unwrap_or_default();
                container["env"] =
                    json!(env.into_iter().chain(self.env.clone()).collect::<Vec<_>>());
            }
            if let Some(list) = container["volumeMounts"].as_array_mut() {
                list.extend(mounts.iter().cloned());
            }
            if let Some(policy) = &self.pull_policy {
                container["imagePullPolicy"] = json!(policy);
            }
            for kind in ["requests", "limits"] {
                let Some(resources) = container["resources"][kind].as_object_mut() else {
                    continue;
                };
                if let Some(count) = resources.remove("nvidia.com/gpu") {
                    resources.insert(gpu.clone(), count);
                }
                if let (Some(disk), 0) = (&disk, index) {
                    resources.insert("ephemeral-storage".into(), json!(disk));
                }
            }
        }
        Ok(())
    }
}

/// The resource name of the GPUs of a task, from the `type` of its accelerator as Nextflow
/// names it: none gives `nvidia.com/gpu`, `amd` gives `amd.com/gpu`, a name with `/` stays.
fn accelerator(placement: &Placement) -> String {
    let named = placement.options.get("accelerator");
    let kind = (placement
        .accelerators
        .iter()
        .find_map(|gpu| gpu.kind.clone()))
    .or_else(|| {
        named
            .and_then(|named| named["type"].as_str())
            .map(str::to_owned)
    });
    match kind {
        None => "nvidia.com/gpu".into(),
        Some(kind) if kind.contains('/') => kind,
        Some(kind) if kind.contains('.') => format!("{kind}/gpu"),
        Some(kind) => format!("{kind}.com/gpu"),
    }
}

/// A Secret or ConfigMap volume for `reference`, `name` or `name/key`: with a key, only that
/// key, at the file name of the mount path, whose folder becomes the mount path.
fn keyed(mut source: Value, kind: &str, target: &mut String) -> Value {
    let field = if kind == "secret" {
        "secretName"
    } else {
        "name"
    };
    let reference = source[field].as_str().unwrap_or_default().to_owned();
    if let Some((name, key)) = reference.split_once('/') {
        let file = target
            .rsplit_once('/')
            .map_or(target.as_str(), |(_, file)| file)
            .to_owned();
        let folder = target
            .rsplit_once('/')
            .map(|(folder, _)| folder)
            .unwrap_or("/");
        *target = if folder.is_empty() {
            "/".into()
        } else {
            folder.to_owned()
        };
        source[field] = json!(name);
        source["items"] = json!([{"key": key, "path": file}]);
    }
    json!({kind: source})
}

/// The source and key of `name/key`; the key is `fallback` without one.
fn split(reference: &str, fallback: &str) -> (String, String) {
    match reference.split_once('/') {
        Some((source, key)) => (source.into(), key.into()),
        None => (reference.into(), fallback.into()),
    }
}

/// A key of a label or annotation, cleaned as Nextflow cleans them: an optional prefix of at
/// most 253 characters and a name of at most 63.
fn key(name: &str, kind: &str) -> Result<String, String> {
    match name.split('/').collect::<Vec<_>>().as_slice() {
        [name] => Ok(cleaned(name, NAME_LENGTH)),
        [prefix, name] => Ok(format!(
            "{}/{}",
            cleaned(prefix, PREFIX_LENGTH),
            cleaned(name, NAME_LENGTH)
        )),
        _ => Err(format!(
            "Invalid key in pod {kind} -- Key can only contain exactly one '/' character"
        )),
    }
}

/// `text` cut to `limit` characters, each run of characters other than letters, digits, `.`,
/// `_` and `-` as one `_`, without other characters than letters and digits at its ends.
fn cleaned(text: &str, limit: usize) -> String {
    let mut clean = String::new();
    let mut other = false;
    for character in text.chars().take(limit) {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            clean.push(character);
            other = false;
        } else if !other {
            clean.push('_');
            other = true;
        }
    }
    clean
        .trim_matches(|character: char| !character.is_ascii_alphanumeric())
        .to_owned()
}

/// The text of a value, also of a number or a flag.
fn plain(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;

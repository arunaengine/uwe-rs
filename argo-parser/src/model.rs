//! The typed model of Argo documents, after the Go types of Argo v4.1.4. Fields that Argo hands
//! to Kubernetes unchanged, and artifact locations, stay raw [`Node`]s.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

use crate::node::{Entry, Node, Value};

/// A string that may also be written as a number or a boolean, like Argo's `AnyString` and
/// Kubernetes' `IntOrString` and `Quantity`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnyString(pub String);

impl AnyString {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AnyString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AnyString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(AnyStringVisitor)
    }
}

struct AnyStringVisitor;

impl<'de> Visitor<'de> for AnyStringVisitor {
    type Value = AnyString;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string, a number or a boolean")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<AnyString, E> {
        Ok(AnyString(text.to_owned()))
    }

    fn visit_bool<E: de::Error>(self, flag: bool) -> Result<AnyString, E> {
        Ok(AnyString(flag.to_string()))
    }

    fn visit_i64<E: de::Error>(self, number: i64) -> Result<AnyString, E> {
        Ok(AnyString(number.to_string()))
    }

    fn visit_u64<E: de::Error>(self, number: u64) -> Result<AnyString, E> {
        Ok(AnyString(number.to_string()))
    }

    fn visit_f64<E: de::Error>(self, number: f64) -> Result<AnyString, E> {
        Ok(AnyString(json_float(number)))
    }

    fn visit_unit<E: de::Error>(self) -> Result<AnyString, E> {
        Ok(AnyString::default())
    }
}

/// A float as Go's `encoding/json` writes it, which is the text Argo sees after it turned YAML
/// into JSON: `1.0` becomes `1`, and exponents are used below 1e-6 and from 1e21.
pub fn json_float(number: f64) -> String {
    if !number.is_finite() {
        return number.to_string();
    }
    let magnitude = number.abs();
    if magnitude != 0.0 && !(1e-6..1e21).contains(&magnitude) {
        let text = format!("{number:e}");
        match text.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                format!("{mantissa}e+{exponent}")
            }
            _ => text,
        }
    } else {
        number.to_string()
    }
}

/// One Argo document: a Workflow, WorkflowTemplate, ClusterWorkflowTemplate or CronWorkflow.
/// The spec is decoded by kind, as [`WorkflowSpec`] or [`CronWorkflowSpec`].
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Document {
    pub api_version: String,
    pub kind: String,
    pub metadata: ObjectMeta,
    pub spec: Option<Node>,
    pub status: Option<Node>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ObjectMeta {
    pub name: String,
    pub generate_name: String,
    pub namespace: String,
    pub labels: BTreeMap<String, AnyString>,
    pub annotations: BTreeMap<String, AnyString>,
    pub uid: Option<Node>,
    pub creation_timestamp: Option<Node>,
    pub resource_version: Option<Node>,
    pub generation: Option<Node>,
    pub owner_references: Option<Node>,
    pub finalizers: Option<Node>,
    pub managed_fields: Option<Node>,
    pub self_link: Option<Node>,
    pub deletion_timestamp: Option<Node>,
    pub deletion_grace_period_seconds: Option<Node>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkflowSpec {
    pub entrypoint: String,
    pub arguments: Arguments,
    pub templates: Vec<Template>,
    pub on_exit: String,
    pub hooks: BTreeMap<String, LifecycleHook>,
    pub workflow_template_ref: Option<WorkflowTemplateRef>,
    pub template_defaults: Option<Template>,
    pub volumes: Vec<Node>,
    pub volume_claim_templates: Vec<Node>,
    pub parallelism: Option<i64>,
    pub active_deadline_seconds: Option<AnyString>,
    pub retry_strategy: Option<RetryStrategy>,
    pub service_account_name: String,
    pub priority: Option<i64>,
    pub workflow_metadata: Option<Node>,
    pub affinity: Option<Node>,
    pub archive_logs: Option<Node>,
    pub artifact_gc: Option<Node>,
    pub artifact_repository_ref: Option<Node>,
    pub automount_service_account_token: Option<Node>,
    pub dns_config: Option<Node>,
    pub dns_policy: Option<Node>,
    pub executor: Option<Node>,
    pub executor_plugins: Option<Node>,
    pub host_aliases: Option<Node>,
    pub host_network: Option<Node>,
    pub image_pull_secrets: Option<Node>,
    pub metrics: Option<Node>,
    pub node_selector: Option<Node>,
    pub pod_disruption_budget: Option<Node>,
    pub pod_gc: Option<Node>,
    pub pod_metadata: Option<Node>,
    pub pod_priority_class_name: Option<Node>,
    pub pod_resources: Option<Node>,
    pub pod_spec_patch: Option<Node>,
    pub scheduler_name: Option<Node>,
    pub security_context: Option<Node>,
    pub shutdown: Option<Node>,
    pub suspend: Option<Node>,
    pub synchronization: Option<Node>,
    pub tolerations: Option<Node>,
    pub ttl_strategy: Option<Node>,
    pub volume_claim_gc: Option<Node>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct CronWorkflowSpec {
    pub workflow_spec: WorkflowSpec,
    pub schedules: Vec<String>,
    pub timezone: Option<Node>,
    pub concurrency_policy: Option<Node>,
    pub starting_deadline_seconds: Option<Node>,
    pub successful_jobs_history_limit: Option<Node>,
    pub failed_jobs_history_limit: Option<Node>,
    pub suspend: Option<Node>,
    pub workflow_metadata: Option<Node>,
    pub stop_strategy: Option<Node>,
    pub when: Option<Node>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkflowTemplateRef {
    pub name: String,
    pub cluster_scope: bool,
}

/// A template: a leaf that does work, a pause, or steps or a DAG that call other templates.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Template {
    pub name: String,
    pub inputs: Inputs,
    pub outputs: Outputs,
    pub steps: Vec<Vec<WorkflowStep>>,
    pub dag: Option<DagTemplate>,
    pub container: Option<Container>,
    pub container_set: Option<ContainerSet>,
    pub script: Option<Container>,
    pub resource: Option<ResourceTemplate>,
    pub suspend: Option<SuspendTemplate>,
    pub http: Option<HttpTemplate>,
    pub plugin: Option<Node>,
    pub data: Option<DataTemplate>,
    pub daemon: Option<bool>,
    pub retry_strategy: Option<RetryStrategy>,
    pub fail_fast: Option<bool>,
    pub parallelism: Option<i64>,
    pub active_deadline_seconds: Option<AnyString>,
    pub timeout: Option<AnyString>,
    pub volumes: Vec<Node>,
    pub init_containers: Vec<Node>,
    pub sidecars: Vec<Node>,
    pub metadata: Option<Node>,
    pub annotations: Option<Node>,
    pub archive_location: Option<Node>,
    pub affinity: Option<Node>,
    pub automount_service_account_token: Option<Node>,
    pub executor: Option<Node>,
    pub host_aliases: Option<Node>,
    pub memoize: Option<Node>,
    pub metrics: Option<Node>,
    pub node_selector: Option<Node>,
    pub pending_timeout: Option<Node>,
    pub pod_resources: Option<Node>,
    pub pod_spec_patch: Option<Node>,
    pub priority: Option<Node>,
    pub priority_class_name: Option<Node>,
    pub resource_claims: Option<Node>,
    pub scheduler_name: Option<Node>,
    pub security_context: Option<Node>,
    pub service_account_name: Option<Node>,
    pub synchronization: Option<Node>,
    pub tolerations: Option<Node>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Inputs {
    pub parameters: Vec<Parameter>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Outputs {
    pub parameters: Vec<Parameter>,
    pub artifacts: Vec<Artifact>,
    pub result: Option<String>,
    pub exit_code: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Arguments {
    pub parameters: Vec<Parameter>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Parameter {
    pub name: String,
    pub value: Option<AnyString>,
    pub default: Option<AnyString>,
    pub value_from: Option<ValueFrom>,
    #[serde(rename = "enum")]
    pub enumeration: Vec<AnyString>,
    pub description: Option<AnyString>,
    pub global_name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ValueFrom {
    pub path: String,
    pub json_path: String,
    pub jq_filter: String,
    pub expression: String,
    pub parameter: String,
    pub event: String,
    pub default: Option<AnyString>,
    pub supplied: Option<Node>,
    pub config_map_key_ref: Option<Node>,
}

/// An artifact: a file or directory, where it is in the container, and where it comes from.
/// Locations stay raw, as their fields differ by storage.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Artifact {
    pub name: String,
    pub path: String,
    pub from: String,
    pub from_expression: String,
    pub optional: bool,
    pub sub_path: String,
    pub global_name: String,
    pub mode: Option<Node>,
    pub archive: Option<Node>,
    pub archive_logs: Option<Node>,
    pub artifact_gc: Option<Node>,
    pub deleted: Option<Node>,
    pub recurse_mode: Option<Node>,
    pub s3: Option<Node>,
    pub gcs: Option<Node>,
    pub azure: Option<Node>,
    pub git: Option<Node>,
    pub http: Option<Node>,
    pub artifactory: Option<Node>,
    pub hdfs: Option<Node>,
    pub oss: Option<Node>,
    pub raw: Option<Node>,
    pub plugin: Option<Node>,
}

/// A step of a `steps` template.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkflowStep {
    pub name: String,
    pub template: String,
    pub template_ref: Option<TemplateRef>,
    pub inline: Option<Box<Template>>,
    pub arguments: Arguments,
    pub when: String,
    pub with_items: Vec<Node>,
    pub with_param: String,
    pub with_sequence: Option<Sequence>,
    pub continue_on: Option<ContinueOn>,
    pub on_exit: String,
    pub hooks: BTreeMap<String, LifecycleHook>,
}

/// A task of a `dag` template.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DagTask {
    pub name: String,
    pub template: String,
    pub template_ref: Option<TemplateRef>,
    pub inline: Option<Box<Template>>,
    pub arguments: Arguments,
    pub when: String,
    pub with_items: Vec<Node>,
    pub with_param: String,
    pub with_sequence: Option<Sequence>,
    pub continue_on: Option<ContinueOn>,
    pub on_exit: String,
    pub hooks: BTreeMap<String, LifecycleHook>,
    pub dependencies: Vec<String>,
    pub depends: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TemplateRef {
    pub name: String,
    pub template: String,
    pub cluster_scope: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Sequence {
    pub count: Option<AnyString>,
    pub start: Option<AnyString>,
    pub end: Option<AnyString>,
    pub format: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ContinueOn {
    pub error: bool,
    pub failed: bool,
}

/// A hook: a template that runs when its expression holds, or on exit for the hook `exit`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LifecycleHook {
    pub template: String,
    pub template_ref: Option<TemplateRef>,
    pub arguments: Arguments,
    pub expression: String,
}

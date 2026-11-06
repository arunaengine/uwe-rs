//! Checks the pod options of a Job against the Job of the real nf-k8s and refuses privileges.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use executor::{Access, ContainerRun};
use task_model::{Accelerator, Image};

use super::*;
use crate::manifest::{Limits, Volumes, job};

/// The Job of a task like the fixture's `TRAIN`, with `options`.
fn train(options: &PodOptions) -> Result<Value, String> {
    let image = Image {
        reference: "ubuntu:24.04".into(),
        platform: None,
    };
    let run = ContainerRun {
        name: "uwe-run-train-1-1",
        image: &image,
        root: Path::new("/cache/scratch/run-TRAIN-1-1"),
        workdir: None,
        mounts: &[],
        owner: (1000, 100),
        cpus: 2,
        memory: Some(3 << 30),
        offline: false,
        wrapper_path: None,
        access: &Access::of((1000, 100), false, None, ""),
    };
    let limits = Limits {
        gpus: 1,
        seconds: Some(3600),
        margin: 0,
    };
    let mut manifest = job("uwe-run-train-1-1", "7-9", &[run], &limits, &Volumes::Host)?;
    let labels = [("team", "genomics"), ("cost-center", "A 12")];
    let placement = Placement {
        cpus: 2,
        memory_bytes: Some(3 << 30),
        disk_bytes: Some(20 << 30),
        accelerators: vec![Accelerator {
            count: 1,
            kind: None,
        }],
        labels: labels
            .map(|(name, value)| (name.into(), value.into()))
            .into(),
        options: [("accelerator".to_owned(), json!({"type": "nvidia.com"}))].into(),
        ..Placement::default()
    };
    options.apply(&mut manifest, &placement)?;
    Ok(manifest)
}

/// The recorded Job of `TRAIN`.
fn recorded() -> Value {
    let text = include_str!("../tests/fixtures/nf_k8s_requests.jsonl");
    let requests = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON"));
    let mut posts = requests.filter(|request| request["method"] == "POST");
    let train = posts.find(|request| request["body"]["metadata"]["labels"]["tier"] == "batch");
    train.expect("the Job of TRAIN")["body"].clone()
}

/// The fixture's nine pod options of `TRAIN` and `k8s.serviceAccount`.
fn fixture_options() -> PodOptions {
    let list = json!([
        {"nodeSelector": "disktype=ssd"},
        {"label": "tier", "value": "batch"},
        {"annotation": "note", "value": "probe"},
        {"toleration": {"key": "gpu", "operator": "Exists", "effect": "NoSchedule"}},
        {"env": "MESSAGE", "value": "hello"},
        {"secret": "creds/token", "mountPath": "/etc/creds/token"},
        {"volumeClaim": "refs", "mountPath": "/refs", "readOnly": true},
        {"imagePullSecret": "regcred"},
        {"priorityClassName": "high"},
    ]);
    let mut options = PodOptions {
        service_account: Some("runner".into()),
        ..PodOptions::default()
    };
    options.add(&list).expect("valid options");
    options
}

#[test]
fn matches_recorded_pods() {
    let ours = train(&fixture_options()).expect("a Job");
    let theirs = recorded();
    for name in ["team", "cost-center", "tier"] {
        let label = &theirs["metadata"]["labels"][name];
        assert_eq!(&ours["metadata"]["labels"][name], label, "{name}");
        assert_eq!(&ours["spec"]["template"]["metadata"]["labels"][name], label);
    }
    assert_eq!(ours["metadata"]["labels"][LABEL], "7-9");
    assert_eq!(
        ours["metadata"]["annotations"],
        theirs["metadata"]["annotations"]
    );
    let (pod, recorded) = (
        &ours["spec"]["template"]["spec"],
        &theirs["spec"]["template"]["spec"],
    );
    for field in [
        "nodeSelector",
        "serviceAccountName",
        "imagePullSecrets",
        "priorityClassName",
        "tolerations",
    ] {
        assert_eq!(pod[field], recorded[field], "{field}");
    }
    let named = |list: &Value| -> Vec<Value> {
        let items = list.as_array().into_iter().flatten();
        let ours = items.filter(|item| {
            item["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("vol-"))
        });
        ours.cloned().collect()
    };
    assert_eq!(named(&pod["volumes"]), named(&recorded["volumes"]));
    let (container, theirs) = (&pod["containers"][0], &recorded["containers"][0]);
    assert_eq!(container["env"], theirs["env"]);
    assert_eq!(
        named(&container["volumeMounts"]),
        named(&theirs["volumeMounts"])
    );
    let limits = &container["resources"]["limits"];
    assert_eq!(
        limits["ephemeral-storage"],
        theirs["resources"]["limits"]["ephemeral-storage"]
    );
    assert_eq!(limits["nvidia.com/gpu"], "1");
    // uwe's own security stays: no service account token, no privileges.
    assert_eq!(pod["automountServiceAccountToken"], false);
    assert_eq!(container["securityContext"]["privileged"], false);
}

#[test]
fn refuses_privileged_pods() {
    for wrong in [
        json!({"privileged": true}),
        json!({"runAsUser": 0}),
        json!({"securityContext": {"runAsNonRoot": false}}),
        json!({"securityContext": {"privileged": true}}),
    ] {
        let error = PodOptions::default().add(&wrong).expect_err("refused");
        assert_eq!(error, PRIVILEGED, "{wrong}");
    }
    let error = PodOptions::default()
        .add(&json!([{"volume": "x"}]))
        .expect_err("unknown");
    assert!(error.starts_with("Unknown pod options: "), "{error}");
    let mut options = PodOptions::default();
    options
        .add(&json!([{"label": "a/b/c", "value": "x"}]))
        .expect("read");
    let error = train(&options).expect_err("an invalid key");
    assert!(error.contains("exactly one '/'"), "{error}");
    let mut amd = Placement::default();
    amd.options
        .insert("accelerator".into(), json!({"type": "amd"}));
    assert_eq!(accelerator(&amd), "amd.com/gpu");
    assert_eq!(cleaned("--A 12!!", NAME_LENGTH), "A_12");
}

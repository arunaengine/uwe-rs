//! Unsupported execution controls must fail before any task is published.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{DriverError, Launch};

fn start_error(spec: &str) -> DriverError {
    let folder = tempfile::tempdir().expect("temporary folder");
    let target = folder.path().join("workflow.yaml");
    std::fs::write(
        &target,
        format!("apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: controls}}\nspec:\n  entrypoint: main\n{spec}"),
    ).expect("workflow written");
    let mut driver = argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .expect("driver");
    driver.start().expect_err("unsupported control must fail")
}

#[test]
fn rejects_workflow_controls() {
    for field in [
        "synchronization: {semaphores: [{database: {key: lock}}]}",
        "podSpecPatch: '{hostNetwork: true}'",
        "podResources: {}",
        "resourceClaims: []",
        "shutdown: Stop",
        "suspend: true",
    ] {
        let error = start_error(&format!(
            "  {field}\n  templates:\n  - name: main\n    container: {{image: alpine}}\n"
        ));
        assert!(error.unsupported, "{field}: {error}");
        assert!(
            error.message.contains(field.split(':').next().unwrap()),
            "{error}"
        );
    }
}

#[test]
fn rejects_template_controls() {
    for field in [
        "synchronization: {semaphores: [{database: {key: lock}}]}",
        "podSpecPatch: '{hostNetwork: true}'",
        "podResources: {}",
        "resourceClaims: []",
        "pendingTimeout: 5s",
    ] {
        for spec in [
            format!(
                "  templates:\n  - name: main\n    {field}\n    container: {{image: alpine}}\n"
            ),
            format!(
                "  templateDefaults:\n    {field}\n  templates:\n  - name: main\n    container: {{image: alpine}}\n"
            ),
        ] {
            let error = start_error(&spec);
            assert!(error.unsupported, "{field}: {error}");
            assert!(
                error.message.contains(field.split(':').next().unwrap()),
                "{error}"
            );
        }
    }
    assert!(
        start_error("  templates:\n  - name: main\n    suspend: {}\n")
            .message
            .contains("manual resume")
    );
}

#[test]
fn rejects_lifecycle_controls() {
    for field in [
        "lifecycle",
        "startupProbe",
        "restartPolicy",
        "restartPolicyRules",
        "resizePolicy",
    ] {
        for spec in [
            format!(
                "  templates:\n  - name: main\n    container: {{image: alpine, {field}: {{}}}}\n"
            ),
            format!(
                "  templates:\n  - name: main\n    initContainers: [{{name: init, image: alpine, {field}: {{}}}}]\n    container: {{image: alpine}}\n"
            ),
        ] {
            let error = start_error(&spec);
            assert!(error.unsupported, "{field}: {error}");
            assert!(error.message.contains(field), "{error}");
        }
    }
}

#[test]
fn refuses_service_lifetimes() {
    for (field, reason) in [
        ("daemon: true", "service readiness"),
        (
            "sidecars: [{name: server, image: debian:stable-slim}]",
            "shared task network",
        ),
    ] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    {field}\n    container: {{image: debian:stable-slim}}\n"
        ));
        assert!(error.unsupported, "{error}");
        assert!(error.message.contains(reason), "{error}");
    }
}

#[test]
fn rejects_liveness_probes() {
    for template in [
        "    container: {image: alpine, livenessProbe: {exec: {command: [sh, -c, 'exit 1']}}}\n",
        "    script: {image: alpine, command: [sh], source: echo ready, livenessProbe: {exec: {command: [sh, -c, 'exit 1']}}}\n",
        "    initContainers: [{name: init, image: alpine, livenessProbe: {exec: {command: [sh, -c, 'exit 1']}}}]\n    container: {image: alpine}\n",
        "    containerSet: {containers: [{name: main, image: alpine, livenessProbe: {exec: {command: [sh, -c, 'exit 1']}}}]}\n",
    ] {
        let error = start_error(&format!("  templates:\n  - name: main\n{template}"));
        assert!(error.unsupported, "{error}");
        assert!(error.message.contains("livenessProbe"), "{error}");
    }
}

#[test]
fn rejects_retry_controls() {
    for strategy in [
        "backoff: {duration: invalid}",
        "retryPolicy: invalid",
        "limit: -1",
        "limit: invalid",
    ] {
        for spec in [
            format!(
                "  retryStrategy: {{{strategy}}}\n  templates:\n  - name: main\n    container: {{image: alpine}}\n"
            ),
            format!(
                "  templates:\n  - name: main\n    retryStrategy: {{{strategy}}}\n    container: {{image: alpine}}\n"
            ),
        ] {
            let error = start_error(&spec);
            assert!(!error.unsupported, "{error}");
            assert!(
                error.message.contains("retry") || error.message.contains("Retry"),
                "{error}"
            );
        }
    }
}

#[test]
fn rejects_invalid_resources() {
    let empty = start_error("  templates:\n  - name: main\n    containerSet: {containers: []}\n");
    assert!(empty.message.contains("at least one container"), "{empty}");
    for resource in [
        "memory: 16Ei",
        "memory: -1",
        "cpu: .inf",
        "nvidia.com/gpu: -1",
        "cpu: invalid",
    ] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    container: {{image: alpine, resources: {{requests: {{{resource}}}}}}}\n"
        ));
        assert!(error.message.contains("resource"), "{error}");
    }
    for resource in [
        "memory: 8Ei",
        "ephemeral-storage: 8Ei",
        "nvidia.com/gpu: 4294967295",
    ] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    containerSet:\n      containers:\n      - {{name: main, image: alpine, resources: {{requests: {{{resource}}}}}}}\n      - {{name: other, image: alpine, resources: {{requests: {{{resource}}}}}}}\n"
        ));
        assert!(error.message.contains("sum overflows"), "{error}");
    }
    let error = start_error(
        "  templates:\n  - name: main\n    container: {image: alpine, resources: {requests: {hugepages-2Mi: 2Mi}}}\n",
    );
    assert!(error.unsupported, "{error}");
}

#[test]
fn requests_respect_limits() {
    for (request, limit) in [("memory: 2Gi", "memory: 1Gi"), ("cpu: 2", "cpu: 1")] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    container: {{image: alpine, resources: {{requests: {{{request}}}, limits: {{{limit}}}}}}}\n",
        ));
        assert!(
            !error.unsupported && error.message.contains("request must not exceed"),
            "{error}"
        );
    }
}

#[test]
fn rejects_node_deadlines() {
    for spec in [
        "  templates:\n  - name: main\n    timeout: invalid\n    container: {image: alpine}\n",
        "  templates:\n  - name: main\n    activeDeadlineSeconds: -1\n    container: {image: alpine}\n",
    ] {
        assert!(!start_error(spec).unsupported);
    }
    for count in ["0", "-1", "invalid"] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    containerSet:\n      retryStrategy: {{retries: {count}}}\n      containers: [{{name: main, image: alpine}}]\n"
        ));
        assert!(error.message.contains("retries"), "{error}");
    }
}

#[test]
fn rejects_external_parameters() {
    for source in ["event: body.message", "supplied: {}"] {
        let error = start_error(&format!(
            "  arguments: {{parameters: [{{name: message, valueFrom: {{{source}, default: fallback}}}}]}}\n  templates:\n  - name: main\n    container: {{image: alpine}}\n"
        ));
        assert!(
            error.unsupported && error.message.contains("valueFrom"),
            "{error}"
        );
    }
}

#[test]
fn rejects_transfer_options() {
    for call in [
        "    steps:\n    - - name: use\n        template: work\n        arguments: {artifacts: [{name: input, s3: {bucket: data, key: input, endpoint: other.example.org}}]}\n",
        "    dag:\n      tasks:\n      - name: use\n        template: work\n        arguments: {artifacts: [{name: input, s3: {bucket: data, key: input, endpoint: other.example.org}}]}\n",
    ] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n{call}  - name: work\n    inputs: {{artifacts: [{{name: input, path: /in}}]}}\n    container: {{image: alpine}}\n",
        ));
        assert!(
            error.unsupported && error.message.contains("S3.endpoint"),
            "{error}"
        );
    }
    for artifact in [
        "git: {repo: 'https://example.org/repository.git'}",
        "s3: {bucket: data, key: input.txt, endpoint: example.org}",
        "http: {url: 'https://example.org/input', headers: [{name: Authorization, value: secret}]}",
        "raw: {data: input}, archive: {tar: {}}",
    ] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    inputs: {{artifacts: [{{name: input, {artifact}}}]}}\n    container: {{image: alpine}}\n"
        ));
        assert!(
            error.unsupported && error.message.contains("artifact"),
            "{error}"
        );
    }
    let error = start_error(
        "  templates:\n  - name: main\n    container: {image: alpine}\n    outputs: {parameters: [{name: result, valueFrom: {jsonPath: '$.result', default: fallback}}]}\n",
    );
    assert!(
        error.unsupported && error.message.contains("valueFrom.path"),
        "{error}"
    );
    let error = start_error(
        "  templates:\n  - name: main\n    container: {image: alpine}\n    outputs: {artifacts: [{name: output, path: /out, gcs: {bucket: data, key: output}}]}\n",
    );
    assert!(
        error.unsupported && error.message.contains("uploads"),
        "{error}"
    );
}

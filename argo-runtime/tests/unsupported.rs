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
fn rejects_unsupported_workflow_controls() {
    for field in [
        "synchronization: {mutexes: [{name: lock}]}",
        "activeDeadlineSeconds: 5",
        "artifactRepositoryRef: {configMap: artifacts}",
        "podSpecPatch: '{}'",
        "podResources: {}",
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
fn rejects_unsupported_template_controls_and_defaults() {
    for field in [
        "synchronization: {mutexes: [{name: lock}]}",
        "memoize: {key: cache, cache: {configMap: {name: cache}}}",
        "podSpecPatch: '{}'",
        "podResources: {}",
        "resourceClaims: []",
        "pendingTimeout: 5s",
        "archiveLocation: {s3: {bucket: outputs}}",
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
    assert!(start_error("  templates:\n  - name: main\n    suspend: {duration: 5s}\n").unsupported);
}

#[test]
fn rejects_unsupported_container_lifecycle_controls() {
    for field in [
        "lifecycle",
        "livenessProbe",
        "readinessProbe",
        "startupProbe",
        "restartPolicy",
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
fn rejects_unsupported_retry_controls_and_invalid_limits() {
    for strategy in [
        "backoff: {duration: 5s}",
        "affinity: {nodeAntiAffinity: {}}",
        "retryPolicy: OnTransientError",
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
            assert_eq!(error.unsupported, !strategy.starts_with("limit"), "{error}");
            assert!(
                error.message.contains("retry") || error.message.contains("Retry"),
                "{error}"
            );
        }
    }
}

#[test]
fn empty_container_sets_and_resource_overflows_fail_without_panicking() {
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
fn differing_resource_requests_and_limits_cannot_be_collapsed() {
    let error = start_error(
        "  templates:\n  - name: main\n    container: {image: alpine, resources: {requests: {memory: 1Gi}, limits: {memory: 2Gi}}}\n",
    );
    assert!(
        error.unsupported && error.message.contains("requests and limits"),
        "{error}"
    );
}

#[test]
fn unsupported_node_deadlines_and_retry_delays_are_not_task_limits() {
    for spec in [
        "  templates:\n  - name: main\n    timeout: 5s\n    container: {image: alpine}\n",
        "  templates:\n  - name: main\n    activeDeadlineSeconds: 5\n    initContainers: [{name: init, image: alpine}]\n    container: {image: alpine}\n",
        "  templates:\n  - name: main\n    containerSet:\n      retryStrategy: {retries: 2, duration: 5s}\n      containers: [{name: main, image: alpine}]\n",
    ] {
        assert!(start_error(spec).unsupported);
    }
    for count in ["0", "-1", "invalid"] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    containerSet:\n      retryStrategy: {{retries: {count}}}\n      containers: [{{name: main, image: alpine}}]\n"
        ));
        assert!(error.message.contains("retries"), "{error}");
    }
}

#[test]
fn external_parameter_sources_cannot_turn_into_defaults_or_placeholders() {
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
fn unsupported_artifact_transfer_options_are_refused() {
    for artifact in [
        "git: {repo: 'https://example.org/repository.git'}",
        "s3: {key: input.txt}",
        "s3: {bucket: data, key: input.txt, endpoint: example.org}",
        "http: {url: 'https://example.org/input', headers: [{name: Authorization, value: secret}]}",
        "raw: {data: input}, mode: 0755",
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
        "  templates:\n  - name: main\n    container: {image: alpine}\n    outputs: {artifacts: [{name: output, path: /out, s3: {bucket: data, key: output}}]}\n",
    );
    assert!(
        error.unsupported && error.message.contains("uploads"),
        "{error}"
    );
}

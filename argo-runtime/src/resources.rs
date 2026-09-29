//! Resources of a task from the Kubernetes resources of a container: CPU cores, bytes of memory
//! and disk, GPUs, and the time limit.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::Node;
use workflow_graph::Resources;

/// Resources from Kubernetes limits, or requests where limits say nothing. The shared task
/// model uses these values for both reservation and the container cap.
pub(crate) fn resources_of(node: &Node) -> Result<Resources, String> {
    let argo_parser::Value::Mapping(fields) = &node.value else {
        return Err("container resources must be a mapping".into());
    };
    for field in fields {
        if !matches!(field.key.as_str(), "limits" | "requests") {
            return Err(format!(
                "unsupported: container resource field {} is not supported",
                field.key
            ));
        }
    }
    if let (Some(limits), Some(requests)) = (node.get("limits"), node.get("requests")) {
        for field in [
            "cpu",
            "memory",
            "ephemeral-storage",
            "nvidia.com/gpu",
            "amd.com/gpu",
        ] {
            if let (Some(limit), Some(request)) = (limits.get(field), requests.get(field)) {
                let limit = crate::run::scalar_text(limit);
                let request = crate::run::scalar_text(request);
                let same = match field {
                    "cpu" => cpu_cores(&request)
                        .zip(cpu_cores(&limit))
                        .is_none_or(|(request, limit)| request <= limit),
                    "memory" => quantity_bytes(&request)
                        .zip(quantity_bytes(&limit))
                        .is_none_or(|(request, limit)| request <= limit),
                    "nvidia.com/gpu" | "amd.com/gpu" => {
                        limit.parse::<u32>().ok() == request.parse::<u32>().ok()
                    }
                    _ => quantity_bytes(&limit) == quantity_bytes(&request),
                };
                if !same {
                    if matches!(field, "cpu" | "memory") {
                        return Err(format!(
                            "container resource {field} request must not exceed its limit"
                        ));
                    }
                    return Err(format!(
                        "unsupported: differing {field} requests and limits are not supported"
                    ));
                }
            }
        }
    }
    let mut resources = Resources::default();
    for part in ["requests", "limits"] {
        let Some(values) = node.get(part) else {
            continue;
        };
        if !matches!(values.value, argo_parser::Value::Mapping(_)) {
            return Err(format!("container resources {part} must be a mapping"));
        }
        if let Some(cpu) = values.get("cpu") {
            resources.cpus = Some(
                cpu_cores(&crate::run::scalar_text(cpu))
                    .filter(|cpu| cpu.is_finite() && *cpu >= 0.0)
                    .ok_or_else(|| {
                        "container resource cpu must be finite and nonnegative".to_owned()
                    })?,
            );
        }
        if let Some(memory) = values.get("memory") {
            resources.memory_bytes = Some(
                quantity_bytes(&crate::run::scalar_text(memory))
                    .ok_or_else(|| "invalid or overflowing container memory resource".to_owned())?,
            );
        }
        if let Some(disk) = values.get("ephemeral-storage") {
            resources.disk_bytes = Some(
                quantity_bytes(&crate::run::scalar_text(disk))
                    .ok_or_else(|| "invalid or overflowing container disk resource".to_owned())?,
            );
        }
        for gpu in ["nvidia.com/gpu", "amd.com/gpu"] {
            if let Some(count) = values.get(gpu) {
                resources.gpus = Some(
                    crate::run::scalar_text(count)
                        .parse()
                        .map_err(|_| format!("invalid container resource {gpu}"))?,
                );
            }
        }
        if let argo_parser::Value::Mapping(entries) = &values.value {
            for entry in entries {
                if !matches!(
                    entry.key.as_str(),
                    "cpu" | "memory" | "ephemeral-storage" | "nvidia.com/gpu" | "amd.com/gpu"
                ) {
                    return Err(format!(
                        "unsupported: container resource {} is not supported",
                        entry.key
                    ));
                }
            }
        }
    }
    Ok(resources)
}

/// CPU cores of a quantity like `500m` or `2`.
fn cpu_cores(text: &str) -> Option<f64> {
    let text = text.trim();
    match text.strip_suffix('m') {
        Some(milli) => milli.parse::<f64>().ok().map(|milli| milli / 1000.0),
        None => text.parse().ok(),
    }
}

/// Bytes of a quantity like `4Gi`, `512M` or `1e9`.
fn quantity_bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let suffixes: [(&str, f64); 12] = [
        ("Ki", 1024.0),
        ("Mi", 1024f64.powi(2)),
        ("Gi", 1024f64.powi(3)),
        ("Ti", 1024f64.powi(4)),
        ("Pi", 1024f64.powi(5)),
        ("Ei", 1024f64.powi(6)),
        ("k", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ];
    let bytes = |number: f64| {
        (number.is_finite() && number >= 0.0 && number < u64::MAX as f64)
            .then_some(number.ceil() as u64)
    };
    for (suffix, factor) in suffixes {
        if let Some(number) = text.strip_suffix(suffix) {
            return number
                .parse::<f64>()
                .ok()
                .and_then(|number| bytes(number * factor));
        }
    }
    text.parse::<f64>().ok().and_then(bytes)
}

/// Seconds of a duration like `30s`, `5m`, `1h30m` or a plain number of seconds.
pub(crate) fn duration_seconds(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Ok(seconds) = text.parse::<u64>() {
        return Some(seconds);
    }
    let mut total = 0f64;
    let mut number = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character.is_ascii_digit() || character == '.' {
            number.push(character);
            continue;
        }
        let mut unit = character.to_string();
        if character == 'm' && characters.peek() == Some(&'s') {
            unit.push('s');
            characters.next();
        }
        let value: f64 = number.parse().ok()?;
        number.clear();
        total += value
            * match unit.as_str() {
                "h" => 3600.0,
                "m" => 60.0,
                "s" => 1.0,
                "ms" => 0.001,
                _ => return None,
            };
    }
    if !number.is_empty() {
        return None;
    }
    Some(total.ceil() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities_and_durations() {
        assert_eq!(cpu_cores("500m"), Some(0.5));
        assert_eq!(cpu_cores("4.7"), Some(4.7));
        assert_eq!(quantity_bytes("4Gi"), Some(4 * 1024 * 1024 * 1024));
        assert_eq!(quantity_bytes("512M"), Some(512_000_000));
        assert_eq!(duration_seconds("1h30m"), Some(5400));
        assert_eq!(duration_seconds("45"), Some(45));
        assert_eq!(duration_seconds("x"), None);
    }

    #[test]
    fn preserves_equivalent_resources() {
        let node = argo_parser::parse_yaml(
            "{requests: {cpu: '0.5', memory: 1Gi}, limits: {cpu: 500m, memory: 1024Mi}}",
        )
        .expect("resources")
        .expect("node");
        let resources = resources_of(&node).expect("equivalent quantities");
        assert_eq!(resources.cpus, Some(0.5));
        assert_eq!(resources.memory_bytes, Some(1024 * 1024 * 1024));
    }
}

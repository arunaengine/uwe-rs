//! Resources of a task from the Kubernetes resources of a container: CPU cores, bytes of memory
//! and disk, GPUs, and the time limit.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::Node;
use workflow_graph::Resources;

/// Resources from Kubernetes requests, or limits where requests say nothing.
pub(crate) fn resources_of(node: &Node) -> Resources {
    let mut resources = Resources::default();
    for part in ["limits", "requests"] {
        let Some(values) = node.get(part) else {
            continue;
        };
        if let Some(cpu) = values
            .get("cpu")
            .map(crate::run::scalar_text)
            .and_then(|text| cpu_cores(&text))
        {
            resources.cpus = Some(cpu);
        }
        if let Some(memory) = values
            .get("memory")
            .map(crate::run::scalar_text)
            .and_then(|text| quantity_bytes(&text))
        {
            resources.memory_bytes = Some(memory);
        }
        if let Some(disk) = values
            .get("ephemeral-storage")
            .map(crate::run::scalar_text)
            .and_then(|text| quantity_bytes(&text))
        {
            resources.disk_bytes = Some(disk);
        }
        for gpu in ["nvidia.com/gpu", "amd.com/gpu"] {
            if let Some(count) = values
                .get(gpu)
                .map(crate::run::scalar_text)
                .and_then(|text| text.parse().ok())
            {
                resources.gpus = Some(count);
            }
        }
    }
    resources
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
    for (suffix, factor) in suffixes {
        if let Some(number) = text.strip_suffix(suffix) {
            return number
                .parse::<f64>()
                .ok()
                .map(|number| (number * factor) as u64);
        }
    }
    text.parse::<f64>().ok().map(|number| number as u64)
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
}

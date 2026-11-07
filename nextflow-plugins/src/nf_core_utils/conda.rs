//! The conda channels as `conda config --show channels` shows them, read from conda's files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::interpreter::parse_yaml;
use nextflow_evaluator::value::{Value, tree};

/// The channels in the order conda uses them, or the text of why they cannot be known. uwe
/// runs no conda program, so it reads the `.condarc` files that conda reads.
pub(super) fn channels(host: &dyn Host) -> Value {
    let Some(program) = program(host) else {
        return Value::string("Cannot run program \"conda\": no conda program in PATH");
    };
    let mut channels: Vec<String> = Vec::new();
    // Later files win, and their channels come first.
    for file in files(host, &program).iter().rev() {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let loaded = match parse_yaml(&text) {
            Ok(loaded) => loaded,
            Err(error) => return Value::string(format!("{}: {error}", file.display())),
        };
        let listed = match tree::get(&loaded, &["channels"]) {
            Some(Value::List(list)) => list.borrow().items.clone(),
            Some(Value::Null) | None => Vec::new(),
            Some(single) => vec![single],
        };
        for channel in listed.iter().map(Value::to_string) {
            if !channels.contains(&channel) {
                channels.push(channel);
            }
        }
    }
    if channels.is_empty() {
        channels.push("defaults".to_owned());
    }
    Value::list(channels.into_iter().map(Value::string).collect())
}

/// The `conda` program in `PATH`.
fn program(host: &dyn Host) -> Option<PathBuf> {
    let path = host.environment("PATH")?;
    std::env::split_paths(&path)
        .map(|folder| folder.join("conda"))
        .find(|program| program.is_file())
}

/// The config files of conda, from the lowest to the highest priority, as its documentation
/// lists them: system, the installation, the user, the active environment, then `CONDARC`.
fn files(host: &dyn Host, program: &Path) -> Vec<PathBuf> {
    let variable = |name: &str| host.environment(name).filter(|value| !value.is_empty());
    let mut folders: Vec<PathBuf> = vec!["/etc/conda".into(), "/var/lib/conda".into()];
    let root = program.parent().and_then(Path::parent);
    folders.extend(root.map(Path::to_path_buf));
    if let Some(config) = variable("XDG_CONFIG_HOME") {
        folders.push(Path::new(&config).join("conda"));
    }
    let home = variable("HOME").map(PathBuf::from);
    if let Some(home) = &home {
        folders.push(home.join(".config/conda"));
        folders.push(home.join(".conda"));
    }
    let mut files: Vec<PathBuf> = (folders.iter())
        .flat_map(|folder| [folder.join(".condarc"), folder.join("condarc")])
        .collect();
    files.extend(home.map(|home| home.join(".condarc")));
    if let Some(prefix) = variable("CONDA_PREFIX") {
        files.push(Path::new(&prefix).join(".condarc"));
        files.push(Path::new(&prefix).join("condarc"));
    }
    files.extend(variable("CONDARC").map(PathBuf::from));
    files
}

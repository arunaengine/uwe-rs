//! Upgrades processes of v1.0 documents to the v1.2 model, following the rules of cwl-upgrader.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::binding::LoadListing;
use crate::expression::Dynamic;
use crate::process::{Class, Kind, Process, Version};
use crate::requirement::{Requirement, RequirementClass};

/// Applies the v1.0 to v1.1 rules to one process; v1.1 to v1.2 changes nothing in the model.
/// Class renames and the old `only_non_null` spelling are handled while reading.
pub fn upgrade(process: &mut Process) {
    if process.version != Version::V1_0 {
        return;
    }
    let kind = process.kind();
    for input in &mut process.inputs {
        let Some(binding) = &mut input.binding else {
            continue;
        };
        if let Some(load_contents) = binding.load_contents.take() {
            input.load_contents = Some(load_contents);
        }
        // Inputs of workflows and expression tools have no command line in v1.1.
        if kind != Kind::CommandLineTool {
            input.binding = None;
        }
    }
    if let Class::CommandLineTool(_) = process.class {
        let location = process.location.clone();
        let present = |process: &Process, wanted: fn(&RequirementClass) -> bool| {
            let all = process.requirements.iter().chain(&process.hints);
            all.clone().any(|requirement| wanted(&requirement.class))
        };
        if !present(process, |class| {
            matches!(class, RequirementClass::NetworkAccess { .. })
        }) {
            process.requirements.push(Requirement {
                class: RequirementClass::NetworkAccess {
                    network_access: Dynamic::Value(true),
                },
                extensions: Vec::new(),
                location: location.clone(),
            });
        }
        if !present(process, |class| {
            matches!(class, RequirementClass::LoadListing { .. })
        }) {
            process.requirements.push(Requirement {
                class: RequirementClass::LoadListing {
                    load_listing: Some(LoadListing::DeepListing),
                },
                extensions: Vec::new(),
                location,
            });
        }
    }
}

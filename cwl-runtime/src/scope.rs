//! The requirements and hints in effect for a process: those of the enclosing workflows and
//! steps, then its own. The last entry of a class wins, and any requirement beats any hint.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::model::{LoadListing, Requirement, RequirementClass};
use workflow_graph::DriverError;

use crate::expression::Evaluator;
use crate::types::Types;

/// Extension requirements that change nothing a task needs, so they can be satisfied by
/// ignoring them; by the last part of their class IRI.
const IGNORED_EXTENSIONS: [&str; 6] = [
    "Secrets",
    "ShmSize",
    "RuntimeConstraints",
    "IntermediateOutput",
    "ProcessProperties",
    "OutputStorageClass",
];

#[derive(Clone, Debug, Default)]
pub struct Scope {
    requirements: Arc<Vec<Requirement>>,
    hints: Arc<Vec<Requirement>>,
}

impl Scope {
    /// The scope inside this one, with more requirements and hints after the current ones.
    pub fn with(&self, requirements: &[Requirement], hints: &[Requirement]) -> Scope {
        if requirements.is_empty() && hints.is_empty() {
            return self.clone();
        }
        let mut all_requirements = self.requirements.as_ref().clone();
        all_requirements.extend(requirements.iter().cloned());
        let mut all_hints = self.hints.as_ref().clone();
        all_hints.extend(hints.iter().cloned());
        Scope {
            requirements: Arc::new(all_requirements),
            hints: Arc::new(all_hints),
        }
    }

    /// The entry that `pick` accepts, and whether it is a requirement rather than a hint.
    pub fn find<'s, T>(
        &'s self,
        pick: impl Fn(&'s RequirementClass) -> Option<T>,
    ) -> Option<(T, bool)> {
        let required = self
            .requirements
            .iter()
            .rev()
            .find_map(|entry| pick(&entry.class));
        if let Some(found) = required {
            return Some((found, true));
        }
        let hinted = self.hints.iter().rev().find_map(|entry| pick(&entry.class));
        hinted.map(|found| (found, false))
    }

    /// Whether a requirement or hint of the class holds.
    pub fn has(&self, pick: impl Fn(&RequirementClass) -> bool) -> bool {
        self.find(|class| pick(class).then_some(())).is_some()
    }

    /// The evaluator for expressions: with JavaScript and its `expressionLib` when
    /// `InlineJavascriptRequirement` is a requirement or a hint.
    pub fn evaluator(&self) -> Evaluator {
        let library = self.find(|class| match class {
            RequirementClass::InlineJavascript { expression_lib } => Some(expression_lib),
            _ => None,
        });
        match library {
            Some((library, _)) => Evaluator::with_javascript(library.clone()),
            None => Evaluator::default(),
        }
    }

    /// The named types of every `SchemaDefRequirement` in scope.
    pub fn types(&self) -> Types {
        let mut types = Types::default();
        for entry in self.requirements.iter().chain(self.hints.iter()) {
            if let RequirementClass::SchemaDef { types: defined } = &entry.class {
                for value_type in defined {
                    types.add(value_type);
                }
            }
        }
        types
    }

    pub fn load_listing(&self) -> Option<LoadListing> {
        self.find(|class| match class {
            RequirementClass::LoadListing { load_listing } => *load_listing,
            _ => None,
        })
        .map(|(listing, _)| listing)
    }

    /// Fails for requirements that iunctus cannot satisfy: unknown classes, and software
    /// packages, which need a package resolver. Hints may always be ignored.
    pub fn check_supported(&self) -> Result<(), DriverError> {
        for entry in self.requirements.iter() {
            match &entry.class {
                RequirementClass::Software { .. } => {
                    return Err(DriverError::unsupported(format!(
                        "{}: SoftwareRequirement as a requirement needs a package resolver, \
                         which iunctus does not have; give it as a hint",
                        entry.location
                    )));
                }
                RequirementClass::Other { class, .. } => {
                    let name = class.rsplit(['#', ':']).next().unwrap_or(class);
                    if !IGNORED_EXTENSIONS.contains(&name) && name != "CUDARequirement" {
                        return Err(DriverError::unsupported(format!(
                            "{}: the requirement `{class}` is not supported",
                            entry.location
                        )));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The GPUs of a `cwltool:CUDARequirement`, a requirement or a hint.
    pub fn gpus(&self) -> Option<u32> {
        self.find(|class| match class {
            RequirementClass::Other { class, value } if class.ends_with("CUDARequirement") => {
                let count = value
                    .get("cudaDeviceCountMin")
                    .and_then(|node| match node.value {
                        cwl_parser::Value::Integer(count) => u32::try_from(count).ok(),
                        _ => None,
                    });
                Some(count.unwrap_or(1))
            }
            _ => None,
        })
        .map(|(count, _)| count)
    }
}

//! The CWL versions that load and the classes of processes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

/// The CWL versions that load, in the order of cwltool's update chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    V1_0,
    V1_1Dev1,
    V1_1,
    V1_2Dev1,
    V1_2Dev2,
    V1_2Dev3,
    V1_2Dev4,
    V1_2Dev5,
    V1_2,
}

const VERSIONS: [(&str, Version); 9] = [
    ("v1.0", Version::V1_0),
    ("v1.1.0-dev1", Version::V1_1Dev1),
    ("v1.1", Version::V1_1),
    ("v1.2.0-dev1", Version::V1_2Dev1),
    ("v1.2.0-dev2", Version::V1_2Dev2),
    ("v1.2.0-dev3", Version::V1_2Dev3),
    ("v1.2.0-dev4", Version::V1_2Dev4),
    ("v1.2.0-dev5", Version::V1_2Dev5),
    ("v1.2", Version::V1_2),
];

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let found = VERSIONS.iter().find(|(name, _)| *name == text);
        found.map(|(_, version)| *version)
    }

    /// Whether backslashes in expressions follow the rule before v1.2.0-dev4.
    pub fn old_escapes(self) -> bool {
        self <= Version::V1_2Dev3
    }

    /// The release a development version led to; features are checked against it.
    pub fn release(self) -> Self {
        match self {
            Version::V1_1Dev1 => Version::V1_1,
            Version::V1_2Dev1
            | Version::V1_2Dev2
            | Version::V1_2Dev3
            | Version::V1_2Dev4
            | Version::V1_2Dev5 => Version::V1_2,
            release => release,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = VERSIONS.iter().find(|(_, version)| version == self);
        formatter.write_str(name.map_or("", |(name, _)| name))
    }
}

/// The class of a process without its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    CommandLineTool,
    ExpressionTool,
    Workflow,
    Operation,
}

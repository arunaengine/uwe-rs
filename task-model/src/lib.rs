//! Language-neutral task records: content digests, the hashed specification and its cache key.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod digest;
mod request;
mod resources;
mod spec;
mod tree;

pub use digest::{Digest, DigestError};
pub use request::{Accelerator, Placement, ReuseScope, TaskRequest};
pub use resources::{LEAST_MEMORY, MOST_TOGETHER, Measured, per_container, right_sized};
pub use spec::{
    ContainerSpec, EnvironmentValue, FORMAT, FileKind, Image, InputFile, OutputRule, SpecError,
    TaskSpec,
};
pub use tree::{Tree, TreeEntry};

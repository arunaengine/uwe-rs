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
    ContainerSpec, EnvironmentValue, FORMAT, FileKind, Image, InputFile, OutputRule, RunAs,
    SpecError, TaskSpec,
};
pub use tree::{Tree, TreeEntry};

/// Whether an image names a Conda environment that iunctus prepares, not a registry image.
pub fn conda_image(image: &str) -> bool {
    image.starts_with("conda-env:") || image.starts_with("conda-container:")
}

#[cfg(test)]
mod tests {
    #[test]
    fn detects_conda_images() {
        assert!(super::conda_image("conda-env:/work/env.yaml"));
        assert!(super::conda_image("conda-container:debian|/work/env.yaml"));
        assert!(!super::conda_image("debian:stable-slim"));
    }
}

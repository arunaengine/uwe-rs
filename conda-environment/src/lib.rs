//! Conda environments, as far as they work without network (decision 12): environment files
//! and their match specs, the order of conda versions, and the lock of an environment, which a
//! small resolver makes from the repodata of its channels in a local folder. The lock names
//! the packages by address and digest, so that an image built from it is the same anywhere.
//! Building the image, and downloading repodata and packages, need the network; see
//! docs/design/conda.md.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod version;

pub use version::{Version, VersionSpec};

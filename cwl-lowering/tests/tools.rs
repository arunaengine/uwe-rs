//! Lowers CWL tools into wir, and keeps tools with unknown requirements native.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_lowering::{Lowering, lower};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn lowered(name: &str) -> Lowering {
    let path = fixtures().join(name);
    let document = cwl_parser::load_file(&path, Default::default()).unwrap();
    lower(&document, &fixtures()).unwrap()
}

#[test]
fn keeps_requirements_native() {
    let lowering = lowered("environment.cwl");
    let reason = "the requirement EnvVarRequirement";
    assert_eq!(lowering.kept[0].reason, reason);
    assert!(wir::verify(&lowering.document).is_ok());
}

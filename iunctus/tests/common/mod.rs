//! What the tests that run the `iunctus` binary share: a command that reads no config file of
//! the user and none of the site, so that results do not depend on the machine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::path::PathBuf;
use std::process::Command;

/// The path of the `iunctus` binary that Cargo built for the tests.
pub const BINARY: &str = env!("CARGO_BIN_EXE_iunctus");

/// The `iunctus` binary with its own empty config folder and no site-wide file.
pub fn command() -> Command {
    isolated(Command::new(BINARY))
}

/// Sets the environment of `command` so that iunctus reads no config file of the user (an
/// empty folder in place of `XDG_CONFIG_HOME`) and none of the site (`IUNCTUS_SITE_CONFIG` is
/// empty). A test that needs a config file sets `XDG_CONFIG_HOME` again after this.
pub fn isolated(mut command: Command) -> Command {
    command
        .env("XDG_CONFIG_HOME", empty_config_folder())
        .env(config_file::SITE_FILE_VARIABLE, "");
    command
}

/// A folder without any config file, made once below the target folder of the tests.
fn empty_config_folder() -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("empty-config");
    std::fs::create_dir_all(&folder).expect("makes the empty config folder");
    folder
}

//! Tells users of iunctus, the old name of uwe, which of its folders to move.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Once;

/// The name of uwe before the rename, which named the folders that it left behind.
const OLD_NAME: &str = "iunctus";

/// Prints, at most once per process, one line on standard error for each folder of iunctus
/// whose new folder does not exist yet.
pub fn hint() {
    static PRINTED: Once = Once::new();
    PRINTED.call_once(|| {
        let config = config_file::user_file().and_then(|file| file.parent().map(Path::to_owned));
        let lines = [
            config.and_then(|folder| line(&folder, "its config files")),
            line(&crate::cache_directory(), "its runs and cache"),
        ];
        for line in lines.into_iter().flatten() {
            eprintln!("{line}");
        }
    });
}

/// The hint for the folder `new` when its old sibling exists and `new` does not.
fn line(new: &Path, kept: &str) -> Option<String> {
    let old = new.with_file_name(OLD_NAME);
    (old.is_dir() && !new.exists()).then(|| {
        format!(
            "uwe: found {} from {OLD_NAME}; move it to {} to keep {kept}",
            old.display(),
            new.display()
        )
    })
}

//! Recorded output with its run-specific values masked, for comparisons.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// Masks the run-specific values of these keys: up to a blank after a key that ends with `=`,
/// else up to `<` or the end of the line.
pub fn masked(text: &str) -> String {
    let keys = [
        "dateComplete=",
        "duration=",
        "Date Started: ",
        "Date Completed: ",
        "Pipeline script hash ID: ",
        "Nextflow Build: ",
        "Nextflow Compile Timestamp: ",
    ];
    let mut text = text.to_owned();
    for key in keys {
        let mut from = 0;
        while let Some(found) = text[from..].find(key) {
            let start = from + found + key.len();
            let ends: &[char] = match key.ends_with('=') {
                true => &[' ', '\n'],
                false => &['<', '\n'],
            };
            let end = text[start..]
                .find(ends)
                .map_or(text.len(), |end| start + end);
            text.replace_range(start..end, "*");
            from = start + 1;
        }
    }
    text
}

//! The ids of the entities that are not files, and the form of times.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// An ISO 8601 time in UTC from milliseconds since the Unix epoch, with milliseconds and the
/// offset written as `+00:00`, the form that the profile's checks accept.
pub(crate) fn time(milliseconds: i64) -> String {
    jiff::Timestamp::from_millisecond(milliseconds)
        .map(|time| {
            time.to_zoned(jiff::tz::TimeZone::UTC)
                .strftime("%Y-%m-%dT%H:%M:%S%.3f%:z")
                .to_string()
        })
        .unwrap_or_default()
}

pub(crate) fn run_id(run: &str) -> String {
    format!("#run/{}", encode(run))
}

pub(crate) fn task_id(task: u64) -> String {
    format!("#task/{task}")
}

pub(crate) fn step_id(step: &str) -> String {
    format!("#step/{}", encode(step))
}

pub(crate) fn tool_id(step: &str) -> String {
    format!("#tool/{}", encode(step))
}

pub(crate) fn parameter_id(name: &str) -> String {
    format!("#parameter/{}", encode(name))
}

/// Percent-encodes what may not stand in the fragment of an `@id`, like spaces.
pub(crate) fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:@!$&'()*+,;=/".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

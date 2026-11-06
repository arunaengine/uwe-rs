//! How nf-report writes its files: pretty JSON, TSV with Groovy's texts of values, uwe's own HTML
//! or a user template with plain placeholders, and the links to the latest reports.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

use crate::report_format::Json;

/// A time in the time zone of this machine as Java's `OffsetDateTime` prints it, with the
/// fraction of a second without its trailing zeros, like `2026-11-06T11:33:24.65+01:00`.
pub(crate) fn time(seconds: i64, nanos: i32) -> String {
    let Ok(stamp) = Timestamp::new(seconds, nanos) else {
        return String::new();
    };
    let zoned = stamp.to_zoned(TimeZone::system());
    let mut text = zoned.strftime("%Y-%m-%dT%H:%M").to_string();
    if zoned.second() != 0 || nanos != 0 {
        text.push_str(&format!(":{:02}", zoned.second()));
    }
    if nanos != 0 {
        let fraction = format!("{nanos:09}");
        text.push('.');
        text.push_str(fraction.trim_end_matches('0'));
    }
    let offset = zoned.offset().seconds();
    match offset {
        0 => text.push('Z'),
        _ => {
            let sign = if offset < 0 { '-' } else { '+' };
            let (hours, minutes) = (offset.abs() / 3600, offset.abs() % 3600 / 60);
            text.push_str(&format!("{sign}{hours:02}:{minutes:02}"));
        }
    }
    text
}

/// A trace time in milliseconds since 1970; a missing one is 1970 itself, as in the plugin.
pub(crate) fn millis_time(millis: Option<i64>) -> String {
    let millis = millis.unwrap_or(0);
    let nanos = i32::try_from(millis.rem_euclid(1000) * 1_000_000).unwrap_or(0);
    time(millis.div_euclid(1000), nanos)
}

/// Now, with nanoseconds.
pub(crate) fn now() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = i64::try_from(since.as_secs()).unwrap_or(0);
    time(seconds, i32::try_from(since.subsec_nanos()).unwrap_or(0))
}

/// A size as Nextflow's `MemoryUnit` prints it, like `91.7 KB` or `234 B`.
pub(crate) fn memory(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes == 0 {
        return "0".to_owned();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let shown = format!("{value:.1}");
    let shown = shown.strip_suffix(".0").unwrap_or(&shown);
    format!("{shown} {}", UNITS[unit])
}

/// A value as Groovy prints it: lists as `[a, b]`, maps as `[key:value]`.
pub(crate) fn groovy(value: &Json) -> String {
    match value {
        Json::Null => "null".to_owned(),
        Json::Bool(flag) => flag.to_string(),
        Json::Number(number) if number.is_f64() => {
            let double = Number::Double(number.as_f64().unwrap_or(0.0));
            Value::Number(double).to_string()
        }
        Json::Number(number) => number.to_string(),
        Json::Text(text) => text.clone(),
        Json::List(items) => {
            let items: Vec<String> = items.iter().map(groovy).collect();
            format!("[{}]", items.join(", "))
        }
        Json::Object(entries) if entries.is_empty() => "[:]".to_owned(),
        Json::Object(entries) => {
            let entries: Vec<String> = (entries.iter())
                .map(|(key, value)| format!("{key}:{}", groovy(value)))
                .collect();
            format!("[{}]", entries.join(", "))
        }
    }
}

/// A TSV cell: empty for null, empty text, zero, false and empty lists and maps.
pub(crate) fn cell(value: &Json) -> String {
    let empty = match value {
        Json::Null => true,
        Json::Bool(flag) => !flag,
        Json::Number(number) => number.as_f64() == Some(0.0),
        Json::Text(text) => text.is_empty(),
        Json::List(items) => items.is_empty(),
        Json::Object(entries) => entries.is_empty(),
    };
    if empty { String::new() } else { groovy(value) }
}

/// A TSV table: the header, then a line per row, tabs between cells, no quoting.
pub(crate) fn tsv(header: &[String], rows: &[Vec<String>]) -> String {
    let mut text = header.join("\t");
    text.push('\n');
    for row in rows {
        text.push_str(&row.join("\t"));
        text.push('\n');
    }
    text
}

/// The keys of the objects in first-seen order, or `fields` when it names some.
pub(crate) fn columns<'a>(
    objects: impl Iterator<Item = &'a Json>,
    fields: &[String],
) -> Vec<String> {
    if !fields.is_empty() {
        return fields.to_vec();
    }
    let mut keys: Vec<String> = Vec::new();
    for object in objects {
        if let Json::Object(entries) = object {
            for (key, _) in entries {
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
        }
    }
    keys
}

/// uwe's own HTML page of a report: its title and its data as nested tables.
pub(crate) fn html(title: &str, report: &Json) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n<style>\nbody {{ font-family: sans-serif; margin: 2em; }}\n\
         table {{ border-collapse: collapse; margin: 0.5em 0; }}\n\
         th, td {{ border: 1px solid #ccc; padding: 0.2em 0.5em; text-align: left; \
         vertical-align: top; }}\nth {{ background: #f2f2f2; }}\n</style>\n</head>\n<body>\n\
         <h1>{title}</h1>\n{}\n</body>\n</html>\n",
        block(report)
    )
}

fn block(value: &Json) -> String {
    match value {
        Json::Object(entries) => {
            let rows: Vec<String> = (entries.iter())
                .map(|(key, value)| {
                    format!("<tr><th>{}</th><td>{}</td></tr>", escape(key), block(value))
                })
                .collect();
            format!("<table>{}</table>", rows.concat())
        }
        Json::List(items)
            if !items.is_empty() && items.iter().all(|item| matches!(item, Json::Object(_))) =>
        {
            let header = columns(items.iter(), &[]);
            let head: Vec<String> = header
                .iter()
                .map(|key| format!("<th>{}</th>", escape(key)))
                .collect();
            let rows: Vec<String> = (items.iter())
                .map(|item| {
                    let cells: Vec<String> = (header.iter())
                        .map(|key| {
                            format!("<td>{}</td>", item.get(key).map(block).unwrap_or_default())
                        })
                        .collect();
                    format!("<tr>{}</tr>", cells.concat())
                })
                .collect();
            format!("<table><tr>{}</tr>{}</table>", head.concat(), rows.concat())
        }
        Json::List(items) => {
            let items: Vec<String> = items
                .iter()
                .map(|item| format!("<li>{}</li>", block(item)))
                .collect();
            format!("<ul>{}</ul>", items.concat())
        }
        Json::Null => String::new(),
        scalar => escape(&groovy(scalar)),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A user template with `$name`, `${name}` and `${name.key}` placeholders from the report;
/// `None` with a warning for a template with other Groovy code, which uwe does not run here.
pub(crate) fn template(text: &str, report: &Json, path: &str) -> Option<String> {
    let unsupported = || {
        tracing::warn!(
            "uwe renders only $name and ${{name.key}} placeholders of nf-report templates; \
             {path} has other Groovy code and is skipped"
        );
        None
    };
    if text.contains("<%") {
        return unsupported();
    }
    let mut rendered = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        rendered.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, length) = match after.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (&inner[..end], end + 2),
                None => return unsupported(),
            },
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                    .unwrap_or(after.len());
                let name = after[..end].trim_end_matches('.');
                (name, name.len())
            }
        };
        if name.is_empty() {
            rendered.push('$');
            rest = after;
            continue;
        }
        let simple = name.split('.').all(|part| {
            part.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if !simple {
            return unsupported();
        }
        let mut found = Some(report);
        for part in name.split('.') {
            found = found.and_then(|value| value.get(part));
        }
        rendered.push_str(&found.map(groovy).unwrap_or("null".to_owned()));
        rest = &after[length..];
    }
    rendered.push_str(rest);
    Some(rendered)
}

/// Writes a file, making its folder.
pub(crate) fn write(path: &Path, text: &str) {
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if let Err(error) = std::fs::write(path, text) {
        tracing::warn!("Failed to write to {}: {error}", path.display());
    }
}

/// Points the `-latest` link at the absolute path of the file, replacing an earlier link.
pub(crate) fn link(link: &Path, target: &Path) {
    let _ = std::fs::remove_file(link);
    if let Err(error) = std::os::unix::fs::symlink(target, link) {
        tracing::warn!("Failed to write to {}: {error}", link.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_like_plugin() {
        assert_eq!(memory(93_900), "91.7 KB");
        assert_eq!(memory(234), "234 B");
        assert_eq!(memory(1024), "1 KB");
        let map = Json::object([("greeting", Json::from("hi"))]);
        assert_eq!(groovy(&map), "[greeting:hi]");
        assert_eq!(cell(&Json::from(0i64)), "");
        let report = Json::object([("summary", Json::object([("total", Json::from(3i64))]))]);
        let rendered = template(
            "<p>$summary.total of ${summary.total}.</p>",
            &report,
            "t.html",
        );
        assert_eq!(rendered.as_deref(), Some("<p>3 of 3.</p>"));
        assert!(template("<% x %>", &report, "t.html").is_none());
    }
}

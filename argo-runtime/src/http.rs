//! Lowers HTTP templates to curl tasks and collects their response files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::HttpTemplate;

use crate::objects::KeyRef;
use crate::run::scalar_text;
use crate::value::Value;

/// The image that runs HTTP templates.
pub const CURL_IMAGE: &str = "curlimages/curl:8.16.0";

/// Where the curl of an HTTP template writes the response body and status code.
pub const RESPONSE_BODY: &str = "/tmp/response-body";
pub const RESPONSE_STATUS: &str = "/tmp/response-status";
pub const RESPONSE_HEADERS: &str = "/tmp/response-headers";

/// The headers that read a Secret, with the Secret's key.
pub(crate) fn secret_headers(http: &HttpTemplate) -> Vec<(String, KeyRef)> {
    http.headers
        .iter()
        .filter_map(|header| {
            let reference = header.get("valueFrom")?.get("secretKeyRef")?;
            let name = header.get("name").map(scalar_text).unwrap_or_default();
            Some((name, KeyRef::of(reference)))
        })
        .collect()
}

/// The shell script that sends the request. Headers that read a Secret get their value from
/// `secrets`, by header name.
pub(crate) fn http_script(
    http: &HttpTemplate,
    secrets: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut command: Vec<String> = vec![
        "curl".into(),
        "--silent".into(),
        "--show-error".into(),
        "--location".into(),
    ];
    if http.insecure_skip_verify {
        command.push("--insecure".into());
    }
    let timeout = match &http.timeout_seconds {
        Some(value) => scalar_text(value)
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| "HTTP timeoutSeconds must be a positive integer".to_owned())?,
        None => 30,
    };
    command.extend(["--max-time".to_owned(), timeout.to_string()]);
    let method = if http.method.is_empty() {
        "GET"
    } else {
        http.method.as_str()
    };
    command.extend(["--request".to_owned(), method.to_owned()]);
    for header in &http.headers {
        let name = header.get("name").map(scalar_text).unwrap_or_default();
        let value = match secrets.get(&name) {
            Some(value) if header.get("valueFrom").is_some() => value.clone(),
            _ => header.get("value").map(scalar_text).unwrap_or_default(),
        };
        command.extend(["--header".to_owned(), format!("{name}: {value}")]);
    }
    let body = if let Some(source) = &http.body_from {
        let bytes = source
            .get("bytes")
            .and_then(argo_parser::Node::as_str)
            .ok_or_else(|| "HTTP bodyFrom requires base64 bytes".to_owned())?;
        let bytes = bytes.replace(['\r', '\n'], "");
        let decoded = crate::sprig::base64_decode(&bytes)
            .map_err(|error| format!("HTTP bodyFrom.bytes: {error}"))?;
        if crate::sprig::base64_encode(&decoded) != bytes {
            return Err("HTTP bodyFrom.bytes must contain valid base64".to_owned());
        }
        let escaped: String = decoded
            .iter()
            .map(|byte| format!("\\0{byte:03o}"))
            .collect();
        Some(format!("printf '%b' {} | ", shell_quote(&escaped)))
    } else if !http.body.is_empty() {
        Some(format!("printf '%s' {} | ", shell_quote(&http.body)))
    } else {
        None
    };
    if body.is_some() {
        command.extend(["--data-binary".to_owned(), "@-".to_owned()]);
    }
    command.extend([
        "--output".to_owned(),
        RESPONSE_BODY.to_owned(),
        "--dump-header".to_owned(),
        RESPONSE_HEADERS.to_owned(),
        "--write-out".to_owned(),
        "%{http_code}".to_owned(),
        http.url.clone(),
    ]);
    let quoted: Vec<String> = command.iter().map(|word| shell_quote(word)).collect();
    Ok(format!(
        "{}{} > {RESPONSE_STATUS}",
        body.unwrap_or_default(),
        quoted.join(" ")
    ))
}

/// What the response of an HTTP template makes of its node.
pub(crate) enum Verdict {
    /// The status is not known, as in a simulated run.
    Unknown,
    Succeeded,
    Failed(String),
    Error(String),
}

/// The verdict of the success condition on the response body and the status code, or of a
/// status from 200 to 299 when the template has no condition.
pub(crate) fn http_verdict(
    http: &HttpTemplate,
    body: &str,
    status: &str,
    headers: &str,
) -> Verdict {
    let Ok(status) = status.trim().parse::<i64>() else {
        return Verdict::Unknown;
    };
    let condition = if http.success_condition.is_empty() {
        "response.statusCode >= 200 && response.statusCode < 300"
    } else {
        http.success_condition.as_str()
    };
    let mut response = BTreeMap::new();
    response.insert("statusCode".to_owned(), Value::Int(status));
    response.insert("body".to_owned(), Value::string(body));
    let mut values: BTreeMap<String, Value> = BTreeMap::new();
    for line in headers.lines() {
        if line.starts_with("HTTP/") {
            values.clear();
        } else if let Some((name, value)) = line.split_once(':') {
            let name = name
                .split('-')
                .map(|part| {
                    let mut chars = part.chars();
                    chars
                        .next()
                        .map(|first| {
                            format!(
                                "{}{}",
                                first.to_ascii_uppercase(),
                                chars.as_str().to_ascii_lowercase()
                            )
                        })
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("-");
            if let Value::Array(list) = values
                .entry(name)
                .or_insert_with(|| Value::Array(Vec::new()))
            {
                list.push(Value::string(value.trim()));
            }
        }
    }
    response.insert("headers".to_owned(), Value::Map(values));
    let env: BTreeMap<String, Value> = [("response".to_owned(), Value::Map(response))].into();
    match crate::expr::evaluate_bool(condition, &env) {
        Ok(true) => Verdict::Succeeded,
        Ok(false) => Verdict::Failed(format!("successCondition '{condition}' evaluated false")),
        Err(error) => Verdict::Error(format!("successCondition '{condition}': {error}")),
    }
}

/// A word quoted for a POSIX shell.
fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+".contains(c))
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_status_conditions() {
        let mut http = HttpTemplate::default();
        assert!(matches!(http_verdict(&http, "", "", ""), Verdict::Unknown));
        assert!(matches!(
            http_verdict(&http, "", "204", ""),
            Verdict::Succeeded
        ));
        assert!(matches!(
            http_verdict(&http, "", "404", ""),
            Verdict::Failed(_)
        ));
        http.success_condition = "response.body == \"ok\"".to_owned();
        assert!(matches!(
            http_verdict(&http, "ok", "500", ""),
            Verdict::Succeeded
        ));
    }
}

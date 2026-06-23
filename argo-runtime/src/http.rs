//! HTTP templates. Argo's agent sends the request itself; here the image `curlimages/curl` runs
//! curl, which writes the response body and status code to files. The status code and the body
//! decide the success condition, as in Argo.
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
pub(crate) fn http_script(http: &HttpTemplate, secrets: &BTreeMap<String, String>) -> String {
    let mut command: Vec<String> = vec![
        "curl".into(),
        "--silent".into(),
        "--show-error".into(),
        "--location".into(),
    ];
    if http.insecure_skip_verify {
        command.push("--insecure".into());
    }
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
    if !http.body.is_empty() {
        command.extend(["--data".to_owned(), http.body.clone()]);
    }
    command.extend([
        "--output".to_owned(),
        RESPONSE_BODY.to_owned(),
        "--write-out".to_owned(),
        "%{http_code}".to_owned(),
        http.url.clone(),
    ]);
    let quoted: Vec<String> = command.iter().map(|word| shell_quote(word)).collect();
    format!("{} > {RESPONSE_STATUS}", quoted.join(" "))
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
pub(crate) fn http_verdict(http: &HttpTemplate, body: &str, status: &str) -> Verdict {
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
    response.insert("headers".to_owned(), Value::Map(BTreeMap::new()));
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
    fn verdicts_follow_the_status_and_the_condition() {
        let mut http = HttpTemplate::default();
        assert!(matches!(http_verdict(&http, "", ""), Verdict::Unknown));
        assert!(matches!(http_verdict(&http, "", "204"), Verdict::Succeeded));
        assert!(matches!(http_verdict(&http, "", "404"), Verdict::Failed(_)));
        http.success_condition = "response.body == \"ok\"".to_owned();
        assert!(matches!(
            http_verdict(&http, "ok", "500"),
            Verdict::Succeeded
        ));
    }
}

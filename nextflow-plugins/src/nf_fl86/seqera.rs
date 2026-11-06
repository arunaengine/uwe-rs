//! nf-fl86's Seqera Action launch: the launch settings of an Action from the Seqera Platform
//! API, with a run name and parameters, sent back as a new workflow launch.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use serde_json::{Value as Json, json};

/// How long one request to the platform may take.
const LIMIT: Duration = Duration::from_secs(120);

/// The launch answer, like `{"workflowId": ...}`, or why the launch did not happen; checks the
/// arguments and that the API base is an HTTPS origin first.
pub(crate) fn launch(
    token: &str,
    run_name: &str,
    params: &Json,
    action: &str,
    workspace: &str,
    base: &str,
) -> Result<Json, String> {
    for (name, value) in [
        ("Seqera token", token),
        ("run name", run_name),
        ("action ID", action),
        ("workspace ID", workspace),
        ("Seqera API base URL", base),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{name} must not be empty"));
        }
    }
    let base = base.trim().trim_end_matches('/');
    let (scheme, rest) = base
        .split_once("://")
        .ok_or("Seqera API base URL is invalid")?;
    let host = rest.split('/').next().unwrap_or_default();
    if scheme != "https" || host.is_empty() || rest.contains(['?', '#']) {
        return Err("Seqera API base URL must be an HTTPS origin".to_owned());
    }
    send_launch(token, run_name, params, action, workspace, base)
}

/// Describes the Action and launches its workflow at `base`.
pub(crate) fn send_launch(
    token: &str,
    run_name: &str,
    params: &Json,
    action: &str,
    workspace: &str,
    base: &str,
) -> Result<Json, String> {
    let workspace = encode(workspace.trim());
    let url = format!(
        "{base}/actions/{}?workspaceId={workspace}",
        encode(action.trim())
    );
    let described = request("describe action", "GET", &url, token, None)?;
    let found = described.get("action").filter(|action| action.is_object());
    let found = found.ok_or("Seqera action response did not contain an action")?;
    let settings = found.get("launch").and_then(Json::as_object);
    let mut settings = settings
        .ok_or("Seqera action response did not contain launch settings")?
        .clone();
    for key in ["id", "dateCreated", "sessionId"] {
        settings.remove(key);
    }
    let environment = settings.remove("computeEnv");
    if !settings.contains_key("computeEnvId")
        && let Some(id) = environment
            .as_ref()
            .and_then(|environment| environment.get("id"))
    {
        settings.insert("computeEnvId".to_owned(), id.clone());
    }
    settings.insert("runName".to_owned(), json!(run_name));
    settings.insert("paramsText".to_owned(), json!(params.to_string()));
    let url = format!("{base}/workflow/launch?workspaceId={workspace}");
    let body = json!({ "launch": settings });
    request(
        "launch workflow",
        "POST",
        &url,
        token,
        Some(body.to_string()),
    )
}

/// One request; the JSON object of a 2xx answer.
fn request(
    operation: &str,
    method: &str,
    url: &str,
    token: &str,
    body: Option<String>,
) -> Result<Json, String> {
    let incomplete = || format!("Seqera {operation} request could not be completed");
    let client = storage::http_client().map_err(|_| incomplete())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let runtime = runtime.map_err(|_| incomplete())?;
    let mut request = match method {
        "POST" => client.post(url).header("Content-Type", "application/json"),
        _ => client.get(url),
    };
    request = (request.timeout(LIMIT))
        .header("Accept", "application/json")
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.body(body);
    }
    let (status, text) = runtime
        .block_on(async {
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let text = response.text().await.map_err(|error| error.to_string())?;
            Ok::<_, String>((status, text))
        })
        .map_err(|_| incomplete())?;
    if !(200..300).contains(&status) {
        return Err(format!("Seqera {operation} failed with HTTP {status}"));
    }
    let answer: Json = serde_json::from_str(&text)
        .map_err(|_| format!("Seqera {operation} returned invalid JSON"))?;
    match answer.is_object() {
        true => Ok(answer),
        false => Err(format!(
            "Seqera {operation} returned an unexpected response"
        )),
    }
}

/// Percent-encodes a path or query part, spaces as `%20`.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

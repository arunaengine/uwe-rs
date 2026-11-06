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

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Serves the describe and launch calls; the request lines and bodies it heard.
    fn platform() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let heard = Arc::new(Mutex::new(Vec::new()));
        let log = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    if line.to_ascii_lowercase().starts_with("authorization:") {
                        log.lock().expect("the log").push(line.trim().to_owned());
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                log.lock().expect("the log").push(format!(
                    "{} {}",
                    first.trim(),
                    String::from_utf8_lossy(&body)
                ));
                let reply = match first.starts_with("GET") {
                    true => {
                        r#"{"action":{"launch":{"id":"x","sessionId":"s","computeEnv":{"id":"ce1"},"pipeline":"p"}}}"#
                    }
                    false => r#"{"workflowId":"w1"}"#,
                };
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = (&stream).write_all(answer.as_bytes());
            }
        });
        (base, heard)
    }

    #[test]
    fn launches_action() {
        let (base, heard) = platform();
        let params = json!({ "input_dir": "/out" });
        let answer = send_launch("tok", "next run", &params, "a 1", "42", &base);
        assert_eq!(answer, Ok(json!({ "workflowId": "w1" })));
        let heard = heard.lock().expect("the log").clone();
        assert_eq!(heard[0], "authorization: Bearer tok");
        assert_eq!(heard[1], "GET /actions/a%201?workspaceId=42 HTTP/1.1 ");
        let body = heard[3].split_once(" HTTP/1.1 ").map(|(_, body)| body);
        let sent: Json = serde_json::from_str(body.unwrap_or_default()).expect("a JSON body");
        assert_eq!(
            sent,
            json!({ "launch": { "computeEnvId": "ce1", "pipeline": "p", "runName": "next run",
                "paramsText": "{\"input_dir\":\"/out\"}" } })
        );
        let refused = launch_check("http://127.0.0.1:9");
        assert_eq!(
            refused,
            Err("Seqera API base URL must be an HTTPS origin".to_owned())
        );
        assert_eq!(
            launch("", "r", &params, "1", "2", "https://x"),
            Err("Seqera token must not be empty".to_owned())
        );
    }

    fn launch_check(base: &str) -> Result<Json, String> {
        launch("tok", "r", &json!({}), "1", "2", base)
    }
}

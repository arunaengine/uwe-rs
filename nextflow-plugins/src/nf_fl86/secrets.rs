//! nf-fl86's `getAWSSecret`: the `SecretString` of AWS Secrets Manager's `GetSecretValue`, over
//! HTTPS with the shared Signature Version 4 signer instead of the `aws` program.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use storage::aws::{self, Credentials, Keys};

/// Where AWS requests go and how they are signed.
#[derive(Clone, Debug)]
pub(crate) struct Access {
    pub region: Option<String>,
    pub credentials: Credentials,
    /// The `AWS_ENDPOINT_URL*` variables, for `aws::endpoint`.
    pub endpoints: Vec<(String, String)>,
}

impl Access {
    /// The region and keys of the `aws` scope, else AWS's default chain over the environment.
    pub(crate) fn of(config: &Config, variable: &dyn Fn(&str) -> Option<String>) -> Access {
        let setting = |path: &[&str]| {
            let found = config.get(path).map(|value| value.to_string());
            found.filter(|value| !value.is_empty() && value != "null")
        };
        let region = setting(&["aws", "region"])
            .or_else(|| variable("AWS_REGION"))
            .or_else(|| variable("AWS_DEFAULT_REGION"));
        let credentials = match (
            setting(&["aws", "accessKey"]),
            setting(&["aws", "secretKey"]),
        ) {
            (Some(id), Some(secret)) => Credentials::Keys(Keys {
                id,
                secret,
                token: setting(&["aws", "sessionToken"]),
            }),
            _ => Credentials::Chain {
                environment: (aws::VARIABLES.iter())
                    .filter_map(|name| Some((name.to_string(), variable(name)?)))
                    .collect(),
                home: variable("HOME").map(Into::into),
            },
        };
        let endpoints = [
            "AWS_ENDPOINT_URL",
            "AWS_ENDPOINT_URL_SECRETS_MANAGER",
            "AWS_ENDPOINT_URL_S3",
        ]
        .into_iter()
        .filter_map(|name| Some((name.to_owned(), variable(name)?)))
        .collect();
        Access {
            region,
            credentials,
            endpoints,
        }
    }

    fn variable(&self, name: &str) -> Option<String> {
        let found = self.endpoints.iter().find(|(known, _)| known == name);
        found.map(|(_, value)| value.clone())
    }

    /// The endpoint of a service in a region, after the endpoint variables.
    pub(crate) fn endpoint(&self, service: &str, default: String) -> String {
        aws::endpoint(service, default, |name| self.variable(name))
    }
}

/// The secret's text, trimmed, with the plugin's messages.
pub(crate) fn secret(id: &str, region: Option<&str>, access: &Access) -> Result<String, String> {
    if id.trim().is_empty() {
        return Err("AWS secret ID must not be empty".to_owned());
    }
    let failed = |details: &str| match details.trim() {
        "" => format!("Failed to fetch AWS secret '{id}'"),
        details => format!("Failed to fetch AWS secret '{id}': {details}"),
    };
    let region = (region
        .map(str::to_owned)
        .filter(|region| !region.is_empty()))
    .or_else(|| access.region.clone())
    .ok_or_else(|| failed("no AWS region is set"))?;
    let default = format!("https://secretsmanager.{region}.amazonaws.com");
    let request = aws::Request {
        service: "secretsmanager".to_owned(),
        region,
        method: "POST".to_owned(),
        url: format!("{}/", access.endpoint("SECRETS_MANAGER", default)),
        headers: vec![
            (
                "content-type".to_owned(),
                "application/x-amz-json-1.1".to_owned(),
            ),
            (
                "x-amz-target".to_owned(),
                "secretsmanager.GetSecretValue".to_owned(),
            ),
        ],
        body: serde_json::json!({ "SecretId": id })
            .to_string()
            .into_bytes(),
    };
    let answer =
        aws::send(request, &access.credentials).map_err(|error| failed(&error.to_string()))?;
    let reply: serde_json::Value = serde_json::from_slice(&answer.body).unwrap_or_default();
    if answer.status >= 300 {
        let message = (reply.get("message").or_else(|| reply.get("Message")))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&answer.reason);
        let kind = reply["__type"].as_str().unwrap_or_default();
        let kind = kind.rsplit('#').next().unwrap_or_default();
        return Err(failed(
            format!("{kind}: {message}").trim_start_matches(": "),
        ));
    }
    match reply["SecretString"].as_str() {
        Some(text) => Ok(text.trim().to_owned()),
        None => Err(failed("the secret has no SecretString")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// Answers one request with this status and body; the request it read.
    fn serve(
        status: &'static str,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a request");
            let mut reader = BufReader::new(&stream);
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
            }
            let mut received = vec![0; length];
            let _ = reader.read_exact(&mut received);
            let answer = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = (&stream).write_all(answer.as_bytes());
            head + &String::from_utf8_lossy(&received)
        });
        (base, handle)
    }

    fn access(base: &str) -> Access {
        Access {
            region: Some("eu-west-1".to_owned()),
            credentials: Credentials::Keys(Keys {
                id: "AKIDEXAMPLE".to_owned(),
                secret: "example".to_owned(),
                token: None,
            }),
            endpoints: vec![(
                "AWS_ENDPOINT_URL_SECRETS_MANAGER".to_owned(),
                base.to_owned(),
            )],
        }
    }

    #[test]
    fn fetches_secret_value() {
        let (base, request) = serve("200 OK", r#"{"Name":"seqera","SecretString":" tok \n"}"#);
        assert_eq!(secret("seqera", None, &access(&base)).as_deref(), Ok("tok"));
        let request = request.join().expect("the request");
        assert!(
            request.contains("x-amz-target: secretsmanager.GetSecretValue"),
            "{request}"
        );
        assert!(request.ends_with(r#"{"SecretId":"seqera"}"#), "{request}");
        assert!(
            request.contains("/eu-west-1/secretsmanager/aws4_request"),
            "{request}"
        );
        let body = r#"{"__type":"ResourceNotFoundException","message":"Secrets Manager can't find the specified secret."}"#;
        let (base, _) = serve("400 Bad Request", body);
        assert_eq!(
            secret("gone", Some("us-east-1"), &access(&base)),
            Err("Failed to fetch AWS secret 'gone': ResourceNotFoundException: Secrets Manager can't find the specified secret.".to_owned())
        );
        let blank = secret(" ", None, &access(&base));
        assert_eq!(blank, Err("AWS secret ID must not be empty".to_owned()));
    }
}

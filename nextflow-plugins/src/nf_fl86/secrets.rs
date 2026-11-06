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

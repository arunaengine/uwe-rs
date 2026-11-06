//! Gets OAuth 2.0 access tokens for Google APIs: from a service account key through a signed
//! JWT, from the refresh token of a user, or from the metadata server of a Google VM.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::RsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::{Value, json};

/// The scope of the tokens: all Google Cloud APIs that the identity may use.
pub const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Where user credentials without a `token_uri` refresh their token.
const USER_TOKEN: &str = "https://oauth2.googleapis.com/token";

/// The metadata server of Google VMs, unless `GCE_METADATA_HOST` names another.
const METADATA_HOST: &str = "metadata.google.internal";

/// How long before its end a token is replaced, so that no call carries an expired one.
const MARGIN: Duration = Duration::from_secs(60);

/// How long one token request may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Where the tokens come from, and the project that the credentials name.
pub struct Credentials {
    pub source: Source,
    /// The `project_id` of a service account key, or the `quota_project_id` of user credentials.
    pub project: Option<String>,
    /// The file the credentials came from, for messages.
    pub file: Option<PathBuf>,
    cached: tokio::sync::Mutex<Option<(String, Instant)>>,
}

/// A source of access tokens.
pub enum Source {
    /// A service account key: a JWT signed with its key goes to its `token_uri`.
    Account {
        email: String,
        key_id: String,
        key: Box<RsaPrivateKey>,
        token_uri: String,
    },
    /// User credentials, as gcloud writes them: their refresh token goes to `token_uri`.
    User {
        client_id: String,
        client_secret: String,
        refresh_token: String,
        token_uri: String,
    },
    /// The metadata server at `host`, which gives the token of the VM's service account.
    Metadata { host: String },
}

/// Why no token came.
#[derive(Debug, thiserror::Error)]
#[error("cannot get a Google access token: {0}")]
pub struct TokenError(pub String);

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key and the refresh token never go into logs.
        let source = match &self.source {
            Source::Account { email, .. } => format!("service account {email}"),
            Source::User { .. } => "user credentials".into(),
            Source::Metadata { host } => format!("metadata server {host}"),
        };
        (formatter.debug_struct("Credentials"))
            .field("source", &source)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn new(source: Source, project: Option<String>) -> Credentials {
        Credentials {
            source,
            project,
            file: None,
            cached: tokio::sync::Mutex::new(None),
        }
    }

    /// Application default credentials: `file`, else the file of
    /// `GOOGLE_APPLICATION_CREDENTIALS`, else gcloud's file, else the metadata server.
    pub fn find(
        file: Option<&Path>,
        variable: impl Fn(&str) -> Option<String>,
    ) -> Result<Credentials, String> {
        let set = |name: &str| variable(name).filter(|value| !value.is_empty());
        if let Some(file) = file {
            return Credentials::read(file);
        }
        if let Some(file) = set("GOOGLE_APPLICATION_CREDENTIALS") {
            return Credentials::read(Path::new(&file));
        }
        let gcloud = (set("CLOUDSDK_CONFIG").map(PathBuf::from))
            .or_else(|| set("HOME").map(|home| Path::new(&home).join(".config/gcloud")));
        let known = gcloud.map(|folder| folder.join("application_default_credentials.json"));
        if let Some(known) = known.filter(|known| known.is_file()) {
            return Credentials::read(&known);
        }
        let host = set("GCE_METADATA_HOST").unwrap_or_else(|| METADATA_HOST.into());
        Ok(Credentials::new(Source::Metadata { host }, None))
    }

    /// The credentials of a service account key or user credentials file.
    pub fn read(file: &Path) -> Result<Credentials, String> {
        let shown = file.display();
        let text = std::fs::read_to_string(file).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("Missing Google credentials file: {shown}"),
            _ => format!("Invalid or corrupted Google credentials file: {shown}"),
        })?;
        let invalid = || format!("Invalid or corrupted Google credentials file: {shown}");
        let json: Value = serde_json::from_str(&text).map_err(|_| invalid())?;
        let field = |name: &str| json[name].as_str().map(str::to_owned);
        let needed = |name: &str| field(name).ok_or_else(invalid);
        let (source, project) = match json["type"].as_str() {
            Some("service_account") => {
                let key = RsaPrivateKey::from_pkcs8_pem(&needed("private_key")?)
                    .map_err(|_| invalid())?;
                let source = Source::Account {
                    email: needed("client_email")?,
                    key_id: field("private_key_id").unwrap_or_default(),
                    key: Box::new(key),
                    token_uri: field("token_uri").unwrap_or_else(|| USER_TOKEN.into()),
                };
                (source, field("project_id"))
            }
            Some("authorized_user") => {
                let source = Source::User {
                    client_id: needed("client_id")?,
                    client_secret: needed("client_secret")?,
                    refresh_token: needed("refresh_token")?,
                    token_uri: field("token_uri").unwrap_or_else(|| USER_TOKEN.into()),
                };
                (source, field("quota_project_id"))
            }
            kind => {
                return Err(format!(
                    "uwe takes service account keys and user credentials, not Google \
                     credentials of type `{}`: {shown}",
                    kind.unwrap_or_default()
                ));
            }
        };
        Ok(Credentials {
            file: Some(file.to_owned()),
            ..Credentials::new(source, project)
        })
    }

    /// The project of the jobs: `named`, else that of the credentials, else
    /// `GOOGLE_CLOUD_PROJECT`. A service account key of another project is refused, as Nextflow
    /// does.
    pub fn project(
        &self,
        named: Option<&str>,
        variable: impl Fn(&str) -> Option<String>,
    ) -> Result<String, String> {
        let account = matches!(self.source, Source::Account { .. });
        if let (Some(named), Some(own), true) = (named, &self.project, account)
            && named != own
        {
            let file = self.file.as_deref().unwrap_or(Path::new("")).display();
            return Err(format!(
                "the Google project `{named}` does not match the project `{own}` of the \
                 credentials file {file}"
            ));
        }
        (named.map(str::to_owned))
            .or_else(|| self.project.clone())
            .or_else(|| variable("GOOGLE_CLOUD_PROJECT").filter(|value| !value.is_empty()))
            .ok_or_else(|| {
                "Google Batch needs a project: set `project` in [executor.google-batch], \
                 `google.project` in the Nextflow config, or GOOGLE_CLOUD_PROJECT"
                    .to_owned()
            })
    }

    /// An access token that is valid for at least another minute.
    pub async fn token(&self) -> Result<String, TokenError> {
        let mut cached = self.cached.lock().await;
        if let Some((token, until)) = cached.as_ref()
            && Instant::now() + MARGIN < *until
        {
            return Ok(token.clone());
        }
        let (token, lifetime) = self.fetch().await?;
        *cached = Some((token.clone(), Instant::now() + lifetime));
        Ok(token)
    }

    /// A new token from the source, and how long it is valid.
    async fn fetch(&self) -> Result<(String, Duration), TokenError> {
        let client = storage::http_client().map_err(TokenError)?;
        let request = match &self.source {
            Source::Account {
                email,
                key_id,
                key,
                token_uri,
            } => {
                let now = (SystemTime::now().duration_since(UNIX_EPOCH))
                    .map_err(|error| TokenError(error.to_string()))?
                    .as_secs();
                let assertion = assertion(email, key_id, key, token_uri, now)?;
                let grant = "urn:ietf:params:oauth:grant-type:jwt-bearer";
                let body = form(&[("grant_type", grant), ("assertion", &assertion)]);
                client.post(token_uri).body(body)
            }
            Source::User {
                client_id,
                client_secret,
                refresh_token,
                token_uri,
            } => {
                let body = form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id),
                    ("client_secret", client_secret),
                    ("refresh_token", refresh_token),
                ]);
                client.post(token_uri).body(body)
            }
            Source::Metadata { host } => {
                let url = format!(
                    "http://{host}/computeMetadata/v1/instance/service-accounts/default/token"
                );
                client.get(url).header("Metadata-Flavor", "Google")
            }
        };
        let request = request
            .header("content-type", "application/x-www-form-urlencoded")
            .timeout(TIMEOUT);
        let answer =
            (request.send().await).map_err(|error| TokenError(error.without_url().to_string()))?;
        let status = answer.status();
        let body: Value = answer.json().await.unwrap_or(Value::Null);
        let token = body["access_token"]
            .as_str()
            .filter(|_| status.is_success());
        let Some(token) = token else {
            // The answer of a refusal names the reason, never a secret of the request.
            let reason = (body["error_description"].as_str())
                .or_else(|| body["error"].as_str())
                .unwrap_or_default();
            return Err(TokenError(format!(
                "the token server answered {status} {reason}"
            )));
        };
        let seconds = body["expires_in"].as_u64().unwrap_or(3600);
        Ok((token.to_owned(), Duration::from_secs(seconds)))
    }
}

/// The JWT that a service account key exchanges for a token: RS256, valid for an hour from
/// `now`, in Unix seconds.
pub fn assertion(
    email: &str,
    key_id: &str,
    key: &RsaPrivateKey,
    token_uri: &str,
    now: u64,
) -> Result<String, TokenError> {
    let header = json!({"alg": "RS256", "typ": "JWT", "kid": key_id});
    let claims = json!({
        "iss": email,
        "scope": SCOPE,
        "aud": token_uri,
        "iat": now,
        "exp": now + 3600,
    });
    let encode = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let signed = format!("{}.{}", encode(&header), encode(&claims));
    let signer = SigningKey::<Sha256>::new(key.clone());
    let signature = (signer.try_sign(signed.as_bytes()))
        .map_err(|error| TokenError(format!("cannot sign the token request: {error}")))?;
    Ok(format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

/// A form body, with every byte but letters, digits and `-._~` percent-encoded.
fn form(pairs: &[(&str, &str)]) -> String {
    let encode = |text: &str| {
        let mut encoded = String::new();
        for byte in text.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    encoded.push(char::from(byte))
                }
                _ => encoded.push_str(&format!("%{byte:02X}")),
            }
        }
        encoded
    };
    let pairs = pairs
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)));
    pairs.collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_forms() {
        let body = form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "1//0g a+b"),
        ]);
        assert_eq!(
            body,
            "grant_type=refresh_token&refresh_token=1%2F%2F0g%20a%2Bb"
        );
    }

    #[test]
    fn reads_credential_files() {
        let folder = tempfile::tempdir().expect("a folder");
        let missing = folder.path().join("none.json");
        let error = Credentials::read(&missing).expect_err("no file");
        assert!(
            error.starts_with("Missing Google credentials file"),
            "{error}"
        );
        let file = folder.path().join("user.json");
        let user = json!({
            "type": "authorized_user",
            "client_id": "id",
            "client_secret": "secret",
            "refresh_token": "refresh",
            "quota_project_id": "lab",
        });
        std::fs::write(&file, user.to_string()).expect("writes");
        let credentials = Credentials::read(&file).expect("credentials");
        assert!(
            matches!(&credentials.source, Source::User { token_uri, .. } if token_uri == USER_TOKEN)
        );
        let shown = format!("{credentials:?}");
        assert!(
            !shown.contains("refresh") && !shown.contains("secret"),
            "{shown}"
        );
        let none = |_: &str| None;
        assert_eq!(credentials.project(None, none).as_deref(), Ok("lab"));
        assert_eq!(
            credentials.project(Some("other"), none).as_deref(),
            Ok("other")
        );
        std::fs::write(&file, "{\"type\": \"external_account\"}").expect("writes");
        let error = Credentials::read(&file).expect_err("another type");
        assert!(error.contains("`external_account`"), "{error}");
        std::fs::write(&file, "{").expect("writes");
        let error = Credentials::read(&file).expect_err("no JSON");
        assert!(error.starts_with("Invalid or corrupted"), "{error}");
        let metadata = Credentials::find(None, |name| match name {
            "GCE_METADATA_HOST" => Some("127.0.0.1:1".into()),
            "HOME" => Some(folder.path().to_string_lossy().into_owned()),
            _ => None,
        });
        let metadata = metadata.expect("the metadata server");
        assert!(matches!(&metadata.source, Source::Metadata { host } if host == "127.0.0.1:1"));
        let error = metadata.project(None, none).expect_err("no project");
        assert!(error.contains("google.project"), "{error}");
    }
}

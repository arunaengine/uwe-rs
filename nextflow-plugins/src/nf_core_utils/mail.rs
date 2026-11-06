//! Sends the completion e-mail through SMTP or AWS SES, with the `mail` and `aws` config scopes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::mpsc;
use std::time::Duration;

use base64::Engine;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Body, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{Message, SmtpTransport, Transport};
use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::{PathValue, Value, tree};
use storage::aws;

use super::text_setting;
use crate::Context;

/// How long a delivery may take, as the plugin waits for it.
const LIMIT: Duration = Duration::from_secs(30);

/// Why a mail was not sent.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// The config asks for a transport that uwe does not have.
    #[error("{0}")]
    Skipped(String),
    #[error("{0}")]
    Failed(String),
    /// The delivery took longer than `LIMIT`; it may still arrive.
    #[error("the delivery timed out")]
    Timeout,
}

/// Sends a message of `functions.nf`: `to`, `subject`, `text`, and for HTML mail `html`, the
/// `logo` file and the `attachment` file. The result has the `status`, `sent`, `timeout`,
/// `skipped` or `failed`, and the `message` of a failure.
pub(super) fn deliver(message: &Value, context: &mut Context) -> Value {
    let outcome = prepare(message, context).and_then(|(route, email)| send(route, email));
    let (status, text) = match outcome {
        Ok(()) => ("sent", String::new()),
        Err(failure @ Failure::Skipped(_)) => ("skipped", failure.to_string()),
        Err(failure @ Failure::Failed(_)) => ("failed", failure.to_string()),
        Err(Failure::Timeout) => ("timeout", String::new()),
    };
    Value::map([
        (Value::string("status"), Value::string(status)),
        (Value::string("message"), Value::string(text)),
    ])
}

/// Where a mail goes: an SMTP server, or AWS SES.
enum Route {
    Smtp(SmtpTransport),
    Ses(Ses),
}

/// The SES v2 endpoint of a region, with the credentials that sign its requests.
struct Ses {
    endpoint: String,
    region: String,
    credentials: aws::Credentials,
}

fn prepare(message: &Value, context: &mut Context) -> Result<(Route, Message), Failure> {
    let config = context.config;
    let host = &*context.host;
    let ses = host.environment("NXF_ENABLE_AWS_SES");
    let region = text_setting(config, &["aws", "region"])
        .or_else(|| host.environment("AWS_REGION"))
        .or_else(|| host.environment("AWS_DEFAULT_REGION"));
    let amazon = crate::catalogue::configured(config)
        .iter()
        .any(|id| id == "nf-amazon");
    // Nextflow's order: SES when asked for, else SMTP, else SES when nf-amazon has a region.
    let route = match text_setting(config, &["mail", "smtp", "host"]) {
        _ if ses.is_some_and(|value| value == "true") => {
            Route::Ses(ses_route(config, host, region))
        }
        Some(server) => Route::Smtp(transport(config, &server)?),
        None if amazon && region.is_some() => Route::Ses(ses_route(config, host, region)),
        None => {
            return Err(Failure::Skipped(
                "uwe sends mail through SMTP or AWS SES, and the config sets no \
                 mail.smtp.host and does not turn on SES"
                    .to_owned(),
            ));
        }
    };
    let from = sender(config, host);
    let email = build(message, &from, &mut *context.host)?;
    Ok((route, email))
}

/// SES in the region of the `aws` scope or the environment, else `us-east-1`, with the keys of
/// the `aws` scope, else AWS's default credential chain.
fn ses_route(config: &Config, host: &dyn Host, region: Option<String>) -> Ses {
    let region = region.unwrap_or_else(|| "us-east-1".to_owned());
    let default = format!("https://email.{region}.amazonaws.com");
    let endpoint = aws::endpoint("SESV2", default, |name| host.environment(name));
    let keys = (text_setting(config, &["aws", "accessKey"]))
        .zip(text_setting(config, &["aws", "secretKey"]));
    let credentials = match keys {
        Some((id, secret)) => aws::Credentials::Keys(aws::Keys {
            id,
            secret,
            token: text_setting(config, &["aws", "sessionToken"]),
        }),
        None => aws::Credentials::Chain {
            environment: (aws::VARIABLES.iter())
                .filter_map(|name| Some((name.to_string(), host.environment(name)?)))
                .collect(),
            home: host.environment("HOME").map(Into::into),
        },
    };
    Ses {
        endpoint,
        region,
        credentials,
    }
}

/// Sends the raw message with SES v2 `SendEmail`.
fn ses_send(ses: Ses, email: &Message) -> Result<(), String> {
    let envelope = email.envelope();
    let from = envelope.from().map(ToString::to_string).unwrap_or_default();
    let to: Vec<String> = envelope.to().iter().map(ToString::to_string).collect();
    let data = base64::engine::general_purpose::STANDARD.encode(email.formatted());
    let body = serde_json::json!({
        "FromEmailAddress": from,
        "Destination": {"ToAddresses": to},
        "Content": {"Raw": {"Data": data}},
    });
    let request = aws::Request {
        service: "ses".to_owned(),
        region: ses.region,
        method: "POST".to_owned(),
        url: format!("{}/v2/email/outbound-emails", ses.endpoint),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: body.to_string().into_bytes(),
    };
    let answer = aws::send(request, &ses.credentials).map_err(|error| error.to_string())?;
    if (200..300).contains(&answer.status) {
        return Ok(());
    }
    let reply: serde_json::Value = serde_json::from_slice(&answer.body).unwrap_or_default();
    let reason = (reply.get("message").or_else(|| reply.get("Message")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&answer.reason);
    Err(format!("AWS SES answered {}: {reason}", answer.status))
}

/// Sends in a thread of its own, so that a server that does not answer cannot hold the run
/// longer than `LIMIT`.
fn send(route: Route, email: Message) -> Result<(), Failure> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("nf-core-utils mail".to_owned())
        .spawn(move || {
            let sent = match route {
                Route::Smtp(transport) => {
                    (transport.send(&email).map(drop)).map_err(|error| error.to_string())
                }
                Route::Ses(ses) => ses_send(ses, &email),
            };
            let _ = sender.send(sent);
        })
        .map_err(|error| Failure::Failed(error.to_string()))?;
    match receiver.recv_timeout(LIMIT) {
        Ok(sent) => sent.map_err(Failure::Failed),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Failure::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(Failure::Failed("the mail sender stopped".to_owned()))
        }
    }
}

/// The SMTP connection of the JavaMail settings that Nextflow passes on: `ssl.enable` or the
/// protocol `smtps` for TLS from the start, `starttls.enable` and `starttls.required`, and
/// the account of `user` and `password` unless `auth` is false.
fn transport(config: &Config, server: &str) -> Result<SmtpTransport, Failure> {
    let setting = |name: &str| config.get(&["mail", "smtp", name]);
    let enabled = |path: &[&str]| {
        config
            .get(path)
            .is_some_and(|value| value.to_string() == "true")
    };
    let protocol = text_setting(config, &["mail", "transport", "protocol"]);
    let implicit =
        enabled(&["mail", "smtp", "ssl", "enable"]) || protocol.as_deref() == Some("smtps");
    let parameters = || {
        TlsParameters::new(server.to_owned()).map_err(|error| Failure::Failed(error.to_string()))
    };
    let tls = if implicit {
        Tls::Wrapper(parameters()?)
    } else if enabled(&["mail", "smtp", "starttls", "required"]) {
        Tls::Required(parameters()?)
    } else if enabled(&["mail", "smtp", "starttls", "enable"]) {
        Tls::Opportunistic(parameters()?)
    } else {
        Tls::None
    };
    let port = match setting("port") {
        None | Some(Value::Null) => match implicit {
            true => 465,
            false => 25,
        },
        Some(value) => value.to_string().parse().map_err(|_| {
            Failure::Failed(format!("the mail.smtp.port {value} is not a port number"))
        })?,
    };
    let mut builder = SmtpTransport::builder_dangerous(server)
        .port(port)
        .tls(tls)
        .timeout(Some(LIMIT));
    let authenticate = setting("auth").is_none_or(|value| value.to_string() != "false");
    if let Some(user) = text_setting(config, &["mail", "smtp", "user"]).filter(|_| authenticate) {
        let password = text_setting(config, &["mail", "smtp", "password"]).unwrap_or_default();
        builder = builder.credentials(Credentials::new(user, password));
    }
    Ok(builder.build())
}

/// `mail.from`, else the user at this host, as JavaMail makes a local address.
fn sender(config: &Config, host: &dyn Host) -> String {
    text_setting(config, &["mail", "from"]).unwrap_or_else(|| {
        let user = host
            .environment("USER")
            .unwrap_or_else(|| "nextflow".to_owned());
        format!("{user}@{}", ClientId::default())
    })
}

/// The message as Nextflow's mailer lays it out: the text and HTML alternatives, then the
/// inline logo and the attachment, all in one `multipart/related`.
fn build(message: &Value, from: &str, host: &mut dyn Host) -> Result<Message, Failure> {
    let field =
        |name: &str| tree::get(message, &[name]).filter(|value| !matches!(value, Value::Null));
    let text = |name: &str| {
        field(name)
            .map(|value| value.to_string())
            .unwrap_or_default()
    };
    let mut alternative = MultiPart::alternative().singlepart(SinglePart::plain(text("text")));
    if let Some(html) = field("html") {
        alternative = alternative.singlepart(SinglePart::html(html.to_string()));
    }
    let mut related = MultiPart::related().multipart(alternative);
    if let Some(logo) = field("logo") {
        let (name, bytes) = read(&logo, host)?;
        let id = "nfcorepipelinelogo".to_owned();
        let kind =
            ContentType::parse("image/png").map_err(|error| Failure::Failed(error.to_string()))?;
        let logo = Attachment::new_inline_with_name(id, name).body(body(bytes), kind);
        related = related.singlepart(logo);
    }
    if let Some(report) = field("attachment") {
        let (name, bytes) = read(&report, host)?;
        let kind = match name.ends_with(".html") || name.ends_with(".htm") {
            true => ContentType::TEXT_HTML,
            false => ContentType::parse("application/octet-stream")
                .map_err(|error| Failure::Failed(error.to_string()))?,
        };
        related = related.singlepart(Attachment::new(name).body(body(bytes), kind));
    }
    let address = |text: &str| {
        text.parse::<Mailbox>()
            .map_err(|error| Failure::Failed(format!("the address {text}: {error}")))
    };
    Message::builder()
        .from(address(from)?)
        .to(address(&text("to"))?)
        .subject(text("subject"))
        .multipart(related)
        .map_err(|error| Failure::Failed(error.to_string()))
}

/// The body of a file: text as text, which lettre may send in 7bit, else base64.
fn body(bytes: Vec<u8>) -> Body {
    match String::from_utf8(bytes) {
        Ok(text) => Body::new(text),
        Err(error) => Body::new(error.into_bytes()),
    }
}

/// The name and bytes of a file to send, also of a remote one.
fn read(value: &Value, host: &mut dyn Host) -> Result<(String, Vec<u8>), Failure> {
    let path = match value {
        Value::Path(path) => (**path).clone(),
        other => PathValue::parse(&other.to_string(), &host.launch_dir()),
    };
    let bytes = (host.read(&path))
        .map_err(|error| Failure::Failed(format!("cannot read {}: {error}", path.uri())))?;
    Ok((path.name().to_owned(), bytes))
}

//! The iRODS protocol: framed XML messages, the negotiation of TLS, and password sign-in through
//! the authentication API of iRODS 4.3 and later.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use md5::{Digest, Md5};
use rustls_platform_verifier::BuilderVerifierExt;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, crypto};

use crate::error::Problem;

/// The numbers of the API requests that uwe sends.
pub(crate) const CREATE: u32 = 601;
pub(crate) const OPEN: u32 = 602;
pub(crate) const STAT: u32 = 633;
pub(crate) const CLOSE: u32 = 673;
pub(crate) const READ: u32 = 675;
pub(crate) const WRITE: u32 = 676;
pub(crate) const COLLECTION: u32 = 681;
pub(crate) const QUERY: u32 = 702;
const AUTHENTICATE: u32 = 110000;

/// How long uwe waits for a server to answer a connection.
const CONNECT: Duration = Duration::from_secs(30);
/// The sizes in the native password hash: a challenge of 64 bytes, a password of up to 50.
const CHALLENGE: usize = 64;
const PASSWORD: usize = 50;
/// The parameters of the shared secret that a TLS session sends, the defaults of iRODS clients.
const ALGORITHM: &str = "AES-256-CBC";
const KEY_SIZE: usize = 32;
const SALT_SIZE: usize = 8;
const HASH_ROUNDS: usize = 16;

/// Who signs in where, with every value known.
pub(crate) struct Account {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub zone: String,
    pub password: String,
    pub login: Login,
    pub policy: Policy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Login {
    Native,
    /// A PAM password, which the server turns into a short native password for the session.
    PamPassword,
}

/// Whether a side of a connection wants TLS: `CS_NEG_REQUIRE`, `CS_NEG_DONT_CARE` or
/// `CS_NEG_REFUSE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Policy {
    Require,
    DontCare,
    Refuse,
}

impl Policy {
    pub fn parse(text: &str) -> Option<Policy> {
        match text.trim() {
            "CS_NEG_REQUIRE" => Some(Policy::Require),
            "CS_NEG_DONT_CARE" => Some(Policy::DontCare),
            "CS_NEG_REFUSE" => Some(Policy::Refuse),
            _ => None,
        }
    }
}

/// Whether a connection uses TLS when both sides have these policies; `None` when they
/// cannot agree.
pub(crate) fn negotiate(client: Policy, server: Policy) -> Option<bool> {
    match (client, server) {
        (Policy::Require, Policy::Refuse) | (Policy::Refuse, Policy::Require) => None,
        (Policy::Refuse, _) | (_, Policy::Refuse) => Some(false),
        _ => Some(true),
    }
}

/// A message: its type, its XML body, an error part and a byte stream.
struct Message {
    kind: String,
    body: Vec<u8>,
    error: Vec<u8>,
    bytes: Vec<u8>,
    info: i32,
}

/// The answer to an API request: its status or count, its XML body and its bytes.
pub(crate) struct Reply {
    pub info: i32,
    pub body: String,
    pub bytes: Vec<u8>,
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// A signed-in connection to an iRODS server.
pub(crate) struct Session {
    stream: Box<dyn Stream>,
}

impl Session {
    /// Connects, agrees on TLS with the server, and signs in.
    pub async fn connect(account: &Account) -> Result<Session, Problem> {
        let address = format!("{}:{}", account.host, account.port);
        tracing::debug!(
            "Connecting to iRODS at {address} as {} (zone: {})",
            account.user,
            account.zone
        );
        let mut tcp = (tokio::time::timeout(CONNECT, TcpStream::connect(&address)).await)
            .map_err(|_| Problem::passing(format!("the iRODS server {address} did not answer")))?
            .map_err(|error| Problem::passing(format!("cannot reach {address}: {error}")))?;
        let startup = format!(
            "<StartupPack_PI><irodsProt>1</irodsProt><reconnFlag>0</reconnFlag>\
             <connectCnt>0</connectCnt><proxyUser>{user}</proxyUser>\
             <proxyRcatZone>{zone}</proxyRcatZone><clientUser>{user}</clientUser>\
             <clientRcatZone>{zone}</clientRcatZone><relVersion>rods4.3.0</relVersion>\
             <apiVersion>d</apiVersion><option>uwe;request_server_negotiation</option>\
             </StartupPack_PI>",
            user = escape(&account.user),
            zone = escape(&account.zone),
        );
        send(&mut tcp, "RODS_CONNECT", startup.as_bytes(), &[], 0).await?;
        let mut answer = receive(&mut tcp).await?;
        let mut tls = false;
        if answer.kind == "RODS_CS_NEG_T" {
            let server = tag(&text(&answer.body), "result").and_then(|text| Policy::parse(&text));
            let agreed = server.and_then(|server| negotiate(account.policy, server));
            let (status, result) = match agreed {
                Some(true) => (1, "CS_NEG_USE_SSL"),
                Some(false) => (1, "CS_NEG_USE_TCP"),
                None => (0, "CS_NEG_FAILURE"),
            };
            let reply = format!(
                "<CS_NEG_PI><status>{status}</status><result>cs_neg_result_kw={result};</result>\
                 </CS_NEG_PI>"
            );
            send(&mut tcp, "RODS_CS_NEG_T", reply.as_bytes(), &[], 0).await?;
            tls = agreed.ok_or_else(|| {
                Problem::fixed(format!(
                    "uwe and the iRODS server do not agree on TLS: uwe has the policy \
                     {:?}, the server {server:?}",
                    account.policy
                ))
            })?;
            answer = receive(&mut tcp).await?;
        } else if account.policy == Policy::Require {
            return Err(Problem::fixed(
                "the iRODS server does not negotiate TLS, which CS_NEG_REQUIRE needs",
            ));
        }
        let status = tag(&text(&answer.body), "status").and_then(|text| text.parse().ok());
        match (answer.kind.as_str(), status) {
            ("RODS_VERSION", Some(status)) if status >= 0 && answer.info >= 0 => {}
            ("RODS_VERSION", status) => {
                let code = status.filter(|status| *status < 0).unwrap_or(answer.info);
                return Err(failure(code, &answer.error));
            }
            _ => return Err(Problem::fixed(format!("{address} is not an iRODS server"))),
        }
        let stream: Box<dyn Stream> = match tls {
            true => upgrade(tcp, &account.host).await?,
            false => Box::new(tcp),
        };
        let mut session = Session { stream };
        if tls {
            session.share_secret().await?;
        }
        session.authenticate(account).await.map_err(|problem| {
            Problem::fixed(format!(
                "Failed to initialize iRODS connection: {}",
                problem.message
            ))
        })?;
        Ok(session)
    }

    /// Sends an API request with an XML body and bytes, and reads its answer. A negative
    /// status is an error.
    pub async fn call(&mut self, api: u32, body: &str, bytes: &[u8]) -> Result<Reply, Problem> {
        let number = i32::try_from(api).unwrap_or(i32::MAX);
        send(
            &mut self.stream,
            "RODS_API_REQ",
            body.as_bytes(),
            bytes,
            number,
        )
        .await?;
        let answer = receive(&mut self.stream).await?;
        if answer.info < 0 {
            return Err(failure(answer.info, &answer.error));
        }
        Ok(Reply {
            info: answer.info,
            body: text(&answer.body),
            bytes: answer.bytes,
        })
    }

    /// Signs in with the native scheme: the MD5 hash of the server's challenge and the
    /// password. A PAM password first buys a native password for the session.
    async fn authenticate(&mut self, account: &Account) -> Result<(), Problem> {
        let mut password = account.password.clone();
        if account.login == Login::PamPassword {
            let answer = self.sign_in(
                account,
                "pam_password",
                json!({
                    "next_operation": "auth_agent_auth_request",
                    "a_pw": account.password,
                    "a_ttl": "0",
                }),
            );
            let answer = answer.await?;
            password = (answer["request_result"].as_str().map(str::to_owned))
                .ok_or_else(|| Problem::fixed("the server sent no password for the PAM session"))?;
        }
        let request = json!({ "next_operation": "auth_agent_auth_request" });
        let answer = self.sign_in(account, "native", request).await?;
        let challenge = (answer["request_result"].as_str())
            .ok_or_else(|| Problem::fixed("the server sent no challenge"))?;
        let response = json!({
            "next_operation": "auth_agent_auth_response",
            "digest": digest(challenge, &password),
        });
        self.sign_in(account, "native", response).await?;
        Ok(())
    }

    /// One step of a sign-in: a JSON request in a `BinBytesBuf_PI`, and its JSON answer.
    async fn sign_in(
        &mut self,
        account: &Account,
        scheme: &str,
        mut request: Value,
    ) -> Result<Value, Problem> {
        request["scheme"] = scheme.into();
        request["user_name"] = account.user.clone().into();
        request["zone_name"] = account.zone.clone().into();
        let text = request.to_string();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&text);
        let body = format!(
            "<BinBytesBuf_PI><buflen>{}</buflen><buf>{encoded}</buf></BinBytesBuf_PI>",
            text.len()
        );
        let reply = self
            .call(AUTHENTICATE, &body, &[])
            .await
            .map_err(|problem| {
                match problem.message.contains("-826000") || problem.message.contains("-827000") {
                    true => {
                        Problem::fixed(format!("Invalid user or password ({})", problem.message))
                    }
                    false => problem,
                }
            })?;
        let buffer = tag(&reply.body, "buf").unwrap_or_default();
        let decoded = (base64::engine::general_purpose::STANDARD.decode(buffer.trim()))
            .unwrap_or_else(|_| buffer.into_bytes());
        let decoded = String::from_utf8_lossy(&decoded);
        Ok(serde_json::from_str(decoded.trim_end_matches('\0')).unwrap_or_default())
    }

    /// Sends the key for encrypted parallel transfers, which iRODS expects after TLS starts;
    /// uwe never uses those transfers.
    async fn share_secret(&mut self) -> Result<(), Problem> {
        let mut key = [0; KEY_SIZE];
        (crypto::aws_lc_rs::default_provider()
            .secure_random
            .fill(&mut key))
        .map_err(|_| Problem::fixed("no random bytes for the iRODS session"))?;
        let header = header(ALGORITHM, KEY_SIZE, SALT_SIZE, HASH_ROUNDS, 0);
        write_all(&mut self.stream, &header).await?;
        send(&mut self.stream, "SHARED_SECRET", &key, &[], 0).await
    }
}

/// Starts TLS on the connection, checking the server's certificate as the system does.
async fn upgrade(tcp: TcpStream, host: &str) -> Result<Box<dyn Stream>, Problem> {
    let failed = |error: String| Problem::fixed(format!("TLS with the iRODS server: {error}"));
    let provider = Arc::new(crypto::aws_lc_rs::default_provider());
    let config = (ClientConfig::builder_with_provider(provider))
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_platform_verifier())
        .map_err(|error| failed(error.to_string()))?
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_owned()).map_err(|error| failed(error.to_string()))?;
    let stream = (TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await)
        .map_err(|error| failed(error.to_string()))?;
    Ok(Box::new(stream))
}

/// The answer to a native challenge: the MD5 hash of the challenge and the password, each
/// padded with zeros, without zero bytes, in base64.
pub(crate) fn digest(challenge: &str, password: &str) -> String {
    let mut buffer = [0; CHALLENGE + PASSWORD];
    for (slot, byte) in buffer[..CHALLENGE].iter_mut().zip(challenge.bytes()) {
        *slot = byte;
    }
    for (slot, byte) in buffer[CHALLENGE..].iter_mut().zip(password.bytes()) {
        *slot = byte;
    }
    let mut hash = Md5::digest(buffer);
    for byte in hash.iter_mut().filter(|byte| **byte == 0) {
        *byte = 1;
    }
    base64::engine::general_purpose::STANDARD.encode(hash)
}

/// An error code of iRODS as a problem: the codes for a missing file or row mean that it is
/// not there, and one for an existing name that it is there.
fn failure(code: i32, error: &[u8]) -> Problem {
    let base = code - code % 1000;
    let detail = tag(&text(error), "msg").map_or(String::new(), |message| format!(": {message}"));
    let message = format!("iRODS error {code}{detail}");
    match base {
        -310000 | -808000 => Problem::missing(),
        -809000 => Problem {
            existing: true,
            ..Problem::fixed(message)
        },
        _ => Problem::fixed(message),
    }
}

/// The header of a message: its length as four bytes, then `MsgHeader_PI`.
fn header(kind: &str, body: usize, error: usize, bytes: usize, info: i32) -> Vec<u8> {
    let header = format!(
        "<MsgHeader_PI><type>{kind}</type><msgLen>{body}</msgLen><errorLen>{error}</errorLen>\
         <bsLen>{bytes}</bsLen><intInfo>{info}</intInfo></MsgHeader_PI>"
    );
    let length = u32::try_from(header.len()).unwrap_or(u32::MAX);
    let mut framed = length.to_be_bytes().to_vec();
    framed.extend_from_slice(header.as_bytes());
    framed
}

async fn send<S: AsyncWrite + Unpin + ?Sized>(
    stream: &mut S,
    kind: &str,
    body: &[u8],
    bytes: &[u8],
    info: i32,
) -> Result<(), Problem> {
    let mut message = header(kind, body.len(), 0, bytes.len(), info);
    message.extend_from_slice(body);
    write_all(stream, &message).await?;
    write_all(stream, bytes).await?;
    stream.flush().await.map_err(broken)
}

async fn write_all<S: AsyncWrite + Unpin + ?Sized>(
    stream: &mut S,
    data: &[u8],
) -> Result<(), Problem> {
    stream.write_all(data).await.map_err(broken)
}

async fn receive<S: AsyncRead + Unpin + ?Sized>(stream: &mut S) -> Result<Message, Problem> {
    let length = stream.read_u32().await.map_err(broken)? as usize;
    // A header is a few hundred bytes; more means that the other side speaks another protocol.
    if length > 1 << 16 {
        return Err(Problem::fixed(
            "the server does not speak the iRODS protocol",
        ));
    }
    let header = text(&read(stream, length).await?);
    let size = |name| tag(&header, name).and_then(|value| value.parse::<usize>().ok());
    let (Some(body), Some(error), Some(bytes)) = (size("msgLen"), size("errorLen"), size("bsLen"))
    else {
        return Err(Problem::fixed(
            "the server sent a message header that is not valid",
        ));
    };
    Ok(Message {
        kind: tag(&header, "type").unwrap_or_default(),
        info: tag(&header, "intInfo")
            .and_then(|info| info.parse().ok())
            .unwrap_or(0),
        body: read(stream, body).await?,
        error: read(stream, error).await?,
        bytes: read(stream, bytes).await?,
    })
}

async fn read<S: AsyncRead + Unpin + ?Sized>(
    stream: &mut S,
    size: usize,
) -> Result<Vec<u8>, Problem> {
    let mut data = vec![0; size];
    stream.read_exact(&mut data).await.map_err(broken)?;
    Ok(data)
}

fn broken(error: std::io::Error) -> Problem {
    Problem::passing(format!("the iRODS connection broke: {error}"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .to_owned()
}

/// The text of the first element with this name, unescaped.
pub(crate) fn tag(xml: &str, name: &str) -> Option<String> {
    tags(xml, name).into_iter().next()
}

/// The texts of all elements with this name, unescaped.
pub(crate) fn tags(xml: &str, name: &str) -> Vec<String> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        found.push(unescape(&after[..end]));
        rest = &after[end + close.len()..];
    }
    found
}

pub(crate) fn escape(text: &str) -> String {
    (text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
    .replace('"', "&quot;")
    .replace('\'', "&apos;")
}

fn unescape(text: &str) -> String {
    (text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\""))
    .replace("&apos;", "'")
    .replace("&#96;", "`")
    .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_tls() {
        use Policy::{DontCare, Refuse, Require};
        assert_eq!(negotiate(Require, DontCare), Some(true));
        assert_eq!(negotiate(DontCare, DontCare), Some(true));
        assert_eq!(negotiate(DontCare, Refuse), Some(false));
        assert_eq!(negotiate(Refuse, DontCare), Some(false));
        assert_eq!(negotiate(Require, Refuse), None);
        assert_eq!(negotiate(Refuse, Require), None);
    }

    #[test]
    fn hashes_challenges() {
        assert_eq!(digest(&"a".repeat(64), "rods"), "Rl9VFDXZK2lsMJU4L4EhWw==");
        let challenge = "0123456789abcdef".repeat(4);
        assert_eq!(digest(&challenge, "rods"), "FyEC9j4SQM5mgkgToh/PrA==");
    }

    #[test]
    fn reads_elements() {
        let xml = "<a><value>x &amp; y</value><value>&apos;z&apos;</value></a>";
        assert_eq!(tags(xml, "value"), ["x & y", "'z'"]);
        assert_eq!(tag(xml, "missing"), None);
        assert_eq!(
            escape("<'a'&\"b\">"),
            "&lt;&apos;a&apos;&amp;&quot;b&quot;&gt;"
        );
        assert!(failure(-310002, b"").missing);
        assert!(failure(-809000, b"").existing);
        let error = b"<RError_PI><count>1</count><RErrMsg_PI><status>-818000</status>\
                      <msg>no access</msg></RErrMsg_PI></RError_PI>";
        assert_eq!(
            failure(-818000, error).message,
            "iRODS error -818000: no access"
        );
    }
}

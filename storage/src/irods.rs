//! Reads and writes the data objects and collections of `irods://` addresses in an iRODS zone,
//! over the iRODS protocol, with the account of the settings, the environment or its file.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::error::Problem;
use crate::irods_protocol::{
    Account, CLOSE, Login, OPEN, Policy, QUERY, READ, STAT, Session, escape, tag, tags,
};
use crate::{Downloaded, Entry, Metadata, address};

/// What an iRODS client signs in with. Unset values come from the environment variables
/// `IRODS_*`, then from the iRODS environment file, then from defaults.
#[derive(Clone, Default)]
pub struct Options {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub zone: Option<String>,
    pub password: Option<String>,
    /// `pam` or `pam_password` for a PAM password; anything else is the native scheme.
    pub authentication: Option<String>,
    /// The TLS policy: `CS_NEG_REQUIRE`, `CS_NEG_DONT_CARE` or `CS_NEG_REFUSE` (the default).
    pub negotiation: Option<String>,
}

/// The options of the `irods` scope of a Nextflow config, for `irods://` addresses that no
/// provider of the config file serves.
static SCOPE: Mutex<Option<Options>> = Mutex::new(None);

/// Uses these options for `irods://` addresses that no provider of the config file serves,
/// from the first request on.
pub fn use_scope(options: Options) {
    *SCOPE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(options);
}

/// The options of the built-in provider.
pub(crate) fn scope() -> Options {
    let scope = SCOPE.lock().unwrap_or_else(|poison| poison.into_inner());
    scope.clone().unwrap_or_default()
}

/// The object types of `RodsObjStat_PI`.
const DATA_OBJECT: &str = "1";
const COLLECTION_OBJECT: &str = "2";
/// The columns of the catalog that listings ask for.
const DATA_NAME: u32 = 403;
const COLLECTION_NAME: u32 = 501;
const PARENT_NAME: u32 = 502;
/// How many rows a page of a listing holds, and how many bytes a read or write moves.
const ROWS: usize = 256;
const CHUNK: usize = 4 << 20;

pub(crate) struct Irods {
    account: Account,
    /// Signed-in sessions that no request uses, for the next ones.
    idle: Mutex<Vec<Session>>,
}

impl Irods {
    pub fn new(options: Options) -> Result<Irods, Problem> {
        Ok(Irods {
            account: account(options, &environment_file(), &|name| {
                std::env::var(name).ok()
            })?,
            idle: Mutex::default(),
        })
    }

    /// The secrets that messages must not show.
    pub fn secrets(&self) -> Vec<String> {
        vec![self.account.password.clone()]
    }

    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let path = logical(target)?;
        let mut session = self.session().await?;
        let found = stat(&mut session, &path).await;
        self.settle(session, &found);
        found
    }

    /// The data objects and collections in a collection.
    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let path = logical(target)?;
        let mut session = self.session().await?;
        let found = list(&mut session, &path).await;
        self.settle(session, &found);
        found
    }

    pub async fn download(&self, target: &str, file: &Path) -> Result<Downloaded, Problem> {
        let path = logical(target)?;
        let mut session = self.session().await?;
        let done = download(&mut session, &path, file).await;
        self.settle(session, &done);
        done
    }

    /// An idle session, else a new one.
    async fn session(&self) -> Result<Session, Problem> {
        let idle = self
            .idle
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop();
        match idle {
            Some(session) => Ok(session),
            None => Session::connect(&self.account).await,
        }
    }

    /// Keeps a session for later requests, unless its connection broke.
    fn settle<T>(&self, session: Session, outcome: &Result<T, Problem>) {
        if outcome.as_ref().is_err_and(|problem| problem.retryable) {
            return;
        }
        let mut idle = self
            .idle
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        idle.push(session);
    }
}

async fn stat(session: &mut Session, path: &str) -> Result<Option<Metadata>, Problem> {
    let answer = match session
        .call(STAT, &object_input(path, 0, 0, 0, &[]), &[])
        .await
    {
        Err(problem) if problem.missing => return Ok(None),
        answer => answer?,
    };
    let field = |name| tag(&answer.body, name).unwrap_or_default();
    let modified = field("modifyTime").trim().parse::<u64>().ok();
    let kind = field("objType");
    Ok(Some(Metadata {
        directory: kind.trim() == COLLECTION_OBJECT,
        modified: modified.map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)),
        size: field("objSize").trim().parse().unwrap_or(0),
        version: modified
            .filter(|_| kind.trim() == DATA_OBJECT)
            .map(|seconds| seconds.to_string()),
    }))
}

async fn list(session: &mut Session, path: &str) -> Result<Vec<Entry>, Problem> {
    match stat(session, path).await? {
        None => return Err(Problem::missing()),
        Some(found) if !found.directory => {
            return Err(Problem::fixed(format!("{path} is not a collection")));
        }
        Some(_) => {}
    }
    let mut entries = Vec::new();
    for child in query(session, COLLECTION_NAME, PARENT_NAME, path).await? {
        let name = child.rsplit('/').next().unwrap_or_default().to_owned();
        entries.push(Entry {
            name,
            directory: true,
        });
    }
    for name in query(session, DATA_NAME, COLLECTION_NAME, path).await? {
        entries.push(Entry {
            name,
            directory: false,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    // A data object with several replicas comes once per replica.
    entries.dedup();
    Ok(entries)
}

async fn download(session: &mut Session, path: &str, file: &Path) -> Result<Downloaded, Problem> {
    let found = stat(session, path).await?.ok_or_else(Problem::missing)?;
    if found.directory {
        return Err(Problem::fixed(format!("{path} is a collection")));
    }
    tracing::debug!("Staging file from iRODS: {path}");
    let descriptor = session
        .call(OPEN, &object_input(path, 0, 0, 0, &[]), &[])
        .await?;
    let mut output = crate::web::open(file, false).await?;
    loop {
        let read = opened_input(descriptor.info, CHUNK);
        let answer = session.call(READ, &read, &[]).await?;
        if answer.bytes.is_empty() {
            break;
        }
        (output.write_all(&answer.bytes).await).map_err(|error| Problem::local(file, error))?;
    }
    output
        .flush()
        .await
        .map_err(|error| Problem::local(file, error))?;
    session
        .call(CLOSE, &opened_input(descriptor.info, 0), &[])
        .await?;
    Ok(Downloaded {
        version: found.version,
    })
}

/// The values of a column of the catalog where another column equals `value`, through every
/// page of the answer.
async fn query(
    session: &mut Session,
    select: u32,
    condition: u32,
    value: &str,
) -> Result<Vec<String>, Problem> {
    let mut values = Vec::new();
    let mut continuation = 0;
    loop {
        let input = format!(
            "<GenQueryInp_PI><maxRows>{ROWS}</maxRows><continueInx>{continuation}</continueInx>\
             <partialStartIndex>0</partialStartIndex><options>0</options>{}\
             <InxIvalPair_PI><iiLen>1</iiLen><inx>{select}</inx><ivalue>1</ivalue>\
             </InxIvalPair_PI><InxValPair_PI><isLen>1</isLen><inx>{condition}</inx>\
             <svalue>{}</svalue></InxValPair_PI></GenQueryInp_PI>",
            pairs(&[]),
            escape(&format!("= '{value}'")),
        );
        let answer = match session.call(QUERY, &input, &[]).await {
            Err(problem) if problem.missing => return Ok(values),
            answer => answer?,
        };
        values.extend(tags(&answer.body, "value"));
        continuation = (tag(&answer.body, "continueInx"))
            .and_then(|text| text.trim().parse::<i64>().ok())
            .unwrap_or(0);
        if continuation <= 0 {
            return Ok(values);
        }
    }
}

/// A `DataObjInp_PI` for a path.
fn object_input(path: &str, flags: u32, mode: u32, size: u64, keywords: &[(&str, &str)]) -> String {
    format!(
        "<DataObjInp_PI><objPath>{}</objPath><createMode>{mode}</createMode>\
         <openFlags>{flags}</openFlags><offset>0</offset><dataSize>{size}</dataSize>\
         <numThreads>0</numThreads><oprType>0</oprType>{}</DataObjInp_PI>",
        escape(path),
        pairs(keywords)
    )
}

/// An `OpenedDataObjInp_PI` for an open data object.
fn opened_input(descriptor: i32, length: usize) -> String {
    format!(
        "<OpenedDataObjInp_PI><l1descInx>{descriptor}</l1descInx><len>{length}</len>\
         <whence>0</whence><oprType>0</oprType><offset>0</offset><bytesWritten>0</bytesWritten>\
         {}</OpenedDataObjInp_PI>",
        pairs(&[])
    )
}

/// A `KeyValPair_PI`: all keywords, then all values.
fn pairs(keywords: &[(&str, &str)]) -> String {
    let mut xml = format!("<KeyValPair_PI><ssLen>{}</ssLen>", keywords.len());
    for (keyword, _) in keywords {
        xml.push_str(&format!("<keyWord>{}</keyWord>", escape(keyword)));
    }
    for (_, value) in keywords {
        xml.push_str(&format!("<svalue>{}</svalue>", escape(value)));
    }
    xml.push_str("</KeyValPair_PI>");
    xml
}

/// The logical path of an address. `irods:///zone/a` and `irods://zone/a` name the same path,
/// as a Nextflow path keeps one slash after the scheme.
fn logical(target: &str) -> Result<String, Problem> {
    let rest = (target.strip_prefix("irods://"))
        .ok_or_else(|| Problem::fixed("it is not an irods:// address"))?;
    let rest = address::decode(rest).replace('\\', "/");
    let path = format!("/{}", rest.trim_start_matches('/'));
    match path.len() > 1 {
        true => Ok(path.trim_end_matches('/').to_owned()),
        false => Ok(path),
    }
}

/// The account of the options, else of the environment `variables`, else of the environment
/// file.
fn account(
    options: Options,
    file: &Value,
    variables: &dyn Fn(&str) -> Option<String>,
) -> Result<Account, Problem> {
    let variable = |name: &str| variables(name).filter(|value| !value.is_empty());
    let in_file = |key: &str| match &file[key] {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    };
    let pick = |given: Option<String>, name: Option<&str>, key: &str| {
        given
            .or_else(|| name.and_then(variable))
            .or_else(|| in_file(key))
    };
    let port = match options.port {
        Some(port) => port,
        None => match pick(None, Some("IRODS_PORT"), "irods_port") {
            Some(port) => (port.parse())
                .map_err(|_| Problem::fixed(format!("the iRODS port {port} is not a number")))?,
            None => 1247,
        },
    };
    let user = pick(options.user, Some("IRODS_USER_NAME"), "irods_user_name")
        .or_else(|| variable("USER"))
        .or_else(|| variable("LOGNAME"))
        .unwrap_or_default();
    let scheme = pick(options.authentication, None, "irods_authentication_scheme");
    let login = match scheme.map(|scheme| scheme.to_lowercase()).as_deref() {
        Some("pam" | "pam_password") => Login::PamPassword,
        _ => Login::Native,
    };
    let policy = match pick(options.negotiation, None, "irods_client_server_policy") {
        None => Policy::Refuse,
        Some(text) => Policy::parse(&text).ok_or_else(|| {
            Problem::fixed(format!(
                "the iRODS TLS policy {text} is unknown; use CS_NEG_REQUIRE, CS_NEG_DONT_CARE \
                 or CS_NEG_REFUSE"
            ))
        })?,
    };
    Ok(Account {
        host: pick(options.host, Some("IRODS_HOST"), "irods_host")
            .unwrap_or_else(|| "localhost".to_owned()),
        port,
        user,
        zone: pick(options.zone, Some("IRODS_ZONE_NAME"), "irods_zone_name").unwrap_or_default(),
        password: options
            .password
            .or_else(|| variable("IRODS_PASSWORD"))
            .unwrap_or_default(),
        login,
        policy,
    })
}

/// The iRODS environment file, `$IRODS_ENVIRONMENT_FILE` or `~/.irods/irods_environment.json`;
/// none when it is missing or cannot be read.
fn environment_file() -> Value {
    let path = match (
        std::env::var_os("IRODS_ENVIRONMENT_FILE"),
        std::env::var_os("HOME"),
    ) {
        (Some(file), _) => PathBuf::from(file),
        (None, Some(home)) => PathBuf::from(home).join(".irods/irods_environment.json"),
        (None, None) => return Value::Null,
    };
    match std::fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(error) => {
            tracing::warn!("Failed to parse iRODS environment file: {error}");
            Value::Null
        }
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!("Failed to parse iRODS environment file: {error}");
            Value::Null
        }),
    }
}

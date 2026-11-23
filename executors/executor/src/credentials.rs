//! Resolves registry secrets from uwe, Docker and Podman without running helpers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use config_file::{ConfigError, ConfigFile, RegistryAuth};

static CONFIGURED: RwLock<Settings> = RwLock::new(Settings {
    configured: BTreeMap::new(),
    active: Vec::new(),
});

static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);

tokio::task_local! {
    /// The settings of the request whose work this task does, from [`Registry::scope`].
    static REQUEST: Registry;
}

thread_local! {
    /// The settings of the requests entered on this thread, the last one counts.
    static ENTERED: RefCell<Vec<(u64, Registry)>> = const { RefCell::new(Vec::new()) };
}

struct Settings {
    configured: BTreeMap<String, RegistryAuth>,
    /// The settings of the requests that run now, with how many hold each. Each request finds
    /// its own through its scope; a lookup outside of every scope only learns from this list
    /// whether one of them must not read the credential files of the service user.
    active: Vec<(Registry, usize)>,
}

/// Registry settings of one request, independent of legacy process defaults.
#[derive(Clone, Debug)]
pub struct Registry {
    configured: BTreeMap<String, RegistryAuth>,
    paths: Vec<PathBuf>,
    /// The HTTP client of the web proxy of the request; `None` for the one of the process.
    client: Option<reqwest::Client>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            configured: BTreeMap::new(),
            paths: paths(),
            client: None,
        }
    }
}

impl Registry {
    pub fn from_config(config: &ConfigFile) -> Result<Self, ConfigError> {
        Ok(Self {
            configured: config_file::registry_auth(config)?,
            paths: paths(),
            client: None,
        })
    }

    /// Sends the registry requests of the request through its own web proxy, or directly for
    /// `None`, unless the environment names one, whatever proxy the process uses.
    pub fn with_proxy(mut self, proxy: Option<&str>) -> Result<Self, String> {
        self.client = Some(storage::Clients::of(proxy)?.plain);
        Ok(self)
    }

    /// Leaves out the Docker and Podman credential files of the service user, so that only
    /// the configured credentials count.
    pub fn without_files(mut self) -> Self {
        self.paths.clear();
        self
    }

    /// Uses these settings on this thread until the returned scope drops. Overlapping requests
    /// may give other credentials: each finds its own on its thread, and in the work that
    /// [`Registry::scope`] runs. Hold the scope until the work of the request has joined.
    pub fn enter(&self) -> Result<Scope, String> {
        let mut settings = CONFIGURED
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        match (settings.active.iter_mut()).find(|(active, _)| active.same(self)) {
            Some((_, count)) => *count += 1,
            None => settings.active.push((self.clone(), 1)),
        }
        drop(settings);
        let id = NEXT_SCOPE.fetch_add(1, Ordering::Relaxed);
        ENTERED.with(|entered| entered.borrow_mut().push((id, self.clone())));
        Ok(Scope {
            id,
            registry: self.clone(),
            _thread: PhantomData,
        })
    }

    /// Whether two settings give the same credentials from the same files.
    fn same(&self, other: &Registry) -> bool {
        self.paths == other.paths && same_auth(&self.configured, &other.configured)
    }

    /// The credentials of a registry host with these settings, wherever the caller runs.
    pub(crate) fn lookup(&self, host: &str) -> Result<Found, String> {
        lookup_in(host, &self.configured, &self.paths)
    }

    /// Runs `work` with these settings, also where it leaves the thread of the request, like
    /// the task attempts on the runtime that all runs share.
    pub fn scope<F: Future>(&self, work: F) -> impl Future<Output = F::Output> + use<F> {
        REQUEST.scope(self.clone(), work)
    }
}

/// Keeps a request's registry settings active in the process and on the thread that entered
/// them. It stays on that thread: dropped on another one, it would leave them there.
pub struct Scope {
    id: u64,
    registry: Registry,
    _thread: PhantomData<*const ()>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let mut settings = CONFIGURED
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let active = &mut settings.active;
        if let Some(index) = (active.iter()).position(|(entry, _)| entry.same(&self.registry)) {
            active[index].1 -= 1;
            if active[index].1 == 0 {
                active.remove(index);
            }
        }
        drop(settings);
        let _ = ENTERED.try_with(|entered| entered.borrow_mut().retain(|(id, _)| *id != self.id));
    }
}

fn same_auth(
    first: &BTreeMap<String, RegistryAuth>,
    second: &BTreeMap<String, RegistryAuth>,
) -> bool {
    // Each request can give its own value for a variable, so the values decide.
    let same = |first: &config_file::Secret, second: &config_file::Secret| {
        first.reveal().ok() == second.reveal().ok()
    };
    first.len() == second.len()
        && first
            .iter()
            .all(|(host, auth)| match (auth, second.get(host)) {
                (
                    RegistryAuth::Basic { username, password },
                    Some(RegistryAuth::Basic {
                        username: other_user,
                        password: other_password,
                    }),
                ) => same(username, other_user) && same(password, other_password),
                (RegistryAuth::Token(token), Some(RegistryAuth::Token(other))) => {
                    same(token, other)
                }
                _ => false,
            })
}

pub(crate) enum Credential {
    Basic(String, String),
    Bearer(String),
    Identity(String),
}

#[derive(Default)]
pub(crate) struct Found {
    pub credential: Option<Credential>,
    pub helper: bool,
}

impl Found {
    pub fn hint(&self) -> &'static str {
        if self.credential.is_some() {
            "; configured registry credentials were rejected"
        } else if self.helper {
            "; credentials are configured only through a credential helper (credsStore or credHelpers); uwe cannot run external helpers, configure registry credentials directly"
        } else {
            "; no registry credentials were found in uwe, Docker or Podman settings"
        }
    }
}

/// Installs the registry section of the merged settings for registry requests outside of any
/// request; a request that runs keeps its own settings.
pub fn configure(config: &ConfigFile) -> Result<(), ConfigError> {
    let auth = config_file::registry_auth(config)?;
    let mut settings = CONFIGURED
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    settings.configured = auth;
    Ok(())
}

/// The HTTP client for registry requests: through the proxy of the request whose work this
/// is, else through the one of the process.
pub(crate) fn client() -> Result<reqwest::Client, String> {
    match current().and_then(|registry| registry.client) {
        Some(client) => Ok(client),
        None => crate::http::client().cloned(),
    }
}

/// The settings of the request whose work this task or thread does.
fn current() -> Option<Registry> {
    (REQUEST.try_with(Registry::clone).ok()).or_else(|| {
        let last = |entered: &RefCell<Vec<(u64, Registry)>>| entered.borrow().last().cloned();
        ENTERED
            .try_with(last)
            .ok()
            .flatten()
            .map(|(_, registry)| registry)
    })
}

pub(crate) fn lookup(host: &str) -> Result<Found, String> {
    let request = (REQUEST.try_with(Registry::clone).ok()).or_else(|| {
        let last = |entered: &RefCell<Vec<(u64, Registry)>>| entered.borrow().last().cloned();
        ENTERED
            .try_with(last)
            .ok()
            .flatten()
            .map(|(_, registry)| registry)
    });
    if let Some(registry) = request {
        return registry.lookup(host);
    }
    // Outside of every request, only the settings of the process count, never those of a
    // request that runs now; while one of them must not read the credential files of the
    // service user, neither does this lookup, which may do its work.
    let settings = CONFIGURED
        .read()
        .unwrap_or_else(|poison| poison.into_inner());
    let isolated = (settings.active.iter()).any(|(active, _)| active.paths.is_empty());
    let registry = Registry {
        configured: settings.configured.clone(),
        paths: paths(),
        client: None,
    };
    drop(settings);
    let registry = match isolated {
        true => registry.without_files(),
        false => registry,
    };
    registry.lookup(host)
}

fn paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let docker = std::env::var_os("DOCKER_CONFIG")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".docker")));
    let mut paths = Vec::new();
    paths.extend(docker.map(|path| path.join("config.json")));
    paths.extend(
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(path).join("containers/auth.json")),
    );
    paths.extend(home.map(|home| home.join(".config/containers/auth.json")));
    paths
}

fn host_key(host: &str) -> &str {
    let host = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(host);
    let host = host
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or(host.trim_end_matches('/'));
    match host {
        "docker.io" | "index.docker.io" => "registry-1.docker.io",
        host => host,
    }
}

fn lookup_in(
    host: &str,
    configured: &BTreeMap<String, RegistryAuth>,
    paths: &[PathBuf],
) -> Result<Found, String> {
    if let Some((_, auth)) = configured
        .iter()
        .find(|(key, _)| host_key(key) == host_key(host))
    {
        let reveal =
            |secret: &config_file::Secret| secret.reveal().map_err(|error| error.to_string());
        let credential = match auth {
            RegistryAuth::Basic { username, password } => {
                Credential::Basic(reveal(username)?, reveal(password)?)
            }
            RegistryAuth::Token(token) => Credential::Bearer(reveal(token)?),
        };
        return Ok(Found {
            credential: Some(credential),
            helper: false,
        });
    }
    let mut found = Found::default();
    for path in paths {
        let Some(file) = read_file(path)? else {
            continue;
        };
        let entries = file.get("auths").and_then(serde_json::Value::as_object);
        if let Some(entry) = entries.and_then(|entries| {
            entries
                .iter()
                .find(|(key, _)| host_key(key) == host_key(host))
                .map(|(_, entry)| entry)
        }) {
            if let Some(token) = entry["identitytoken"]
                .as_str()
                .filter(|text| !text.is_empty())
            {
                found.credential = Some(Credential::Identity(token.to_owned()));
                return Ok(found);
            }
            if let Some(auth) = entry["auth"].as_str().filter(|text| !text.is_empty()) {
                let invalid = || "invalid registry auth encoding (credentials redacted)".to_owned();
                let bytes = base64::prelude::BASE64_STANDARD
                    .decode(auth)
                    .map_err(|_| invalid())?;
                let text = String::from_utf8(bytes).map_err(|_| invalid())?;
                let (user, password) = text.split_once(':').ok_or_else(invalid)?;
                found.credential = Some(Credential::Basic(user.to_owned(), password.to_owned()));
                return Ok(found);
            }
        }
        found.helper |= file["credsStore"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
            || file["credHelpers"]
                .as_object()
                .is_some_and(|helpers| helpers.keys().any(|key| host_key(key) == host_key(host)));
    }
    Ok(found)
}

fn read_file(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "invalid registry credential file (contents redacted)".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("cannot read registry credential file".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two overlapping requests, one without the credential files of the service user, each
    /// get their own credentials. Runs again in a child process whose home has a Docker file.
    #[test]
    fn scopes_registry_files() {
        if std::env::var_os("UWE_TEST_REGISTRY_FILES").is_none() {
            let home = tempfile::tempdir().unwrap();
            let docker = home.path().join(".docker");
            std::fs::create_dir_all(&docker).unwrap();
            let file = r#"{"auths":{"private.example":{"identitytoken":"service"}}}"#;
            std::fs::write(docker.join("config.json"), file).unwrap();
            let test = "credentials::tests::scopes_registry_files";
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--test-threads=1"])
                .env("UWE_TEST_REGISTRY_FILES", "1")
                .env("HOME", home.path())
                .env_remove("DOCKER_CONFIG")
                .env_remove("XDG_RUNTIME_DIR")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{stdout}{stderr}");
            assert!(
                stdout.contains("1 passed"),
                "the child ran no test: {stdout}"
            );
            return;
        }
        let found = |registry: Option<&Registry>| {
            let ask = || lookup("private.example").map(|found| found.credential.is_some());
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let task = match registry.cloned() {
                Some(registry) => runtime.spawn(registry.scope(async move { ask() })),
                None => runtime.spawn(async move { ask() }),
            };
            runtime.block_on(task).unwrap()
        };
        let service = Registry::default();
        let isolated = Registry::default().without_files();
        let _service = service.enter().unwrap();
        let (ready, overlapping) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let request = std::thread::spawn(move || {
            let _isolated = (isolated.enter())
                .expect("a request without the files of the service user may overlap");
            let here = lookup("private.example").unwrap().credential.is_some();
            ready.send(here).unwrap();
            released.recv().unwrap();
        });
        assert!(
            !overlapping.recv().unwrap(),
            "the isolated request read the service files"
        );
        assert_eq!(found(Some(&Registry::default().without_files())), Ok(false));
        assert_eq!(found(Some(&service)), Ok(true));
        assert!(lookup("private.example").unwrap().credential.is_some());
        assert_eq!(
            found(None),
            Ok(false),
            "a lookup outside both requests reads no files while one must not"
        );
        let other = "[registry.\"private.example\"]\ntoken = \"other\"";
        let other = ConfigFile::parse(other, Path::new("other.toml")).unwrap();
        let other = Registry::from_config(&other).unwrap();
        let scope = (other.enter()).expect("a request with other credentials may overlap");
        assert!(matches!(
            lookup("private.example").unwrap().credential,
            Some(Credential::Bearer(token)) if token == "other"
        ));
        drop(scope);
        release.send(()).unwrap();
        request.join().unwrap();
        assert_eq!(found(None), Ok(true));
    }

    /// Two runs with other tokens for the same registry overlap, and each finds its own, on
    /// its thread and in the tasks that its scope runs; a lookup outside of both is refused.
    #[test]
    fn overlaps_registry_settings() {
        let config = |token: &str| {
            let text = format!("[registry.\"private.example\"]\ntoken = \"{token}\"");
            ConfigFile::parse(&text, Path::new("registry.toml")).unwrap()
        };
        let token = |found: Result<Found, String>| match found.unwrap().credential {
            Some(Credential::Bearer(token)) => token,
            _ => panic!("no token"),
        };
        let first = Registry::from_config(&config("first")).unwrap();
        let second = Registry::from_config(&config("second")).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let runs: Vec<_> = [(first.clone(), "first"), (second.clone(), "second")]
            .into_iter()
            .map(|(registry, expected)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let _scope = (registry.enter()).expect("other credentials may overlap");
                    barrier.wait();
                    assert_eq!(token(lookup("private.example")), expected);
                    let runtime = tokio::runtime::Runtime::new().unwrap();
                    let task = runtime.spawn(registry.scope(async { lookup("private.example") }));
                    assert_eq!(token(runtime.block_on(task).unwrap()), expected);
                    barrier.wait();
                })
            })
            .collect();
        barrier.wait();
        // A lookup outside of both runs never borrows the credentials of either.
        let outside = lookup("private.example").unwrap().credential;
        assert!(!matches!(outside, Some(Credential::Bearer(_))));
        assert_eq!(token(first.lookup("private.example")), "first");
        assert_eq!(token(second.lookup("private.example")), "second");
        barrier.wait();
        for run in runs {
            run.join().unwrap();
        }
        // Requests that give different values for the same variable may overlap too.
        let given = |value: &str| {
            let mut secrets = config_file::SecretValues::default();
            secrets.insert("REGISTRY_TOKEN", value).unwrap();
            secrets.allow_environment(false);
            let text =
                "[registry.\"private.example\"]\ntoken = { environment = \"REGISTRY_TOKEN\" }";
            let config = ConfigFile::parse(text, Path::new("given.toml")).unwrap();
            Registry::from_config(&config.with_secrets(secrets)).unwrap()
        };
        let (one, two) = (given("one"), given("two"));
        let _one = one.enter().unwrap();
        assert_eq!(token(lookup("private.example")), "one");
        let two_scope = two.enter().unwrap();
        assert_eq!(
            token(lookup("private.example")),
            "two",
            "the last entered counts"
        );
        drop(two_scope);
        assert_eq!(token(lookup("private.example")), "one");
    }

    /// A lookup outside of every request does not borrow the credentials of the one request
    /// that runs, and the scope of a request stays on its thread.
    #[test]
    fn refuses_borrowed_credentials() {
        let text = "[registry.\"borrowed.example\"]\ntoken = \"request\"";
        let config = ConfigFile::parse(text, Path::new("borrowed.toml")).unwrap();
        let registry = Registry::from_config(&config).unwrap();
        let (entered, check) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let request = std::thread::spawn(move || {
            let _scope = registry.enter().unwrap();
            entered.0.send(()).unwrap();
            check.1.recv().unwrap();
        });
        entered.1.recv().unwrap();
        let found = lookup("borrowed.example").unwrap().credential;
        assert!(found.is_none(), "the lookup took the token of the request");
        check.0.send(()).unwrap();
        request.join().unwrap();
        // Compiles only while `Scope` is not `Send`: with both impls, the call is ambiguous.
        trait Ambiguous<A> {
            fn some() {}
        }
        impl<T: ?Sized> Ambiguous<()> for T {}
        impl<T: ?Sized + Send> Ambiguous<u8> for T {}
        <Scope as Ambiguous<_>>::some();
    }

    /// The registry requests of a request go through its own web proxy.
    #[test]
    fn uses_request_proxy() {
        use std::io::{BufRead, BufReader, Write};
        let named = ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"];
        if named.iter().any(|name| std::env::var_os(name).is_some()) {
            return;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut first = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut first)
                .unwrap();
            let answer = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
            stream.write_all(answer.as_bytes()).unwrap();
            first
        });
        let registry = Registry::default().with_proxy(Some(&proxy)).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let asked = registry.scope(async {
            let client = client().unwrap();
            client
                .get("http://registry.invalid/v2/")
                .send()
                .await
                .map(|answer| answer.status())
        });
        assert_eq!(runtime.block_on(asked).unwrap(), 204);
        let first = server.join().unwrap();
        assert!(
            first.starts_with("GET http://registry.invalid/v2/"),
            "{first}"
        );
    }

    #[test]
    fn respects_source_order() {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            ["docker.json", "runtime.json", "podman.json"].map(|name| directory.path().join(name));
        for (index, path) in paths.iter().enumerate() {
            let auth = base64::prelude::BASE64_STANDARD.encode(format!("user:password{index}"));
            std::fs::write(
                path,
                format!(r#"{{"auths":{{"https://index.docker.io/v1/":{{"auth":"{auth}"}}}}}}"#),
            )
            .unwrap();
        }
        let config = ConfigFile::parse(
            "[registry.\"docker.io\"]\ntoken = \"configured\"",
            Path::new("config.toml"),
        )
        .unwrap();
        let configured = config_file::registry_auth(&config).unwrap();
        assert!(
            matches!(lookup_in("registry-1.docker.io", &configured, &paths).unwrap().credential, Some(Credential::Bearer(token)) if token == "configured")
        );
        for index in 0..3 {
            let found = lookup_in("registry-1.docker.io", &BTreeMap::new(), &paths).unwrap();
            assert!(
                matches!(found.credential, Some(Credential::Basic(user, password)) if user == "user" && password == format!("password{index}"))
            );
            std::fs::remove_file(&paths[index]).unwrap();
        }
        assert!(
            lookup_in("registry-1.docker.io", &BTreeMap::new(), &paths)
                .unwrap()
                .credential
                .is_none()
        );
    }

    #[test]
    fn skips_service_files() {
        let directory = tempfile::tempdir().unwrap();
        let docker = directory.path().join("config.json");
        std::fs::write(
            &docker,
            r#"{"auths":{"host":{"identitytoken":"identity"}}}"#,
        )
        .unwrap();
        let registry = Registry {
            configured: BTreeMap::new(),
            paths: vec![docker],
            client: None,
        };
        let found = |registry: &Registry| {
            (lookup_in("host", &registry.configured, &registry.paths).unwrap()).credential
        };
        assert!(found(&registry).is_some());
        assert!(found(&registry.without_files()).is_none());
    }

    #[test]
    fn bypasses_external_helpers() {
        let directory = tempfile::tempdir().unwrap();
        let docker = directory.path().join("config.json");
        let podman = directory.path().join("auth.json");
        std::fs::write(&docker, r#"{"credHelpers":{"host":"external"}}"#).unwrap();
        let found = lookup_in("host", &BTreeMap::new(), std::slice::from_ref(&docker)).unwrap();
        assert!(found.helper && found.credential.is_none());
        std::fs::write(
            &podman,
            r#"{"auths":{"host":{"identitytoken":"identity"}}}"#,
        )
        .unwrap();
        let found = lookup_in("host", &BTreeMap::new(), &[docker.clone(), podman]).unwrap();
        assert!(
            matches!(found.credential, Some(Credential::Identity(token)) if token == "identity")
        );
        std::fs::write(&docker, r#"{"credsStore":"external"}"#).unwrap();
        assert!(
            lookup_in("host", &BTreeMap::new(), std::slice::from_ref(&docker))
                .unwrap()
                .helper
        );
        std::fs::write(&docker, r#"{"auths":{"host":{"auth":"private-value"}}}"#).unwrap();
        let error = lookup_in("host", &BTreeMap::new(), &[docker])
            .err()
            .unwrap();
        assert!(!error.contains("private-value"));
    }
}

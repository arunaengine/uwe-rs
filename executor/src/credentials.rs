//! Resolves registry secrets from uwe, Docker and Podman without running helpers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
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
    /// The settings of the requests that run now, with how many hold each. They give the same
    /// values for their credentials and differ at most in the credential files they read.
    active: Vec<(Registry, usize)>,
}

/// Registry settings of one request, independent of legacy process defaults.
#[derive(Clone, Debug)]
pub struct Registry {
    configured: BTreeMap<String, RegistryAuth>,
    paths: Vec<PathBuf>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            configured: BTreeMap::new(),
            paths: paths(),
        }
    }
}

impl Registry {
    pub fn from_config(config: &ConfigFile) -> Result<Self, ConfigError> {
        Ok(Self {
            configured: config_file::registry_auth(config)?,
            paths: paths(),
        })
    }

    /// Leaves out the Docker and Podman credential files of the service user, so that only
    /// the configured credentials count.
    pub fn without_files(mut self) -> Self {
        self.paths.clear();
        self
    }

    /// Overlapping requests must give the same values for their credentials, across worker
    /// threads, but each one may leave out the credential files. Hold the returned scope on
    /// this thread until the asynchronous work of the request has joined.
    pub fn enter(&self) -> Result<Scope, String> {
        let mut settings = CONFIGURED
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if (settings.active.iter())
            .any(|(active, _)| !same_auth(&active.configured, &self.configured))
        {
            return Err(
                "concurrent requests with different registry settings require separate worker processes"
                    .into(),
            );
        }
        match (settings.active.iter_mut()).find(|(active, _)| active.paths == self.paths) {
            Some((_, count)) => *count += 1,
            None => settings.active.push((self.clone(), 1)),
        }
        drop(settings);
        let id = NEXT_SCOPE.fetch_add(1, Ordering::Relaxed);
        ENTERED.with(|entered| entered.borrow_mut().push((id, self.clone())));
        Ok(Scope {
            id,
            paths: self.paths.clone(),
        })
    }

    /// Runs `work` with these settings, also where it leaves the thread of the request, like
    /// the task attempts on the runtime that all runs share.
    pub fn scope<F: Future>(&self, work: F) -> impl Future<Output = F::Output> + use<F> {
        REQUEST.scope(self.clone(), work)
    }
}

/// Keeps a request's registry settings active in the process and on the thread that entered them.
pub struct Scope {
    id: u64,
    paths: Vec<PathBuf>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let mut settings = CONFIGURED
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let active = &mut settings.active;
        if let Some(index) = active
            .iter()
            .position(|(entry, _)| entry.paths == self.paths)
        {
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

/// Installs the registry section of the merged settings for registry requests.
pub fn configure(config: &ConfigFile) -> Result<(), ConfigError> {
    let auth = config_file::registry_auth(config)?;
    let mut settings = CONFIGURED
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    if (settings.active.iter()).any(|(active, _)| !same_auth(&active.configured, &auth)) {
        return Err(config.root().invalid(
            "registry",
            "registry settings cannot change while a request is active",
        ));
    }
    settings.configured = auth;
    Ok(())
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
        return lookup_in(host, &registry.configured, &registry.paths);
    }
    let settings = CONFIGURED
        .read()
        .unwrap_or_else(|poison| poison.into_inner());
    let (registry, mixed) = match settings.active.as_slice() {
        [] => {
            let configured = settings.configured.clone();
            let registry = Registry {
                configured,
                paths: paths(),
            };
            (registry, false)
        }
        [(active, _)] => (active.clone(), false),
        [(active, _), ..] => (active.clone().without_files(), true),
    };
    drop(settings);
    let found = lookup_in(host, &registry.configured, &registry.paths)?;
    // Without its request, a lookup cannot tell whether it may read the files of the service user.
    if mixed && found.credential.is_none() {
        return Err(format!(
            "the registry credentials for {host} are asked outside of their request, while the running requests differ in whether they read the credential files of the service user"
        ));
    }
    Ok(found)
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
        assert!(
            found(None).is_err(),
            "a lookup outside both requests must not pick the files of either"
        );
        let other = "[registry.\"private.example\"]\ntoken = \"other\"";
        let other = ConfigFile::parse(other, Path::new("other.toml")).unwrap();
        assert!(Registry::from_config(&other).unwrap().enter().is_err());
        release.send(()).unwrap();
        request.join().unwrap();
        assert_eq!(found(None), Ok(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fences_concurrent_settings() {
        let config = |token| {
            ConfigFile::parse(
                &format!("[registry.\"private.example\"]\ntoken = \"{token}\""),
                Path::new("registry.toml"),
            )
            .unwrap()
        };
        let first = Registry::from_config(&config("first")).unwrap();
        let second_config = config("second");
        let second = Registry::from_config(&second_config).unwrap();
        let first_scope = first.enter().unwrap();
        let same = ConfigFile::parse(
            "[registry.\"private.example\"]\ntoken = \"first\"",
            Path::new("another.toml"),
        )
        .unwrap();
        drop(Registry::from_config(&same).unwrap().enter().unwrap());
        let (ready, reached) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let _scope = first.enter().unwrap();
            ready.send(()).unwrap();
            released.await.unwrap();
            assert!(matches!(
                lookup("private.example").unwrap().credential,
                Some(Credential::Bearer(token)) if token == "first"
            ));
        });
        reached.await.unwrap();
        drop(first_scope);
        assert!(second.enter().is_err());
        assert!(configure(&second_config).is_err());
        assert!(
            tokio::spawn(async {
                matches!(
                    lookup("private.example").unwrap().credential,
                    Some(Credential::Bearer(token)) if token == "first"
                )
            })
            .await
            .unwrap()
        );
        release.send(()).unwrap();
        worker.await.unwrap();
        let scope = second.enter().unwrap();
        assert!(matches!(
            lookup("private.example").unwrap().credential,
            Some(Credential::Bearer(token)) if token == "second"
        ));
        drop(scope);
        // Requests that give different values for the same variable use different credentials.
        let given = |value: &str| {
            let mut secrets = config_file::SecretValues::default();
            secrets.insert("REGISTRY_TOKEN", value).unwrap();
            secrets.allow_environment(false);
            let text =
                "[registry.\"private.example\"]\ntoken = { environment = \"REGISTRY_TOKEN\" }";
            let config = ConfigFile::parse(text, Path::new("given.toml")).unwrap();
            Registry::from_config(&config.with_secrets(secrets)).unwrap()
        };
        let _scope = given("first").enter().unwrap();
        drop(given("first").enter().unwrap());
        assert!(
            given("second").enter().is_err(),
            "an overlapping request with another value for the same variable must be refused"
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

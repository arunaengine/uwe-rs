//! Storage providers from the config file: which provider serves an address, web servers with
//! credentials that never show in messages, files downloaded once, and folders.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::WebServer;
use common::ftp::FtpServer;
use config_file::ConfigFile;
use storage::settings::Kind;
use storage::{Entry, Metadata, Settings, Storage, StorageError, StoreLocation};

fn config(text: &str) -> ConfigFile {
    ConfigFile::parse(text, Path::new("/home/me/.config/uwe/config.toml")).expect("TOML")
}

fn storage(text: &str) -> Storage {
    Storage::from_config(&config(text)).expect("valid settings")
}

fn settings_error(text: &str) -> String {
    Settings::from_config(&config(text))
        .expect_err("invalid settings")
        .to_string()
}

fn s3_put_server(version: Option<&str>) -> (String, std::sync::mpsc::Receiver<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let version = version.map(str::to_owned);
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        send.send((request.trim_end().to_owned(), body)).unwrap();
        let version = version.as_deref().map_or(String::new(), |version| {
            format!("x-amz-version-id: {version}\r\n")
        });
        stream
            .write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: \"etag-only\"\r\n{version}Connection: close\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
    });
    (endpoint, receive)
}

#[test]
fn parses_provider_settings() {
    let settings = Settings::from_config(&config(
        "[store]\nlocation = \"s3://cache/uwe\"\n\
         [storage.lab]\ntype = \"s3\"\nprefixes = [\"s3://lab-data/\", \"s3://cache/\"]\n\
         endpoint = \"https://minio.lab.example.org/\"\naccess_key_id = \"lab\"\n\
         secret_access_key = { environment = \"LAB_SECRET\" }\n\
         [storage.mirror]\ntype = \"folder\"\nprefixes = \"s3://ngi-igenomes/\"\npath = \"/data/igenomes\"\n\
         [storage.web]\ntype = \"https\"\nprefixes = [\"https://data.example.org/private/\"]\n\
         token = { environment = \"DATA_TOKEN\" }\nheaders = { X-Api-Key = \"key\" }\n",
    ))
    .expect("valid settings");
    assert_eq!(settings.store.as_deref(), Some("s3://cache/uwe"));
    let names: Vec<&str> = settings.providers.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["lab", "mirror", "web"]);
    let Kind::S3(lab) = &settings.providers[0].kind else {
        panic!("an S3 provider");
    };
    assert_eq!(
        lab.endpoint.as_deref(),
        Some("https://minio.lab.example.org")
    );
    assert_eq!(
        lab.secret_access_key
            .as_ref()
            .and_then(|secret| secret.variable()),
        Some("LAB_SECRET")
    );
    assert_eq!(settings.providers[1].prefixes, ["s3://ngi-igenomes/"]);
    let Kind::Https(web) = &settings.providers[2].kind else {
        panic!("a web provider");
    };
    assert_eq!(web.headers.len(), 1);
    assert_eq!(web.headers[0].0, "X-Api-Key");
}

#[test]
fn lists_s3_providers() {
    let storage = Storage::from_config(&config(
        "[storage.lab]\ntype = \"s3\"\nprefixes = [\"s3://lab-data/\"]\n\
         endpoint = \"https://minio.lab.example.org/\"\n\
         [storage.aws]\ntype = \"s3\"\nprefixes = [\"s3://open/\"]\n\
         [storage.web]\ntype = \"https\"\nprefixes = [\"https://data.example.org/\"]\n",
    ))
    .expect("valid settings");
    assert_eq!(
        storage.s3_providers(),
        [
            ("aws".to_owned(), vec!["s3://open/".to_owned()], None),
            (
                "lab".to_owned(),
                vec!["s3://lab-data/".to_owned()],
                Some("https://minio.lab.example.org".to_owned())
            ),
        ]
    );
}

#[test]
fn gives_https_accounts() {
    let storage = Storage::from_config(&config(
        "[storage.git]\ntype = \"https\"\nprefixes = [\"https://git.example.org/team/\"]\n\
         token = \"secret-token\"\n",
    ))
    .expect("valid settings");
    let account = storage.account("https://git.example.org/team/pipeline.git");
    assert_eq!(
        account.unwrap(),
        Some(("git".into(), "secret-token".into()))
    );
    assert_eq!(
        storage
            .account("https://git.example.org/other.git")
            .unwrap(),
        None
    );
}

#[test]
fn locates_settings_errors() {
    let file = "/home/me/.config/uwe/config.toml";
    assert_eq!(
        settings_error("[storage.a]\nprefixes = [\"s3://a/\"]\n"),
        format!(
            "{file}:1: `[storage.a]` needs `type`, the kind of provider: one of s3, \
             google-cloud-storage, azure-blob-storage, https, ftp, folder, drs"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"dropbox\"\n"),
        format!(
            "{file}:2: `storage.a.type` is `dropbox`, which uwe does not know; use \
             one of s3, google-cloud-storage, azure-blob-storage, https, ftp, folder, drs"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"s3\"\n"),
        format!(
            "{file}:1: `[storage.a]` needs `prefixes`, the addresses it serves, like \
             prefixes = [\"s3://my-bucket/\"]"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"s3\"\nprefixes = [\"gs://a/\"]\n"),
        format!(
            "{file}:3: `storage.a.prefixes` holds \"gs://a/\"; a `s3` provider needs an \
             address that starts with s3://, http://, https://"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"s3\"\nprefixes = [\"s3://a/\"]\nregoin = \"x\"\n"),
        format!(
            "{file}:4: `[storage.a]` has no setting `regoin`; it knows `type`, \
             `prefixes`, `endpoint`, `region`, `path_style`, `access_key_id`, \
             `secret_access_key`, `session_token` and `anonymous`"
        )
    );
    assert_eq!(
        settings_error(
            "[storage.a]\ntype = \"s3\"\nprefixes = [\"s3://a/\"]\naccess_key_id = \"x\"\n"
        ),
        format!(
            "{file}:1: `[storage.a]` needs `secret_access_key`, which goes with `access_key_id`"
        )
    );
    assert_eq!(
        settings_error(
            "[storage.a]\ntype = \"folder\"\nprefixes = [\"s3://a/\"]\npath = \"data\"\n"
        ),
        format!("{file}:4: `storage.a.path` must be an absolute path, like \"/data/mirror\"")
    );
    assert_eq!(
        settings_error(
            "[storage.a]\ntype = \"https\"\nprefixes = [\"https://x/\"]\n\
             [storage.b]\ntype = \"https\"\nprefixes = [\"https://x/\"]\n"
        ),
        format!(
            "{file}:4: the storage providers `a` and `b` both serve \"https://x/\"; give \
             it to one of them"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"https\"\nprefixes = [\"https://\"]\ntoken = \"t\"\n"),
        format!(
            "{file}:3: `storage.a.prefixes` holds \"https://\", which serves every server, so \
             its account would go to all of them; name the server, like \
             \"https://git.example.org/\""
        )
    );
    // Without an account, a provider may serve every server.
    let open = "[storage.a]\ntype = \"https\"\nprefixes = [\"https://\"]\n";
    assert!(Settings::from_config(&config(open)).is_ok());
    assert_eq!(
        settings_error("[store]\nplace = \"/x\"\n"),
        format!("{file}:2: `[store]` has no setting `place`; it knows `location` and `local`")
    );
}

#[tokio::test]
async fn longest_prefix_wins() {
    let root = tempfile::tempdir().expect("a folder");
    let (all, deep) = (root.path().join("all"), root.path().join("deep"));
    std::fs::create_dir_all(all.join("bucket/deep")).unwrap();
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(all.join("bucket/a.txt"), "from all").unwrap();
    std::fs::write(all.join("bucket/deep/b.txt"), "from all").unwrap();
    std::fs::write(deep.join("b.txt"), "from deep").unwrap();
    let storage = storage(&format!(
        "[storage.all]\ntype = \"folder\"\nprefixes = [\"s3://\"]\npath = \"{}\"\n\
         [storage.deep]\ntype = \"folder\"\nprefixes = [\"s3://bucket/deep\"]\npath = \"{}\"\n",
        all.display(),
        deep.display()
    ));
    let downloads = root.path().join("downloads");
    let read = |path: std::path::PathBuf| std::fs::read_to_string(path).unwrap();
    let a = storage
        .fetch("s3://bucket/a.txt", &downloads)
        .await
        .unwrap();
    assert_eq!(read(a), "from all");
    let b = storage
        .fetch("s3://bucket/deep/b.txt", &downloads)
        .await
        .unwrap();
    assert_eq!(read(b), "from deep");
    // A prefix ends at a name: `s3://bucket/deep` does not serve `s3://bucket/deeper`.
    let found = storage.stat("s3://bucket/deeper/b.txt").await.unwrap();
    assert_eq!(found, None);
    let error = storage.stat("az://container/blob").await.unwrap_err();
    assert!(error.unsupported());
    assert_eq!(
        error.to_string(),
        "no storage provider serves az://container/blob; name one in the config file, like \
         [storage.<name>] with its type and prefixes = [\"az://container/\"]"
    );
}

#[tokio::test]
async fn supports_folder_operations() {
    let root = tempfile::tempdir().expect("a folder");
    let mirror = root.path().join("mirror");
    std::fs::create_dir_all(mirror.join("data/sub")).unwrap();
    std::fs::write(mirror.join("data/a.txt"), "alpha").unwrap();
    let storage = storage(&format!(
        "[storage.mirror]\ntype = \"folder\"\nprefixes = [\"s3://mirror/\"]\npath = \"{}\"\n",
        mirror.display()
    ));
    assert_eq!(
        shape(storage.stat("s3://mirror/data/a.txt").await.unwrap()),
        Some((false, 5))
    );
    assert_eq!(
        shape(storage.stat("s3://mirror/data").await.unwrap()),
        Some((true, 0))
    );
    assert_eq!(
        storage.list("s3://mirror/data").await.unwrap(),
        [
            Entry {
                name: "a.txt".into(),
                directory: false
            },
            Entry {
                name: "sub".into(),
                directory: true
            }
        ]
    );
    let local = root.path().join("result.txt");
    std::fs::write(&local, "result").unwrap();
    storage
        .upload(&local, "s3://mirror/results/r.txt")
        .await
        .unwrap();
    let written = std::fs::read_to_string(mirror.join("results/r.txt")).unwrap();
    assert_eq!(written, "result");
    let escape = storage.stat("s3://mirror/../secret").await.unwrap_err();
    assert!(escape.to_string().contains("leaves the folder"), "{escape}");
    match storage.store("s3://mirror/store").unwrap() {
        StoreLocation::Folder(path) => assert_eq!(path, mirror.join("store")),
        StoreLocation::Objects(_) => panic!("a folder"),
    }
    match storage.store("/local/store").unwrap() {
        StoreLocation::Folder(path) => assert_eq!(path, Path::new("/local/store")),
        StoreLocation::Objects(_) => panic!("a folder"),
    }
    let error = storage
        .store("https://example.org/store")
        .err()
        .expect("read only");
    assert!(error.to_string().contains("only reads"), "{error}");
}

#[tokio::test]
async fn sends_provider_credentials() {
    let server = WebServer::start(
        &[("/private/samples.csv", "id,reads\na,1\n")],
        Some(("Authorization", "Bearer token-7431")),
    );
    let address = format!("{}/private/samples.csv", server.address);
    let downloads = tempfile::tempdir().expect("a folder");
    let with = |token: &str| {
        storage(&format!(
            "[storage.private]\ntype = \"https\"\nprefixes = [\"{}/private/\"]\ntoken = \"{token}\"\n",
            server.address
        ))
    };
    let right = with("token-7431");
    assert_eq!(
        shape(right.stat(&address).await.unwrap()),
        Some((false, 13))
    );
    let copy = right.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "id,reads\na,1\n");

    // A wrong token is an error that names the address, never the token.
    let error = with("wrong-token-5521")
        .stat(&address)
        .await
        .expect_err("refused");
    let message = error.to_string();
    assert!(!message.contains("wrong-token-5521") && !message.contains("token-7431"));
    assert_eq!(
        message,
        format!(
            "cannot find {address} (storage provider `private`): the server refused access \
             (401 Unauthorized); check the credentials of the provider"
        )
    );
    assert!(!error.retryable());
    // Without a provider, the public web answers that the file is not there.
    assert_eq!(Storage::default().stat(&address).await.unwrap(), None);
    // The public web and a web provider cannot list folders or keep files.
    let error = right.list(&server.address).await.unwrap_err();
    assert!(error.unsupported());
    let error = right.upload(Path::new("/x"), &address).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("cannot write {address}: storage provider `private` only reads")
    );
}

#[tokio::test]
async fn sends_configured_credentials() {
    let header = WebServer::start(&[("/a", "a")], Some(("X-Api-Key", "key-1")));
    let basic = WebServer::start(
        &[("/b", "b")],
        Some(("Authorization", "Basic dXNlcjpwYXNz")),
    );
    let storage = storage(&format!(
        "[storage.header]\ntype = \"https\"\nprefixes = [\"{}/\"]\nheaders = {{ X-Api-Key = \"key-1\" }}\n\
         [storage.basic]\ntype = \"https\"\nprefixes = [\"{}/\"]\nuser = \"user\"\npassword = \"pass\"\n",
        header.address, basic.address
    ));
    let found = storage
        .stat(&format!("{}/a", header.address))
        .await
        .unwrap();
    assert!(found.is_some());
    let found = storage.stat(&format!("{}/b", basic.address)).await.unwrap();
    assert!(found.is_some());
}

#[tokio::test]
async fn downloads_files_once() {
    let server = WebServer::start(&[("/data/reads.fq", "ACGT")], None);
    let address = format!("{}/data/reads.fq", server.address);
    let downloads = tempfile::tempdir().expect("a folder");
    let storage = Storage::default();
    let first = storage.fetch(&address, downloads.path()).await.unwrap();
    let second = storage.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "ACGT");
    // The size of the copy answers without a request; the version was asked once.
    let found = storage.metadata(&address, downloads.path()).await.unwrap();
    assert_eq!(found.map(|found| found.size), Some(4));
    assert_eq!(
        *server.requests.lock().unwrap(),
        ["HEAD /data/reads.fq", "GET /data/reads.fq"]
    );
    // A file that is noted as found needs no second request either.
    let other = WebServer::start(&[("/b", "bb")], None);
    let address = format!("{}/b", other.address);
    let found = storage.metadata(&address, downloads.path()).await.unwrap();
    assert_eq!(found.map(|found| found.size), Some(2));
    let found = storage.metadata(&address, downloads.path()).await.unwrap();
    assert_eq!(found.map(|found| found.size), Some(2));
    assert_eq!(other.served(), 1);
}

#[tokio::test]
async fn rechecks_old_notes() {
    let server = WebServer::start(&[("/a.txt", "now")], None);
    let address = format!("{}/a.txt", server.address);
    let downloads = tempfile::tempdir().expect("a folder");
    // An earlier process noted another version of the file, without asking for its version.
    let name = Storage::default().download_name(&address).unwrap();
    std::fs::write(
        downloads.path().join(format!("{name}.metadata")),
        "file 99\n",
    )
    .unwrap();
    let storage = Storage::default();
    let found = storage.metadata(&address, downloads.path()).await.unwrap();
    assert_eq!(
        found.map(|found| found.size),
        Some(3),
        "the size of this version"
    );
    let copy = storage.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "now");
    assert_eq!(
        server.served(),
        2,
        "one request for the version, one download"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_concurrent_downloads() {
    let body = "ACGT".repeat(1 << 18);
    let server = WebServer::start(&[("/reads.fq", &body)], None);
    let address = format!("{}/reads.fq", server.address);
    let downloads = tempfile::tempdir().expect("a folder");
    let storage = Storage::default();
    let fetch = || storage.fetch(&address, downloads.path());
    let copies = tokio::join!(fetch(), fetch(), fetch(), fetch());
    for copy in [copies.0, copies.1, copies.2, copies.3] {
        let copy = copy.expect("a copy");
        assert_eq!(std::fs::read_to_string(&copy).expect("reads"), body);
    }
    assert_eq!(
        server.served(),
        2,
        "one request for the version, one download"
    );
}

#[tokio::test]
async fn restarts_unversioned_downloads() {
    let server = WebServer::start(&[("/big", "BBBBCCCC")], None);
    let address = format!("{}/big", server.address);
    let downloads = tempfile::tempdir().expect("a folder");
    // A stopped run left bytes of an older representation without its version.
    let name = storage::download_name(&address);
    std::fs::write(
        downloads.path().join(format!("{name}.4294967295.partial")),
        "AAAA",
    )
    .unwrap();
    let copy = Storage::default()
        .fetch(&address, downloads.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "BBBBCCCC");
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests, ["HEAD /big", "GET /big"]);
}

#[test]
fn waits_for_requests() {
    let server = WebServer::start(&[("/a.txt", "text")], None);
    let downloads = tempfile::tempdir().expect("a folder");
    let blocking = Storage::default().blocking();
    let address = format!("{}/a.txt", server.address);
    let copy = blocking.fetch(&address, downloads.path()).expect("a copy");
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "text");
    let missing = blocking.metadata(&format!("{}/b.txt", server.address), downloads.path());
    assert_eq!(missing.expect("an answer"), None);
    let address = format!("{}/b.txt", server.address);
    let error: StorageError = blocking
        .fetch(&address, downloads.path())
        .expect_err("not there");
    assert!(matches!(error, StorageError::Missing { .. }));
    assert_eq!(
        error.to_string(),
        format!("{address} does not exist (the web, without credentials)")
    );
}

#[tokio::test]
async fn reads_ftp_inputs() {
    let server = FtpServer::start(
        &[
            ("/pub/data/a.txt", "alpha"),
            ("/pub/data/sub/b.txt", "beta"),
        ],
        "reader",
        "ftp-secret-3318",
    );
    let with = |password: &str| {
        storage(&format!(
            "[storage.archive]\ntype = \"ftp\"\nprefixes = [\"{}/pub/\"]\nuser = \"reader\"\n\
             password = \"{password}\"\n",
            server.address
        ))
    };
    let storage = with("ftp-secret-3318");
    let file = format!("{}/pub/data/a.txt", server.address);
    let folder = format!("{}/pub/data", server.address);
    assert_eq!(shape(storage.stat(&file).await.unwrap()), Some((false, 5)));
    assert_eq!(shape(storage.stat(&folder).await.unwrap()), Some((true, 0)));
    let missing = format!("{}/pub/none.txt", server.address);
    assert_eq!(storage.stat(&missing).await.unwrap(), None);
    assert_eq!(
        storage.list(&folder).await.unwrap(),
        [
            Entry {
                name: "a.txt".into(),
                directory: false
            },
            Entry {
                name: "sub".into(),
                directory: true
            }
        ]
    );
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage.fetch(&file, downloads.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "alpha");
    // A stopped download starts anew rather than appending another version's suffix.
    let other = format!("{}/pub/data/sub/b.txt", server.address);
    let name = storage::download_name(&other);
    std::fs::write(
        downloads.path().join(format!("{name}.4294967295.partial")),
        "old",
    )
    .unwrap();
    let copy = storage.fetch(&other, downloads.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "beta");
    // A wrong password is an error that shows no password.
    let error = with("wrong-3318").stat(&file).await.unwrap_err();
    let message = error.to_string();
    assert!(message.starts_with(&format!(
        "cannot find {file} (storage provider `archive`): "
    )));
    assert!(
        !message.contains("wrong-3318") && !message.contains("ftp-secret-3318"),
        "{message}"
    );
}

#[tokio::test]
async fn reads_builtin_ftp() {
    // Without a provider, uwe logs in as `anonymous`, as public servers ask.
    let public = FtpServer::start(&[("/pub/a.txt", "alpha")], "anonymous", "anonymous@");
    let file = format!("{}/pub/a.txt", public.address);
    let found = Storage::default().stat(&file).await.unwrap();
    assert_eq!(found.map(|metadata| metadata.size), Some(5));
    // A user and password in the address log in as that user.
    let private = FtpServer::start(&[("/pub/b.txt", "beta")], "reader", "pass-4471");
    let server = private
        .address
        .strip_prefix("ftp://")
        .expect("an ftp address");
    let file = format!("ftp://reader:pass-4471@{server}/pub/b.txt");
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = Storage::default()
        .fetch(&file, downloads.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "beta");
    // A wrong password in the address is an error that does not show it.
    let wrong = format!("ftp://reader:wrong-4471@{server}/pub/b.txt");
    let message = Storage::default()
        .stat(&wrong)
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("reader:[secret]@"), "{message}");
    assert!(!message.contains("wrong-4471"), "{message}");
}

/// Whether a file is a directory, and its size; its version depends on when it was made.
fn shape(found: Option<Metadata>) -> Option<(bool, u64)> {
    found.map(|found| (found.directory, found.size))
}

#[tokio::test]
async fn records_remote_reads() {
    let server = WebServer::start(&[("/a.csv", "id\n1\n")], None);
    let downloads = tempfile::tempdir().expect("a folder of downloads");
    let address = format!("{}/a.csv", server.address);
    let storage = Storage::default();
    let copy = storage.fetch(&address, downloads.path()).await.unwrap();
    let fetched = storage.fetched();
    assert_eq!(fetched.len(), 1);
    let noted = &fetched[0];
    assert_eq!(noted.address, address);
    assert_eq!(noted.copy, copy);
    let digest = "sha256:7cde7fb64fd82bd152710cf238e017b9ab46c0592483edc067ba4f6c75fac108";
    assert_eq!(noted.digest, digest);
    assert_eq!(noted.size, 5);
    let version = noted.version.clone().expect("the ETag of the download");
    assert!(version.starts_with('"'), "{version}");
    // A later process that reads the copy knows the same, with a request for the version only.
    let served = server.served();
    let later = Storage::default();
    later.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(later.fetched(), fetched);
    assert_eq!(server.served(), served + 1);
    let served = server.served();
    // A pinned copy answers for its address in every kind of read.
    let kept = downloads.path().join("kept.csv");
    std::fs::write(&kept, "id\n2\n").unwrap();
    let repeat = Storage::default();
    let other = format!("{}/b.csv", server.address);
    let pinned = storage::Fetched {
        address: other.clone(),
        copy: kept.clone(),
        ..noted.clone()
    };
    repeat.pin(pinned.clone());
    assert_eq!(repeat.fetch(&other, downloads.path()).await.unwrap(), kept);
    let found = repeat.metadata(&other, downloads.path()).await.unwrap();
    assert_eq!(shape(found), Some((false, 5)));
    let file = downloads.path().join("b.csv");
    repeat.copy_of(&other, &file).await.unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "id\n2\n");
    assert_eq!(repeat.fetched(), [pinned]);
    assert_eq!(server.served(), served, "no request for a pinned address");
}

#[tokio::test]
async fn isolates_storage_downloads() {
    let body = "ACGT".repeat(1 << 18);
    let server = common::WebServer::start(&[("/data", &body)], None);
    let target = format!("{}/data", server.address);
    let root = tempfile::tempdir().unwrap();
    let first = Storage::default();
    let second = Storage::default();
    let (a, b) = tokio::join!(
        first.fetch(&target, root.path()),
        second.fetch(&target, root.path())
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a, b);
    assert_eq!(std::fs::read_to_string(a).unwrap(), body);
}

#[tokio::test]
async fn publishes_complete_uploads() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("mirror");
    let storage = storage(&format!(
        "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
        destination.display()
    ));
    let mut handles = Vec::new();
    for number in 0..16 {
        let source = root.path().join(number.to_string());
        std::fs::write(&source, vec![number as u8; 1024 * 1024]).unwrap();
        let storage = storage.clone();
        handles.push(tokio::spawn(async move {
            storage.upload(&source, "s3://bucket/out").await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    let bytes = std::fs::read(destination.join("out")).unwrap();
    assert_eq!(bytes.len(), 1024 * 1024);
    assert!(bytes[0] < 16 && bytes.iter().all(|byte| *byte == bytes[0]));
    assert_eq!(std::fs::read_dir(destination).unwrap().count(), 1);
}

#[tokio::test]
async fn preserves_failed_destination() {
    let server = common::WebServer::start(&[], None);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("old");
    std::fs::write(&file, "old").unwrap();
    assert!(
        Storage::default()
            .download(&format!("{}/missing", server.address), &file)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn rejects_unsolicited_ranges() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/data", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }
        stream.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 4-7/8\r\nConnection: close\r\n\r\nCCCC").unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("old");
    std::fs::write(&file, "old").unwrap();
    let error = Storage::default()
        .download(&address, &file)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("partial response"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn rejects_mixed_versions() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let versions = Arc::new(Mutex::new(Vec::new()));
    let seen = versions.clone();
    std::thread::spawn(move || {
        let mut gets = 0;
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                headers.push(line.to_ascii_lowercase());
            }
            if request.starts_with("HEAD ") {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9437184\r\nETag: \"original\"\r\nConnection: close\r\n\r\n").unwrap();
            } else {
                seen.lock().unwrap().push(
                    headers
                        .iter()
                        .find(|line| line.starts_with("if-match:"))
                        .cloned(),
                );
                gets += 1;
                if gets == 1 {
                    stream.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 8388608\r\nContent-Range: bytes 0-8388607/9437184\r\nETag: \"original\"\r\nConnection: close\r\n\r\n").unwrap();
                    stream.write_all(&vec![b'A'; 8388608]).unwrap();
                } else {
                    stream.write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    break;
                }
            }
        }
    });
    let storage = storage(&format!(
        "[storage.objects]\ntype='s3'\nprefixes='s3://bucket/'\nendpoint='{endpoint}'\nregion='us-east-1'\nanonymous=true"
    ));
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("old");
    std::fs::write(&file, "old").unwrap();
    assert!(storage.download("s3://bucket/data", &file).await.is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    let versions = versions.lock().unwrap();
    assert_eq!(versions.len(), 2);
    assert!(
        versions
            .iter()
            .all(|tag| tag.as_deref() == Some("if-match: \"original\"\r\n"))
    );
}

#[tokio::test]
async fn cleans_cancelled_downloads() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/data", listener.local_addr().unwrap());
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (stop, stopped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\nAAAA",
            )
            .unwrap();
        ready.send(()).unwrap();
        stopped.recv().unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("old");
    std::fs::write(&file, "old").unwrap();
    let destination = file.clone();
    let download =
        tokio::spawn(async move { Storage::default().download(&address, &destination).await });
    waiting.await.unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    download.abort();
    assert!(download.await.unwrap_err().is_cancelled());
    stop.send(()).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn requires_strong_version() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        assert!(request.starts_with("HEAD "));
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9437184\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let storage = storage(&format!(
        "[storage.objects]\ntype='s3'\nprefixes='s3://bucket/'\nendpoint='{endpoint}'\nregion='us-east-1'\nanonymous=true"
    ));
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("old");
    std::fs::write(&file, "old").unwrap();
    let error = storage
        .download("s3://bucket/data", &file)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no strong version"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn preserves_versioned_locations() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().take(3).flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
            }
            seen.lock().unwrap().push(request.trim_end().to_owned());
            if request.starts_with("HEAD ") {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"old\"\r\nx-amz-version-id: v-1\r\nConnection: close\r\n\r\n").unwrap();
            } else {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"old\"\r\nx-amz-version-id: v-1\r\nConnection: close\r\n\r\nold!").unwrap();
            }
        }
    });
    let objects = storage(&format!(
        "[storage.objects]\ntype='s3'\nprefixes='s3://bucket/'\nendpoint='{endpoint}'\nregion='us-east-1'\nanonymous=true"
    ));
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("selected");
    objects
        .download("s3://bucket/a%3Fb%2525?versionId=v-1", &file)
        .await
        .expect("downloads the selected version");
    assert_eq!(std::fs::read(&file).unwrap(), b"old!");
    assert_eq!(
        *requests.lock().unwrap(),
        [
            "HEAD /bucket/a%3Fb%2525?versionId=v-1 HTTP/1.1",
            "HEAD /bucket/a%3Fb%2525?versionId=v-1 HTTP/1.1",
            "GET /bucket/a%3Fb%2525?versionId=v-1 HTTP/1.1",
        ]
    );

    let error = objects
        .upload(&file, "s3://bucket/output?versionId=v-1")
        .await
        .expect_err("cannot overwrite a selected version");
    assert!(
        error
            .to_string()
            .contains("cannot write to a selected object version"),
        "{error}"
    );

    let mirror = tempfile::tempdir().unwrap();
    let folder = storage(&format!(
        "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
        mirror.path().display()
    ));
    let error = folder
        .upload(&file, "s3://bucket/output?versionId=v-1")
        .await
        .expect_err("folder mirrors cannot overwrite a selected version");
    assert!(
        error.to_string().contains("cannot select or write"),
        "{error}"
    );
    assert!(!mirror.path().join("output").exists());
}

#[tokio::test]
async fn receipts_follow_writes() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"data").unwrap();

    for (returned, expected) in [
        (Some("written-v1"), Some("written-v1")),
        (None, None),
        (Some("null"), None),
    ] {
        let (endpoint, request) = s3_put_server(returned);
        let objects = storage(&format!(
            "[storage.objects]\ntype='s3'\nprefixes='s3://bucket/'\nendpoint='{endpoint}'\nregion='us-east-1'\nanonymous=true"
        ));
        let receipt = objects
            .upload_with_receipt(&source, "s3://bucket/output")
            .await
            .expect("uploads with a receipt");
        assert_eq!(receipt.address, "s3://bucket/output");
        assert_eq!(receipt.size, 4);
        assert_eq!(receipt.version.as_deref(), expected);
        assert_eq!(
            request.recv().unwrap(),
            ("PUT /bucket/output HTTP/1.1".into(), b"data".to_vec())
        );
    }

    let (endpoint, request) = s3_put_server(None);
    let objects = storage(&format!(
        "[storage.objects]\ntype='s3'\nprefixes='s3://bucket/'\nendpoint='{endpoint}'\nregion='us-east-1'\nanonymous=true"
    ));
    objects
        .upload(&source, "s3://bucket/plain")
        .await
        .expect("the existing upload API remains supported");
    assert_eq!(
        request.recv().unwrap(),
        ("PUT /bucket/plain HTTP/1.1".into(), b"data".to_vec())
    );

    let mirror = root.path().join("mirror");
    let folder = storage(&format!(
        "[storage.mirror]\ntype='folder'\nprefixes='s3://mirror/'\npath='{}'",
        mirror.display()
    ));
    let receipt = folder
        .upload_with_receipt(&source, "s3://mirror/output")
        .await
        .expect("copies to a folder provider");
    assert_eq!(receipt.address, "s3://mirror/output");
    assert_eq!(receipt.size, 4);
    assert_eq!(receipt.version, None);
    assert_eq!(std::fs::read(mirror.join("output")).unwrap(), b"data");
}

#[tokio::test]
async fn isolates_provider_cache() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let downloads = tempfile::tempdir().unwrap();
    let address = "s3://bucket/data";
    let legacy = downloads.path().join(storage::download_name(address));
    std::fs::write(&legacy, "unverifiable hybrid").unwrap();
    std::fs::write(legacy.with_extension("metadata"), "file 19").unwrap();
    let mut copies = Vec::new();
    for (root, body) in roots.iter().zip(["AAAA", "BBBBCCCC"]) {
        std::fs::write(root.path().join("data"), body).unwrap();
        let text = format!(
            "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
            root.path().display()
        );
        let configured = storage(&text);
        let found = configured
            .metadata(address, downloads.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.size, body.len() as u64);
        let copy = configured.fetch(address, downloads.path()).await.unwrap();
        assert_eq!(
            copy,
            configured.download_path(address, downloads.path()).unwrap()
        );
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), body);
        let later = storage(&text);
        assert_eq!(later.fetch(address, downloads.path()).await.unwrap(), copy);
        assert_eq!(
            configured
                .blocking()
                .download_path(address, downloads.path())
                .unwrap(),
            copy
        );
        copies.push(copy);
    }
    assert_ne!(copies[0], copies[1]);
    assert_eq!(
        std::fs::read_to_string(legacy).unwrap(),
        "unverifiable hybrid"
    );
}

#[tokio::test]
async fn refetches_changed_files() {
    let root = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let file = root.path().join("samplesheet.csv");
    std::fs::write(&file, "id\n1\n").unwrap();
    let text = format!(
        "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
        root.path().display()
    );
    let address = "s3://bucket/samplesheet.csv";
    let first = storage(&text).fetch(address, downloads.path()).await;
    assert_eq!(std::fs::read_to_string(first.unwrap()).unwrap(), "id\n1\n");
    // The user changes the file between two runs, keeping its size.
    std::fs::write(&file, "id\n2\n").unwrap();
    let changed = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let opened = std::fs::File::options().write(true).open(&file).unwrap();
    opened.set_modified(changed).unwrap();
    let run = storage(&text);
    let second = run.fetch(address, downloads.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "id\n2\n");
    // Within a run the version is asked once, so every read gets the same copy.
    std::fs::write(&file, "id\n3\n").unwrap();
    assert_eq!(run.fetch(address, downloads.path()).await.unwrap(), second);
    let path = run.current_path(address, downloads.path()).await.unwrap();
    assert_eq!(path, second);
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "id\n2\n");
    let third = storage(&text)
        .fetch(address, downloads.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(third).unwrap(), "id\n3\n");
}

#[tokio::test]
async fn isolates_storage_sessions() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("database");
    std::fs::write(&source, b"first").unwrap();
    let providers = storage(&format!(
        "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
        root.path().display()
    ));
    let first = providers.session();
    let second = providers.session();
    let cloned = first.clone();
    let address = "s3://bucket/database";
    let downloads = root.path().join("first");
    let copy = first.fetch(address, &downloads).await.unwrap();
    let original = first.fetched();
    first.pin(original[0].clone());
    assert_eq!(cloned.fetched(), original);
    assert!(second.fetched().is_empty());
    assert!(providers.fetched().is_empty());

    std::fs::write(&source, b"other").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))
        .unwrap();
    let changed = second
        .fetch(address, &root.path().join("second"))
        .await
        .unwrap();
    assert_eq!(std::fs::read(changed).unwrap(), b"other");
    assert_ne!(second.fetched()[0].version, original[0].version);
    assert_ne!(second.fetched()[0].digest, original[0].digest);
    assert_eq!(cloned.fetch(address, &downloads).await.unwrap(), copy);
    assert_eq!(std::fs::read(copy).unwrap(), b"first");
    assert_eq!(first.fetched(), original);
    assert_eq!(cloned.fetched(), original);
    assert!(providers.fetched().is_empty());
    drop(first);
    assert!(storage::hold_downloads(&downloads).unwrap().is_none());
    drop(cloned);
    assert!(storage::hold_downloads(&downloads).unwrap().is_some());
}

#[tokio::test]
async fn observes_downloaded_children() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("data");
    std::fs::write(&source, b"child content").unwrap();
    let storage = storage(&format!(
        "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
        root.path().display()
    ));
    let parent = storage.current("s3://bucket/").await.unwrap().unwrap();
    assert!(parent.directory);
    let address = "s3://bucket/data";
    let expected = storage.stat(address).await.unwrap().unwrap();
    let copy = storage
        .fetch(address, &root.path().join("downloads"))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"child content");
    let recorded = storage.fetched();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].size, expected.size);
    assert_eq!(recorded[0].version, expected.version);
}

#[tokio::test]
async fn holds_read_downloads() {
    let root = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("data"), "data").unwrap();
    let text = format!(
        "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
        root.path().display()
    );
    let alone = storage::hold_downloads(downloads.path()).unwrap();
    // A run that wants to read downloads waits while clean-up holds them.
    let reading = storage(&text);
    let fetch = reading.fetch("s3://bucket/data", downloads.path());
    let waited = tokio::time::timeout(std::time::Duration::from_millis(200), fetch).await;
    assert!(waited.is_err());
    drop(alone);
    let copy = reading
        .fetch("s3://bucket/data", downloads.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "data");
    assert!(storage::hold_downloads(downloads.path()).unwrap().is_none());
    drop(reading);
    assert!(storage::hold_downloads(downloads.path()).unwrap().is_some());
}

#[tokio::test]
async fn rejects_unverifiable_cache() {
    let server = WebServer::start(&[("/data", "fresh")], None);
    let address = format!("{}/data", server.address);
    let downloads = tempfile::tempdir().unwrap();
    let legacy = downloads.path().join(storage::download_name(&address));
    std::fs::write(&legacy, "hybrid").unwrap();
    std::fs::write(legacy.with_extension("version"), "\"unproven\"").unwrap();
    let configured = Storage::default();
    let copy = configured.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "fresh");
    assert_eq!(std::fs::read_to_string(legacy).unwrap(), "hybrid");
    assert_ne!(
        configured.fetched()[0].version.as_deref(),
        Some("\"unproven\"")
    );
}

#[test]
fn identity_excludes_secrets() {
    let configured = |endpoint, secret| {
        storage(&format!(
            "[storage.remote]\ntype='s3'\nprefixes='s3://bucket/'\nendpoint='{endpoint}'\nregion='us-east-1'\naccess_key_id='key'\nsecret_access_key={secret}"
        ))
    };
    let key = |provider: Storage| provider.download_name("s3://bucket/data").unwrap();
    assert_eq!(
        key(configured("https://one", "'old'")),
        key(configured("https://one", "'new'"))
    );
    assert_eq!(
        key(configured("https://one", "'old'")),
        key(configured(
            "https://one",
            "{ environment = 'UWE_CAS16_MISSING_SECRET' }"
        ))
    );
    assert_ne!(
        key(configured("https://one", "'old'")),
        key(configured("https://two", "'old'"))
    );
    let public = Storage::default();
    assert_ne!(
        public
            .download_name("https://host?x=same:one@tail")
            .unwrap(),
        public
            .download_name("https://host?x=same:two@tail")
            .unwrap()
    );
    assert_ne!(
        public.download_name("https://host#same:one@tail").unwrap(),
        public.download_name("https://host#same:two@tail").unwrap()
    );
}

#[tokio::test]
async fn publishes_immutable_versions() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/data", listener.local_addr().unwrap());
    let (observed, second_head) = tokio::sync::oneshot::channel();
    let (ready, first_request) = tokio::sync::oneshot::channel();
    let (release, second_response) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut ready = Some(ready);
        let mut observed = Some(observed);
        let mut heads = 0;
        let mut second_response = Some(second_response);
        let mut versions = ["A", "B"].into_iter();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
            }
            // Both callers ask for the version first, and find the same one.
            if request.starts_with("HEAD") {
                let head = "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"A\"\r\nConnection: close\r\n\r\n";
                stream.write_all(head.as_bytes()).unwrap();
                heads += 1;
                if heads == 2 {
                    observed.take().unwrap().send(()).unwrap();
                }
                continue;
            }
            let Some(version) = versions.next() else {
                continue;
            };
            if version == "A" {
                ready.take().unwrap().send(()).unwrap();
            }
            let delayed = if version == "A" {
                second_response.take()
            } else {
                None
            };
            std::thread::spawn(move || {
                // The first transfer waits until the other process observes the same identity.
                if let Some(delayed) = delayed {
                    delayed.recv().unwrap();
                }
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"{version}\"\r\nConnection: close\r\n\r\n{version}{version}{version}{version}").as_bytes()).unwrap();
            });
        }
    });
    let downloads = tempfile::tempdir().unwrap();
    let first = Storage::default();
    let second = Storage::default();
    let fetch = |storage: Storage| {
        let address = address.clone();
        let downloads = downloads.path().to_owned();
        tokio::spawn(async move { storage.fetch(&address, &downloads).await })
    };
    let a = fetch(first.clone());
    first_request.await.unwrap();
    let b = fetch(second.clone());
    second_head.await.unwrap();
    release.send(()).unwrap();
    let a = a.await.unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "AAAA");
    let b = b.await.unwrap().unwrap();
    assert_eq!(a, b);
    let bytes = std::fs::read_to_string(&a).unwrap();
    assert_eq!(bytes, "AAAA");
    let expected = "\"A\"";
    assert_eq!(first.fetched(), second.fetched());
    assert_eq!(first.fetched()[0].version.as_deref(), Some(expected));
    assert_eq!(
        first
            .metadata(&address, downloads.path())
            .await
            .unwrap()
            .unwrap()
            .version
            .as_deref(),
        Some(expected)
    );
    let repeat = Storage::default();
    assert_eq!(repeat.fetch(&address, downloads.path()).await.unwrap(), a);
    assert_eq!(std::fs::read_to_string(b).unwrap(), bytes);
    assert_eq!(repeat.fetched(), first.fetched());
    // The bundle, its transfer lock, and the downloads reader lock.
    assert_eq!(std::fs::read_dir(downloads.path()).unwrap().count(), 3);
}

#[tokio::test]
async fn preserves_folder_addresses() {
    let root = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let configured = storage(&format!(
        "[storage.mirror]\ntype='folder'\nprefixes='https://'\npath='{}'",
        root.path().display()
    ));
    let mut copies = Vec::new();
    for (address, body) in [
        ("https://github.com/o/r/raw/main/data", "first"),
        ("https://raw.githubusercontent.com/o/r/main/data", "second"),
    ] {
        let source = root.path().join(address.strip_prefix("https://").unwrap());
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(source, body).unwrap();
        let copy = configured.fetch(address, downloads.path()).await.unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), body);
        copies.push(copy);
    }
    assert_ne!(copies[0], copies[1]);
    assert_eq!(std::fs::read_to_string(&copies[0]).unwrap(), "first");
}

/// A web server that answers each request with `answer` of its path and keeps the headers of
/// each request, in lower case.
fn header_server(answer: fn(&str) -> String) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
            let mut first = String::new();
            reader.read_line(&mut first).expect("a request line");
            let path = first.split(' ').nth(1).unwrap_or_default().to_owned();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|count| count > 2) {
                kept.lock()
                    .unwrap()
                    .push(format!("{path} {}", line.trim().to_lowercase()));
                line.clear();
            }
            let mut stream = stream;
            let _ = stream.write_all(answer(&path).as_bytes());
        }
    });
    (address, seen)
}

#[tokio::test]
async fn redirects_drop_headers() {
    let (other, elsewhere) = header_server(|_| {
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nthere".to_owned()
    });
    static OTHER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OTHER.set(other).unwrap();
    // The server of the provider sends `/moved` on within itself, and `/away` to another one.
    let (server, here) = header_server(|path| {
        let (status, location) = match path {
            "/moved" => ("302 Found", "/file".to_owned()),
            "/away" => ("302 Found", format!("{}/file", OTHER.get().unwrap())),
            _ => {
                return "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nhere"
                    .into();
            }
        };
        format!(
            "HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    });
    let storage = storage(&format!(
        "[storage.keyed]\ntype = \"https\"\nprefixes = [\"{server}/\"]\nheaders = {{ X-Api-Key = \"key-1\" }}\n"
    ));
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("moved");
    storage
        .download(&format!("{server}/moved"), &file)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"here");
    let keyed = |seen: &Mutex<Vec<String>>, path: &str| {
        let line = format!("{path} x-api-key: key-1");
        seen.lock().unwrap().contains(&line)
    };
    assert!(keyed(&here, "/file"), "the same server gets the header");
    let file = folder.path().join("away");
    storage
        .download(&format!("{server}/away"), &file)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"there");
    assert!(
        !elsewhere.lock().unwrap().is_empty(),
        "the other server was asked"
    );
    assert!(
        !keyed(&elsewhere, "/file"),
        "another server never gets the header"
    );
}

#[test]
fn download_process() {
    let Ok(address) = std::env::var("UWE_TEST_DOWNLOAD") else {
        return;
    };
    let folder = std::path::PathBuf::from(std::env::var("UWE_TEST_DOWNLOADS").unwrap());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let copy = runtime
        .block_on(Storage::default().fetch(&address, &folder))
        .unwrap();
    assert_eq!(std::fs::read(copy).unwrap(), b"shared database");
}

#[test]
fn shares_process_downloads() {
    let server = WebServer::start(&[("/database", "shared database")], None);
    let address = format!("{}/database", server.address);
    let downloads = tempfile::tempdir().unwrap();
    let start = || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "download_process"])
            .env("UWE_TEST_DOWNLOAD", &address)
            .env("UWE_TEST_DOWNLOADS", downloads.path())
            .spawn()
            .unwrap()
    };
    let mut first = start();
    let mut second = start();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.as_str() == "GET /database")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.as_str() == "HEAD /database")
            .count(),
        2
    );
}

#[tokio::test]
async fn rejects_changed_observation() {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/database", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for (mut stream, version) in listener
            .incoming()
            .take(4)
            .map(Result::unwrap)
            .zip(["A", "B", "A", "A"])
        {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nETag: \"{version}\"\r\nConnection: close\r\n\r\n").unwrap();
            if request.starts_with("GET ") {
                stream.write_all(version.as_bytes()).unwrap();
            }
        }
    });
    let downloads = tempfile::tempdir().unwrap();
    assert!(
        Storage::default()
            .fetch(&address, downloads.path())
            .await
            .unwrap_err()
            .retryable()
    );
    let copy = Storage::default()
        .fetch(&address, downloads.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read(copy).unwrap(), b"A");
    server.join().unwrap();
}

#[tokio::test]
async fn publication_keeps_existing() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("destination");
    std::fs::write(&source, b"first").unwrap();
    let address = format!("file://{}", target.display());
    let storage = Storage::default();
    assert!(storage.upload_absent(&source, &address).await.unwrap());
    std::fs::write(&source, b"replacement").unwrap();
    assert!(!storage.upload_absent(&source, &address).await.unwrap());
    assert_eq!(std::fs::read(target).unwrap(), b"first");
}

#[test]
fn download_owner() {
    use std::io::Read;
    let Ok(path) = std::env::var("UWE_TEST_OWNER") else {
        return;
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    println!("owner-ready");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]);
}

#[tokio::test]
async fn recovers_download_owner() {
    let server = WebServer::start(&[("/database", "shared database")], None);
    let address = format!("{}/database", server.address);
    let downloads = tempfile::tempdir().unwrap();
    let storage = Storage::default();
    storage.current(&address).await.unwrap();
    let lock = downloads
        .path()
        .join(storage.download_name(&address).unwrap())
        .with_extension("lock");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "download_owner", "--nocapture"])
        .env("UWE_TEST_OWNER", lock)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        if line.contains("owner-ready") {
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let copy = storage.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(std::fs::read(copy).unwrap(), b"shared database");
}

#[tokio::test]
async fn creates_one_destination() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let target = root.path().join("destination");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    let address = format!("file://{}", target.display());
    let storage = Storage::default();
    let (a, b) = tokio::join!(
        storage.upload_absent(&first, &address),
        storage.upload_absent(&second, &address)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a, b);
    assert_eq!(
        std::fs::read(target).unwrap(),
        if a {
            b"first".to_vec()
        } else {
            b"second".to_vec()
        }
    );
}

//! Storage providers from the config file: which provider serves an address, web servers with
//! credentials that never show in messages, files downloaded once, and folders.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

use common::WebServer;
use common::ftp::FtpServer;
use config_file::ConfigFile;
use storage::settings::Kind;
use storage::{Entry, Metadata, Settings, Storage, StorageError, StoreLocation};

fn config(text: &str) -> ConfigFile {
    ConfigFile::parse(text, Path::new("/home/me/.config/iunctus/config.toml")).expect("TOML")
}

fn storage(text: &str) -> Storage {
    Storage::from_config(&config(text)).expect("valid settings")
}

fn settings_error(text: &str) -> String {
    Settings::from_config(&config(text))
        .expect_err("invalid settings")
        .to_string()
}

#[test]
fn settings_name_providers_and_the_store() {
    let settings = Settings::from_config(&config(
        "[store]\nlocation = \"s3://cache/iunctus\"\n\
         [storage.lab]\ntype = \"s3\"\nprefixes = [\"s3://lab-data/\", \"s3://cache/\"]\n\
         endpoint = \"https://minio.lab.example.org/\"\naccess_key_id = \"lab\"\n\
         secret_access_key = { environment = \"LAB_SECRET\" }\n\
         [storage.mirror]\ntype = \"folder\"\nprefixes = \"s3://ngi-igenomes/\"\npath = \"/data/igenomes\"\n\
         [storage.web]\ntype = \"https\"\nprefixes = [\"https://data.example.org/private/\"]\n\
         token = { environment = \"DATA_TOKEN\" }\nheaders = { X-Api-Key = \"key\" }\n",
    ))
    .expect("valid settings");
    assert_eq!(settings.store.as_deref(), Some("s3://cache/iunctus"));
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
fn wrong_settings_name_the_line() {
    let file = "/home/me/.config/iunctus/config.toml";
    assert_eq!(
        settings_error("[storage.a]\nprefixes = [\"s3://a/\"]\n"),
        format!(
            "{file}, line 1: `[storage.a]` needs `type`, the kind of provider: one of s3, \
             google-cloud-storage, azure-blob-storage, https, ftp, folder"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"dropbox\"\n"),
        format!(
            "{file}, line 2: `storage.a.type` is `dropbox`, which iunctus does not know; use \
             one of s3, google-cloud-storage, azure-blob-storage, https, ftp, folder"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"s3\"\n"),
        format!(
            "{file}, line 1: `[storage.a]` needs `prefixes`, the addresses it serves, like \
             prefixes = [\"s3://my-bucket/\"]"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"s3\"\nprefixes = [\"gs://a/\"]\n"),
        format!(
            "{file}, line 3: `storage.a.prefixes` holds \"gs://a/\"; a `s3` provider needs an \
             address that starts with s3://, http://, https://"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"s3\"\nprefixes = [\"s3://a/\"]\nregoin = \"x\"\n"),
        format!(
            "{file}, line 4: `[storage.a]` has no setting `regoin`; it knows `type`, \
             `prefixes`, `endpoint`, `region`, `path_style`, `access_key_id`, \
             `secret_access_key`, `session_token` and `anonymous`"
        )
    );
    assert_eq!(
        settings_error(
            "[storage.a]\ntype = \"s3\"\nprefixes = [\"s3://a/\"]\naccess_key_id = \"x\"\n"
        ),
        format!(
            "{file}, line 1: `[storage.a]` needs `secret_access_key`, which goes with `access_key_id`"
        )
    );
    assert_eq!(
        settings_error(
            "[storage.a]\ntype = \"folder\"\nprefixes = [\"s3://a/\"]\npath = \"data\"\n"
        ),
        format!("{file}, line 4: `storage.a.path` must be an absolute path, like \"/data/mirror\"")
    );
    assert_eq!(
        settings_error(
            "[storage.a]\ntype = \"https\"\nprefixes = [\"https://x/\"]\n\
             [storage.b]\ntype = \"https\"\nprefixes = [\"https://x/\"]\n"
        ),
        format!(
            "{file}, line 4: the storage providers `a` and `b` both serve \"https://x/\"; give \
             it to one of them"
        )
    );
    assert_eq!(
        settings_error("[store]\nplace = \"/x\"\n"),
        format!("{file}, line 2: `[store]` has no setting `place`; it knows `location`")
    );
}

#[tokio::test]
async fn the_longest_prefix_wins() {
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
async fn folders_read_list_and_write() {
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
async fn web_providers_send_their_credentials() {
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
async fn headers_and_passwords_are_sent_too() {
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
async fn files_are_downloaded_once() {
    let server = WebServer::start(&[("/data/reads.fq", "ACGT")], None);
    let address = format!("{}/data/reads.fq", server.address);
    let downloads = tempfile::tempdir().expect("a folder");
    let storage = Storage::default();
    let first = storage.fetch(&address, downloads.path()).await.unwrap();
    let second = storage.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "ACGT");
    // The size of the copy answers without a request.
    let found = storage.metadata(&address, downloads.path()).await.unwrap();
    assert_eq!(found.map(|found| found.size), Some(4));
    assert_eq!(server.served(), 1);
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
async fn stopped_downloads_continue() {
    let server = WebServer::start(&[("/big", "0123456789")], None);
    let address = format!("{}/big", server.address);
    let downloads = tempfile::tempdir().expect("a folder");
    // A run that was stopped left the first four bytes.
    let name = storage::download_name(&address);
    std::fs::write(
        downloads.path().join(format!("{name}.4294967295.partial")),
        "0123",
    )
    .unwrap();
    let copy = Storage::default()
        .fetch(&address, downloads.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "0123456789");
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests, ["GET /big"]);
}

#[test]
fn drivers_wait_for_requests() {
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
async fn ftp_servers_serve_inputs() {
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
    // A stopped download continues where it ended.
    let other = format!("{}/pub/data/sub/b.txt", server.address);
    let name = storage::download_name(&other);
    std::fs::write(
        downloads.path().join(format!("{name}.4294967295.partial")),
        "be",
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
async fn ftp_servers_without_a_provider_are_read_as_nextflow_reads_them() {
    // Without a provider, iunctus logs in as `anonymous`, as public servers ask.
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
async fn remote_files_that_a_process_reads_are_noted_and_can_be_pinned() {
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
    // A later process that reads the copy knows the same, without a request.
    let served = server.served();
    let later = Storage::default();
    later.fetch(&address, downloads.path()).await.unwrap();
    assert_eq!(later.fetched(), fetched);
    assert_eq!(server.served(), served);
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

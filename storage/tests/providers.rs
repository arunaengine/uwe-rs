//! Storage providers from the config file: which provider serves an address, web servers with
//! credentials that never show in messages, files downloaded once, and folders.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

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
            "{file}:1: `[storage.a]` needs `type`, the kind of provider: one of s3, \
             google-cloud-storage, azure-blob-storage, https, ftp, folder"
        )
    );
    assert_eq!(
        settings_error("[storage.a]\ntype = \"dropbox\"\n"),
        format!(
            "{file}:2: `storage.a.type` is `dropbox`, which iunctus does not know; use \
             one of s3, google-cloud-storage, azure-blob-storage, https, ftp, folder"
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
        settings_error("[store]\nplace = \"/x\"\n"),
        format!("{file}:2: `[store]` has no setting `place`; it knows `location` and `local`")
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

#[tokio::test(flavor = "multi_thread")]
async fn tasks_that_read_a_file_at_once_share_one_download() {
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
    assert_eq!(server.served(), 1);
}

#[tokio::test]
async fn stopped_downloads_restart_without_reusing_unversioned_bytes() {
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

#[tokio::test]
async fn distinct_storage_instances_can_download_the_same_target_concurrently() {
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
async fn concurrent_folder_uploads_publish_one_complete_source() {
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
async fn failed_download_preserves_the_destination_and_cleans_its_temporary_file() {
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
async fn a_whole_http_download_rejects_an_unsolicited_partial_response() {
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
async fn changing_object_version_between_ranges_cannot_publish_a_hybrid() {
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
async fn cancelled_download_keeps_old_destination_and_removes_its_private_file() {
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
async fn object_download_without_a_strong_version_fails_before_reading_ranges() {
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

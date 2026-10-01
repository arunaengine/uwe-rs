//! Checks separate S3 input and store providers against local RustFS servers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use config_file::ConfigFile;
use storage::{Entry, Metadata, Storage, StoreLocation};

/// An environment variable that `just storage-test` sets.
fn variable(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is set by `just storage-test`"))
}

/// The two providers: `inputs` serves `s3://inputs/`, `store` serves `s3://cache/`, each on
/// its own server. The secret keys come from environment variables.
fn two_providers(inputs_secret: &str) -> Storage {
    let text = format!(
        "[storage.inputs]\ntype = \"s3\"\nprefixes = [\"s3://inputs/\"]\nendpoint = \"{}\"\n\
         access_key_id = {{ environment = \"IUNCTUS_TEST_INPUTS_KEY\" }}\n\
         secret_access_key = {inputs_secret}\n\
         [storage.store]\ntype = \"s3\"\nprefixes = [\"s3://cache/\"]\nendpoint = \"{}\"\n\
         access_key_id = {{ environment = \"IUNCTUS_TEST_STORE_KEY\" }}\n\
         secret_access_key = {{ environment = \"IUNCTUS_TEST_STORE_SECRET\" }}\n",
        variable("IUNCTUS_TEST_INPUTS_ENDPOINT"),
        variable("IUNCTUS_TEST_STORE_ENDPOINT"),
    );
    let config = ConfigFile::parse(&text, Path::new("/test/config.toml")).expect("TOML");
    Storage::from_config(&config).expect("valid settings")
}

fn right() -> Storage {
    two_providers("{ environment = \"IUNCTUS_TEST_INPUTS_SECRET\" }")
}

#[tokio::test]
#[ignore = "needs two S3 servers: just storage-test"]
async fn separates_input_store() {
    let storage = right();
    let folder = tempfile::tempdir().expect("a folder");
    let local = folder.path().join("a.csv");
    std::fs::write(&local, "id,reads\na,1\n").unwrap();
    storage
        .upload(&local, "s3://inputs/samples/a.csv")
        .await
        .expect("an upload");
    let file = storage.stat("s3://inputs/samples/a.csv").await.unwrap();
    let file = file.expect("the file");
    assert_eq!((file.directory, file.size), (false, 13));
    assert!(file.version.is_some(), "an object has an ETag");
    let samples: Option<Metadata> = storage.stat("s3://inputs/samples").await.unwrap();
    assert_eq!(
        samples.map(|found| (found.directory, found.size, found.version)),
        Some((true, 0, None))
    );
    assert_eq!(storage.stat("s3://inputs/none.csv").await.unwrap(), None);
    assert_eq!(
        storage.list("s3://inputs/samples/").await.unwrap(),
        [Entry {
            name: "a.csv".into(),
            directory: false
        }]
    );
    let downloads = folder.path().join("downloads");
    let copy = storage
        .fetch("s3://inputs/samples/a.csv", &downloads)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "id,reads\na,1\n");

    // The store lives below `s3://cache/iunctus` on the other server.
    let StoreLocation::Objects(operator) = storage.store("s3://cache/iunctus").unwrap() else {
        panic!("an object store");
    };
    operator
        .write("cas/x", "stored")
        .await
        .expect("a write to the store");
    let stored = storage.stat("s3://cache/iunctus/cas/x").await.unwrap();
    assert_eq!(stored.map(|found| found.size), Some(6));
    // The server of the inputs has no such bucket: each provider has its own endpoint.
    let crossed = storage.stat("s3://inputs/iunctus/cas/x").await.unwrap();
    assert_eq!(crossed, None);
}

#[tokio::test]
#[ignore = "needs two S3 servers: just storage-test"]
async fn redacts_wrong_secret() {
    let storage = two_providers("\"wrong-secret-7788\"");
    let error = storage
        .stat("s3://inputs/samples/a.csv")
        .await
        .expect_err("refused");
    let message = error.to_string();
    assert_eq!(
        message,
        "cannot find s3://inputs/samples/a.csv (storage provider `inputs`): the service \
         refused access; check the credentials of the provider"
    );
    for secret in [
        "wrong-secret-7788".to_owned(),
        variable("IUNCTUS_TEST_INPUTS_SECRET"),
        variable("IUNCTUS_TEST_STORE_SECRET"),
    ] {
        assert!(!message.contains(&secret), "{message}");
    }
}

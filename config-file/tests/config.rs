//! The config file: typed settings, how files merge, secrets from environment variables, and
//! errors that name the file and the line.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::{ConfigError, ConfigFile};

fn parse(text: &str) -> Result<ConfigFile, ConfigError> {
    ConfigFile::parse(text, Path::new("/site/config.toml"))
}

fn write(folder: &Path, name: &str, text: &str) -> PathBuf {
    let file = folder.join(name);
    std::fs::write(&file, text).expect("the file is written");
    file
}

#[test]
fn settings_have_types() {
    let config = parse(
        "[storage.lab]\ntype = \"s3\"\nprefixes = [\"s3://a/\", \"s3://b/\"]\n\
         path_style = true\nparts = 4\nshare = 0.5\n",
    )
    .expect("the text is read");
    let storage = config
        .section("storage")
        .unwrap()
        .expect("a storage section");
    let tables = storage.tables().expect("tables");
    assert_eq!(tables.len(), 1);
    let lab = &tables[0];
    assert_eq!(lab.name(), "storage.lab");
    assert_eq!(lab.origin().line, 1);
    assert_eq!(lab.text("type").unwrap().as_deref(), Some("s3"));
    assert_eq!(
        lab.texts("prefixes").unwrap(),
        Some(vec!["s3://a/".to_owned(), "s3://b/".to_owned()])
    );
    assert_eq!(lab.flag("path_style").unwrap(), Some(true));
    assert_eq!(lab.integer("parts").unwrap(), Some(4));
    assert_eq!(lab.number("share").unwrap(), Some(0.5));
    assert_eq!(lab.number("parts").unwrap(), Some(4.0));
    assert_eq!(lab.text("region").unwrap(), None);
    // One text counts as a list of one.
    let one = parse("prefixes = \"s3://a/\"").unwrap();
    assert_eq!(
        one.root().texts("prefixes").unwrap(),
        Some(vec!["s3://a/".into()])
    );
    assert!(config.section("store").unwrap().is_none());
}

#[test]
fn later_keys_win() {
    let folder = tempfile::tempdir().expect("a folder");
    let site = write(
        folder.path(),
        "site.toml",
        "[storage.lab]\ntype = \"s3\"\nendpoint = \"https://site\"\nprefixes = [\"s3://a/\"]\n\
         [store]\nlocation = \"/site/store\"\n",
    );
    let user = write(
        folder.path(),
        "user.toml",
        "[storage.lab]\nendpoint = \"https://user\"\nprefixes = [\"s3://b/\"]\n\
         [storage.web]\ntype = \"https\"\n",
    );
    let named = write(
        folder.path(),
        "named.toml",
        "[store]\nlocation = \"s3://c/store\"\n",
    );
    let config = ConfigFile::read(&[site.clone(), user.clone(), named.clone()]).expect("read");
    assert_eq!(config.files(), [site.clone(), user.clone(), named]);
    let storage = config.section("storage").unwrap().unwrap();
    let lab = storage.table("lab").unwrap().expect("the lab provider");
    // The type comes from the site file, the endpoint from the user file, and a list is
    // replaced as a whole.
    assert_eq!(lab.text("type").unwrap().as_deref(), Some("s3"));
    assert_eq!(
        lab.text("endpoint").unwrap().as_deref(),
        Some("https://user")
    );
    assert_eq!(lab.texts("prefixes").unwrap(), Some(vec!["s3://b/".into()]));
    // The table keeps the place of its first definition; an error about a value names the
    // file that set it.
    assert_eq!(&*lab.origin().file, site.as_path());
    let error = lab.invalid("endpoint", "is not a web address").to_string();
    assert_eq!(
        error,
        format!(
            "{}:2: `storage.lab.endpoint` is not a web address",
            user.display()
        )
    );
    assert!(storage.table("web").unwrap().is_some());
    let store = config.section("store").unwrap().unwrap();
    assert_eq!(
        store.text("location").unwrap().as_deref(),
        Some("s3://c/store")
    );
}

#[test]
fn resolves_secret_sources() {
    let config = parse(
        "[web]\ntoken = \"written-secret\"\nkey = { environment = \"PATH\" }\n\
         gone = { environment = \"UWE_TEST_VARIABLE_THAT_IS_NOT_SET\" }\n",
    )
    .unwrap();
    let web = config.section("web").unwrap().unwrap();
    let token = web.secret("token").unwrap().expect("a token");
    assert_eq!(token.reveal().unwrap(), "written-secret");
    assert_eq!(token.variable(), None);
    // Printing a secret never shows it.
    assert!(!format!("{token:?}").contains("written-secret"));
    let key = web.secret("key").unwrap().expect("a key");
    assert_eq!(key.variable(), Some("PATH"));
    assert_eq!(key.reveal().unwrap(), std::env::var("PATH").unwrap());
    assert_eq!(format!("{key:?}"), "Secret($PATH)");
    let gone = web.secret("gone").unwrap().expect("a reference");
    let error = gone
        .reveal()
        .expect_err("the variable is not set")
        .to_string();
    assert_eq!(
        error,
        "/site/config.toml:4: the environment variable \
         UWE_TEST_VARIABLE_THAT_IS_NOT_SET, which holds this secret, is not set"
    );
    assert!(web.secret("absent").unwrap().is_none());
}

#[test]
fn errors_include_locations() {
    let syntax = parse("[storage.lab]\ntype = \"s3\nregion = 1\n").expect_err("broken");
    assert!(
        syntax.to_string().starts_with("/site/config.toml:2: "),
        "{syntax}"
    );

    let config = parse(
        "[storage.lab]\ntype = \"s3\"\nprefixes = [\"s3://a/\", 3]\npath_style = \"yes\"\n\
         endpont = \"https://x\"\nkey = { environment = 1 }\nother = { variable = \"X\" }\n",
    )
    .unwrap();
    let lab = config.section("storage").unwrap().unwrap().table("lab");
    let lab = lab.unwrap().unwrap();
    let message = |error: ConfigError| error.to_string();
    assert_eq!(
        message(lab.texts("prefixes").unwrap_err()),
        "/site/config.toml:3: `storage.lab.prefixes` must be a list of texts, not a whole number"
    );
    assert_eq!(
        message(lab.flag("path_style").unwrap_err()),
        "/site/config.toml:4: `storage.lab.path_style` must be true or false, not text"
    );
    assert_eq!(
        message(lab.only(&["type", "prefixes", "endpoint"]).unwrap_err()),
        "/site/config.toml:5: `[storage.lab]` has no setting `endpont`; it knows `type`, \
         `prefixes` and `endpoint`"
    );
    assert_eq!(
        message(lab.secret("key").unwrap_err()),
        "/site/config.toml:6: `storage.lab.key` needs the name of an environment \
         variable, like key = { environment = \"NAME\" }"
    );
    assert_eq!(
        message(lab.secret("other").unwrap_err()),
        "/site/config.toml:7: `[storage.lab.other]` has no setting `variable`; it knows \
         `environment`"
    );
    assert_eq!(
        message(lab.missing("region", "like region = \"eu-central-1\"")),
        "/site/config.toml:1: `[storage.lab]` needs `region`, like region = \"eu-central-1\""
    );
    assert_eq!(
        message(config.root().only(&["store"]).unwrap_err()),
        "/site/config.toml:1: uwe has no section `storage`; it knows `store`"
    );
    let table = parse("storage = 3").unwrap();
    assert_eq!(
        message(table.section("storage").unwrap_err()),
        "/site/config.toml:1: `storage` must be a table, not a whole number"
    );
}

#[test]
fn requires_named_file() {
    let folder = tempfile::tempdir().expect("a folder");
    let missing = folder.path().join("missing.toml");
    let error = ConfigFile::read(std::slice::from_ref(&missing)).expect_err("no such file");
    assert!(
        error.to_string().starts_with(&format!(
            "cannot read the config file {}: ",
            missing.display()
        )),
        "{error}"
    );
    assert!(ConfigFile::default().section("storage").unwrap().is_none());
}

//! nf-dotenv's `dotenv(key)` against nf-dotenv 2.0.1 with Nextflow 26.04.6.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_plugins::{Context, call};

/// A host with its own environment variables.
struct Environment(Vec<(&'static str, &'static str)>);

impl Host for Environment {
    fn print(&mut self, _: &str) {}

    fn environment(&self, name: &str) -> Option<String> {
        let found = self.0.iter().find(|(key, _)| *key == name);
        found.map(|(_, value)| (*value).to_owned())
    }
}

/// A project folder with the fixture as its `.env`, or without one.
fn project(fixture: Option<&str>) -> tempfile::TempDir {
    let folder = tempfile::tempdir().expect("a temporary directory");
    if let Some(name) = fixture {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nf_dotenv")
            .join(format!("{name}.txt"));
        std::fs::copy(source, folder.path().join(".env")).expect("copies the fixture");
    }
    folder
}

/// A config that loads this text.
fn loaded(folder: &Path, text: &str) -> Config {
    let file = folder.join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: folder.display().to_string(),
        launch_dir: folder.display().to_string(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Environment(Vec::new())).expect("loads")
}

/// `key=[value]` for each key, as the probe script prints them, or the first error.
fn lookup(
    folder: &Path,
    config: &Config,
    keys: &[&str],
    environment: Vec<(&'static str, &'static str)>,
) -> String {
    let mut host = Environment(environment);
    let project = folder.display().to_string();
    let mut context = Context {
        host: &mut host,
        config,
        params: Value::map([]),
        project_dir: &project,
        cli_params: &[],
        workflow: Value::Null,
    };
    let mut text = String::new();
    for key in keys {
        match call(
            "nf-dotenv",
            "dotenv",
            vec![Value::string(*key)],
            &mut context,
        ) {
            Ok(value) => text.push_str(&format!("{key}=[{value}]\n")),
            Err(Error::Thrown { exception, .. }) => {
                let message = exception.message.replace(&project, "<probe-dir>");
                return text + &message;
            }
            Err(other) => panic!("{other}"),
        }
    }
    text
}

fn case(fixture: Option<&str>, keys: &[&str]) -> String {
    let folder = project(fixture);
    lookup(folder.path(), &Config::default(), keys, Vec::new())
}

#[test]
fn reads_values() {
    let keys = [
        "FOO",
        "SPACED",
        "DQ",
        "SQ",
        "INLINE",
        "DQ_INLINE",
        "EMPTY",
        "HASH",
        "MULTI",
        "ESC",
        "REF",
        "DUP",
        "EQ",
        "INDENT",
        "TAB",
        "A1",
        "A3",
        "A5",
        "A6",
        "a.b-c",
        "A8",
        "A9",
        "A10",
        "A11",
        "A12",
    ];
    assert_eq!(
        case(Some("values"), &keys),
        "FOO=[bar]\nSPACED=[spaced value]\nDQ=[double quoted]\nSQ=['single quoted']\n\
         INLINE=[value]\nDQ_INLINE=[quoted # not comment]\nEMPTY=[]\nHASH=[a]\n\
         MULTI=[line1\nline2]\nESC=[a\\nb]\nREF=[${FOO}]\nDUP=[two]\nEQ=[a=b]\nINDENT=[x]\n\
         TAB=[tabbed]\nA1=[lead space]\nA3=['x']\nA5=[x]\nA6=[ x ]\na.b-c=[dots]\nA8=[]\n\
         A9=[x\\]\nA10=[#]\nA11=[]\nA12=[a\\\"b]\n"
    );
    assert_eq!(case(Some("crlf"), &["CR", "FOO"]), "CR=[crlf]\nFOO=[bar]\n");
    assert_eq!(
        case(Some("unterminated-swallows"), &["FOO"]),
        "Could not find key FOO in dotenv <probe-dir>/./.env"
    );
    assert_eq!(
        case(Some("missing-key"), &["NOPE"]),
        "Could not find key NOPE in dotenv <probe-dir>/./.env"
    );
}

#[test]
fn prefers_environment() {
    let folder = project(Some("precedence"));
    let environment = vec![
        ("FOO", "fromenv"),
        ("EMPTYENV", ""),
        ("HOME_SET", "fromenv"),
    ];
    let keys = ["FOO", "EMPTYENV", "HOME_SET"];
    assert_eq!(
        lookup(folder.path(), &Config::default(), &keys, environment),
        "FOO=[fromenv]\nEMPTYENV=[]\nHOME_SET=[fromenv]\n"
    );
    let empty = project(None);
    let environment = vec![("FOO", "fromenv")];
    let failure = lookup(empty.path(), &Config::default(), &["FOO"], environment);
    assert!(
        failure.ends_with("error: Could not find <probe-dir>/./.env on the classpath"),
        "{failure}"
    );
}

#[test]
fn reports_bad_files() {
    let wrapper = "Could not find dotenv file at path <probe-dir>/./.env\n\nConsider modifying \
                   the following properties in your Nextflow config:\n\n\
                   \tdotenv.filename = '.env'\n\tdotenv.relative = '.'\n\n\
                   Original `dotenv` parsing error: ";
    let cases = [
        (None, "Could not find <probe-dir>/./.env on the classpath"),
        (Some("export"), "Malformed entry export EXPORTED=yes"),
        (
            Some("indented-comment"),
            "Malformed entry    # indented comment",
        ),
        (Some("no-equals"), "Malformed entry JUSTKEY"),
        (
            Some("continuation"),
            "Malformed entry A2=\"x\"y\nA3=x # c\nA4=x #c",
        ),
        (
            Some("unmatched-quotes"),
            "Malformed entry, unmatched quotes A13=\"a\"b\"",
        ),
        (Some("single-quote-multiline"), "Malformed entry m2'"),
    ];
    for (fixture, inner) in cases {
        assert_eq!(
            case(fixture, &["FOO"]),
            format!("{wrapper}{inner}"),
            "{fixture:?}"
        );
    }
}

#[test]
fn reads_config_options() {
    let folder = project(Some("missing-key"));
    let sub = folder.path().join("sub");
    std::fs::create_dir(&sub).expect("a folder");
    std::fs::write(sub.join("vars.env"), "FOO=from-sub\n").expect("writes");
    let config = loaded(
        folder.path(),
        "dotenv.relative = 'sub'\ndotenv.filename = 'vars.env'\n",
    );
    assert_eq!(
        lookup(folder.path(), &config, &["FOO"], Vec::new()),
        "FOO=[from-sub]\n"
    );
    let absolute = format!("dotenv.relative = '{}'\n", sub.display());
    let config = loaded(
        folder.path(),
        &(absolute + "dotenv.filename = 'vars.env'\n"),
    );
    assert_eq!(
        lookup(folder.path(), &config, &["FOO"], Vec::new()),
        "FOO=[from-sub]\n"
    );
    let config = loaded(
        folder.path(),
        "dotenv.relative = ''\ndotenv.filename = ''\n",
    );
    assert_eq!(
        lookup(folder.path(), &config, &["FOO"], Vec::new()),
        "FOO=[bar]\n"
    );
}

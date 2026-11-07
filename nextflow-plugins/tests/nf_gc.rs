//! The cleanup switch of nf-gc's `nfGc.gc_mode` and nf-boost's `boost.cleanup`, with the errors
//! that stop a run before it starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::catalogue;

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

/// A config that loads this text.
fn loaded(text: &str) -> Config {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
}

#[test]
fn switches_cleanup() {
    let cases = [
        ("plugins { id 'nf-gc@0.3.0' }", true),
        (
            "plugins { id 'nf-gc@0.3.0' }\nnfGc.gc_mode = ' process '",
            true,
        ),
        (
            "plugins { id 'nf-gc@0.3.0' }\nnfGc { gc_mode = 'artifact' }",
            true,
        ),
        ("plugins { id 'nf-boost' }\nboost.cleanup = 'v2'", true),
        ("plugins { id 'nf-boost' }\nboost.cleanup = true", true),
        ("plugins { id 'nf-boost' }\nboost.cleanup = false", false),
        ("plugins { id 'nf-boost' }", false),
        ("boost.cleanup = true\nnfGc.gc_mode = 'artifact'", false),
    ];
    for (text, on) in cases {
        let config = loaded(text);
        let observers = catalogue::observers(&config, std::path::Path::new("/launch"), &|name| {
            std::env::var(name).ok()
        })
        .expect("a valid config");
        assert!(observers.is_empty(), "{text}");
        assert_eq!(nextflow_plugins::cleanup(&config), on, "{text}");
    }
}

#[test]
fn rejects_gc_settings() {
    let cases = [
        (
            "nfGc.gc_mode = 'eager'",
            "Unsupported nf-gc gc_mode 'eager'. Supported values: process, artifact",
        ),
        (
            "nfGc = 'artifact'",
            "nf-gc config 'nfGc' must be a configuration scope",
        ),
    ];
    for (text, message) in cases {
        let config = loaded(&format!("plugins {{ id 'nf-gc@0.3.0' }}\n{text}\n"));
        let error = catalogue::observers(&config, std::path::Path::new("/launch"), &|name| {
            std::env::var(name).ok()
        })
        .err();
        assert_eq!(error.as_deref(), Some(message), "{text}");
        assert!(!nextflow_plugins::cleanup(&config), "{text}");
    }
}

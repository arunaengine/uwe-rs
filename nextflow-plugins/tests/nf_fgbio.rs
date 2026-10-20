//! nf-fgbio's `readStructure` against the plugin nf-fgbio 1.0.0 with Nextflow 26.04.6.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_plugins::{Context, call};

struct Silent;

impl Host for Silent {
    fn print(&mut self, _: &str) {}
}

/// The read structure as text, or the message of the exception that caused the failure.
fn read_structure(text: &str) -> String {
    let config = Config::default();
    let mut host = Silent;
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: "/",
        cli_params: &[],
    };
    match call(
        "nf-fgbio",
        "readStructure",
        vec![Value::string(text)],
        &mut context,
    ) {
        Ok(value) => value.to_string(),
        Err(Error::Thrown { exception, .. }) => {
            let cause = exception.cause.as_ref().expect("a cause");
            format!("{} of {}: {}", exception.class, cause.class, cause.message)
        }
        Err(other) => panic!("{other}"),
    }
}

#[test]
fn parses_read_structures() {
    assert_eq!(read_structure("3M2S+T"), "3M2S+T");
    assert_eq!(read_structure(" 8m 1t "), "8M1T");
    assert_eq!(
        read_structure("10B20C+T"),
        "InvocationTargetException of IllegalArgumentException: Read structure segment had \
         unknown type: 10B[20C]+T"
    );
    assert_eq!(
        read_structure("8M+T+B"),
        "InvocationTargetException of IllegalArgumentException: requirement failed: Variable \
         length (+) can only be used in the last segment: 8M+T+B"
    );
    assert_eq!(
        read_structure("0T"),
        "InvocationTargetException of IllegalArgumentException: Read structure contained zero \
         length segments: 0T"
    );
    assert_eq!(
        read_structure("X"),
        "InvocationTargetException of IllegalArgumentException: Read structure missing length \
         information: [X]"
    );
}

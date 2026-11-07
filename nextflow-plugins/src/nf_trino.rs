//! nf-trino 0.1.0: the functions of nf-sqldb 0.7.0, with Trino and AWS Athena URLs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_sqldb::{Flavour, run};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["fromQuery", "sqlInsert", "sqlExecute"],
    call: Some(call),
    script: Some(SCRIPT),
    factories: &["fromQuery"],
    scopes: &["sql"],
    ..Plugin::new("nf-trino", Kind::Functions, State::Partial)
};

/// The `sqlInsert` operator, as nf-sqldb's script defines it, on the writer of nf-trino.
const SCRIPT: &str = "include { sqlWriter } from 'plugin/nf-trino'\n\
    def sqlInsert(source, Map options = [:]) {\n    def writer = sqlWriter(options)\n    \
    def passed = source.map { item -> writer.add(item); item }\n    \
    passed.subscribe(onComplete: { writer.close() })\n    return passed\n}\n";

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    run(Flavour::Trino, name, arguments, context)
}

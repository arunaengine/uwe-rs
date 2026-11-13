//! nf-databricks 0.1.0: the functions of nf-sqldb 0.7.0 on Databricks SQL warehouses, which uwe
//! reaches through the SQL Statement Execution API instead of a JDBC driver.
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
    ..Plugin::new("nf-databricks", Kind::Functions, State::Partial)
};

/// The `sqlInsert` operator, as nf-sqldb's script defines it, on the writer of nf-databricks.
const SCRIPT: &str = "include { sqlWriter } from 'plugin/nf-databricks'\n\
    def sqlInsert(source, Map options = [:]) {\n    def writer = sqlWriter(options)\n    \
    def passed = source.map { item -> writer.add(item); item }\n    \
    passed.subscribe(onComplete: { writer.close() })\n    return passed\n}\n";

/// Runs the functions with the nf-sqldb 0.7.0 code that the plugin bundles, as nf-trino does.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    run(Flavour::Trino, name, arguments, context)
}

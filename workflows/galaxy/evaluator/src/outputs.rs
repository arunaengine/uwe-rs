//! Decides which outputs a job creates: Galaxy's output filters on the raw parameter values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use galaxy_parser::tool::Tool;
use python_evaluator::{ErrorKind, Interpreter};

use crate::Error;
use crate::values::{self, Job};

/// The names of the outputs that `job` creates. An output is left out when one of its filters
/// is false. A filter that raises a Python error keeps the output, as in Galaxy; a feature
/// that python-evaluator lacks is an error.
pub fn created(
    tool: &Tool,
    job: &Job,
    interpreter: &mut Interpreter,
) -> Result<Vec<String>, Error> {
    let globals = interpreter.new_globals("__filters__", None);
    for (name, value) in values::raw(tool, job)? {
        globals.set(&name, value);
    }
    let mut created = Vec::new();
    'outputs: for output in &tool.outputs {
        for filter in &output.filters {
            let holds = interpreter
                .eval_text(filter.trim(), &globals)
                .and_then(|value| interpreter.truthy(&value));
            match holds {
                Ok(false) => continue 'outputs,
                Ok(true) => {}
                Err(error) if matches!(error.kind, ErrorKind::Exception(_)) => {}
                Err(source) => {
                    return Err(Error::Python {
                        line: output.line,
                        source,
                    });
                }
            }
        }
        created.push(output.name.clone());
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::render;
    use crate::values::Job;

    const TOOL: &str = r#"<tool id="t"><command>$log [${bool($log)}] ${log.ext}</command><inputs>
        <section name="keep"><param name="log" type="boolean" checked="false"/></section></inputs>
        <outputs><data name="log" format="txt"><filter>keep['log'] is True</filter></data>
        <data name="kept" format="txt"><filter>keep['missing']</filter></data></outputs></tool>"#;

    fn job(log: bool) -> Job {
        let mut job = Job {
            parameters: json!({"keep": {"log": log}}),
            ..Job::default()
        };
        job.outputs
            .insert("log".to_owned(), "out/log.txt".to_owned());
        job.outputs
            .insert("kept".to_owned(), "out/kept.txt".to_owned());
        job
    }

    #[test]
    fn filters_raw_values() {
        let tool = galaxy_parser::tool::parse(TOOL, "t.xml", &mut |_| unreachable!("no imports"))
            .expect("a tool");
        let on = render(&tool, &job(true)).expect("renders");
        assert_eq!(
            on.created,
            ["log", "kept"],
            "a failing filter keeps its output"
        );
        assert_eq!(on.command.as_deref(), Some("out/log.txt [True] txt"));
        let off = render(&tool, &job(false)).expect("renders");
        assert_eq!(off.created, ["kept"]);
        assert_eq!(
            off.command.as_deref(),
            Some("None [False] txt"),
            "an absent output is empty"
        );
    }
}

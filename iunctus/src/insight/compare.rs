//! `iunctus compare <run> [<run>]`: what differs between two runs, of the same workflow or of
//! two versions of it. With one run, it is compared with the run of its workflow before it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use run_database::{Comparison, Pairing, Run, Side, TaskPair};

use super::{json_format, print_json};
use crate::arguments::Arguments;
use crate::runs::{local_time, open};
use crate::{Failure, failure};

/// How many tasks each part of the text lists; JSON lists all.
const LISTED: usize = 50;

pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(first) = arguments.target.as_ref().map(|id| id.to_string_lossy()) else {
        return Err(Failure::usage(
            "iunctus: name two runs to compare, like `iunctus compare 1a0f1cc4c10-3a2487 \
             1a0f1d02e11-3b0001`, or one to compare with the run before it; `iunctus runs` \
             lists the runs",
        ));
    };
    let json = json_format(arguments, "compare")?;
    let database = open(arguments)?;
    let broken = |error: run_database::DatabaseError| failure(error.to_string());
    let (first, second) = match arguments.targets.first() {
        Some(second) => (first.into_owned(), second.clone()),
        None => {
            let before = database.run_before(&first).map_err(broken)?;
            let Some(before) = before else {
                return Err(failure(format!(
                    "iunctus: no run of the same workflow came before run {first}; name the \
                     second run"
                )));
            };
            (before, first.into_owned())
        }
    };
    super::run(&database, &first)?;
    super::run(&database, &second)?;
    let comparison = (database.compare(&first, &second).map_err(broken)?)
        .ok_or_else(|| failure(format!("iunctus: cannot compare runs {first} and {second}")))?;
    if json {
        return print_json(serde_json::to_value(&comparison));
    }
    print!("{}", text(&comparison));
    Ok(())
}

/// What differs, in plain lines.
fn text(comparison: &Comparison) -> String {
    let mut text = format!(
        "comparing run {} with run {}\n",
        described(&comparison.first),
        described(&comparison.second)
    );
    if comparison.changes.is_empty() {
        text.push_str("the runs have the same workflow, revision, parameters and inputs\n");
    } else {
        text.push_str("\nthe runs:\n");
        for change in &comparison.changes {
            let _ = writeln!(text, "- {change}");
        }
    }
    let count = |pairing: Pairing| {
        (comparison.tasks.iter())
            .filter(|pair| pair.pairing == pairing)
            .count()
    };
    let in_first = (comparison.tasks.iter())
        .filter(|pair| pair.first.is_some())
        .count();
    let in_second = (comparison.tasks.iter())
        .filter(|pair| pair.second.is_some())
        .count();
    let _ = writeln!(
        text,
        "\ntasks: {in_first} in the first run, {in_second} in the second: {} the same, {} \
         changed, {} with other outputs, {} only in the first run, {} only in the second",
        count(Pairing::Same),
        count(Pairing::Changed),
        count(Pairing::Outputs),
        count(Pairing::First),
        count(Pairing::Second),
    );
    let parts = [
        (Pairing::Changed, "changed"),
        (
            Pairing::Outputs,
            "the same task, with outputs of other content",
        ),
        (Pairing::First, "only in the first run"),
        (Pairing::Second, "only in the second run"),
    ];
    for (pairing, title) in parts {
        let pairs: Vec<&TaskPair> = (comparison.tasks.iter())
            .filter(|pair| pair.pairing == pairing)
            .collect();
        section(&mut text, title, &pairs);
    }
    let ended: Vec<&TaskPair> = (comparison.tasks.iter())
        .filter(|pair| pair.pairing == Pairing::Same && !pair.results.is_empty())
        .collect();
    section(
        &mut text,
        "the same task, which ended otherwise or came from elsewhere",
        &ended,
    );
    text
}

/// A run in a few words: its id, when it started, and its executor.
fn described(run: &Run) -> String {
    let executor = if run.simulated {
        "simulated"
    } else {
        run.executor.as_str()
    };
    format!("{} ({}, {executor})", run.id, local_time(run.started))
}

/// A part of the text: a title and each pair with what differs.
fn section(text: &mut String, title: &str, pairs: &[&TaskPair]) {
    if pairs.is_empty() {
        return;
    }
    let _ = writeln!(text, "\n{title}:");
    for pair in pairs.iter().take(LISTED) {
        let _ = writeln!(text, "  {}", name(pair));
        for line in pair
            .changes
            .iter()
            .chain(&pair.outputs)
            .chain(&pair.results)
        {
            let _ = writeln!(text, "  - {line}");
        }
    }
    if pairs.len() > LISTED {
        let _ = writeln!(
            text,
            "  and {} more; `-format json` lists them all",
            pairs.len() - LISTED
        );
    }
}

/// The name of a task and its number in each run that has it.
fn name(pair: &TaskPair) -> String {
    let task = |side: &Side| side.task;
    match (&pair.first, &pair.second) {
        (Some(a), Some(b)) if a.task == b.task => format!("{} (task {})", a.name, a.task),
        (Some(a), Some(b)) => format!(
            "{} (task {} in the first run, task {} in the second)",
            a.name,
            task(a),
            task(b)
        ),
        (Some(side), None) | (None, Some(side)) => format!("{} (task {})", side.name, side.task),
        (None, None) => pair.step.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn run(id: &str) -> Run {
        Run {
            id: id.into(),
            lineage: id.into(),
            resumes: None,
            workflow: "/w/main.nf".into(),
            language: "nextflow".into(),
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: None,
            process: 1,
            started: 0,
            ended: None,
            state: "succeeded".into(),
            error: None,
            replays: None,
            replays_task: None,
            tasks: BTreeMap::new(),
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        }
    }

    fn side(task: u64, name: &str) -> Option<Side> {
        Some(Side {
            task,
            name: name.into(),
            state: "succeeded".into(),
            exit_code: Some(0),
            reused: None,
            image: None,
        })
    }

    #[test]
    fn the_text_lists_what_differs_by_kind() {
        let comparison = Comparison {
            first: run("a"),
            second: run("b"),
            changes: vec!["the parameter `x` is `1` in the first run and `2` in the second".into()],
            tasks: vec![
                TaskPair {
                    step: "ALIGN".into(),
                    pairing: Pairing::Changed,
                    first: side(2, "ALIGN (s1)"),
                    second: side(5, "ALIGN (s1)"),
                    changes: vec!["the script changed".into()],
                    outputs: Vec::new(),
                    results: Vec::new(),
                },
                TaskPair {
                    step: "NEW".into(),
                    pairing: Pairing::Second,
                    first: None,
                    second: side(6, "NEW"),
                    changes: Vec::new(),
                    outputs: Vec::new(),
                    results: Vec::new(),
                },
            ],
        };
        let text = text(&comparison);
        assert!(
            text.contains("\nthe runs:\n- the parameter `x` is `1`"),
            "{text}"
        );
        assert!(text.contains("tasks: 1 in the first run, 2 in the second: 0 the same, 1 changed"));
        assert!(text.contains(
            "\nchanged:\n  ALIGN (s1) (task 2 in the first run, task 5 in the second)\n  - the \
             script changed\n"
        ));
        assert!(
            text.contains("\nonly in the second run:\n  NEW (task 6)\n"),
            "{text}"
        );
    }
}

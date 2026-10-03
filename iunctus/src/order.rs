//! The order check, `iunctus check-order <workflow>`: runs a workflow twice, once with the
//! results of its tasks handed to the workflow in the order it asked for them and once in the
//! reverse order, and compares the steps of the two runs by the digests of what their tasks
//! read and made. A step whose tasks or outputs change with the order depends on which task
//! finished first, like a step that collects files in the order their tasks finish.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use run_database::{StepChange, StepDifference};
use workflow_graph::Order;

use crate::arguments::Arguments;
use crate::{Failure, Ran, execute, failure, runs};

/// How many tasks may be pending at once in each run, unless `-max-pending` says: enough that
/// the reverse order hands many results to the workflow in another order.
const PENDING: usize = 64;

/// `iunctus check-order`: two runs, the comparison of their steps, and a report. It fails when
/// the outputs of a step depend on the order in which tasks finished.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    if arguments.order.is_some() || arguments.resume.is_some() || arguments.page {
        return Err(Failure::usage(
            "iunctus: `iunctus check-order` chooses the orders of its two runs itself; leave out \
             `-order`, `-resume` and `-page`",
        ));
    }
    let pending = arguments.max_pending.unwrap_or(PENDING);
    let run = |order: Order| {
        let arguments = Arguments {
            order: Some(order),
            max_pending: Some(pending),
            ..arguments.clone()
        };
        execute(&arguments, true, &crate::repeat::Pinned::default())
    };
    let first = run(Order::AsAsked)?;
    let second = run(Order::Reversed)?;
    eprintln!(
        "iunctus: ran the workflow twice: run {} with the results of tasks in the order the \
         workflow asked for them, run {} in the reverse order",
        first.id, second.id
    );
    let ended = |ran: &Ran| ran.execution.error.as_ref().map(ToString::to_string);
    let (a, b) = (ended(&first), ended(&second));
    if a != b {
        let said = |error: Option<String>| error.unwrap_or_else(|| "succeeded".into());
        return Err(failure(format!(
            "iunctus: the workflow depends on the order in which tasks finished: the run in the \
             order asked ended with: {}; the run in the reverse order ended with: {}",
            said(a),
            said(b)
        )));
    }
    let database = runs::open(arguments)?;
    let differences =
        (database.compare_steps(&first.id, &second.id)).map_err(crate::changes::broken)?;
    let (text, depends) = report(&differences);
    print!("{text}");
    match (depends, a) {
        (_, Some(error)) => Err(failure(format!(
            "iunctus: both runs failed the same way: {error}"
        ))),
        (true, None) => Err(failure(
            "iunctus: the outputs of the workflow depend on the order in which tasks finished",
        )),
        (false, None) => Ok(()),
    }
}

/// The report of the check, and whether a step depends on the order.
fn report(differences: &[StepDifference]) -> (String, bool) {
    let of = |change: StepChange| -> Vec<&StepDifference> {
        (differences.iter())
            .filter(|difference| difference.change == change)
            .collect()
    };
    let roots: Vec<&StepDifference> = (differences.iter())
        .filter(|difference| matches!(difference.change, StepChange::Tasks | StepChange::Made))
        .collect();
    let mut text = String::new();
    if roots.is_empty() {
        text.push_str(
            "No step depends on the order in which tasks finished: each step made the same \
             outputs in both runs.\n",
        );
    } else {
        text.push_str(&match roots.len() {
            1 => "1 step depends on the order in which tasks finished:\n".to_owned(),
            steps => format!("{steps} steps depend on the order in which tasks finished:\n"),
        });
        for difference in &roots {
            text.push_str(&format!("  {}: {}\n", difference.step, said(difference)));
        }
    }
    let following = of(StepChange::Inputs);
    if !following.is_empty() {
        text.push_str("Steps whose inputs differ because steps before them differ:\n");
        for difference in following {
            text.push_str(&format!("  {}: {}\n", difference.step, said(difference)));
        }
    }
    let unrepeatable = of(StepChange::Outputs);
    if !unrepeatable.is_empty() {
        text.push_str(
            "Steps whose tools gave other bytes from the same tasks, which is no matter of \
             order (the reproducibility check finds these):\n",
        );
        for difference in unrepeatable {
            text.push_str(&format!("  {}: {}\n", difference.step, said(difference)));
        }
    }
    (text, !roots.is_empty())
}

/// What differs in a step, in words.
fn said(difference: &StepDifference) -> String {
    let outputs = match difference.outputs.as_slice() {
        [] => String::new(),
        paths => format!("; other bytes in {}", paths.join(", ")),
    };
    match difference.change {
        StepChange::Tasks => format!(
            "{} in the order asked, {} in the reverse order",
            count(difference.tasks.0, "task", "tasks"),
            count(difference.tasks.1, "task", "tasks")
        ),
        StepChange::Made => format!(
            "its tasks read the same content, but get another command, script or order of \
             inputs{outputs}"
        ),
        StepChange::Inputs => match difference.after.as_slice() {
            [] => format!("its tasks read other content{outputs}"),
            steps => format!("its tasks read what {} made{outputs}", steps.join(", ")),
        },
        StepChange::Outputs => format!("the same tasks{outputs}"),
    }
}

fn count(number: usize, one: &str, many: &str) -> String {
    match number {
        1 => format!("1 {one}"),
        _ => format!("{number} {many}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn difference(step: &str, change: StepChange, after: &[&str]) -> StepDifference {
        StepDifference {
            step: step.into(),
            change,
            tasks: (2, 2),
            outputs: vec!["list.txt".into()],
            after: after.iter().map(|step| (*step).to_owned()).collect(),
        }
    }

    #[test]
    fn lists_order_dependencies() {
        let (text, depends) = report(&[
            difference("COLLECT", StepChange::Made, &[]),
            difference("REPORT", StepChange::Inputs, &["COLLECT"]),
        ]);
        assert!(depends);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "1 step depends on the order in which tasks finished:"
        );
        assert!(
            lines[1].starts_with("  COLLECT: its tasks read the same content"),
            "{text}"
        );
        assert!(
            lines[3].contains("REPORT: its tasks read what COLLECT made"),
            "{text}"
        );
        let (text, depends) = report(&[]);
        assert!(!depends);
        assert!(text.starts_with("No step depends"), "{text}");
    }
}

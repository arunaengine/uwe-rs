//! Compiles the `depends` and `dependencies` of DAG tasks into control trees of status tests.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::model::DagTask;
use wir::{Control, ResultKind, StatusTest};

/// The control tree of a task, and the tasks whose results it names; a bare name holds when
/// the task succeeded, was skipped or runs as a daemon, or failed when it continues on that.
pub fn control(task: &DagTask, tasks: &[DagTask]) -> Result<(Control, Vec<String>), String> {
    let text = match task.depends.trim().is_empty() {
        true => task.dependencies.join(" && "),
        false => task.depends.clone(),
    };
    if text.trim().is_empty() {
        return Ok((Control::All(Vec::new()), Vec::new()));
    }
    let tokens = lex(&text)?;
    let mut parser = Parser {
        tokens,
        at: 0,
        tasks,
        names: Vec::new(),
    };
    let tree = parser.any()?;
    if parser.at != parser.tokens.len() {
        return Err(format!("depends {text:?} has more after its end"));
    }
    Ok((tree, parser.names))
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Name(String),
    And,
    Or,
    Not,
    Open,
    Close,
}

fn lex(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {}
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            '!' => tokens.push(Token::Not),
            '&' | '|' if chars.peek() == Some(&c) => {
                chars.next();
                tokens.push(if c == '&' { Token::And } else { Token::Or });
            }
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => {
                let mut name = c.to_string();
                while let Some(&next) = chars.peek() {
                    if !(next.is_ascii_alphanumeric() || matches!(next, '-' | '_' | '.')) {
                        break;
                    }
                    name.push(next);
                    chars.next();
                }
                tokens.push(Token::Name(name));
            }
            other => return Err(format!("depends {text:?} has an unknown sign {other:?}")),
        }
    }
    Ok(tokens)
}

struct Parser<'t> {
    tokens: Vec<Token>,
    at: usize,
    tasks: &'t [DagTask],
    names: Vec<String>,
}

impl Parser<'_> {
    fn any(&mut self) -> Result<Control, String> {
        let mut items = vec![self.all()?];
        while self.take(&Token::Or) {
            items.push(self.all()?);
        }
        Ok(one(items, Control::Any))
    }

    fn all(&mut self) -> Result<Control, String> {
        let mut items = vec![self.unary()?];
        while self.take(&Token::And) {
            items.push(self.unary()?);
        }
        Ok(one(items, Control::All))
    }

    fn unary(&mut self) -> Result<Control, String> {
        if self.take(&Token::Not) {
            return Ok(Control::Not(Box::new(self.unary()?)));
        }
        if self.take(&Token::Open) {
            let inner = self.any()?;
            if !self.take(&Token::Close) {
                return Err("depends has an open parenthesis".into());
            }
            return Ok(inner);
        }
        match self.tokens.get(self.at).cloned() {
            Some(Token::Name(name)) => {
                self.at += 1;
                self.test(&name)
            }
            other => Err(format!("depends needs a task name, not {other:?}")),
        }
    }

    /// The status test of `task` or `task.Result`.
    fn test(&mut self, name: &str) -> Result<Control, String> {
        let (task, result) = match name.split_once('.') {
            Some((task, result)) => (task, Some(result)),
            None => (name, None),
        };
        let Some(found) = self.tasks.iter().find(|known| known.name == task) else {
            return Err(format!("depends names {task}, which is no task of the DAG"));
        };
        if !self.names.iter().any(|known| known == task) {
            self.names.push(task.to_owned());
        }
        let on = match result {
            None => {
                let mut on = vec![
                    ResultKind::Succeeded,
                    ResultKind::Skipped,
                    ResultKind::Daemoned,
                ];
                let continues = found.continue_on.unwrap_or_default();
                if continues.error {
                    on.push(ResultKind::Errored);
                }
                if continues.failed {
                    on.push(ResultKind::Failed);
                }
                on
            }
            Some("Succeeded") => vec![ResultKind::Succeeded],
            Some("Failed") => vec![ResultKind::Failed],
            Some("Errored") => vec![ResultKind::Errored],
            Some("Skipped") => vec![ResultKind::Skipped],
            Some("Omitted") => vec![ResultKind::Omitted],
            Some("Daemoned") => vec![ResultKind::Daemoned],
            Some("AnySucceeded") => vec![ResultKind::AnySucceeded],
            Some("AllFailed") => vec![ResultKind::AllFailed],
            Some(other) => return Err(format!("depends tests {task}.{other}, no Argo result")),
        };
        Ok(Control::Result(StatusTest {
            op: task.to_owned(),
            on,
        }))
    }

    fn take(&mut self, token: &Token) -> bool {
        let found = self.tokens.get(self.at) == Some(token);
        self.at += usize::from(found);
        found
    }
}

/// One item as it is, several joined by `join`.
fn one(mut items: Vec<Control>, join: fn(Vec<Control>) -> Control) -> Control {
    match items.len() {
        1 => items.remove(0),
        _ => join(items),
    }
}

/// The failures of tasks that a control tree tests for: a task that holds by them handles
/// them once it succeeds, as an Argo DAG does not fail by a failure that a task ran for.
pub fn handled(tree: &Control) -> Vec<wir::Dependency> {
    let mut found: Vec<wir::Dependency> = Vec::new();
    for test in tree.tests() {
        for result in &test.on {
            let status = match result {
                ResultKind::Failed | ResultKind::AllFailed => wir::Status::Failed,
                ResultKind::Errored => wir::Status::Errored,
                _ => continue,
            };
            match found.iter_mut().find(|known| known.op == test.op) {
                Some(known) if !known.on.contains(&status) => known.on.push(status),
                Some(_) => {}
                None => found.push(wir::Dependency {
                    op: test.op.clone(),
                    on: vec![status],
                }),
            }
        }
    }
    found
}

/// The tree without daemon tests on ops other than `tasks`: only a dialect task runs a daemon.
pub fn daemons(tree: &Control, tasks: &std::collections::BTreeSet<String>) -> Control {
    match tree {
        Control::All(items) => {
            Control::All(items.iter().map(|item| daemons(item, tasks)).collect())
        }
        Control::Any(items) => {
            Control::Any(items.iter().map(|item| daemons(item, tasks)).collect())
        }
        Control::Not(inner) => Control::Not(Box::new(daemons(inner, tasks))),
        Control::Result(test) => {
            let mut test = test.clone();
            if !tasks.contains(&test.op) {
                test.on.retain(|result| *result != ResultKind::Daemoned);
            }
            Control::Result(test)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(name: &str, depends: &str) -> DagTask {
        DagTask {
            name: name.into(),
            depends: depends.into(),
            ..DagTask::default()
        }
    }

    #[test]
    fn compiles_depends_trees() {
        let tasks = [
            task("a", ""),
            task("b-1", ""),
            task("c", "a && !(b-1.Failed || a.Skipped)"),
        ];
        let (tree, names) = control(&tasks[2], &tasks).unwrap();
        assert_eq!(names, ["a", "b-1"]);
        let result =
            |op: &str, on: Vec<ResultKind>| Control::Result(StatusTest { op: op.into(), on });
        let bare = vec![
            ResultKind::Succeeded,
            ResultKind::Skipped,
            ResultKind::Daemoned,
        ];
        let expected = Control::All(vec![
            result("a", bare),
            Control::Not(Box::new(Control::Any(vec![
                result("b-1", vec![ResultKind::Failed]),
                result("a", vec![ResultKind::Skipped]),
            ]))),
        ]);
        assert_eq!(tree, expected);
        assert_eq!(handled(&tree)[0].op, "b-1");
        assert!(control(&task("d", "x"), &tasks).is_err(), "an unknown task");
        let listed = DagTask {
            dependencies: vec!["a".into(), "b-1".into()],
            ..task("e", "")
        };
        let (tree, _) = control(&listed, &tasks).unwrap();
        assert!(matches!(tree, Control::All(items) if items.len() == 2));
    }
}

//! Simulates file-producing shell commands and reports incomplete or failed scripts.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod condition;
mod lexer;
mod programs;
mod words;

use std::collections::HashMap;
use std::ops::Range;

use condition::Entry;
use lexer::{Command, Line, Piece, Token, assignment, logical_lines, pipelines, tokens};
use words::{expand, expand_all, expand_text, normal};

/// What a script makes, as far as its text tells.
#[derive(Debug, Default)]
pub struct Simulation {
    /// The files and directories the script makes, by path relative to the working directory.
    pub made: Vec<Made>,
    /// Whether every command was understood, so that the script makes no other files.
    pub complete: bool,
    /// The command that surely fails, where the script ends, as `bash -e` ends it.
    pub failure: Option<Failure>,
    /// The arguments of the tools that the simulation does not know, which may name the files
    /// they make.
    pub words: Vec<String>,
}

/// A command of a script that surely fails, like `gzip x` when `x.gz` exists.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// The exit status of the command.
    pub status: i32,
    /// What the command prints to standard error.
    pub message: String,
}

/// A file or directory that a script makes.
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub path: String,
    pub directory: bool,
    /// The text the script writes, when it is known.
    pub content: Option<String>,
    /// Whether the text is compressed with gzip.
    pub gzip: bool,
    /// An input that the file is a copy of, like the source of `cp`.
    pub copy_of: Option<String>,
}

/// Simulates a script. `inputs` are the paths of the task's inputs, which the script does not
/// make.
pub fn simulate(script: &str, inputs: &[&str]) -> Simulation {
    let mut shell = Shell {
        variables: HashMap::new(),
        made: Vec::new(),
        complete: true,
        failure: None,
        words: Vec::new(),
        inputs,
    };
    let lines = logical_lines(script);
    shell.run(&lines);
    Simulation {
        made: shell.made,
        complete: shell.complete,
        failure: shell.failure,
        words: shell.words,
    }
}

struct Shell<'a> {
    variables: HashMap<String, String>,
    made: Vec<Made>,
    complete: bool,
    failure: Option<Failure>,
    words: Vec<String>,
    inputs: &'a [&'a str],
}

/// The standard output of a command, as far as it is known.
#[derive(Clone, Debug)]
enum Output {
    Text(String),
    Gzip(String),
    Unknown,
}

impl Shell<'_> {
    fn run(&mut self, lines: &[Line]) {
        let mut index = 0;
        while index < lines.len() && self.failure.is_none() {
            let words = self.words_of(&lines[index]);
            match words.first().map(String::as_str) {
                Some("for") => index = self.for_loop(lines, index),
                Some("if") => index = self.if_statement(lines, index),
                Some("while" | "until" | "case" | "function" | "select") => {
                    self.complete = false;
                    index = skip_block(lines, index);
                }
                _ => {
                    self.line(&lines[index]);
                    index += 1;
                }
            }
        }
    }

    /// The plain words of a line, for its keywords.
    fn words_of(&self, line: &Line) -> Vec<String> {
        tokens(&line.text)
            .into_iter()
            .map_while(|token| match token {
                Token::Word(pieces) => Some(expand(&pieces, &self.variables).unwrap_or_default()),
                _ => None,
            })
            .collect()
    }

    /// `for name in words; do commands; done`, on one line or several; gives the index of
    /// the line after the loop.
    fn for_loop(&mut self, lines: &[Line], start: usize) -> usize {
        // The loop as one text: the lines from `for` to the `done` that closes it.
        let end = skip_block(lines, start);
        let text: Vec<String> = lines[start..end]
            .iter()
            .map(|line| line.text.clone())
            .collect();
        let bodies: Vec<String> = lines[start..end]
            .iter()
            .flat_map(|line| line.bodies.clone())
            .collect();
        let text = text.join("; ");
        let tokens = tokens(&text);
        let mut words = tokens.iter();
        let name = match (words.next(), words.next(), words.next()) {
            (Some(Token::Word(_)), Some(Token::Word(name)), Some(Token::Word(keyword)))
                if expand(keyword, &self.variables).as_deref() == Some("in") =>
            {
                expand(name, &self.variables)
            }
            _ => None,
        };
        let mut items = Vec::new();
        let mut known = name.is_some();
        let mut rest = Vec::new();
        let mut in_items = true;
        for token in words {
            match token {
                Token::Word(word) if in_items => match expand_all(word, &self.variables) {
                    Some(expanded) if expanded == ["do"] => in_items = false,
                    Some(expanded) => items.extend(expanded),
                    None => known = false,
                },
                Token::End if in_items => {}
                token => rest.push(token.clone()),
            }
        }
        // The body ends with `done`.
        let done = rest
            .iter()
            .rposition(|token| matches!(token, Token::Word(word) if word == &text_piece("done")));
        let (Some(name), true, Some(done)) = (name, known, done) else {
            self.complete = false;
            return end;
        };
        let body = rest[..done].to_vec();
        if has_block(&body) {
            self.complete = false;
            return end;
        }
        for item in items {
            self.variables.insert(name.clone(), item);
            for pipeline in pipelines(body.clone()) {
                self.pipeline(&pipeline, &bodies);
            }
        }
        end
    }

    /// `if condition; then commands; elif ...; else commands; fi`, on one line or several.
    /// The simulation follows the commands of the first condition that holds, when it can
    /// decide the conditions; gives the index of the line after the statement.
    fn if_statement(&mut self, lines: &[Line], start: usize) -> usize {
        let end = skip_block(lines, start);
        let text: Vec<String> = lines[start..end]
            .iter()
            .map(|line| line.text.clone())
            .collect();
        let bodies: Vec<String> = lines[start..end]
            .iter()
            .flat_map(|line| line.bodies.clone())
            .collect();
        let tokens = tokens(&text.join("; "));
        let Some(clauses) = clauses(&tokens) else {
            self.complete = false;
            return end;
        };
        for (condition, commands) in clauses {
            let holds = match condition {
                Some(condition) => self.condition(&tokens[condition]),
                None => Some(true),
            };
            match holds {
                Some(true) if !has_block(&tokens[commands.clone()]) => {
                    // The here documents of the commands come after those before them.
                    let before = tokens[..commands.start]
                        .iter()
                        .filter(|token| **token == Token::HereDocument)
                        .count();
                    let bodies = bodies.get(before..).unwrap_or_default();
                    for pipeline in pipelines(tokens[commands].to_vec()) {
                        self.pipeline(&pipeline, bodies);
                    }
                    return end;
                }
                Some(false) => {}
                _ => {
                    self.complete = false;
                    return end;
                }
            }
        }
        end
    }

    /// Whether the condition of an `if` holds: one test command with known words.
    fn condition(&self, tokens: &[Token]) -> Option<bool> {
        let mut words = Vec::new();
        let mut ended = false;
        for token in tokens {
            match token {
                Token::Word(_) if ended => return None,
                Token::Word(word) => words.push(expand(word, &self.variables)?),
                Token::End => ended = !words.is_empty(),
                _ => return None,
            }
        }
        condition::decide(&words, |path| {
            if let Some(made) = self.find(path) {
                if made.directory {
                    Entry::Directory
                } else {
                    Entry::File
                }
            } else if self.inputs.contains(&normal(path).as_str()) {
                Entry::Input
            } else {
                Entry::Absent
            }
        })
    }

    fn line(&mut self, line: &Line) {
        for pipeline in pipelines(tokens(&line.text)) {
            self.pipeline(&pipeline, &line.bodies);
        }
    }

    fn pipeline(&mut self, pipeline: &[Command], bodies: &[String]) {
        let mut input = Output::Unknown;
        for command in pipeline {
            if self.failure.is_some() {
                return;
            }
            input = self.command(command, input, bodies);
        }
    }

    /// Records the command that fails; the script ends there.
    fn fail(&mut self, status: i32, message: String) {
        self.failure.get_or_insert(Failure { status, message });
    }

    /// Runs a command with its standard input; gives its standard output.
    fn command(&mut self, command: &Command, input: Output, bodies: &[String]) -> Output {
        if command.other {
            self.complete = false;
            return Output::Unknown;
        }
        if command.words.is_empty() && command.outputs.is_empty() {
            return input;
        }
        // The commands of command substitutions run first; what they print stays unknown.
        let substituted: Vec<String> = (command.words.iter())
            .chain(command.outputs.iter().map(|(target, _)| target))
            .flatten()
            .filter_map(|piece| match piece {
                Piece::Command(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        for text in substituted {
            for pipeline in pipelines(tokens(&text)) {
                self.pipeline(&pipeline, &[]);
            }
        }
        // Assignments like `prefix=sample` before any command word.
        let mut words = command.words.as_slice();
        while let Some((first, rest)) = words.split_first() {
            match assignment(first) {
                Some((name, value)) => {
                    let value = expand(&value, &self.variables);
                    match value {
                        Some(value) if rest.is_empty() => {
                            self.variables.insert(name, value);
                        }
                        None if rest.is_empty() => {
                            self.variables.remove(&name);
                        }
                        _ => {}
                    }
                    words = rest;
                }
                None => break,
            }
        }
        let input = match command.here_document.and_then(|index| bodies.get(index)) {
            Some(body) => Output::Text(expand_text(body, &self.variables)),
            None => input,
        };
        let mut arguments = Vec::new();
        for word in words {
            match expand_all(word, &self.variables) {
                Some(expanded) => arguments.extend(expanded.into_iter().map(Some)),
                None => arguments.push(None),
            }
        }
        let output = if arguments.is_empty() {
            input
        } else {
            self.program(&arguments, input)
        };
        let mut result = output.clone();
        for (target, append) in &command.outputs {
            // A target that brace expansion makes several words of is an error of bash.
            if let Some(words) = expand_all(target, &self.variables)
                && words.len() > 1
            {
                let text = expand(target, &self.variables).unwrap_or_default();
                self.fail(1, format!("{text}: ambiguous redirect"));
                return Output::Unknown;
            }
            match expand(target, &self.variables) {
                Some(target) => {
                    let content = match &output {
                        Output::Text(text) => Some(text.clone()),
                        Output::Gzip(text) => Some(text.clone()),
                        Output::Unknown => None,
                    };
                    let gzip = matches!(output, Output::Gzip(_));
                    self.make_file(&target, content, gzip, *append);
                }
                None => self.complete = false,
            }
            result = Output::Text(String::new());
        }
        result
    }

    /// The joined text of files, when all of them were made with known text.
    fn contents(&self, files: &[String]) -> Output {
        let mut text = String::new();
        for file in files {
            match self.find(file) {
                Some(Made {
                    content: Some(content),
                    gzip: false,
                    ..
                }) => text.push_str(content),
                _ => return Output::Unknown,
            }
        }
        Output::Text(text)
    }

    fn find(&self, path: &str) -> Option<&Made> {
        let path = normal(path);
        self.made.iter().find(|made| made.path == path)
    }

    /// Records a file; `append` keeps the text of a file made before and adds to it.
    fn make_file(&mut self, path: &str, content: Option<String>, gzip: bool, append: bool) {
        let path = normal(path);
        if path.is_empty() || path == "/dev/null" || path.starts_with("/dev/") {
            return;
        }
        if self.inputs.contains(&path.as_str()) {
            return;
        }
        if let Some(parent) = path.rsplit_once('/').map(|(parent, _)| parent.to_owned()) {
            self.make_directory(&parent);
        }
        match self.made.iter_mut().find(|made| made.path == path) {
            Some(made) if append => {
                made.content = match (made.content.take(), content) {
                    (Some(mut old), Some(new)) => {
                        old.push_str(&new);
                        Some(old)
                    }
                    _ => None,
                };
            }
            Some(made) => {
                made.content = content;
                made.gzip = gzip;
            }
            None => self.made.push(Made {
                path,
                directory: false,
                content,
                gzip,
                copy_of: None,
            }),
        }
    }

    /// Records a directory with the directories above it, as `mkdir -p` makes them.
    fn make_directory(&mut self, path: &str) {
        let path = normal(path);
        if path.is_empty() || path == "/" || self.made.iter().any(|made| made.path == path) {
            return;
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.make_directory(parent);
        }
        self.made.push(Made {
            path,
            directory: true,
            content: None,
            gzip: false,
            copy_of: None,
        });
    }
}

fn text_piece(text: &str) -> Vec<Piece> {
    vec![Piece::Text {
        text: text.to_owned(),
        quoted: false,
    }]
}

/// Whether commands hold a loop, an `if` or a `case`, which the simulation does not follow
/// inside other blocks.
fn has_block(tokens: &[Token]) -> bool {
    tokens.iter().any(|token| {
        matches!(token, Token::Word(word) if ["for", "if", "while", "until", "case"]
            .iter()
            .any(|keyword| word == &text_piece(keyword)))
    })
}

/// A word of plain text, which may be a keyword like `then`.
fn keyword(token: &Token) -> Option<&str> {
    match token {
        Token::Word(word) => match word.as_slice() {
            [
                Piece::Text {
                    text,
                    quoted: false,
                },
            ] => Some(text.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// A clause of an `if` statement as ranges of its tokens: the condition (none for `else`)
/// and the commands.
type Clause = (Option<Range<usize>>, Range<usize>);

/// The clauses of an `if` statement. `None` when the tokens are not a whole `if` statement.
fn clauses(tokens: &[Token]) -> Option<Vec<Clause>> {
    if tokens.first().and_then(keyword) != Some("if") {
        return None;
    }
    let mut clauses = Vec::new();
    let mut condition = Some(1..1);
    let mut commands_start = None;
    let mut depth = 0;
    let mut command_start = true;
    for (index, token) in tokens.iter().enumerate().skip(1) {
        let word = if command_start { keyword(token) } else { None };
        command_start = matches!(token, Token::End | Token::Pipe);
        match (word, depth) {
            (Some("if" | "for" | "while" | "until" | "case" | "select"), _) => depth += 1,
            (Some("fi" | "done" | "esac"), 0) => {
                let start = commands_start?;
                clauses.push((condition, start..index));
                let last = tokens[index + 1..].iter().all(|token| *token == Token::End);
                return (word == Some("fi") && last).then_some(clauses);
            }
            (Some("fi" | "done" | "esac"), _) => depth -= 1,
            (Some("then"), 0) => {
                let start = condition.as_ref()?.start;
                condition = Some(start..index);
                commands_start = Some(index + 1);
                command_start = true;
            }
            (Some("elif"), 0) => {
                clauses.push((condition, commands_start?..index));
                condition = Some(index + 1..index + 1);
                commands_start = None;
                command_start = true;
            }
            (Some("else"), 0) => {
                clauses.push((condition, commands_start?..index));
                condition = None;
                commands_start = Some(index + 1);
                command_start = true;
            }
            (Some("then" | "else" | "do"), _) => command_start = true,
            _ => {}
        }
    }
    None
}

/// The index of the line after the block that starts at `start`: the line whose `done`,
/// `fi` or `esac` closes it, counting blocks inside.
fn skip_block(lines: &[Line], start: usize) -> usize {
    let mut depth = 0;
    for (index, line) in lines.iter().enumerate().skip(start) {
        for token in tokens(&line.text) {
            if let Token::Word(word) = token {
                let Some(Piece::Text {
                    text,
                    quoted: false,
                }) = word.first()
                else {
                    continue;
                };
                match text.as_str() {
                    "for" | "while" | "until" | "if" | "case" | "select" => depth += 1,
                    "done" | "fi" | "esac" => depth -= 1,
                    _ => {}
                }
            }
        }
        if depth <= 0 {
            return index + 1;
        }
    }
    lines.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(simulation: &Simulation) -> Vec<(&str, Option<&str>)> {
        simulation
            .made
            .iter()
            .filter(|made| !made.directory)
            .map(|made| (made.path.as_str(), made.content.as_deref()))
            .collect()
    }

    #[test]
    fn makes_stub_files() {
        let script = "touch s.lf.{drp,drs}\nmkdir -p db\ntouch db/x.txt\n\
                      echo \"\" | gzip > s.fq.gz\nprintf 'a\\tb\\n' > t.tsv\n\
                      cat <<-END_PANELS > panels.txt\nv1\nv2\nEND_PANELS\n\
                      prefix=out\necho hi > ${prefix}.txt\nfor i in 1 2; do touch part_$i.txt; done";
        let simulation = simulate(script, &[]);
        assert!(simulation.complete);
        assert_eq!(
            files(&simulation),
            [
                ("s.lf.drp", Some("")),
                ("s.lf.drs", Some("")),
                ("db/x.txt", Some("")),
                ("s.fq.gz", Some("\n")),
                ("t.tsv", Some("a\tb\n")),
                ("panels.txt", Some("v1\nv2\n")),
                ("out.txt", Some("hi\n")),
                ("part_1.txt", Some("")),
                ("part_2.txt", Some("")),
            ]
        );
        assert!(
            simulation
                .made
                .iter()
                .any(|made| made.path == "db" && made.directory)
        );
        let nested = simulate("mkdir -p s-busco/g.fa/run_x", &[]);
        let directories: Vec<&str> = nested.made.iter().map(|made| made.path.as_str()).collect();
        assert_eq!(
            directories,
            ["s-busco", "s-busco/g.fa", "s-busco/g.fa/run_x"]
        );
    }

    #[test]
    fn tools_mark_incomplete() {
        let simulation = simulate("samtools sort -o x.bam in.bam\ntouch done.txt", &["in.bam"]);
        assert!(!simulation.complete);
        assert_eq!(files(&simulation), [("done.txt", Some(""))]);
        assert!(!simulate("touch $(date).txt", &[]).complete);
        assert!(!simulate("if [ -s a ]; then touch b; fi", &[]).complete);
        // A command substitution runs its command, which may be a tool.
        assert!(!simulate("lengths=$(unify.py -s x)\ntouch done.txt", &[]).complete);
        assert!(simulate("echo $(date) > d.txt", &[]).complete);
        assert!(simulate("echo $((1 + 2)) > n.txt", &[]).complete);
    }

    /// As gzip, which does not replace a file without `-f`: the script ends there, as the
    /// PORECHOP stub of nf-core/methylong does in Nextflow 26.04.6.
    #[test]
    fn gzip_rejects_existing() {
        let script = "touch s.fastq\ngzip s.fastq\ntouch after.txt";
        let simulation = simulate(script, &["s.fastq.gz"]);
        let failure = simulation.failure.expect("a failure");
        assert_eq!(failure.status, 2);
        assert_eq!(
            failure.message,
            "gzip: s.fastq.gz already exists;\tnot overwritten"
        );
        assert!(!simulation.made.iter().any(|made| made.path == "after.txt"));
        let forced = simulate("touch s.fastq\ngzip -f s.fastq", &["s.fastq.gz"]);
        assert!(forced.failure.is_none());
    }

    /// As bash, a redirect to a word that brace expansion makes several of fails.
    #[test]
    fn rejects_ambiguous_redirects() {
        let simulation = simulate("echo '' | gzip > s_{1..2}.fq.gz\ntouch after.txt", &[]);
        let failure = simulation.failure.expect("a failure");
        assert_eq!(failure.status, 1);
        assert_eq!(failure.message, "s_{1..2}.fq.gz: ambiguous redirect");
    }

    #[test]
    fn follows_conditional_commands() {
        let script = "if [ \"false\" == true ]; then\n    touch s.fq.gz\nelse\n    \
                      touch s_1.fq.gz\n    cat <<-END > s_2.txt\n    two\n    END\nfi\n\
                      if [ -f s_1.fq.gz ]; then touch found.txt; elif [ x ]; then touch no.txt; fi";
        let simulation = simulate(script, &[]);
        assert!(simulation.complete);
        assert_eq!(
            files(&simulation),
            [
                ("s_1.fq.gz", Some("")),
                ("s_2.txt", Some("    two\n")),
                ("found.txt", Some("")),
            ]
        );
    }
}

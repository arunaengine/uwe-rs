//! Reads the shell script of a task into lines, words and commands, for the simulator:
//! continued lines, here documents, quotes, variables and redirects.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// A line of commands with the bodies of its here documents.
#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    pub bodies: Vec<String>,
}

/// The lines of a script, with continued lines joined and here documents taken out.
pub fn logical_lines(script: &str) -> Vec<Line> {
    let raw: Vec<&str> = script.lines().collect();
    let mut lines = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        let mut text = raw[index].to_owned();
        index += 1;
        while text.ends_with('\\') && index < raw.len() {
            text.pop();
            text.push(' ');
            text.push_str(raw[index]);
            index += 1;
        }
        let mut bodies = Vec::new();
        for (delimiter, strip_tabs) in here_documents(&text) {
            let mut body = String::new();
            while index < raw.len() {
                let line = raw[index];
                index += 1;
                let line = if strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line
                };
                if line.trim() == delimiter {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            bodies.push(body);
        }
        lines.push(Line { text, bodies });
    }
    lines
}

/// The delimiters of the here documents a line starts, with whether `<<-` strips tabs.
fn here_documents(line: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("<<") {
        let after = &rest[start + 2..];
        if let Some(string) = after.strip_prefix('<') {
            // A here string, `<<<`.
            rest = string;
            continue;
        }
        let strip_tabs = after.starts_with('-');
        let after = after.trim_start_matches('-').trim_start();
        let delimiter: String = after
            .chars()
            .take_while(|c| !c.is_whitespace() && !matches!(c, ';' | '>' | '|' | '&' | ')'))
            .collect();
        let delimiter = delimiter.trim_matches(['\'', '"']).to_owned();
        if !delimiter.is_empty() {
            found.push((delimiter, strip_tabs));
        }
        rest = after;
    }
    found
}

/// A part of a word: text, a variable, the commands of a command substitution, or something
/// else the simulation cannot know, like `${name:-default}`.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text {
        text: String,
        quoted: bool,
    },
    Variable {
        name: String,
        quoted: bool,
    },
    /// `$(commands)` or a backquoted command, whose output is not known.
    Command(String),
    Unknown,
}

/// A token of a command line.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Word(Vec<Piece>),
    /// `;`, `&&`, `||` or `&`, which end a command.
    End,
    Pipe,
    /// `>`, `>>`, `<`, `2>`, `&>` and the like; `true` for a redirect of standard output.
    Redirect {
        output: bool,
        append: bool,
    },
    /// `<<` or `<<-`; the here document is the next body of the line.
    HereDocument,
    /// `(`, `)`, `{`, `}` and other syntax that the simulation does not follow.
    Other,
}

pub fn tokens(text: &str) -> Vec<Token> {
    let characters: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut pieces: Vec<Piece> = Vec::new();
    let finish = |pieces: &mut Vec<Piece>, tokens: &mut Vec<Token>| {
        if !pieces.is_empty() {
            tokens.push(Token::Word(std::mem::take(pieces)));
        }
    };
    let push_text = |pieces: &mut Vec<Piece>, c: char, quoted: bool| match pieces.last_mut() {
        Some(Piece::Text { text, quoted: q }) if *q == quoted => text.push(c),
        _ => pieces.push(Piece::Text {
            text: c.to_string(),
            quoted,
        }),
    };
    while index < characters.len() {
        let c = characters[index];
        match c {
            ' ' | '\t' => {
                finish(&mut pieces, &mut tokens);
                index += 1;
            }
            '#' if pieces.is_empty() => break,
            ';' => {
                finish(&mut pieces, &mut tokens);
                tokens.push(Token::End);
                index += 1;
            }
            '&' | '|' => {
                finish(&mut pieces, &mut tokens);
                let double = characters.get(index + 1) == Some(&c);
                if c == '&' && characters.get(index + 1) == Some(&'>') {
                    // `&>` redirects both outputs.
                    let append = characters.get(index + 2) == Some(&'>');
                    tokens.push(Token::Redirect {
                        output: true,
                        append,
                    });
                    index += if append { 3 } else { 2 };
                    continue;
                }
                tokens.push(if c == '|' && !double {
                    Token::Pipe
                } else {
                    Token::End
                });
                index += if double { 2 } else { 1 };
            }
            '>' | '<' => {
                // A number right before, like `2>`, is the stream of the redirect.
                let stream = match pieces.as_slice() {
                    [
                        Piece::Text {
                            text,
                            quoted: false,
                        },
                    ] if text.chars().all(|c| c.is_ascii_digit()) => {
                        let stream = text.clone();
                        pieces.clear();
                        Some(stream)
                    }
                    _ => None,
                };
                finish(&mut pieces, &mut tokens);
                if c == '<' && characters.get(index + 1) == Some(&'<') {
                    if characters.get(index + 2) == Some(&'<') {
                        tokens.push(Token::Other);
                        index += 3;
                        continue;
                    }
                    tokens.push(Token::HereDocument);
                    index += 2;
                    if characters.get(index) == Some(&'-') {
                        index += 1;
                    }
                    continue;
                }
                let append = c == '>' && characters.get(index + 1) == Some(&'>');
                let output = c == '>' && stream.as_deref().is_none_or(|stream| stream == "1");
                // `>&2` and the like send output to another stream, not to a file.
                let skip = if append { 2 } else { 1 };
                if characters.get(index + skip) == Some(&'&') {
                    index += skip + 1;
                    while characters.get(index).is_some_and(|c| c.is_ascii_digit()) {
                        index += 1;
                    }
                    continue;
                }
                tokens.push(Token::Redirect { output, append });
                index += skip;
            }
            '(' | ')' | '{' | '}' if pieces.is_empty() && c != '{' => {
                tokens.push(Token::Other);
                index += 1;
            }
            '\'' => {
                index += 1;
                let mut text = String::new();
                while index < characters.len() && characters[index] != '\'' {
                    text.push(characters[index]);
                    index += 1;
                }
                index += 1;
                pieces.push(Piece::Text { text, quoted: true });
            }
            '"' => {
                index += 1;
                while index < characters.len() && characters[index] != '"' {
                    match characters[index] {
                        '\\' if index + 1 < characters.len() => {
                            let next = characters[index + 1];
                            if !matches!(next, '"' | '\\' | '$' | '`') {
                                push_text(&mut pieces, '\\', true);
                            }
                            push_text(&mut pieces, next, true);
                            index += 2;
                        }
                        '$' | '`' => index = dollar(&characters, index, true, &mut pieces),
                        other => {
                            push_text(&mut pieces, other, true);
                            index += 1;
                        }
                    }
                }
                index += 1;
                // An empty quoted word is still a word.
                if pieces.is_empty() {
                    pieces.push(Piece::Text {
                        text: String::new(),
                        quoted: true,
                    });
                }
            }
            '\\' => {
                if let Some(next) = characters.get(index + 1) {
                    push_text(&mut pieces, *next, true);
                }
                index += 2;
            }
            '$' | '`' => index = dollar(&characters, index, false, &mut pieces),
            other => {
                push_text(&mut pieces, other, false);
                index += 1;
            }
        }
    }
    finish(&mut pieces, &mut tokens);
    tokens
}

/// Reads `$name`, `${name}`, `$(command)` or a backquoted command at `index`, and gives the
/// index after it.
pub fn dollar(characters: &[char], index: usize, quoted: bool, pieces: &mut Vec<Piece>) -> usize {
    if characters[index] == '`' {
        let end = characters[index + 1..]
            .iter()
            .position(|c| *c == '`')
            .map_or(characters.len(), |end| index + 1 + end + 1);
        let inner = characters[index + 1..end.saturating_sub(1).max(index + 1)].iter();
        pieces.push(Piece::Command(inner.collect()));
        return end;
    }
    match characters.get(index + 1) {
        Some('(') => {
            let mut depth = 0;
            let mut end = index + 1;
            while end < characters.len() {
                match characters[end] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                end += 1;
            }
            // `$((...))` is arithmetic, which makes nothing.
            if characters.get(index + 2) == Some(&'(') {
                pieces.push(Piece::Unknown);
            } else {
                let inner = characters[index + 2..end.min(characters.len())].iter();
                pieces.push(Piece::Command(inner.collect()));
            }
            end + 1
        }
        Some('{') => {
            let end = characters[index..]
                .iter()
                .position(|c| *c == '}')
                .map_or(characters.len(), |end| index + end);
            let name: String = characters[index + 2..end].iter().collect();
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.is_empty() {
                pieces.push(Piece::Variable { name, quoted });
            } else {
                // `${name:-default}` and other expansions.
                pieces.push(Piece::Unknown);
            }
            end + 1
        }
        Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
            let name: String = characters[index + 1..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                .collect();
            let length = name.chars().count();
            pieces.push(Piece::Variable { name, quoted });
            index + 1 + length
        }
        Some(c) if c.is_ascii_digit() || matches!(c, '?' | '#' | '@' | '*' | '$' | '!') => {
            pieces.push(Piece::Unknown);
            index + 2
        }
        _ => {
            pieces.push(Piece::Text {
                text: "$".into(),
                quoted,
            });
            index + 1
        }
    }
}

/// A command: its words, and where its output goes.
#[derive(Clone, Debug, Default)]
pub struct Command {
    pub words: Vec<Vec<Piece>>,
    /// The target of each redirect of standard output, with whether it appends.
    pub outputs: Vec<(Vec<Piece>, bool)>,
    /// The here document that is its standard input, by index in the bodies of the line.
    pub here_document: Option<usize>,
    /// Syntax that the simulation does not follow.
    pub other: bool,
}

/// The pipelines of a line, each a list of commands.
pub fn pipelines(tokens: Vec<Token>) -> Vec<Vec<Command>> {
    let mut pipelines = vec![vec![Command::default()]];
    let mut here_documents = 0;
    let mut tokens = tokens.into_iter().peekable();
    while let Some(token) = tokens.next() {
        let pipeline = pipelines.last_mut().expect("a pipeline");
        let command = pipeline.last_mut().expect("a command");
        match token {
            Token::Word(word) => command.words.push(word),
            Token::End => pipelines.push(vec![Command::default()]),
            Token::Pipe => pipeline.push(Command::default()),
            Token::Redirect { output, append } => match tokens.next() {
                Some(Token::Word(target)) if output => command.outputs.push((target, append)),
                Some(Token::Word(_)) => {}
                _ => command.other = true,
            },
            Token::HereDocument => {
                // The delimiter word.
                tokens.next();
                command.here_document = Some(here_documents);
                here_documents += 1;
            }
            Token::Other => command.other = true,
        }
    }
    pipelines
}

/// `name=value` as the first word of a command.
pub fn assignment(word: &[Piece]) -> Option<(String, Vec<Piece>)> {
    let Some(Piece::Text {
        text,
        quoted: false,
    }) = word.first()
    else {
        return None;
    };
    let (name, rest) = text.split_once('=')?;
    let valid = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if !valid {
        return None;
    }
    let mut value = Vec::new();
    if !rest.is_empty() {
        value.push(Piece::Text {
            text: rest.to_owned(),
            quoted: false,
        });
    }
    value.extend(word[1..].iter().cloned());
    Some((name.to_owned(), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Piece {
        Piece::Text {
            text: text.into(),
            quoted: false,
        }
    }

    #[test]
    fn joins_shell_lines() {
        let lines = logical_lines("cat <<-END > a.txt\n\tx\nEND\necho a \\\n  b");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].bodies, ["x\n"]);
        assert_eq!(lines[1].text, "echo a    b");
    }

    #[test]
    fn parses_shell_words() {
        let tokens = tokens("echo 'a b' \"$x\"y $(date) 2>&1 >> out.txt; true");
        assert_eq!(
            tokens,
            [
                Token::Word(vec![text("echo")]),
                Token::Word(vec![Piece::Text {
                    text: "a b".into(),
                    quoted: true
                }]),
                Token::Word(vec![
                    Piece::Variable {
                        name: "x".into(),
                        quoted: true
                    },
                    text("y")
                ]),
                Token::Word(vec![Piece::Command("date".into())]),
                Token::Redirect {
                    output: true,
                    append: true
                },
                Token::Word(vec![text("out.txt")]),
                Token::End,
                Token::Word(vec![text("true")]),
            ]
        );
        let commands = pipelines(tokens);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0][0].outputs.len(), 1);
        assert!(assignment(&[text("prefix=s1")]).is_some());
        assert!(assignment(&[text("--flag=1")]).is_none());
    }
}

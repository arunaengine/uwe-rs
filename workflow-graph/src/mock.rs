//! The mock executors: a task does not run, it only makes the files it would make. The mock TES
//! executor also writes the GA4GH TES v1.1 task document that a TES server would receive. So a
//! whole workflow runs through without containers or a server, and the TES documents can be
//! checked or sent later.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{ExecuteError, Executor, Outcome, OutputFile, Task};

/// The mock TES executor.
pub struct MockTes {
    /// Holds `tes/<id>.json` and `work/<id>/`, the outputs of each task.
    pub directory: PathBuf,
    /// Prints one line per task when set.
    pub verbose: bool,
}

impl MockTes {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        MockTes {
            directory: directory.into(),
            verbose: false,
        }
    }

    /// Where the outputs of a task go.
    pub fn work_directory(&self, task: &Task) -> PathBuf {
        self.directory.join("work").join(task.id.to_string())
    }
}

impl Executor for MockTes {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let failed = |error: std::io::Error| ExecuteError::Executor {
            task: task.name.clone(),
            message: error.to_string(),
        };
        let work = self.work_directory(task);
        std::fs::create_dir_all(&work).map_err(failed)?;
        let tes = self.directory.join("tes");
        std::fs::create_dir_all(&tes).map_err(failed)?;
        let document = tes_task(task, &work);
        let text = serde_json::to_string_pretty(&document).unwrap_or_default();
        std::fs::write(tes.join(format!("{}.json", task.id)), text).map_err(failed)?;
        if self.verbose {
            println!("[mock tes] {}", task.name);
        }
        let files = pretend(task, &work).map_err(failed)?;
        Ok(success(task, files))
    }
}

/// The mock executor without TES documents: each task only makes its files in
/// `<directory>/<id>/`.
pub struct Mock {
    pub directory: PathBuf,
    /// Prints one line per task when set.
    pub verbose: bool,
}

impl Mock {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Mock {
            directory: directory.into(),
            verbose: false,
        }
    }
}

impl Executor for Mock {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let work = self.directory.join(task.id.to_string());
        if self.verbose {
            println!("[mock] {}", task.name);
        }
        let files = std::fs::create_dir_all(&work)
            .and_then(|()| pretend(task, &work))
            .map_err(|error| ExecuteError::Executor {
                task: task.name.clone(),
                message: error.to_string(),
            })?;
        Ok(success(task, files))
    }
}

fn success(task: &Task, files: Vec<OutputFile>) -> Outcome {
    Outcome {
        id: task.id,
        exit_code: 0,
        files,
        stdout: String::new(),
        stderr: String::new(),
    }
}

/// The TES v1.1 `tesTask` for a task whose outputs go to `work`.
pub fn tes_task(task: &Task, work: &Path) -> Value {
    let workdir = task.workdir.clone().unwrap_or_else(|| "/work".into());
    let inside = |path: &str| {
        if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("{}/{}", workdir.trim_end_matches('/'), path)
        }
    };
    let inputs: Vec<Value> = task
        .inputs
        .iter()
        .map(|input| {
            let mut entry = json!({
                "path": inside(&input.path),
                "type": if input.directory { "DIRECTORY" } else { "FILE" },
            });
            match &input.content {
                Some(content) => entry["content"] = json!(content),
                None => entry["url"] = json!(url(&input.source)),
            }
            entry
        })
        .collect();
    let outputs: Vec<Value> = task
        .outputs
        .iter()
        .map(|pattern| {
            let wildcard = pattern.contains(['*', '?', '[', '{']);
            let mut entry = json!({
                "path": inside(pattern),
                "url": format!("file://{}/{}", work.display(), pattern.trim_start_matches('/')),
                "type": if pattern.ends_with('/') { "DIRECTORY" } else { "FILE" },
            });
            if wildcard {
                entry["path_prefix"] = json!(inside(""));
                entry["url"] = json!(format!("file://{}/", work.display()));
            }
            entry
        })
        .collect();
    let mut executor = json!({
        "image": task.image.clone().unwrap_or_default(),
        "command": task.command,
        "workdir": workdir,
        "env": task.environment,
    });
    if let Some(stdin) = &task.stdin {
        executor["stdin"] = json!(stdin);
    }
    let mut resources = serde_json::Map::new();
    if let Some(cpus) = task.resources.cpus {
        resources.insert("cpu_cores".into(), json!(cpus.ceil() as u64));
    }
    if let Some(memory) = task.resources.memory_bytes {
        resources.insert("ram_gb".into(), json!(memory as f64 / 1e9));
    }
    if let Some(disk) = task.resources.disk_bytes {
        resources.insert("disk_gb".into(), json!(disk as f64 / 1e9));
    }
    json!({
        "name": task.name,
        "description": task.step,
        "inputs": inputs,
        "outputs": outputs,
        "resources": resources,
        "executors": [executor],
        "tags": { "iunctus-task": task.id.to_string(), "iunctus-step": task.step },
    })
}

fn url(source: &str) -> String {
    if source.contains("://") {
        source.to_owned()
    } else {
        let path = std::path::absolute(source).unwrap_or_else(|_| PathBuf::from(source));
        format!("file://{}", path.display())
    }
}

/// Pretends to run a task in `work` and returns its outputs. Inputs with literal content are
/// written first, as a TES server places them. An output is each name in the script that its
/// pattern matches, like the files a stub `touch`es, else one empty file named after the
/// pattern, or a directory for a pattern that ends with `/`. A `.gz` file is an empty gzip file,
/// so that it can be read, and a `versions.yml` gets the one entry that nf-core code reads, as
/// a module's stub writes it.
pub fn pretend(task: &Task, work: &Path) -> std::io::Result<Vec<OutputFile>> {
    for input in &task.inputs {
        if let Some(content) = &input.content
            && inside(&input.path)
        {
            let path = work.join(&input.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, content)?;
        }
    }
    let inputs: Vec<&str> = task
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect();
    // What the script makes. This is the one place where a simulation of the script can take
    // over from reading the names in it.
    let made = match &task.script {
        Some(script) => script_words(script, &inputs),
        None => Vec::new(),
    };
    // Each path to make, with whether it is a directory, and the outputs among them.
    let mut paths: Vec<(String, bool)> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    for pattern in &task.outputs {
        let pattern = pattern.trim_start_matches('/');
        let base = pattern.trim_end_matches('/');
        let literal = !base.contains(['*', '?', '[', '{']);
        let within: Vec<&(String, bool)> = made
            .iter()
            .filter(|(word, _)| literal && word.starts_with(&format!("{base}/")))
            .collect();
        let matched: Vec<&(String, bool)> = made
            .iter()
            .filter(|(word, _)| glob_matches(base, word) && has_literal(base))
            .collect();
        if literal && !within.is_empty() {
            // Files in a directory output, like `touch db/a.txt` for `db`.
            paths.extend(within.into_iter().cloned());
            paths.push((base.to_owned(), true));
            outputs.push(base.to_owned());
        } else if !matched.is_empty() {
            for (word, directory) in matched {
                paths.push((word.clone(), *directory || pattern.ends_with('/')));
                outputs.push(word.clone());
            }
        } else {
            let name = file_name(pattern);
            let directory = name.ends_with('/');
            let name = name.trim_end_matches('/').to_owned();
            paths.push((name.clone(), directory));
            outputs.push(name);
        }
    }
    // Deeper paths first, so that a directory that holds other outputs is not made a file.
    paths.sort_by_key(|(name, _)| std::cmp::Reverse(name.matches('/').count()));
    for (name, directory) in paths {
        let path = work.join(&name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if path.exists() {
            // Made already, as a directory of a deeper path or as an input.
        } else if directory {
            std::fs::create_dir_all(&path)?;
        } else if name == "versions.yml" {
            std::fs::write(&path, format!("\"{}\":\n    mock: 1.0.0\n", task.step))?;
        } else if name.ends_with(".gz") {
            std::fs::write(&path, EMPTY_GZIP)?;
        } else {
            std::fs::write(&path, "")?;
        }
    }
    let mut files: Vec<OutputFile> = Vec::new();
    for name in outputs {
        if !files.iter().any(|file| file.path == name) {
            let location = work.join(&name);
            files.push(OutputFile {
                directory: location.is_dir(),
                path: name,
                location,
            });
        }
    }
    Ok(files)
}

/// The same as [`pretend`], by the name of the first version of this module.
pub fn make_outputs(task: &Task, work: &Path) -> std::io::Result<Vec<OutputFile>> {
    pretend(task, work)
}

/// Whether a relative path stays inside the working directory.
fn inside(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.split('/').any(|component| component == "..")
}

/// A gzip file of no data, so that `.gz` outputs can be read.
pub const EMPTY_GZIP: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// The files a script makes, each with whether it is a directory: the targets of `>` and
/// `>>`, the arguments of `touch` and `mkdir`, and the last argument of `mv`, `cp` and `ln`.
/// Names with variables or globs, and inputs, do not count.
fn script_words(script: &str, inputs: &[&str]) -> Vec<(String, bool)> {
    let mut words: Vec<(String, bool)> = Vec::new();
    let mut add = |word: &str, directory: bool| {
        let word = word.trim_start_matches("./").trim_end_matches('/');
        let skip = word.is_empty()
            || word.starts_with(['-', '&'])
            || word.contains(['$', '=', '*', '?', '[', '{', '\\'])
            || inputs.contains(&word);
        if skip {
            return;
        }
        match words.iter_mut().find(|(known, _)| known == word) {
            Some((_, known)) => *known |= directory,
            None => words.push((word.to_owned(), directory)),
        }
    };
    let commands = script.replace("&&", "\n").replace("||", "\n");
    for command in commands.split(['\n', ';', '|']) {
        let tokens: Vec<&str> = command
            .split_whitespace()
            .map(|token| token.trim_matches(['"', '\'']))
            .collect();
        for (index, token) in tokens.iter().enumerate() {
            // `>`, `>>`, `2>` and `&>` with the target in the token or the next one.
            let redirect = token.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
            if let Some(target) = redirect.strip_prefix('>') {
                let target = target.trim_start_matches('>');
                match target {
                    "" => tokens
                        .get(index + 1)
                        .into_iter()
                        .for_each(|next| add(next, false)),
                    target => add(target, false),
                }
            }
        }
        let arguments: Vec<&str> = tokens
            .iter()
            .skip(1)
            .take_while(|token| !token.contains(['>', '<']))
            .filter(|token| !token.starts_with('-'))
            .copied()
            .collect();
        match tokens.first().copied() {
            Some("touch") => arguments.iter().for_each(|argument| add(argument, false)),
            Some("mkdir") => arguments.iter().for_each(|argument| add(argument, true)),
            Some("mv" | "cp" | "ln") => arguments
                .last()
                .into_iter()
                .for_each(|last| add(last, false)),
            _ => {}
        }
    }
    words
}

/// Whether a glob pattern has text besides wildcards, so that not every word matches it.
fn has_literal(pattern: &str) -> bool {
    pattern
        .chars()
        .any(|c| !matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | ',' | '/'))
}

/// A file name that a glob matches: `*` becomes `mock`, `?` becomes `x`, and a group like
/// `{html,zip}` or `[ab]` its first choice.
pub fn file_name(pattern: &str) -> String {
    let mut name = String::new();
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '*' => {
                let double = characters.next_if_eq(&'*').is_some();
                if double && characters.next_if_eq(&'/').is_some() {
                    continue;
                }
                name.push_str("mock");
            }
            '?' => name.push('x'),
            '{' => {
                let group: String = characters.by_ref().take_while(|c| *c != '}').collect();
                name.push_str(group.split(',').next().unwrap_or_default());
            }
            '[' => {
                let group: String = characters.by_ref().take_while(|c| *c != ']').collect();
                let group = group.trim_start_matches(['!', '^']);
                name.extend(group.chars().next());
            }
            other => name.push(other),
        }
    }
    name
}

/// A small shell glob matcher: `*`, `?`, `[...]` and `{a,b}`; `*` does not cross `/`.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    if let Some(open) = pattern.find('{')
        && let Some(close) = pattern[open..].find('}')
    {
        let close = open + close;
        return pattern[open + 1..close].split(',').any(|choice| {
            glob_matches(
                &format!("{}{}{}", &pattern[..open], choice, &pattern[close + 1..]),
                text,
            )
        });
    }
    fn go(pattern: &[char], text: &[char]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some(('*', rest)) => {
                let double = rest.first() == Some(&'*');
                let rest = if double { &rest[1..] } else { rest };
                (0..=text.len())
                    .take_while(|&skip| double || !text[..skip].contains(&'/'))
                    .any(|skip| go(rest, &text[skip..]))
            }
            Some(('?', rest)) => text.first().is_some_and(|c| *c != '/') && go(rest, &text[1..]),
            Some(('[', rest)) => {
                let Some(end) = rest.iter().position(|c| *c == ']') else {
                    return text.first() == Some(&'[') && go(rest, &text[1..]);
                };
                let (class, rest) = (&rest[..end], &rest[end + 1..]);
                let (negate, class) = match class.first() {
                    Some('!' | '^') => (true, &class[1..]),
                    _ => (false, class),
                };
                let Some(&c) = text.first() else { return false };
                let mut found = false;
                let mut index = 0;
                while index < class.len() {
                    if index + 2 < class.len() && class[index + 1] == '-' {
                        found |= class[index] <= c && c <= class[index + 2];
                        index += 3;
                    } else {
                        found |= class[index] == c;
                        index += 1;
                    }
                }
                found != negate && go(rest, &text[1..])
            }
            Some((p, rest)) => text.first() == Some(p) && go(rest, &text[1..]),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    go(&pattern, &text)
}

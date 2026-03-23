//! The mock TES executor: each task becomes the GA4GH TES v1.1 task document that a TES
//! server would receive, written to disk, and the task "runs" by making an empty file for each
//! output. So a whole workflow runs through without containers or a server, and the TES
//! documents can be checked or sent later.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{ExecuteError, Executor, Outcome, OutputFile, Task};

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
        let files = make_outputs(task, &work).map_err(failed)?;
        Ok(Outcome {
            id: task.id,
            exit_code: 0,
            files,
            stdout: String::new(),
            stderr: String::new(),
        })
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

/// Makes one empty file (or directory, for a path ending in `/`) for each output, under
/// `work`. A glob gets one file name that it matches, preferring a name the script writes.
pub fn make_outputs(task: &Task, work: &Path) -> std::io::Result<Vec<OutputFile>> {
    let words = task.script.as_deref().map(script_words).unwrap_or_default();
    let mut files: Vec<OutputFile> = Vec::new();
    for pattern in &task.outputs {
        let relative = pattern.trim_start_matches('/');
        let directory = relative.ends_with('/');
        let relative = relative.trim_end_matches('/');
        if relative.is_empty() || relative == "." {
            continue;
        }
        let name = if relative.contains(['*', '?', '[', '{']) {
            words
                .iter()
                .find(|word| glob_matches(relative, word))
                .cloned()
                .unwrap_or_else(|| file_name(relative))
        } else {
            relative.to_owned()
        };
        let location = work.join(&name);
        if let Some(parent) = location.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if directory {
            std::fs::create_dir_all(&location)?;
        } else if !location.exists() {
            if name.ends_with(".gz") {
                std::fs::write(&location, EMPTY_GZIP)?;
            } else {
                std::fs::write(&location, "")?;
            }
        }
        if !files.iter().any(|file| file.path == name) {
            files.push(OutputFile {
                path: name,
                location,
                directory,
            });
        }
    }
    Ok(files)
}

/// A gzip file of no data, so that `.gz` outputs can be read.
pub const EMPTY_GZIP: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// Words of a script that name files it writes: targets of `>` and arguments of `touch`.
fn script_words(script: &str) -> Vec<String> {
    let mut words = Vec::new();
    for command in script.split(['\n', ';', '|', '&']) {
        let tokens: Vec<&str> = command
            .split_whitespace()
            .map(|token| token.trim_matches(['"', '\'']))
            .collect();
        for (index, token) in tokens.iter().enumerate() {
            if let Some(target) = token.strip_prefix('>') {
                let target = target.trim_start_matches('>');
                let target = if target.is_empty() {
                    tokens.get(index + 1).copied().unwrap_or_default()
                } else {
                    target
                };
                words.push(target.to_owned());
            }
        }
        if tokens.first() == Some(&"touch") {
            words.extend(tokens[1..].iter().map(|word| word.to_string()));
        }
    }
    words.retain(|word| !word.is_empty() && !word.contains(['$', '*', '?', '{']));
    words
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

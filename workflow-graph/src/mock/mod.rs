//! The mock executors: a task does not run, it only makes the files it would make. The mock TES
//! executor also writes the GA4GH TES v1.1 task document that a TES server would receive. So a
//! whole workflow runs through without containers or a server, and the TES documents can be
//! checked or sent later.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod shell;

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{ExecuteError, Executor, Outcome, OutputFile, Task};
use shell::{Failure, Made};

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
        let run = pretend_run(task, &work).map_err(failed)?;
        Ok(outcome(task, run))
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
        let run = std::fs::create_dir_all(&work)
            .and_then(|()| pretend_run(task, &work))
            .map_err(|error| ExecuteError::Executor {
                task: task.name.clone(),
                message: error.to_string(),
            })?;
        Ok(outcome(task, run))
    }
}

/// The outcome of a task that pretended to run: its files, or the failure of its script.
fn outcome(task: &Task, run: Result<Vec<OutputFile>, Failure>) -> Outcome {
    let (exit_code, files, stderr) = match run {
        Ok(files) => (0, files, String::new()),
        Err(failure) => (failure.status, Vec::new(), failure.message),
    };
    Outcome {
        id: task.id,
        exit_code,
        files,
        stdout: String::new(),
        stderr,
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
/// written first, as a TES server places them. A simulation of the script tells which files
/// it makes and their text, like the files a stub `touch`es or writes with `echo`. An output
/// is each made file that its pattern matches. When nothing matches, it is one empty file
/// named after the pattern, or a directory for a pattern that ends with `/`, so that the run
/// goes on; a driver that knows an output is optional can check the simulation itself
/// ([`shell::simulate`]). A `.gz` file without text is an empty gzip file, so that it can be
/// read, and a `versions.yml` gets the one entry that nf-core code reads.
pub fn pretend(task: &Task, work: &Path) -> std::io::Result<Vec<OutputFile>> {
    Ok(pretend_run(task, work)?.unwrap_or_default())
}

/// The same as [`pretend`], but a script that surely fails, like one that runs `gzip x` when
/// `x.gz` exists, gives its failure instead of files, as it would end so when it ran.
pub fn pretend_run(task: &Task, work: &Path) -> std::io::Result<Result<Vec<OutputFile>, Failure>> {
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
    let simulation = match &task.script {
        Some(script) => shell::simulate(script, &inputs),
        None => command_words(&task.command, &inputs),
    };
    if let Some(failure) = simulation.failure {
        return Ok(Err(failure));
    }
    let made: Vec<Made> = simulation
        .made
        .iter()
        .map(|made| Made {
            path: made.path.trim_start_matches('/').to_owned(),
            ..made.clone()
        })
        .collect();
    // Each path to make, and the outputs of each pattern; none for a pattern nothing matched.
    let mut paths: Vec<Made> = Vec::new();
    let mut outputs: Vec<Vec<String>> = Vec::new();
    for pattern in &task.outputs {
        let pattern = pattern.trim_start_matches('/');
        let base = pattern.trim_end_matches('/');
        let literal = !base.contains(['*', '?', '[', '{']);
        let within: Vec<&Made> = made
            .iter()
            .filter(|made| literal && made.path.starts_with(&format!("{base}/")))
            .collect();
        let matched: Vec<&Made> = made
            .iter()
            .filter(|made| glob_matches(base, &made.path) && has_literal(base))
            .collect();
        let mut names = Vec::new();
        if literal && !within.is_empty() {
            // Files in a directory output, like `touch db/a.txt` for `db`.
            paths.extend(within.into_iter().cloned());
            paths.push(directory(base));
            names.push(base.to_owned());
        } else {
            for made in matched {
                let mut made = made.clone();
                made.directory |= pattern.ends_with('/');
                names.push(made.path.clone());
                paths.push(made);
            }
        }
        outputs.push(names);
    }
    // Outputs that the script does not tell how it makes are made up, so that a run goes on:
    // the files that the arguments of its tools name, else a name after the pattern.
    let named = named_files(&simulation.words, &inputs);
    for (pattern, names) in task.outputs.iter().zip(outputs.iter_mut()) {
        if !names.is_empty() {
            continue;
        }
        let base = pattern.trim_start_matches('/').trim_end_matches('/');
        let by_tools: Vec<&String> = named
            .iter()
            .filter(|name| has_literal(base) && glob_matches(base, name))
            .collect();
        if !by_tools.is_empty() {
            // A pattern that ends with `/` wants directories.
            for name in by_tools {
                paths.push(match pattern.ends_with('/') {
                    true => directory(name),
                    false => placeholder(name),
                });
                names.push(name.clone());
            }
            continue;
        }
        let name = file_name(pattern.trim_start_matches('/'));
        let is_directory = name.ends_with('/');
        let name = name.trim_end_matches('/').to_owned();
        paths.push(if is_directory {
            directory(&name)
        } else {
            placeholder(&name)
        });
        names.push(name);
    }
    // Deeper paths first, so that a directory that holds other outputs is not made a file.
    paths.sort_by_key(|made| std::cmp::Reverse(made.path.matches('/').count()));
    for made in paths {
        write_made(task, work, &made)?;
    }
    let outputs: Vec<String> = outputs.into_iter().flatten().collect();
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
    Ok(Ok(files))
}

/// Writes a file or directory that a task makes; one that exists stays, like an input.
fn write_made(task: &Task, work: &Path, made: &Made) -> std::io::Result<()> {
    let path = work.join(&made.path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        // Made already, as a directory of a deeper path or as an input.
        return Ok(());
    }
    if made.directory {
        return std::fs::create_dir_all(&path);
    }
    // A versions file gets the entry that nf-core code reads, unless the script makes it
    // empty, as `touch versions.yml` does.
    if made.path == "versions.yml" && made.content.as_deref() != Some("") {
        return std::fs::write(&path, format!("\"{}\":\n    mock: 1.0.0\n", task.step));
    }
    // A copy of a local input file has its bytes.
    let source = made.copy_of.as_ref().and_then(|copied| {
        let input = task.inputs.iter().find(|input| input.path == *copied)?;
        match &input.content {
            Some(content) => Some(content.clone().into_bytes()),
            None if !input.source.contains("://") => std::fs::read(&input.source).ok(),
            None => None,
        }
    });
    if let Some(bytes) = source {
        return std::fs::write(&path, bytes);
    }
    let text = made.content.as_deref().unwrap_or_default();
    if made.gzip && !text.is_empty() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, text.as_bytes())?;
        return std::fs::write(&path, encoder.finish()?);
    }
    if made.path.ends_with(".gz") && text.is_empty() {
        return std::fs::write(&path, EMPTY_GZIP);
    }
    std::fs::write(&path, text)
}

fn directory(path: &str) -> Made {
    Made {
        path: path.to_owned(),
        directory: true,
        content: None,
        gzip: false,
        copy_of: None,
    }
}

/// An empty file for an output that the script does not tell how it makes.
fn placeholder(path: &str) -> Made {
    Made {
        path: path.to_owned(),
        directory: false,
        content: None,
        gzip: false,
        copy_of: None,
    }
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

/// The files a command without a script may make: the files its arguments name.
fn command_words(command: &[String], inputs: &[&str]) -> shell::Simulation {
    let made = named_files(command.get(1..).unwrap_or_default(), inputs)
        .into_iter()
        .map(|name| placeholder(&name))
        .collect();
    // A command may make any file, so the simulation is not complete.
    shell::Simulation {
        made,
        complete: false,
        failure: None,
        words: Vec::new(),
    }
}

/// The files that the arguments of a tool may name: the parts between `,` and `=`, like
/// `a.bam` of `--out=a.bam` or `--out a.bam,b.bam`. Only patterns with text besides wildcards
/// choose among them. Flags, absolute paths and inputs do not count.
fn named_files(arguments: &[String], inputs: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in arguments.iter().flat_map(|word| word.split([',', '='])) {
        let part = part.trim_start_matches("./");
        let skip = part.is_empty()
            || part.starts_with(['-', '/'])
            || part.contains(['$', '*', '?', '[', '{', '\\', ' '])
            || inputs.contains(&part);
        if !skip && !names.iter().any(|known| known == part) {
            names.push(part.to_owned());
        }
    }
    names
}

/// Whether a glob pattern has text besides wildcards, so that not every word matches it.
fn has_literal(pattern: &str) -> bool {
    pattern
        .chars()
        .any(|c| !matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | ',' | '/'))
}

/// A file name that a glob matches: `*` becomes `mock`, `?` becomes `x`, and a group like
/// `{html,zip}` or `[ab]` its first choice. `**/` is one directory, `mock/`, as a Java glob
/// needs one.
pub fn file_name(pattern: &str) -> String {
    let mut name = String::new();
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '*' => {
                let double = characters.next_if_eq(&'*').is_some();
                if double && characters.next_if_eq(&'/').is_some() {
                    name.push_str("mock/");
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

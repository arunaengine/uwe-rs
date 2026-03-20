//! Runs a Nextflow script: the runtime asks for tasks, the task runner executes or reuses them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use content_store::Store;
use executor::local::LocalContainer;
use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::glob;
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::value::PathValue;
use nextflow_runtime::task::{
    ENVIRONMENT, STDOUT, command_script, container, read_environment, task_request,
};
use nextflow_runtime::{Loader, Options, Run, TaskOutcome, TaskRequest};
use task_runner::Runner;

use crate::arguments::Arguments;
use crate::remote;

pub struct Settings {
    pub store: PathBuf,
    pub scratch: PathBuf,
    pub engine: String,
    pub stub: bool,
    /// Whether tasks only pretend to run: each output pattern gets one empty file.
    pub mock: bool,
}

/// Why a run failed, and whether the cause is a feature iunctus does not support yet.
pub struct Failure {
    pub message: String,
    pub unsupported: bool,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Failure {
            unsupported: error.unsupported(),
            message: error.to_string(),
        }
    }
}

fn failure(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        unsupported: false,
    }
}

/// Prints what the workflow prints, and downloads remote files into a cache.
struct Console {
    downloads: PathBuf,
}

impl Host for Console {
    fn print(&mut self, text: &str) {
        print!("{text}");
    }

    fn download(&mut self, path: &PathValue) -> Result<PathBuf, Error> {
        let Some(url) = remote::https_url(path) else {
            return Err(Error::Unsupported {
                message: format!("reading remote files like {}", path.uri()),
                span: Default::default(),
            });
        };
        remote::download(&url, &self.downloads).map_err(io_error)
    }

    fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
        if path.prefix.is_none() {
            return match std::fs::metadata(&path.path) {
                Ok(metadata) => Ok(Some(FileMetadata {
                    directory: metadata.is_dir(),
                    size: metadata.len(),
                })),
                // As in Java, a path below a file does not exist.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(io_error(format!("{}: {error}", path.path))),
            };
        }
        match remote::https_url(path) {
            Some(url) => remote::metadata(path, &url).map_err(io_error),
            None => Err(Error::Unsupported {
                message: format!("reading remote files like {}", path.uri()),
                span: Default::default(),
            }),
        }
    }
}

fn io_error(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("IOException", message)),
        span: Default::default(),
    }
}

/// `iunctus config`: the config of a project in the flat form of `nextflow config -flat`.
pub fn print_config(arguments: &Arguments) -> Result<(), Failure> {
    let project = arguments.target.clone().unwrap_or_else(|| ".".into());
    let config = load_config(&project, arguments)?;
    print!("{}", config.flat());
    Ok(())
}

/// The host of a run; remote files are cached in `downloads` of the cache directory.
fn console() -> Console {
    Console {
        downloads: crate::cache_directory().join("downloads"),
    }
}

/// The config files of a run, as `nextflow run` finds them.
fn config_files(project: &Path, arguments: &Arguments) -> Vec<String> {
    let launch = std::env::current_dir().unwrap_or_default();
    let project = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
    nextflow_config::files(&project, &launch, &arguments.configs)
        .iter()
        .map(|file| file.to_string_lossy().into_owned())
        .collect()
}

/// Loads the config files of a project directory, as `nextflow run` does.
fn load_config(project: &Path, arguments: &Arguments) -> Result<Config, Failure> {
    let launch = std::env::current_dir().map_err(|error| failure(error.to_string()))?;
    let project = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
    let files = nextflow_config::files(&project, &launch, &arguments.configs);
    let options = nextflow_config::Options {
        profiles: arguments.profiles.clone(),
        params: arguments.params.clone(),
        project_dir: project.to_string_lossy().into_owned(),
        launch_dir: launch.to_string_lossy().into_owned(),
    };
    nextflow_config::load(&files, &options, &mut console()).map_err(|error| Failure {
        unsupported: error.unsupported(),
        message: error.to_string(),
    })
}

pub fn run(target: &Path, arguments: &Arguments, settings: &Settings) -> Result<(), Failure> {
    // A project directory runs its `manifest.mainScript`, else `main.nf`.
    let (project, script) = if target.is_dir() {
        let config = load_config(target, arguments)?;
        let main = config
            .get(&["manifest", "mainScript"])
            .map_or("main.nf".into(), |name| name.to_string());
        (target.to_owned(), target.join(main))
    } else {
        // The parent of a bare file name like `main.nf` is empty.
        let project = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        (project.to_owned(), target.to_owned())
    };
    let config = load_config(&project, arguments)?;
    let module = Loader::default().load(&script).map_err(|error| Failure {
        unsupported: error
            .messages
            .iter()
            .any(|message| message.contains("not supported yet")),
        message: error.to_string(),
    })?;
    let store = Store::local(&settings.store).map_err(|error| failure(error.to_string()))?;
    let executor = LocalContainer {
        engine: settings.engine.clone(),
        scratch: settings.scratch.clone(),
    };
    let run_id = run_id();
    let runner = Runner::new(
        store,
        executor,
        &run_id,
        &settings.scratch.join("downloads"),
    )
    .map_err(|error| failure(error.to_string()))?;
    let options = Options {
        stub: settings.stub,
        container_engine: Some(settings.engine.clone()),
        config,
        profile: arguments
            .profiles
            .as_ref()
            .map_or("standard".into(), |names| names.join(",")),
        project_dir: std::path::absolute(&project)
            .unwrap_or(project.clone())
            .to_string_lossy()
            .into_owned(),
        script_file: std::path::absolute(&script)
            .unwrap_or(script.clone())
            .to_string_lossy()
            .into_owned(),
        run_name: run_id.clone(),
        config_files: config_files(&project, arguments),
        command_line: std::env::args().collect::<Vec<_>>().join(" "),
    };
    let mut console = console();
    let (mut run, mut queue) = Run::start(&module, &options, &mut console)?;
    let mut pinned = HashMap::new();
    let mock = settings.scratch.join("mock").join(&run_id);
    while !queue.is_empty() {
        let request = queue.remove(0);
        let outcome = if settings.mock {
            pretend(&request, &mock).map_err(|error| failure(error.to_string()))?
        } else {
            execute(&runner, &request, &mut pinned)?
        };
        queue.extend(run.complete(outcome, &mut console)?);
    }
    run.finish(&mut console)?;
    Ok(())
}

/// Executes or reuses one task and turns its stored result into the outcome the runtime reads.
fn execute(
    runner: &Runner<LocalContainer>,
    request: &TaskRequest,
    pinned: &mut HashMap<String, Option<String>>,
) -> Result<TaskOutcome, Failure> {
    let problem = |error: &dyn std::fmt::Display| failure(format!("{}: {error}", request.name));
    let image = match container(request) {
        Some(image) => {
            if !pinned.contains_key(&image) {
                let digest = runner
                    .executor()
                    .pin(&image)
                    .map_err(|error| problem(&error))?;
                pinned.insert(image.clone(), digest);
            }
            Some(pinned[&image].clone().unwrap_or(image))
        }
        None => None,
    };
    let script = runner
        .import_bytes(command_script(request).as_bytes())
        .map_err(|error| problem(&error))?;
    let mut inputs = Vec::new();
    for (source, _) in &request.staged {
        if source.prefix.is_some() {
            return Err(Failure {
                message: format!(
                    "{}: remote input files are not supported yet: {}",
                    request.name,
                    source.uri()
                ),
                unsupported: true,
            });
        }
        let (digest, _) = runner
            .import(Path::new(&source.path))
            .map_err(|error| problem(&error))?;
        inputs.push(digest);
    }
    let model = task_request(request, image, script, &inputs);
    let result = runner.execute(&model).map_err(|error| problem(&error))?;
    println!(
        "[{}] {}",
        if result.reused { "cached" } else { "done" },
        request.name
    );
    let mut outcome = TaskOutcome {
        id: request.id,
        exit_status: result.exit_code.unwrap_or(-1),
        ..TaskOutcome::default()
    };
    for file in &result.files {
        let text = |runner: &Runner<LocalContainer>| {
            runner
                .read(&file.location)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        match file.path.as_str() {
            STDOUT => outcome.stdout = text(runner).map_err(|error| problem(&error))?,
            ENVIRONMENT => {
                let variables = read_environment(&text(runner).map_err(|error| problem(&error))?);
                for (index, _) in request.eval.iter().enumerate() {
                    let (value, status) = variables
                        .get(&format!("nxf_out_eval_{}", index + 1))
                        .cloned()
                        .unwrap_or_default();
                    if status != 0 {
                        return Err(failure(format!(
                            "{}: an `eval` output command failed with exit status {status}: {value}",
                            request.name
                        )));
                    }
                    outcome.eval.push(value);
                }
                for name in &request.env {
                    let value = variables
                        .get(&**name)
                        .map(|(value, _)| value.clone())
                        .unwrap_or_default();
                    outcome.env.insert(name.to_string(), value);
                }
            }
            _ => {}
        }
        let location = runner
            .local_path(&file.location)
            .unwrap_or_else(|| PathBuf::from(&file.location));
        let path = PathValue::parse(&location.to_string_lossy(), "/");
        outcome.files.push((file.path.clone(), path));
    }
    Ok(outcome)
}

/// The mock executor: a task that does not run. Its output files are the names in its script
/// that the output patterns match, like those a stub `touch`es, else one empty file for each
/// pattern, which is an empty gzip file for a `.gz` name; its other outputs are empty text. A `versions.yml` gets the one entry that nf-core
/// code reads, as a module's stub writes it.
fn pretend(request: &TaskRequest, directory: &Path) -> std::io::Result<TaskOutcome> {
    let work = directory.join(request.id.0.to_string());
    std::fs::create_dir_all(&work)?;
    // The script and name of the task, to compare with a Nextflow run.
    std::fs::write(work.join(".command.sh"), &request.script)?;
    std::fs::write(work.join(".command.name"), &request.name)?;
    println!("[mock] {}", request.name);
    let mut outcome = TaskOutcome {
        id: request.id,
        eval: vec![String::new(); request.eval.len()],
        ..TaskOutcome::default()
    };
    for name in &request.env {
        outcome.env.insert(name.to_string(), String::new());
    }
    let staged: Vec<&str> = request
        .staged
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();
    let words = script_words(&request.script, &staged);
    // Each path to make, with whether it is a directory, and the outputs among them.
    let mut paths: Vec<(String, bool)> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    for pattern in &request.output_patterns {
        let literal = !pattern.trim_end_matches('/').contains(['*', '?', '[', '{']);
        let inside: Vec<&(String, bool)> = words
            .iter()
            .filter(|(word, _)| {
                literal && word.starts_with(&format!("{}/", pattern.trim_end_matches('/')))
            })
            .collect();
        let matched: Vec<&(String, bool)> = words
            .iter()
            .filter(|(word, _)| glob::matches(pattern, word) && has_literal(pattern))
            .collect();
        if literal && !inside.is_empty() {
            // Files in a directory output, like `touch db/a.txt` for `db`.
            paths.extend(inside.into_iter().cloned());
            let directory = pattern.trim_end_matches('/').to_owned();
            paths.push((directory.clone(), true));
            outputs.push(directory);
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
            // Made already, as a directory of a deeper path.
        } else if directory {
            std::fs::create_dir_all(&path)?;
        } else if name == "versions.yml" {
            std::fs::write(
                &path,
                format!("\"{}\":\n    mock: 1.0.0\n", request.process),
            )?;
        } else if name.ends_with(".gz") {
            std::fs::write(&path, EMPTY_GZIP)?;
        } else {
            std::fs::write(&path, "")?;
        }
    }
    for name in outputs {
        let path = PathValue::parse(&work.join(&name).to_string_lossy(), "/");
        if !outcome.files.iter().any(|(known, _)| *known == name) {
            outcome.files.push((name, path));
        }
    }
    Ok(outcome)
}

/// A gzip file of no data, for mock outputs whose name ends with `.gz`, so that they can be read.
const EMPTY_GZIP: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// The words of a script that may name files it makes, each with whether `mkdir` makes it:
/// no options, variables, globs or assignments, and no staged inputs.
fn script_words(script: &str, staged: &[&str]) -> Vec<(String, bool)> {
    let mut words: Vec<(String, bool)> = Vec::new();
    let mut after_mkdir = false;
    for word in script.split(|c: char| c.is_whitespace() || "\"';|&<>()`".contains(c)) {
        if word.is_empty() {
            continue;
        }
        if word == "mkdir" {
            after_mkdir = true;
            continue;
        }
        let skip = word.starts_with('-')
            || word.contains(['$', '=', '*', '?', '[', '{', '\\'])
            || staged.contains(&word);
        if !word.starts_with('-') {
            let mkdir = std::mem::take(&mut after_mkdir);
            if skip {
                continue;
            }
            let word = word.trim_start_matches("./").trim_end_matches('/');
            match words.iter_mut().find(|(known, _)| known == word) {
                Some((_, directory)) => *directory |= mkdir,
                None => words.push((word.to_owned(), mkdir)),
            }
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

/// A file name that a glob pattern matches: `*` becomes `mock`, `?` becomes `x`, and a group
/// like `{html,zip}` or `[ab]` its first choice.
fn file_name(pattern: &str) -> String {
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

/// A run id from the clock and the process id.
fn run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("{now:x}-{:x}", std::process::id())
}

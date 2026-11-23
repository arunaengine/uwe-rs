//! One run of a Galaxy tool as a task: its command rendered for staged files, its configfiles
//! and outputs, and the check of an attempt by the exit code rules of the tool.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::PathBuf;

use cheetah_evaluator::{DataFile, Job};
use galaxy_parser::tool::Tool;
use serde_json::{Map, Value as Json};
use workflow_graph::{DriverError, Input, Outcome, Task, TaskId};

use crate::job::Dataset;

/// The working folder of a job in its container, where relative output paths start.
const WORKDIR: &str = "/galaxy/job";
const INPUTS: &str = "/galaxy/inputs";
/// The folder of the tool, `$__tool_directory__`, for its scripts.
const TOOL: &str = "/galaxy/tool";
const CONFIGS: &str = "/galaxy/configs";
const SCRIPT: &str = ".galaxy/command.sh";

/// A tool file read with its macros, with the folder of its scripts.
#[derive(Clone, Debug)]
pub(crate) struct Loaded {
    pub tool: Tool,
    pub path: PathBuf,
}

/// A task of a tool run, with the outputs it makes.
pub(crate) struct Planned {
    pub task: Task,
    pub made: Vec<Made>,
}

/// An output that a task makes: its name, its path in the working folder and its datatype.
#[derive(Clone, Debug)]
pub(crate) struct Made {
    pub name: String,
    pub path: String,
    pub ext: String,
}

/// The task that runs `loaded` with the parameters and files of a job, in `image`.
pub(crate) fn build(
    id: TaskId,
    (name, step): (&str, &str),
    loaded: &Loaded,
    (parameters, datasets): (&Map<String, Json>, &BTreeMap<String, Vec<Dataset>>),
    image: &str,
) -> Result<Planned, DriverError> {
    let tool = &loaded.tool;
    let mut inputs = Vec::new();
    let mut job = Job {
        parameters: Json::Object(parameters.clone()),
        tool_directory: TOOL.to_owned(),
        ..Job::default()
    };
    let mut number = 0;
    for (path, files) in datasets {
        let mut staged = Vec::new();
        for file in files {
            let at = format!("{INPUTS}/{}", staged_name(number, &file.ext));
            number += 1;
            inputs.push(Input {
                path: at.clone(),
                source: file.location.clone(),
                ..Input::default()
            });
            staged.push(DataFile {
                path: at,
                ext: file.ext.clone(),
                name: file.name.clone(),
            });
        }
        job.datasets.insert(path.clone(), staged);
    }
    let mut made = Vec::new();
    for output in &tool.outputs {
        // A format that follows an input takes the datatype of its first file.
        let source = (output.format_source.as_ref())
            .and_then(|input| datasets.get(input).or_else(|| suffixed(datasets, input)))
            .and_then(|files| files.first())
            .map(|file| file.ext.clone());
        let ext = (source.or_else(|| output.format.clone())).unwrap_or_else(|| "data".to_owned());
        let path = match &output.from_work_dir {
            Some(path) => path.clone(),
            None => format!("outputs/{}.{ext}", output.name),
        };
        job.outputs
            .insert(output.name.clone(), format!("{WORKDIR}/{path}"));
        made.push(Made {
            name: output.name.clone(),
            path,
            ext,
        });
    }
    for configfile in &tool.configfiles {
        let path = match &configfile.filename {
            Some(file) => format!("{WORKDIR}/{file}"),
            None => format!("{CONFIGS}/{}", configfile.name),
        };
        job.configfiles.insert(configfile.name.clone(), path);
    }
    let rendered = cheetah_evaluator::render(tool, &job)
        .map_err(|error| DriverError::new(format!("{}: {error}", loaded.path.display())))?;
    made.retain(|output| rendered.created.contains(&output.name));
    if let Some(output) = (tool.outputs.iter())
        .find(|output| output.kind == "collection" && rendered.created.contains(&output.name))
    {
        return Err(DriverError::unsupported(format!(
            "tool {}: the output collection {} is not supported yet",
            tool.id, output.name
        )));
    }
    for (name, text) in &rendered.configfiles {
        inputs.push(Input {
            path: job.configfiles.get(name).cloned().unwrap_or_default(),
            content: Some(text.clone()),
            ..Input::default()
        });
    }
    let command = rendered.command.unwrap_or_default();
    let script = format!("mkdir -p outputs\n{command}\n");
    inputs.push(Input {
        path: SCRIPT.to_owned(),
        content: Some(script.clone()),
        ..Input::default()
    });
    let templates = std::iter::once(tool.command.as_ref().map(|c| c.text.as_str())).chain(
        tool.configfiles
            .iter()
            .map(|c| Some(c.template.text.as_str())),
    );
    if templates
        .flatten()
        .any(|text| text.contains("__tool_directory__"))
    {
        let folder = loaded.path.parent().map(PathBuf::from).unwrap_or_default();
        inputs.push(Input {
            path: TOOL.to_owned(),
            source: std::path::absolute(&folder)
                .unwrap_or(folder)
                .to_string_lossy()
                .into_owned(),
            directory: true,
            ..Input::default()
        });
    }
    let task = Task {
        id,
        name: name.to_owned(),
        step: step.to_owned(),
        image: Some(image.to_owned()),
        command: vec!["bash".to_owned(), SCRIPT.to_owned()],
        script: Some(script),
        workdir: Some(WORKDIR.to_owned()),
        inputs,
        outputs: made.iter().map(|output| output.path.clone()).collect(),
        ..Task::default()
    };
    Ok(Planned { task, made })
}

/// The name of a staged input file, like Galaxy's `dataset_<n>.dat`: its number and datatype,
/// so that no character of its original name, which stays its `name`, reaches a command.
fn staged_name(number: usize, ext: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.');
    let ext = ext.trim_start_matches('.');
    match !ext.is_empty() && ext.chars().all(safe) {
        true => format!("dataset_{number}.{ext}"),
        false => format!("dataset_{number}.dat"),
    }
}

/// The files of an input named by its last part, as `format_source` may name an input
/// inside a section or conditional.
fn suffixed<'a>(
    datasets: &'a BTreeMap<String, Vec<Dataset>>,
    name: &str,
) -> Option<&'a Vec<Dataset>> {
    let leaf = format!("|{name}");
    (datasets.iter())
        .find(|(path, _)| path.ends_with(&leaf))
        .map(|(_, files)| files)
}

/// Why an attempt failed, by the `<stdio>` exit code rules of the tool: without rules, any
/// exit code but 0 fails, as Galaxy's `detect_errors="exit_code"` does. Then by its regex rules.
pub(crate) fn refused(tool: &Tool, outcome: &Outcome) -> Option<String> {
    if outcome.error.is_some() {
        return Some(format!("tool {}: the task could not run", tool.id));
    }
    let code = outcome.exit_code;
    let rules = &tool.stdio.exit_codes;
    let fatal = match rules.is_empty() {
        true => code != 0,
        false => rules.iter().any(|rule| {
            let value = |key: &str| {
                (rule.iter())
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.as_str())
            };
            let level = value("level").unwrap_or("fatal");
            level.starts_with("fatal") && value("range").is_some_and(|range| within(range, code))
        }),
    };
    match fatal {
        true => Some(format!("tool {} failed with exit code {code}", tool.id)),
        false => matched(tool, outcome),
    }
}

/// Why an attempt failed by the `<stdio>` regex rules of the tool: a `fatal` rule whose pattern
/// Galaxy finds, ignoring case, in its stream. A level or pattern uwe cannot check fails too.
fn matched(tool: &Tool, outcome: &Outcome) -> Option<String> {
    for rule in &tool.stdio.regexes {
        let value = |key: &str| {
            (rule.iter())
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.to_ascii_lowercase())
        };
        let fatal = match value("level").as_deref().unwrap_or("fatal") {
            "fatal" | "fatal_oom" => true,
            "log" | "qc" | "warning" => false,
            other => {
                return Some(format!(
                    "tool {}: the regex level {other} is not supported",
                    tool.id
                ));
            }
        };
        let pattern =
            (rule.iter().find(|(name, _)| name == "match")).map_or("", |(_, value)| value);
        let regex = match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(regex) => regex,
            Err(error) => {
                return Some(format!(
                    "tool {}: the regex {pattern} is not supported: {error}",
                    tool.id
                ));
            }
        };
        let streams = match value("source").as_deref().unwrap_or("both") {
            "stdout" => [Some(&outcome.stdout), None],
            "stderr" => [None, Some(&outcome.stderr)],
            "both" => [Some(&outcome.stdout), Some(&outcome.stderr)],
            other => {
                return Some(format!(
                    "tool {}: the regex source {other} is not supported",
                    tool.id
                ));
            }
        };
        if fatal
            && streams
                .into_iter()
                .flatten()
                .any(|text| regex.is_match(text))
        {
            let description = (rule.iter())
                .find(|(name, _)| name == "description")
                .map_or(pattern, |(_, value)| value);
            return Some(format!("tool {} failed: {description}", tool.id));
        }
    }
    None
}

/// Whether `code` is in a Galaxy range like `1:`, `:-1`, `3:5` or `2`.
fn within(range: &str, code: i32) -> bool {
    let bound = |text: &str, open: i64| match text.trim() {
        "" => Some(open),
        text => text.parse::<i64>().ok(),
    };
    let code = i64::from(code);
    match range.split_once(':') {
        Some((low, high)) => match (bound(low, i64::MIN), bound(high, i64::MAX)) {
            (Some(low), Some(high)) => low <= code && code <= high,
            _ => false,
        },
        None => bound(range, i64::MIN).is_some_and(|only| only == code),
    }
}

/// The files of the outputs that an accepted attempt made, by output name.
pub(crate) fn collected(
    tool: &Tool,
    made: &[Made],
    outcome: &Outcome,
) -> Result<BTreeMap<String, Vec<Dataset>>, String> {
    let mut outputs = BTreeMap::new();
    for output in made {
        let path = output.path.trim_start_matches("./");
        let Some(file) =
            (outcome.files.iter()).find(|file| file.path.trim_start_matches("./") == path)
        else {
            return Err(format!(
                "tool {}: the job made no file for the output {}",
                tool.id, output.name
            ));
        };
        let dataset = Dataset {
            location: file.location.clone(),
            ext: output.ext.clone(),
            name: output.name.clone(),
        };
        outputs.insert(output.name.clone(), vec![dataset]);
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_safe_names() {
        assert_eq!(staged_name(0, "fastqsanger.gz"), "dataset_0.fastqsanger.gz");
        assert_eq!(staged_name(3, "it's;rm"), "dataset_3.dat");
        assert_eq!(staged_name(4, ""), "dataset_4.dat");
    }

    #[test]
    fn checks_stdio_regexes() {
        let tool = |rules: &str| {
            let xml = format!("<tool id=\"t\"><command>x</command><stdio>{rules}</stdio></tool>");
            galaxy_parser::tool::parse(&xml, "t.xml", &mut |_| unreachable!("no imports"))
                .expect("a tool")
        };
        let outcome = Outcome {
            stderr: "Error: Out of Memory".to_owned(),
            ..Outcome::default()
        };
        let fatal =
            tool(r#"<regex match="out of memory" source="stderr" description="no memory"/>"#);
        assert_eq!(
            refused(&fatal, &outcome).as_deref(),
            Some("tool t failed: no memory")
        );
        let warning = tool(r#"<regex match="memory" level="warning"/>"#);
        assert_eq!(refused(&warning, &outcome), None);
        let other = tool(r#"<regex match="memory" source="stdout"/>"#);
        assert_eq!(refused(&other, &outcome), None);
        let unknown = tool(r#"<regex match="memory" level="loud"/>"#);
        assert!(refused(&unknown, &outcome).is_some_and(|why| why.contains("not supported")));
        let lookahead = tool(r#"<regex match="(?=memory)"/>"#);
        assert!(refused(&lookahead, &outcome).is_some_and(|why| why.contains("not supported")));
    }

    #[test]
    fn galaxy_ranges() {
        assert!(within("1:", 3) && !within("1:", 0));
        assert!(within(":-1", -2) && !within(":-1", 0));
        assert!(within("3:5", 4) && !within("3:5", 6));
        assert!(within("2", 2) && !within("2", 1));
    }
}

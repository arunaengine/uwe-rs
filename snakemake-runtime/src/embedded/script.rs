//! Scripts and wrappers: the script with its preamble in front, as Snakemake's `script()`
//! writes it to `.snakemake/scripts`, the command that runs it, and the files it needs besides
//! the job's inputs: the stand-in of the snakemake package for Python, and the folder of the
//! script, whose other files scripts import or source.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::Input;

use super::{Context, SCRIPTS_FOLDER, Setting, bash, julia, literal, python, r, rust};

/// The languages of scripts, by the end of their file name, as Snakemake's `get_language`
/// tells them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Python,
    R,
    RMarkdown,
    Julia,
    Rust,
    Bash,
    Xonsh,
    Hy,
}

impl Language {
    pub fn of(file_name: &str) -> Option<Language> {
        let endings = [
            (".py", Language::Python),
            (".R", Language::R),
            (".Rmd", Language::RMarkdown),
            (".jl", Language::Julia),
            (".rs", Language::Rust),
            (".sh", Language::Bash),
            (".xsh", Language::Xonsh),
            (".hy", Language::Hy),
        ];
        endings
            .iter()
            .find(|(ending, _)| file_name.ends_with(ending))
            .map(|(_, language)| *language)
    }

    /// Whether the script's code is Python, which imports the stand-in of the snakemake
    /// package.
    pub fn is_python(self) -> bool {
        matches!(self, Language::Python | Language::Xonsh | Language::Hy)
    }

    /// The endings of the files that scripts of this language load from their own folder.
    fn endings(self) -> &'static [&'static str] {
        match self {
            Language::Python | Language::Xonsh | Language::Hy => &[".py", ".xsh", ".hy"],
            Language::R | Language::RMarkdown => &[".R", ".r", ".Rmd"],
            Language::Julia => &[".jl"],
            Language::Rust => &[".rs"],
            Language::Bash => &[".sh", ".bash"],
        }
    }
}

/// The error of a script that is none of the languages Snakemake runs.
pub const UNKNOWN_LANGUAGE: &str =
    "Script must be one of the following filetypes: [.py .R .Rmd .jl .rs .sh .xsh .hy]";

/// A script of a job: where it is and its text.
pub struct Source {
    /// The absolute path of the script on this machine, or its web address.
    pub location: String,
    /// Its text, `None` when it was not read: a file on the web in a simulated run.
    pub text: Option<String>,
}

impl Source {
    pub fn is_local(&self) -> bool {
        !self.location.contains("://")
    }

    pub fn file_name(&self) -> &str {
        self.location
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&self.location)
    }

    /// The folder of the script, or the web address of its folder, as `snakemake.scriptdir`.
    pub fn folder(&self) -> String {
        match self.location.trim_end_matches('/').rsplit_once('/') {
            Some((folder, _)) if !folder.is_empty() => folder.to_owned(),
            _ => "/".to_owned(),
        }
    }
}

/// What a script adds to its task.
pub struct Parts {
    pub command: Vec<String>,
    pub inputs: Vec<Input>,
}

/// The task parts of a script. `workdir` is the working directory of the workflow, whose
/// files are not all given to the task, only those that the job names.
pub fn build(
    mut context: Context,
    source: &Source,
    setting: &Setting,
    workdir: &Path,
) -> Result<Parts, String> {
    let name = source.file_name().to_owned();
    let language = Language::of(&name).ok_or_else(|| UNKNOWN_LANGUAGE.to_owned())?;
    context.scriptdir = source.folder();
    let text = source.text.clone().unwrap_or_else(|| {
        format!(
            "# iunctus reads {} when the job runs for real\n",
            source.location
        )
    });
    let path = format!("{SCRIPTS_FOLDER}/{name}");
    let (combined, command) = combined(language, &context, source, &text, &path)?;
    let mut inputs = vec![literal(&path, &combined)];
    if language.is_python() {
        inputs.push(setting.package()?);
    }
    if source.is_local() {
        inputs.extend(folder_inputs(
            Path::new(&source.location),
            language,
            workdir,
        ));
    }
    Ok(Parts { command, inputs })
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// The script with its preamble and the command that runs it, as each of Snakemake's script
/// classes writes and runs it.
fn combined(
    language: Language,
    context: &Context,
    source: &Source,
    text: &str,
    path: &str,
) -> Result<(String, Vec<String>), String> {
    Ok(match language {
        Language::Python | Language::Xonsh | Language::Hy => {
            let file = if source.is_local() {
                std::fs::canonicalize(&source.location)
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| source.location.clone())
            } else {
                source.location.clone()
            };
            let searchpaths = if source.is_local() {
                vec![context.scriptdir.clone()]
            } else {
                Vec::new()
            };
            let preamble = python::preamble(context, &searchpaths, &file);
            match language {
                Language::Python => (format!("{preamble}{text}"), strings(&["python", path])),
                Language::Xonsh => (
                    format!("{preamble}{text}"),
                    strings(&[
                        "xonsh",
                        "-DRAISE_SUBPROC_ERROR=true",
                        "-DXONSH_SHOW_TRACEBACK=true",
                        path,
                    ]),
                ),
                _ => (
                    format!("(pys #[[{preamble}]]){text}"),
                    strings(&["hy", path]),
                ),
            }
        }
        Language::R => (
            format!("{}{text}", r::object(context, Some(""))?),
            strings(&["Rscript", "--vanilla", path]),
        ),
        Language::RMarkdown => {
            let [output] = context.output.values.as_slice() else {
                return Err("RMarkdown scripts (.Rmd) may only have a single output file.".into());
            };
            // Snakemake gives the absolute paths of the output and the working directory,
            // which R finds out in the container. rmarkdown 2.30 writes next to the document
            // unless `output_dir` names the folder of the output too.
            let output = output.text();
            let output = if Path::new(&output).is_absolute() {
                format!("\"{output}\"")
            } else {
                format!("file.path(getwd(), \"{output}\")")
            };
            let expression = format!(
                "rmarkdown::render(\"{path}\", output_file={output}, \
                 output_dir=dirname({output}), quiet=TRUE, knit_root_dir = getwd(), \
                 params = list(rmd=\"{path}\"))"
            );
            (
                r::markdown(text, &r::object(context, None)?)?,
                vec![
                    "Rscript".into(),
                    "--vanilla".into(),
                    "-e".into(),
                    expression,
                ],
            )
        }
        Language::Julia => (
            format!("{}{text}", julia::preamble(context)?),
            strings(&["julia", path]),
        ),
        Language::Rust => {
            let mut command = vec!["rust-script".to_owned()];
            for dependency in rust::DEPENDENCIES {
                command.push("-d".into());
                command.push((*dependency).to_owned());
            }
            command.extend(strings(&[path, "--", "--features", rust::FEATURES]));
            (rust::combine(&rust::preamble(context)?, text), command)
        }
        Language::Bash => (
            bash::combine(&bash::preamble(context), text),
            strings(&["bash", path]),
        ),
    })
}

/// The files of the script's folder, which the task sees at their paths on this machine, as
/// Snakemake's scripts see them: the folder itself, or, when it holds the working directory,
/// only its files of the script's language.
fn folder_inputs(script: &Path, language: Language, workdir: &Path) -> Vec<Input> {
    let Some(folder) = script.parent() else {
        return Vec::new();
    };
    let absolute = |path: &Path| path.to_string_lossy().into_owned();
    if !workdir.starts_with(folder) {
        return vec![Input {
            path: absolute(folder),
            source: absolute(folder),
            directory: true,
            content: None,
            ..Default::default()
        }];
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .filter(|path| {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    language
                        .endings()
                        .iter()
                        .any(|ending| name.ends_with(ending))
                })
                .collect()
        })
        .unwrap_or_default();
    if !files.iter().any(|file| file == script) && script.is_file() {
        files.push(script.to_path_buf());
    }
    files.sort();
    files
        .iter()
        .map(|file| Input {
            path: absolute(file),
            source: absolute(file),
            directory: false,
            content: None,
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Language, Source};

    #[test]
    fn detects_script_languages() {
        assert_eq!(Language::of("plot.py"), Some(Language::Python));
        assert_eq!(Language::of("plot.R"), Some(Language::R));
        assert_eq!(Language::of("report.Rmd"), Some(Language::RMarkdown));
        // Snakemake knows `.R` only.
        assert_eq!(Language::of("plot.r"), None);
    }

    #[test]
    fn resolves_web_scripts() {
        let source = Source {
            location: "https://example.org/wrappers/bio/x/wrapper.py".into(),
            text: None,
        };
        assert!(!source.is_local());
        assert_eq!(source.file_name(), "wrapper.py");
        assert_eq!(source.folder(), "https://example.org/wrappers/bio/x");
    }
}

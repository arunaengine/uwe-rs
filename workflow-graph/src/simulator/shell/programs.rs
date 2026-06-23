//! The programs that the simulator follows in a script, like `touch`, `echo`,
//! `cat`, `cp` and `gzip`; any other program makes the simulation incomplete.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::words::{escapes, normal, printf};
use super::{Made, Output, Shell};

impl Shell<'_> {
    /// Runs a program with its arguments; `None` stands for an argument the simulation does
    /// not know.
    pub(super) fn program(&mut self, arguments: &[Option<String>], input: Output) -> Output {
        let Some(Some(name)) = arguments.first() else {
            self.complete = false;
            return Output::Unknown;
        };
        let (flags, operands): (Vec<&Option<String>>, Vec<&Option<String>>) = arguments[1..]
            .iter()
            .partition(|argument| argument.as_deref().is_some_and(|a| a.starts_with('-')));
        let flag = |wanted: char| {
            flags.iter().any(|flag| {
                flag.as_deref()
                    .is_some_and(|flag| !flag.starts_with("--") && flag.contains(wanted))
            })
        };
        let known: Option<Vec<String>> =
            operands.iter().map(|operand| (*operand).clone()).collect();
        match name.as_str() {
            "true" | ":" | "set" | "sleep" | "wait" | "exit" | "export" | "unset" | "shopt" => {
                Output::Text(String::new())
            }
            "echo" => {
                let text: Vec<String> = arguments[1..]
                    .iter()
                    .filter(|argument| !matches!(argument.as_deref(), Some("-n" | "-e" | "-ne")))
                    .map(|argument| argument.clone().unwrap_or_default())
                    .collect();
                let mut text = text.join(" ");
                if flag('e') {
                    text = escapes(&text);
                }
                if !flag('n') {
                    text.push('\n');
                }
                Output::Text(text)
            }
            "printf" => match arguments.get(1) {
                Some(Some(format)) => {
                    let values: Vec<String> = arguments[2..]
                        .iter()
                        .map(|argument| argument.clone().unwrap_or_default())
                        .collect();
                    Output::Text(printf(format, &values))
                }
                _ => Output::Unknown,
            },
            "cat" => match known {
                Some(files) if files.is_empty() => input,
                Some(files) => self.contents(&files),
                None => Output::Unknown,
            },
            "touch" => {
                self.each_operand(&known, |shell, path| {
                    if shell.find(path).is_none() {
                        shell.make_file(path, Some(String::new()), false, false);
                    }
                });
                Output::Text(String::new())
            }
            "mkdir" => {
                self.each_operand(&known, |shell, path| shell.make_directory(path));
                Output::Text(String::new())
            }
            "rm" => {
                self.each_operand(&known, |shell, path| {
                    let path = normal(path);
                    shell.made.retain(|made| {
                        made.path != path && !made.path.starts_with(&format!("{path}/"))
                    });
                });
                Output::Text(String::new())
            }
            "cp" | "mv" | "ln" => {
                match known.as_deref() {
                    Some([sources @ .., target]) if !sources.is_empty() => {
                        for source in sources {
                            self.copy(source, target, name == "mv");
                        }
                    }
                    _ => self.complete = false,
                }
                Output::Text(String::new())
            }
            "gzip" | "bgzip" | "pigz" => self.gzip(arguments, input, flag('c'), flag('k')),
            "tar" | "zip" => {
                // An archive that the command writes, by `-f file` or as the first operand of
                // `zip`; the listing of `tar -t` is not known.
                let file = match name.as_str() {
                    "tar" => arguments.iter().enumerate().find_map(|(index, argument)| {
                        let argument = argument.as_deref()?;
                        let creates = argument.contains('c') && argument.ends_with('f');
                        creates
                            .then(|| arguments.get(index + 1).cloned().flatten())
                            .flatten()
                    }),
                    _ => known
                        .as_ref()
                        .and_then(|operands| operands.first().cloned()),
                };
                match file {
                    Some(file) => self.make_file(&file, None, false, false),
                    None if flag('t') || flag('x') => self.complete &= !flag('x'),
                    None => self.complete = false,
                }
                Output::Unknown
            }
            // Filters, which only read and print.
            "grep" | "sed" | "sort" | "uniq" | "head" | "tail" | "cut" | "tr" | "wc" | "zcat"
            | "basename" | "dirname" | "date" | "ls" | "find" | "awk" | "paste" | "xargs" => {
                Output::Unknown
            }
            // A tool, which may make files that its arguments name.
            _ => {
                self.complete = false;
                self.words.extend(arguments[1..].iter().flatten().cloned());
                Output::Unknown
            }
        }
    }

    /// Calls `f` with each known operand; an unknown one makes the simulation incomplete.
    fn each_operand(&mut self, operands: &Option<Vec<String>>, mut f: impl FnMut(&mut Self, &str)) {
        match operands {
            Some(operands) => operands.iter().for_each(|operand| f(self, operand)),
            None => self.complete = false,
        }
    }

    /// `gzip file`, `gzip -c file`, and `gzip` in a pipe.
    fn gzip(
        &mut self,
        arguments: &[Option<String>],
        input: Output,
        stdout: bool,
        keep: bool,
    ) -> Output {
        let mut files = Vec::new();
        let mut skip_next = false;
        for argument in &arguments[1..] {
            match argument.as_deref() {
                _ if skip_next => skip_next = false,
                // Options with a value, like `bgzip -@ 4`.
                Some("-@" | "--threads" | "-p" | "-l" | "--compress-level") => skip_next = true,
                Some(flag) if flag.starts_with('-') => {}
                Some(file) => files.push(file.to_owned()),
                None => {
                    self.complete = false;
                    return Output::Unknown;
                }
            }
        }
        let decompress = arguments[1..]
            .iter()
            .any(|argument| matches!(argument.as_deref(), Some("-d" | "--decompress")));
        if decompress {
            self.complete = false;
            return Output::Unknown;
        }
        if files.is_empty() {
            return match input {
                Output::Text(text) => Output::Gzip(text),
                other => other,
            };
        }
        if stdout {
            return match self.contents(&files) {
                Output::Text(text) => Output::Gzip(text),
                other => other,
            };
        }
        let force = arguments[1..]
            .iter()
            .any(|argument| matches!(argument.as_deref(), Some("-f" | "--force")));
        for file in files {
            // As gzip, which does not replace a file without `-f`, like an input.
            let target = format!("{file}.gz");
            let exists =
                self.find(&target).is_some() || self.inputs.contains(&normal(&target).as_str());
            if exists && !force {
                self.fail(
                    2,
                    format!("gzip: {target} already exists;\tnot overwritten"),
                );
                return Output::Unknown;
            }
            let content = self.find(&file).and_then(|made| made.content.clone());
            self.make_file(
                &format!("{file}.gz"),
                content.or(Some(String::new())),
                true,
                false,
            );
            if !keep {
                let path = normal(&file);
                self.made.retain(|made| made.path != path);
            }
        }
        Output::Text(String::new())
    }

    /// `cp`, `mv` or `ln` of `source` to `target`, which may be a directory.
    fn copy(&mut self, source: &str, target: &str, remove: bool) {
        let source = normal(source);
        let target = normal(target);
        let into_directory =
            target.is_empty() || self.find(&target).is_some_and(|made| made.directory);
        let target = if into_directory {
            let name = source.rsplit('/').next().unwrap_or(&source);
            normal(&format!("{target}/{name}"))
        } else {
            target
        };
        let copied = self.find(&source).cloned();
        match copied {
            Some(made) => {
                self.made.retain(|other| other.path != target);
                if remove {
                    self.made.retain(|other| other.path != source);
                }
                self.made.push(Made {
                    path: target,
                    ..made
                });
            }
            None => {
                let input = self
                    .inputs
                    .contains(&source.as_str())
                    .then(|| source.clone());
                self.made.retain(|other| other.path != target);
                self.made.push(Made {
                    path: target,
                    directory: false,
                    content: None,
                    gzip: false,
                    copy_of: input,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::simulate;

    fn files(simulation: &super::super::Simulation) -> Vec<(&str, Option<&str>)> {
        simulation
            .made
            .iter()
            .filter(|made| !made.directory)
            .map(|made| (made.path.as_str(), made.content.as_deref()))
            .collect()
    }

    #[test]
    fn copies_moves_and_compressions() {
        let script = "echo x > a.txt\ncp a.txt b.txt\nmv b.txt c.txt\ngzip a.txt\ncp in.fa copy.fa";
        let simulation = simulate(script, &["in.fa"]);
        assert!(simulation.complete);
        assert_eq!(
            files(&simulation),
            [
                ("c.txt", Some("x\n")),
                ("a.txt.gz", Some("x\n")),
                ("copy.fa", None)
            ]
        );
        let copy = simulation.made.iter().find(|made| made.path == "copy.fa");
        assert_eq!(copy.and_then(|made| made.copy_of.as_deref()), Some("in.fa"));
        // Heredocs of versions keep their text; commands in them give nothing.
        let versions = simulate(
            "cat <<-END_VERSIONS > versions.yml\n\"P\":\n    tool: $(tool --version)\nEND_VERSIONS",
            &[],
        );
        assert_eq!(
            files(&versions),
            [("versions.yml", Some("\"P\":\n    tool: \n"))]
        );
    }
}

//! nf-core-utils against the plugin 0.5.0 and 0.7.0 with Nextflow 26.04.6: the probe scripts
//! of `fixtures/nf_core_utils`, whose `record.sh` names the recorded runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod servers;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_runtime::{Loader, Options, Run};

use crate::servers::{Recorder, http, masked, smtp};

/// The settings of the probe runs, `configs/common.config` of the recording.
const COMMON: &str = "manifest {\n    name = 'nf-core/probe'\n    version = '1.0.0'\n    \
    doi = 'https://doi.org/10.5281/zenodo.1, 10.5281/zenodo.2'\n}\nparams {\n    \
    outdir = 'results'\n    genome = 'GRCh38'\n    genomes = [GRCh38: [fasta: \
    '${params.igenomes_base}/Homo_sapiens/genome.fa', gtf: 'genes.gtf']]\n    email = null\n    \
    hook_url = null\n    max_multiqc_email_size = '25.MB'\n    monochrome_logs = true\n}\n";

/// A console that keeps each print, so that each log message stays one piece.
struct Console {
    prints: Vec<String>,
    launch: String,
    environment: HashMap<String, String>,
}

impl Host for Console {
    fn print(&mut self, text: &str) {
        self.prints.push(text.to_owned());
    }

    fn launch_dir(&self) -> String {
        self.launch.clone()
    }

    fn environment(&self, name: &str) -> Option<String> {
        self.environment.get(name).cloned()
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_core_utils")
}

fn expected(name: &str) -> String {
    let text = std::fs::read_to_string(fixtures().join("expected").join(name)).expect("reads");
    text.replace("<probe>", &fixtures().to_string_lossy())
}

/// Runs a script of the fixtures, or of `project`, with the probe settings, a pinned release
/// and `extra` settings, launched in `launch`.
fn run(
    script: &Path,
    release: &str,
    extra: &str,
    launch: &Path,
    environment: &[(&str, &str)],
) -> (Vec<String>, Result<(), Error>) {
    let project = script
        .parent()
        .expect("a folder")
        .to_string_lossy()
        .into_owned();
    let mut console = Console {
        prints: Vec::new(),
        launch: launch.to_string_lossy().into_owned(),
        environment: (environment.iter())
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
    };
    let file = launch.join("probe.config");
    let plugins = format!("plugins {{\n    id 'nf-core-utils@{release}'\n}}\n");
    std::fs::write(&file, format!("{plugins}{COMMON}{extra}")).expect("writes");
    let settings = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: project.clone(),
        launch_dir: console.launch.clone(),
        work_dir: launch.join("work").to_string_lossy().into_owned(),
    };
    let config = nextflow_config::load(&[file], &settings, &mut console).expect("loads");
    let name = script
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
    let options = Options {
        config,
        project_dir: project.clone(),
        script_file: script.to_string_lossy().into_owned(),
        run_name: "tiny_watson".into(),
        config_files: vec![format!("v{release}.config"), "fake.config".into()],
        command_line: format!(
            "nextflow run {name} -c v{release}.config -c fake.config -ansi-log false"
        ),
        ..Options::default()
    };
    let module = Loader::default().load(script).expect("loads");
    let result = Run::start(&module, &options, &mut console).and_then(|(mut run, requests)| {
        assert!(requests.is_empty(), "the probes run no tasks");
        run.finish(&mut console)
    });
    (console.prints, result)
}

/// A transcript by probe: the lines of each, and apart from them the warnings, each on one line
/// as Nextflow's console shows them.
#[derive(Debug, Default, PartialEq)]
struct Transcript {
    sections: Vec<(String, Vec<String>)>,
    warnings: Vec<String>,
}

impl Transcript {
    /// A recorded console: run lines of Nextflow left out, and a probe result that follows a
    /// log line on the same line split from it.
    fn recorded(text: &str) -> Transcript {
        let mut transcript = Transcript::default();
        for line in text.lines() {
            let skipped = [
                "\u{1b}[33mNextflow",
                "Downloading plugin",
                "[PIPELINE]",
                "[WORKDIR]",
            ];
            if skipped.iter().any(|start| line.starts_with(start)) || line.starts_with("[SUCCESS]")
            {
                continue;
            }
            match line.strip_prefix("[WARN] WARN: ") {
                Some(warning) => transcript.warnings.push(flat(warning)),
                None => transcript.line(line),
            }
        }
        transcript.finish()
    }

    fn heard(prints: &[String]) -> Transcript {
        let mut transcript = Transcript::default();
        for print in prints {
            match print.strip_prefix("WARN: ") {
                // The console of the recording shows the dash as `?`.
                Some(warning) => transcript
                    .warnings
                    .push(flat(&warning.replace('\u{2014}', "?"))),
                None => print.lines().for_each(|line| transcript.line(line)),
            }
        }
        transcript.finish()
    }

    fn line(&mut self, line: &str) {
        if let Some(name) = line.strip_prefix("===== ") {
            self.sections.push((name.to_owned(), Vec::new()));
            return;
        }
        let Some((_, lines)) = self.sections.last_mut() else {
            return;
        };
        match line
            .strip_suffix("returned")
            .filter(|logged| !logged.is_empty())
        {
            Some(logged) => lines.extend([logged.to_owned(), "returned".to_owned()]),
            None => lines.push(line.to_owned()),
        }
    }

    fn finish(mut self) -> Transcript {
        for (_, lines) in &mut self.sections {
            while lines.last().is_some_and(|line| line.is_empty()) {
                lines.pop();
            }
        }
        self.warnings.sort();
        self
    }

    /// Sets the lines of a probe that uwe answers differently, for the reason given.
    fn answer(&mut self, probe: &str, lines: &[&str], _reason: &str) {
        let section = self.sections.iter_mut().find(|(name, _)| name == probe);
        let (_, found) = section.unwrap_or_else(|| panic!("the probe {probe}"));
        *found = lines.iter().map(|line| line.to_string()).collect();
    }

    fn replace_warning(&mut self, from: &str, to: Option<&str>) {
        let index = self.warnings.iter().position(|warning| warning == from);
        let index = index.unwrap_or_else(|| panic!("the warning {from}"));
        match to {
            Some(to) => self.warnings[index] = to.to_owned(),
            None => drop(self.warnings.remove(index)),
        }
        self.warnings.sort();
    }
}

fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn probes_release_five() {
    let launch = tempfile::tempdir().expect("a temporary directory");
    let mail = Recorder::start(smtp);
    let hooks = Recorder::start(http);
    let servers = format!(
        "mail {{\n    from = 'probe@example.org'\n    smtp.host = '127.0.0.1'\n    \
         smtp.port = {}\n}}\nparams.email = 'someone@example.org'\n\
         params.hook_url = 'http://127.0.0.1:{}'\n",
        mail.address.port(),
        hooks.address.port()
    );
    let script = fixtures().join("core.nf");
    let (prints, result) = run(&script, "0.5.0", &servers, launch.path(), &[]);
    let sessions = mail.finish();
    let posts = hooks.finish();
    result.expect("runs");
    let mut wanted = Transcript::recorded(&expected("core-0.5.0.txt"));
    let smtp = "0.5.0 piped the mail into the sendmail program; uwe sends it through SMTP";
    wanted.answer("completionEmail", &["returned"], smtp);
    wanted.answer("completionEmail plaintext", &["returned"], smtp);
    assert_eq!(Transcript::heard(&prints), wanted);
    assert_eq!(sessions.len(), 2, "the HTML mail and the plain text mail");
    assert_eq!(posts.len(), 3, "the Slack, Teams and failing hooks");
}

#[test]
fn probes_release_additions() {
    let launch = tempfile::tempdir().expect("a temporary directory");
    let script = fixtures().join("new07.nf");
    let extra = "params.email = 'someone@example.org'\n";
    let platform = [("TOWER_WORKFLOW_ID", "abc")];
    let (prints, result) = run(&script, "0.7.0", extra, launch.path(), &platform);
    result.expect("runs");
    assert_eq!(
        Transcript::heard(&prints),
        Transcript::recorded(&expected("new07.txt"))
    );
    let report = launch
        .path()
        .join("results/pipeline_info/pipeline_report.txt");
    let report = std::fs::read_to_string(report).expect("a report");
    let recorded = expected("report.txt").replace("gloomy_mandelbrot", "tiny_watson");
    let recorded = recorded.replace(" -profile standard", "");
    assert_eq!(masked(&report), masked(&recorded));
}

/// Runs a script of its own in a project folder, launched there.
fn own(release: &str, script: &str, environment: &[(&str, &str)]) -> Vec<String> {
    let project = tempfile::tempdir().expect("a temporary directory");
    let path = project.path().join("main.nf");
    std::fs::write(&path, script).expect("writes");
    let (prints, result) = run(&path, release, "", project.path(), environment);
    result.expect("runs");
    prints
}

#[test]
fn checks_pinned_functions() {
    let calls = "include { getReferencesFile; getReferencesValue; citationsOnTheFly; \
                 getReferencePath } from 'plugin/nf-core-utils'\n\
                 def root(e) { e.cause != null ? root(e.cause) : e }\n\
                 def probe(body) {\n    try { println(body.call()) }\n    \
                 catch (e) { println(\"${root(e).getClass().simpleName}: ${root(e).message}\") }\n}\n\
                 workflow {\n    def list = [[[id: 'a', fasta: '${params.igenomes_base}/a.fa'], 'x'], \
                 [[id: 'b'], 'y']]\n    \
                 probe { getReferencesFile(list, null, 'fasta', '/base') }\n    \
                 probe { getReferencesValue(list, 'given', 'fasta') }\n    \
                 probe { citationsOnTheFly([], []) }\n    probe { getReferencePath() }\n}\n";
    let old = own("0.5.0", calls, &[]).concat();
    assert_eq!(
        old,
        "[[[id:a], /base/a.fa], null]\n[[[id:a], given], [[id:b], given]]\n\
         AbortOperationException: `citationsOnTheFly` is not a function of nf-core-utils 0.5.0\n\
         AbortOperationException: `getReferencePath` is not a function of nf-core-utils 0.5.0\n"
    );
    let new = own("0.7.0", calls, &[]).concat();
    assert_eq!(
        new,
        "AbortOperationException: `getReferencesFile` is not a function of nf-core-utils 0.7.0\n\
         AbortOperationException: `getReferencesValue` is not a function of nf-core-utils 0.7.0\n\
         [:]\n\
         IllegalArgumentException: getReferencePath takes named arguments: references, genome, \
         igenomesIgnore, datasheetPath\n"
    );
}

#[test]
fn reads_conda_channels() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let bin = home.path().join("bin");
    std::fs::create_dir(&bin).expect("creates");
    std::fs::write(bin.join("conda"), "").expect("writes");
    let user = home.path().join(".condarc");
    let script = "include { checkCondaChannels } from 'plugin/nf-core-utils'\n\
                  workflow {\n    println(checkCondaChannels())\n}\n";
    let path = bin.to_string_lossy();
    let home_text = home.path().to_string_lossy();
    let environment = [("PATH", &*path), ("HOME", &*home_text)];
    std::fs::write(
        &user,
        "channels:\n  - conda-forge\n  - defaults\n  - bioconda\n",
    )
    .expect("writes");
    assert_eq!(own("0.7.0", script, &environment), ["true\n"]);
    std::fs::write(&user, "channels: [bioconda, conda-forge]\n").expect("writes");
    let prints = own("0.7.0", script, &environment);
    assert_eq!(prints.last().map(String::as_str), Some("false\n"));
    let warning = prints[0].clone();
    assert!(
        warning.contains("The observed channel order is\n    [bioconda, conda-forge]\n"),
        "{warning}"
    );
    std::fs::remove_file(&user).expect("removes");
    let prints = own("0.7.0", script, &environment);
    assert!(prints[0].contains("[defaults]"), "conda's default channel");
}

#[test]
fn skips_other_transports() {
    let script = "include { completionEmail } from 'plugin/nf-core-utils'\n\
                  workflow {\n    completionEmail([:], 'a@b.c', null, true, null, true, [])\n}\n";
    let prints = own("0.7.0", script, &[]);
    assert_eq!(prints.len(), 4, "{prints:?}");
    assert!(prints[0].starts_with("WARN: Email template not found: "));
    assert!(prints[1].ends_with("/assets/email_template.html\n"));
    assert_eq!(
        prints[2..],
        [
            "WARN: Cannot write email reports - output directory is null\n",
            "WARN: [nf-core/probe] Did not send the summary e-mail to a@b.c: uwe sends mail only \
             through SMTP, and the config sets no mail.smtp.host\n",
        ]
    );
}

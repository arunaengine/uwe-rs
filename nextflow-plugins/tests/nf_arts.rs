//! nf-arts against the plugin 1.0.0 with Nextflow 26.04.6: a real run of the probe gives a
//! picture with the recorded frame, colours and caption, a PNG when asked, and the config errors.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{
    Attempt, Engine, Executor, Files, Launch, LocalFiles, Outcome, OutputFile, Running,
};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Writes what the scripts of the probe write, one file per output, as a local executor would.
struct Probe(PathBuf);

impl Executor for Probe {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let folder = self.0.join(task.id.to_string());
        std::fs::create_dir_all(&folder).expect("creates");
        let input = (task.inputs.iter())
            .find(|input| input.content.is_none())
            .expect("an input file");
        let names: Vec<String> = match task.step.as_str() {
            "SPLIT" => (0..3).map(|place| format!("part_{place:02}.txt")).collect(),
            _ => vec![format!("{}.up.txt", input.path.trim_end_matches(".txt"))],
        };
        let files = (names.into_iter())
            .map(|name| {
                std::fs::write(folder.join(&name), "x\n").expect("writes");
                OutputFile {
                    location: folder.join(&name).to_string_lossy().into_owned(),
                    path: name,
                    ..OutputFile::default()
                }
            })
            .collect();
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }

    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(LocalFiles))
    }
}

/// Runs the probe with these `arts` settings; the error of the run, if it failed.
fn run(folder: &Path, arts: &str) -> Option<String> {
    let at = folder.to_string_lossy();
    let probe = fixtures().join("nf_fairscape");
    let script = std::fs::read_to_string(probe.join("main.nf")).expect("reads");
    let script = script.replace("publishDir '", &format!("publishDir '{at}/"));
    std::fs::write(folder.join("main.nf"), script).expect("writes");
    std::fs::copy(probe.join("input.txt"), folder.join("input.txt")).expect("copies");
    let config = format!(
        "plugins {{ id 'nf-arts@1.0.0' }}\nmanifest {{ name = 'probe-arts'; version = '0.0.1' }}\n\
         arts {{ {arts} }}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes");
    let launch = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Quiet),
        ..Settings::default()
    };
    let mut driver = match Nextflow::new(&launch, settings) {
        Ok(driver) => driver,
        Err(error) => return Some(error.message),
    };
    let execution = Engine::new(&mut Probe(folder.join("tasks"))).run("main.nf", &mut driver);
    execution.error.map(|error| error.to_string())
}

#[test]
fn draws_recorded_frame() {
    let launch = tempfile::tempdir().expect("a folder");
    let folder = launch.path();
    let file = folder.join("art.svg");
    let error = run(
        folder,
        &format!("width = 400; height = 400; file = '{}'", file.display()),
    );
    assert_eq!(error, None);
    let drawn = std::fs::read_to_string(&file).expect("a picture");
    let recorded = std::fs::read_to_string(fixtures().join("nf_arts/art-sleepy_fermat.svg"));
    let recorded = recorded.expect("reads");
    let head = |text: &str| text.lines().take(3).collect::<Vec<_>>().join("\n");
    assert_eq!(head(&drawn), head(&recorded));
    let caption = |text: &str| {
        let line = text
            .lines()
            .find(|line| line.contains("fill-opacity=\"0.90\""))
            .unwrap_or("");
        line.split('>').next().unwrap_or_default().to_owned()
    };
    assert_eq!(caption(&drawn), caption(&recorded));
    assert!(
        drawn.contains(">probe-arts \u{b7} 0.0.1 \u{b7} uwe</text>"),
        "{drawn}"
    );
    let colors = ["#5b2a86", "#fde725"].map(|color| drawn.contains(&format!("stroke=\"{color}\"")));
    assert_eq!(colors, [true, true], "{drawn}");
    assert!(drawn.ends_with("</g>\n</svg>\n"));
}

#[test]
fn writes_png() {
    let launch = tempfile::tempdir().expect("a folder");
    let file = launch.path().join("art.png");
    let settings = format!(
        "file = '{}'; style = 'mondrian'; width = 200; height = 200",
        file.display()
    );
    assert_eq!(run(launch.path(), &settings), None);
    let png = std::fs::read(&file).expect("a picture");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        &png[16..26],
        &[0, 0, 0, 200, 0, 0, 0, 200, 8, 2],
        "200 x 200, 8-bit RGB"
    );
}

#[test]
fn rejects_bad_settings() {
    let cases = [
        (
            "style = 'cubist'",
            "Unknown arts.style 'cubist'. Use one of: radial, hilbert, ridgeline, phyllotaxis, \
             flowfield, mondrian, tree, ginkgo",
        ),
        (
            "colors = ['#12345g']",
            "Invalid arts color '#12345g'. Use #rrggbb",
        ),
        (
            "width = 8",
            "arts.width and arts.height must be between 16 and 10000, got 8x1200",
        ),
        (
            "file = 'art.gif'",
            "Unsupported arts.file extension 'art.gif'. Use .svg or .png",
        ),
        (
            "title = 'maybe'",
            "arts.title must be true or false, got 'maybe'",
        ),
        (
            "height = 'tall'",
            "arts.height must be a whole number, got 'tall'",
        ),
    ];
    for (settings, message) in cases {
        let launch = tempfile::tempdir().expect("a folder");
        let error = run(launch.path(), settings).unwrap_or_default();
        assert!(error.contains(message), "{settings}: {error}");
    }
}

//! Compares recorded workflow files, inputs and image digests with their current state.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use run_database::{Database, ImagePin, RemoteFile, Run, Source, SourceKind};
use storage::Storage;

use crate::arguments::{self, Arguments};
use crate::{Failure, failure, revision, runs, sources};

/// What happened to one thing since the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// It has other content now.
    Changed,
    /// It is gone.
    Gone,
    /// It is new: the run did not have it.
    New,
    /// It cannot be told: the reason says why.
    Unknown,
}

/// One thing that changed, or that cannot be compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: Kind,
    /// The file, address or image.
    pub name: String,
    /// More in words, like the version now and then.
    pub detail: Option<String>,
}

/// One kind of thing that a run depends on, like the files of its workflow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// What the part holds, like `files of the workflow`.
    pub what: &'static str,
    /// How many of them the run recorded.
    pub total: usize,
    pub changes: Vec<Change>,
    /// A sentence more, like the Git revision now and then.
    pub note: Option<String>,
}

/// What changed since a run.
pub struct Changes {
    pub run: Run,
    pub parts: Vec<Part>,
}

impl Changes {
    /// Whether anything changed, or cannot be told.
    pub fn any(&self) -> bool {
        (self.parts.iter()).any(|part| !part.changes.is_empty() || part.note.is_some())
    }

    /// The files of the workflow or the command line that changed.
    pub fn sources_changed(&self) -> bool {
        (self.parts.iter().take(2))
            .any(|part| (part.changes.iter()).any(|change| change.kind != Kind::Unknown))
    }
}

/// How an image is found now: the image that its tag names at its registry, pinned to a digest.
pub type Resolve<'a> = &'a dyn Fn(&str) -> Result<Option<String>, String>;

/// `uwe changes <run>`: what changed since the run.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let database = runs::open(arguments)?;
    let run = named_run(&database, arguments, "changes")?;
    let setup = crate::setup::Setup::load(arguments)?;
    let _registry = setup.registry.enter().map_err(Failure::setup)?;
    let changes = found(&database, run, &setup.storage, &registry)?;
    print!("{}", report(&changes));
    Ok(())
}

/// The run that a command names as its first argument.
pub fn named_run(
    database: &Database,
    arguments: &Arguments,
    command: &str,
) -> Result<Run, Failure> {
    let Some(id) = arguments
        .target
        .as_ref()
        .map(|id| id.to_string_lossy().into_owned())
    else {
        return Err(Failure::usage(format!(
            "uwe: name the run, like `uwe {command} <run>`; `uwe runs` lists the runs"
        )));
    };
    let run = database
        .run(&id)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    run.ok_or_else(|| {
        failure(format!(
            "uwe: the run database has no run `{id}`; `uwe runs` lists the runs"
        ))
    })
}

/// An error of the run database, which the machine must fix.
pub fn broken(error: run_database::DatabaseError) -> Failure {
    Failure::setup(format!("uwe: {error}"))
}

/// The runtime of the engine, for the requests that the commands make.
pub fn runtime() -> Result<&'static tokio::runtime::Runtime, Failure> {
    workflow_graph::runtime().map_err(|error| Failure::setup(format!("uwe: {error}")))
}

/// The image that a tag names now at its registry.
pub fn registry(image: &str) -> Result<Option<String>, String> {
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    runtime
        .block_on(executor::registry::pin(image))
        .map_err(|error| error.to_string())
}

/// What changed since a run, with the storage providers to ask about remote files and `resolve`
/// to ask about images.
pub fn found(
    database: &Database,
    run: Run,
    storage: &Storage,
    resolve: Resolve,
) -> Result<Changes, Failure> {
    let recorded = database.sources(&run.id).map_err(broken)?;
    let arguments = recorded_arguments(database, &run)?;
    let runtime = runtime()?;
    let now = runtime.block_on(sources::collect(Path::new(&run.workflow), &arguments));
    let mut workflow = compared(
        &recorded,
        &now.files,
        SourceKind::Workflow,
        "files of the workflow",
    );
    let revision_now = revision::revision(Path::new(&run.workflow));
    if revision_now != run.revision {
        let shown = |revision: &Option<String>| match revision {
            Some(revision) => revision.chars().take(12).collect(),
            None => "none".to_owned(),
        };
        workflow.note = Some(format!(
            "Its Git revision is {} now; the run had {}.",
            shown(&revision_now),
            shown(&run.revision)
        ));
    }
    let named = compared(
        &recorded,
        &now.files,
        SourceKind::Named,
        "files that the command line names",
    );
    let inputs = local_inputs(database, &run, &recorded)?;
    let remote = remote_files(&database.remote_files(&run.id).map_err(broken)?, storage);
    let images = images(&database.images(&run.id).map_err(broken)?, resolve);
    Ok(Changes {
        run,
        parts: vec![workflow, named, inputs, remote, images],
    })
}

/// The command line of a run, with its paths made absolute from the folder the run started in.
pub fn recorded_arguments(database: &Database, run: &Run) -> Result<Arguments, Failure> {
    let mut ancestry = vec![run.clone()];
    let mut seen = BTreeSet::from([run.id.clone()]);
    while ancestry
        .last()
        .is_some_and(|run| run.arguments.first().is_some_and(|word| word == "repeat"))
    {
        let current = ancestry.last().unwrap();
        let parent = current.repeats.as_ref().ok_or_else(|| {
            failure(format!(
                "uwe: run {} recorded no repeat parent; its original options are unavailable",
                current.id
            ))
        })?;
        if !seen.insert(parent.clone()) {
            return Err(failure(format!(
                "uwe: repeat ancestry of run {} contains a cycle",
                run.id
            )));
        }
        let parent = database.run(parent).map_err(broken)?.ok_or_else(|| {
            failure(format!(
                "uwe: repeat parent {parent} is missing; its original options are unavailable"
            ))
        })?;
        ancestry.push(parent);
    }
    let original = ancestry.pop().unwrap();
    let mut parsed = parsed_arguments(&original)?;
    let mut parent = original;
    while let Some(current) = ancestry.pop() {
        let given = parsed_arguments(&current)?;
        crate::repeat::chosen_here(&mut parsed, &given);
        let previous = database.sources(&parent.id).map_err(broken)?;
        let sources = database.sources(&current.id).map_err(broken)?;
        let previous: Vec<_> = previous
            .iter()
            .filter(|source| source.kind == SourceKind::Named)
            .collect();
        let named: Vec<_> = sources
            .iter()
            .filter(|source| source.kind == SourceKind::Named)
            .collect();
        let moved = |path: &PathBuf| {
            let index = previous
                .iter()
                .position(|source| Path::new(&source.path) == path);
            index
                .and_then(|index| {
                    named
                        .get(index)
                        .filter(|source| source.digest == previous[index].digest)
                })
                .map_or_else(|| path.clone(), |source| PathBuf::from(&source.path))
        };
        parsed.configs = parsed.configs.iter().map(moved).collect();
        parsed.inputs = parsed.inputs.as_ref().map(moved);
        parsed.params = current.parameters.clone();
        parsed.target = Some(PathBuf::from(&current.workflow));
        parent = current;
    }
    Ok(parsed)
}

fn parsed_arguments(run: &Run) -> Result<Arguments, Failure> {
    let mut parsed = match run.arguments.split_first() {
        Some((_, rest)) => arguments::parse(rest).map_err(|message| {
            failure(format!(
                "uwe: the command line of run {} is not valid: {message}",
                run.id
            ))
        })?,
        None => Arguments::default(),
    };
    let launch = PathBuf::from(run.launch.clone().unwrap_or_else(|| ".".into()));
    let absolute = |path: &PathBuf| match path.is_absolute() {
        true => path.clone(),
        false => launch.join(path),
    };
    parsed.target = Some(PathBuf::from(&run.workflow));
    parsed.revision = None;
    parsed.latest = false;
    parsed.configs = parsed.configs.iter().map(absolute).collect();
    parsed.inputs = parsed.inputs.as_ref().map(absolute);
    parsed.output_dir = parsed.output_dir.as_ref().map(absolute);
    Ok(parsed)
}

/// The files of one kind that differ between the run and now, by path.
fn compared(recorded: &[Source], now: &[Source], kind: SourceKind, what: &'static str) -> Part {
    let by_path = |sources: &[Source]| -> BTreeMap<String, (String, bool)> {
        (sources.iter())
            .filter(|source| source.kind == kind)
            .map(|source| {
                (
                    source.path.clone(),
                    (source.digest.clone(), source.executable),
                )
            })
            .collect()
    };
    let (then, now) = (by_path(recorded), by_path(now));
    let mut changes = Vec::new();
    for (path, digest) in &then {
        let kind = match now.get(path) {
            None => Kind::Gone,
            Some(other) if other != digest => Kind::Changed,
            Some(_) => continue,
        };
        changes.push(change(kind, path, None));
    }
    // A new file of the workflow may change it too; a file that the run named cannot be new.
    for path in now.keys().filter(|path| !then.contains_key(*path)) {
        changes.push(change(Kind::New, path, None));
    }
    Part {
        what,
        total: then.len(),
        changes,
        note: None,
    }
}

/// The files on this machine that tasks of the run read and no task made, compared by digest,
/// else by size where the run did not know the digest, as a simulated run does not.
fn local_inputs(database: &Database, run: &Run, recorded: &[Source]) -> Result<Part, Failure> {
    let files = database.files(&run.id, None).map_err(broken)?;
    let made: BTreeSet<&str> = (files.iter())
        .filter(|file| file.role == "output")
        .map(|file| file.location.as_str())
        .collect();
    let folder = run.workflow_folder.as_deref().map(Path::new);
    let of_workflow: BTreeSet<PathBuf> = (recorded.iter())
        .filter(|source| source.kind == SourceKind::Workflow)
        .filter_map(|source| folder.map(|folder| folder.join(&source.path)))
        .collect();
    let mut seen = BTreeSet::new();
    let mut part = Part {
        what: "files on this machine that tasks read",
        total: 0,
        changes: Vec::new(),
        note: None,
    };
    let runtime = runtime()?;
    for file in files.iter().filter(|file| file.role == "input") {
        let location = file.location.as_str();
        let local = !location.is_empty() && storage::address::scheme(location).is_none();
        let inside_store = (run.store.as_deref()).is_some_and(|store| location.starts_with(store));
        if !local
            || made.contains(location)
            || inside_store
            || of_workflow.contains(Path::new(location))
            || !seen.insert(location)
        {
            continue;
        }
        part.total += 1;
        let path = Path::new(location);
        let Ok(found) = std::fs::metadata(path) else {
            part.changes.push(change(Kind::Gone, location, None));
            continue;
        };
        if file.directory || found.is_dir() {
            continue;
        }
        let differs = match &file.digest {
            Some(digest) => match runtime.block_on(content_store::hash_file(path)) {
                Ok((now, _)) => now.to_string() != *digest,
                Err(error) => {
                    let detail = format!("it cannot be read: {error}");
                    part.changes
                        .push(change(Kind::Unknown, location, Some(detail)));
                    continue;
                }
            },
            None => file.size.is_some_and(|size| size != found.len()),
        };
        if differs {
            part.changes.push(change(Kind::Changed, location, None));
        }
    }
    Ok(part)
}

/// The remote files of the run, asked about at their providers: gone, or of another version or
/// size. A provider that gives no version leaves a file of the same size unknown.
fn remote_files(files: &[RemoteFile], storage: &Storage) -> Part {
    let mut part = Part {
        what: "remote files",
        total: files.len(),
        changes: Vec::new(),
        note: None,
    };
    let blocking = storage.blocking();
    for file in files {
        if file.address.contains("[secret]") {
            let detail = "its address holds a password, which the run did not record".to_owned();
            part.changes
                .push(change(Kind::Unknown, &file.address, Some(detail)));
            continue;
        }
        let found = match blocking.stat(&file.address) {
            Ok(found) => found,
            Err(error) => {
                let detail = format!("it cannot be asked for: {error}");
                part.changes
                    .push(change(Kind::Unknown, &file.address, Some(detail)));
                continue;
            }
        };
        let Some(found) = found else {
            part.changes.push(change(Kind::Gone, &file.address, None));
            continue;
        };
        let (kind, detail) = match (&file.version, &found.version) {
            (Some(then), Some(now)) if then != now => (
                Kind::Changed,
                format!("its version is {now} now; the run read {then}"),
            ),
            (Some(_), Some(_)) => continue,
            _ if found.size != file.size => (
                Kind::Changed,
                format!(
                    "it has {} bytes now; the run read {}",
                    found.size, file.size
                ),
            ),
            _ => (
                Kind::Unknown,
                "it has the same size, and its provider gives no version to compare".to_owned(),
            ),
        };
        part.changes.push(change(kind, &file.address, Some(detail)));
    }
    part
}

/// The images of the run, whose tags may name other images now.
fn images(images: &[ImagePin], resolve: Resolve) -> Part {
    let mut part = Part {
        what: "images",
        total: images.len(),
        changes: Vec::new(),
        note: None,
    };
    let digest = |image: &str| image.split_once('@').map(|(_, digest)| digest.to_owned());
    for image in images {
        // An image ID names the image itself, which cannot change; no registry knows it.
        if image.ran.parse::<task_model::Digest>().is_ok() {
            continue;
        }
        if let Some(path) = image.ran.strip_prefix("docker-archive:") {
            if let Some((kind, detail)) = archive_change(Path::new(path)) {
                part.changes.push(change(kind, &image.named, Some(detail)));
            }
            continue;
        }
        if !image.pinned() {
            let detail = "the run used an image of this machine without a digest".to_owned();
            part.changes
                .push(change(Kind::Unknown, &image.named, Some(detail)));
            continue;
        }
        let (kind, detail) = match resolve(&image.named) {
            Ok(Some(now)) if digest(&now) == digest(&image.ran) => continue,
            Ok(Some(now)) => (
                Kind::Changed,
                format!("it names {now} now; the run used {}", image.ran),
            ),
            Ok(None) => (Kind::Unknown, "its registry gives no digest".to_owned()),
            Err(error) => (
                Kind::Unknown,
                format!("its registry cannot be asked: {error}"),
            ),
        };
        part.changes.push(change(kind, &image.named, Some(detail)));
    }
    part
}

/// How an image archive of this machine differs from the one that a run used: an archive
/// named by its image ID, `sha256-<hex>.tar`, must still hold that image.
fn archive_change(path: &Path) -> Option<(Kind, String)> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let recorded = (name.strip_prefix("sha256-"))
        .and_then(|rest| rest.strip_suffix(".tar"))
        .map(|hex| format!("sha256:{hex}"))
        .filter(|id| id.parse::<task_model::Digest>().is_ok());
    let Some(recorded) = recorded else {
        let detail = "the run used an image archive of this machine without an image ID";
        return Some((Kind::Unknown, detail.to_owned()));
    };
    match slurm_executor::archive_id(path) {
        Ok(now) if now == recorded => None,
        Ok(now) => Some((
            Kind::Changed,
            format!("its archive holds {now} now; the run used {recorded}"),
        )),
        Err(error) => Some((Kind::Changed, format!("its archive is gone: {error}"))),
    }
}

fn change(kind: Kind, name: &str, detail: Option<String>) -> Change {
    Change {
        kind,
        name: name.to_owned(),
        detail,
    }
}

/// The changes as text for people.
pub fn report(changes: &Changes) -> String {
    let run = &changes.run;
    let mut text = format!(
        "Changes since run {} of {}, which started {}:\n",
        run.id,
        run.workflow,
        runs::local_time(run.started)
    );
    for part in &changes.parts {
        let changed = (part.changes.iter())
            .filter(|change| change.kind != Kind::Unknown)
            .count();
        let _ = match (part.total, changed, part.changes.len()) {
            (0, 0, 0) => writeln!(text, "The {}: the run recorded none.", part.what),
            (total, 0, 0) => writeln!(text, "The {}: none changed ({total}).", part.what),
            (total, changed, _) => {
                writeln!(text, "The {}: {changed} of {total} changed.", part.what)
            }
        };
        for change in &part.changes {
            let kind = match change.kind {
                Kind::Changed => "changed",
                Kind::Gone => "gone",
                Kind::New => "new",
                Kind::Unknown => "unknown",
            };
            let _ = match &change.detail {
                Some(detail) => writeln!(text, "  {kind:<8} {}: {detail}", change.name),
                None => writeln!(text, "  {kind:<8} {}", change.name),
            };
        }
        if let Some(note) = &part.note {
            let _ = writeln!(text, "  {note}");
        }
    }
    if !changes.any() {
        text.push_str("Nothing changed since the run.\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mode_changes() {
        let before = source(SourceKind::Workflow, "helper", "sha256:aa");
        let mut after = before.clone();
        after.executable = true;
        let changes = compared(&[before], &[after], SourceKind::Workflow, "workflow");
        assert_eq!(changes.changes.len(), 1);
        assert_eq!(changes.changes[0].kind, Kind::Changed);
    }

    fn source(kind: SourceKind, path: &str, digest: &str) -> Source {
        Source {
            kind,
            path: path.into(),
            digest: digest.into(),
            size: 1,
            executable: false,
        }
    }

    #[test]
    fn classifies_file_changes() {
        let then = [
            source(SourceKind::Workflow, "main.nf", "sha256:1"),
            source(SourceKind::Workflow, "gone.nf", "sha256:2"),
            source(SourceKind::Workflow, "same.nf", "sha256:3"),
        ];
        let now = [
            source(SourceKind::Workflow, "main.nf", "sha256:9"),
            source(SourceKind::Workflow, "same.nf", "sha256:3"),
            source(SourceKind::Workflow, "new.nf", "sha256:4"),
        ];
        let part = compared(&then, &now, SourceKind::Workflow, "files of the workflow");
        let found: Vec<(Kind, &str)> = (part.changes.iter())
            .map(|change| (change.kind.clone(), change.name.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::Gone, "gone.nf"),
                (Kind::Changed, "main.nf"),
                (Kind::New, "new.nf")
            ]
        );
        assert_eq!(part.total, 3);
    }

    #[test]
    fn detects_image_changes() {
        let pins = [
            ImagePin {
                named: "debian:stable-slim".into(),
                ran: "debian@sha256:11".into(),
            },
            ImagePin {
                named: "tool:1".into(),
                ran: "tool@sha256:22".into(),
            },
            ImagePin {
                named: "built".into(),
                ran: "built".into(),
            },
        ];
        let resolve = |image: &str| -> Result<Option<String>, String> {
            match image {
                "debian:stable-slim" => Ok(Some("debian@sha256:33".into())),
                _ => Ok(Some("tool@sha256:22".into())),
            }
        };
        let part = images(&pins, &resolve);
        let found: Vec<(Kind, &str)> = (part.changes.iter())
            .map(|change| (change.kind.clone(), change.name.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::Changed, "debian:stable-slim"),
                (Kind::Unknown, "built")
            ]
        );
        let detail = part.changes[0].detail.as_deref().unwrap_or_default();
        assert!(detail.contains("debian@sha256:33 now"), "{detail}");
    }

    #[test]
    fn compares_archive_digests() {
        let folder = tempfile::tempdir().expect("a folder");
        let config = br#"{"config":{}}"#;
        let id = task_model::Digest::sha256(config);
        let write = |name: &str| {
            let path = folder.path().join(name);
            let mut archive = tar::Builder::new(std::fs::File::create(&path).expect("a file"));
            let manifest = br#"[{"Config":"config.json","Layers":[]}]"#;
            for (name, bytes) in [
                ("config.json", &config[..]),
                ("manifest.json", &manifest[..]),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_cksum();
                archive.append_data(&mut header, name, bytes).expect("adds");
            }
            archive.finish().expect("writes");
            format!("docker-archive:{}", path.display())
        };
        let same = write(&format!("sha256-{}.tar", id.hex()));
        let other = write(&format!("sha256-{}.tar", "ab".repeat(32)));
        let plain = write("tool.tar");
        let gone = format!("docker-archive:/gone/sha256-{}.tar", "cd".repeat(32));
        let pins = [
            id.to_string(),
            same,
            other.clone(),
            plain.clone(),
            gone.clone(),
        ]
        .map(|image| ImagePin {
            named: image.clone(),
            ran: image,
        });
        let resolve =
            |image: &str| -> Result<Option<String>, String> { panic!("no registry knows {image}") };
        let part = images(&pins, &resolve);
        let found: Vec<(Kind, &str)> = (part.changes.iter())
            .map(|change| (change.kind.clone(), change.name.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::Changed, other.as_str()),
                (Kind::Unknown, plain.as_str()),
                (Kind::Changed, gone.as_str())
            ]
        );
        let detail = part.changes[0].detail.as_deref().unwrap_or_default();
        assert!(detail.contains(&format!("holds {id} now")), "{detail}");
    }

    #[test]
    fn validates_repeat_ancestry() {
        let root = tempfile::tempdir().expect("a folder");
        let path = root.path().join("runs.sqlite");
        let start = |id: &str, parent: Option<&str>, words: &[&str]| run_database::RunStart {
            id: id.into(),
            resumes: None,
            workflow: "/launch/main.nf".into(),
            language: workflow_graph::Language::Nextflow,
            revision: None,
            arguments: words.iter().map(|word| (*word).into()).collect(),
            parameters: vec![("sample".into(), "alpha".into())],
            executor: "local".into(),
            simulated: true,
            store: None,
            program: None,
            launch: Some("/launch".into()),
            workflow_folder: Some("/launch".into()),
            repeats: parent.map(str::to_owned),
        };
        let original = start(
            "original",
            None,
            &[
                "run",
                "main.nf",
                "-profile",
                "analysis",
                "-configfile",
                "extra.config",
                "-inputs",
                "inputs.json",
                "--sample=alpha",
                "-no-cache",
            ],
        );
        let mut first = start(
            "first",
            Some("original"),
            &["repeat", "original", "-executor", "local"],
        );
        first.workflow = "/restored/main.nf".into();
        let mut second = start(
            "second",
            Some("first"),
            &["repeat", "first", "-max-pending", "3"],
        );
        second.workflow = first.workflow.clone();
        for run in [&original, &first, &second] {
            let named = if run.repeats.is_none() {
                "/launch"
            } else {
                "/restored/named"
            };
            let mut recorder = Database::open(&path)
                .expect("opens")
                .record(run)
                .expect("records");
            recorder.sources(&[
                source(
                    SourceKind::Named,
                    &format!("{named}/extra.config"),
                    "sha256:1",
                ),
                source(
                    SourceKind::Named,
                    &format!("{named}/inputs.json"),
                    "sha256:2",
                ),
            ]);
            recorder.ended(None);
            recorder.close().expect("closes");
        }
        let database = Database::open(&path).expect("opens");
        let run = database.run("second").expect("reads").expect("a run");
        let options =
            recorded_arguments(&database, &run).unwrap_or_else(|error| panic!("{}", error.message));
        assert_eq!(options.profiles, Some(vec!["analysis".into()]));
        assert_eq!(options.params, original.parameters);
        assert_eq!(
            options.configs,
            vec![PathBuf::from("/restored/named/extra.config")]
        );
        assert_eq!(options.inputs, Some("/restored/named/inputs.json".into()));
        assert_eq!(options.executor.as_deref(), Some("local"));
        assert_eq!(options.max_pending, Some(3));
        assert!(options.no_cache);
        drop(database);
        for (id, parent, diagnostic) in [
            ("missing", "gone", "is missing"),
            ("cycle", "cycle", "cycle"),
        ] {
            let run = start(id, Some(parent), &["repeat", parent]);
            let mut recorder = Database::open(&path)
                .expect("opens")
                .record(&run)
                .expect("records");
            recorder.ended(None);
            recorder.close().expect("closes");
            let database = Database::open(&path).expect("opens");
            let run = database.run(id).expect("reads").expect("a run");
            let error = recorded_arguments(&database, &run).expect_err("unavailable ancestry");
            assert!(error.message.contains(diagnostic), "{}", error.message);
        }
    }
}

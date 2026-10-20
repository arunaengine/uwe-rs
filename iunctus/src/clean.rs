//! Previews and removes unretained store entries and completed run scratch directories.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use run_database::Database;
use task_runner::clean::{Keep, Plan, maintenance_within, plan, remove_guarded};

use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, cache_directory, failure, runs};

/// How long `clean -remove` waits for tasks that prepare their inputs, which hold the store
/// for a short time each.
const WAIT: Duration = Duration::from_secs(300);

/// What the rules of the command line keep, in words, for the first lines; `None` without any
/// rule. `-keep-days 0` keeps nothing by time.
fn rules(arguments: &Arguments) -> Option<Vec<String>> {
    let given = arguments.keep_days.is_some()
        || arguments.keep_slower_than.is_some()
        || !arguments.keep_runs.is_empty();
    if !given {
        return None;
    }
    let mut rules = Vec::new();
    match arguments.keep_days {
        Some(1) => rules.push("what runs made or used in the last day".to_owned()),
        Some(days) if days > 1 => {
            rules.push(format!("what runs made or used in the last {days} days"));
        }
        _ => {}
    }
    if let Some(time) = arguments.keep_slower_than {
        rules.push(format!(
            "the results of tasks that ran {} or longer",
            duration(time)
        ));
    }
    match arguments.keep_runs.as_slice() {
        [] => {}
        [run] => rules.push(format!("what run {run} needs")),
        runs => rules.push(format!("what runs {} need", runs.join(", "))),
    }
    Some(rules)
}

/// `iunctus clean`.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(mut rules) = rules(arguments) else {
        return Err(Failure::usage(
            "iunctus: say what `iunctus clean` keeps: -keep-days <n> keeps what runs made or \
             used in the last n days, -keep-run <run> what a run needs, -keep-slower-than \
             <time> the results of tasks that ran that long, like 10m; -keep-days 0 keeps only \
             what runs that go on need",
        ));
    };
    let broken = |error: &dyn std::fmt::Display| failure(format!("iunctus: {error}"));
    let setup = Setup::load(arguments)?;
    let location = setup.store_location(arguments);
    let store = setup.store(arguments)?;
    let database = runs::open(arguments)?;
    for run in &arguments.keep_runs {
        if database.run(run).map_err(|error| broken(&error))?.is_none() {
            return Err(failure(format!(
                "iunctus: the run database has no run `{run}` to keep; `iunctus runs` lists \
                 the runs"
            )));
        }
    }
    let runtime = workflow_graph::runtime().map_err(|error| broken(&error))?;
    let waiting = || {
        eprintln!(
            "iunctus: waiting up to {} for tasks that prepare their inputs; new tasks wait \
             meanwhile",
            duration(WAIT)
        );
    };
    let maintenance = match arguments.remove {
        true => Some(
            runtime
                .block_on(maintenance_within(&store, WAIT, waiting))
                .map_err(|error| broken(&error))?,
        ),
        false => None,
    };
    if let Some(guard) = &maintenance {
        guard.ensure_cleanup().map_err(|error| broken(&error))?;
    }
    if arguments.remove {
        remote_attempts_finished(&cache_directory().join("scratch"))?;
    }
    let leases = runtime
        .block_on(store.held_leases("runs/"))
        .map_err(|error| broken(&error))?;
    let now = SystemTime::now();
    let used_since = (arguments.keep_days)
        .map(|days| now - Duration::from_secs(days.saturating_mul(24 * 60 * 60)));
    // The runs whose files stay: the named ones, the recent ones, and the ones that go on with
    // the runs that they resume.
    let mut kept: BTreeSet<String> = arguments.keep_runs.iter().cloned().collect();
    if let Some(since) = used_since {
        let since = milliseconds(since);
        kept.extend(database.runs_since(since).map_err(|error| broken(&error))?);
    }
    let running = database.running_runs().map_err(|error| broken(&error))?;
    let mut active_since = None::<i64>;
    for run in &running {
        kept.extend(
            database
                .lineage(&run.lineage)
                .map_err(|error| broken(&error))?,
        );
        active_since = Some(active_since.map_or(run.started, |since| since.min(run.started)));
    }
    // Runs of other machines, or of another run database, that use the store now.
    for holder in &leases {
        kept.insert(holder.run.clone());
        active_since = Some(active_since.map_or(holder.since, |since| since.min(holder.since)));
    }
    let listed: Vec<String> = kept.iter().cloned().collect();
    let references = database
        .references(&listed)
        .map_err(|error| broken(&error))?;
    let linked = linked_results(&database, &store)?;
    if !linked.is_empty() {
        let files = count(linked.len(), "stored file");
        rules.push(format!("the {files} that published results link to"));
    }
    let keep = Keep {
        used_since,
        slower_than: arguments.keep_slower_than,
        runs: kept.clone(),
        locations: (references.locations.iter())
            .filter_map(|location| store.location_of(location))
            .chain(linked)
            .collect(),
        digests: (references.digests.iter())
            .filter_map(|digest| digest.parse().ok())
            .collect(),
        active_since: active_since.map(time),
    };
    let planned = runtime
        .block_on(plan(&store, &keep))
        .map_err(|error| broken(&error))?;
    let scratch = scratch_folders(&database, &kept, &running_ids(&running, &leases))?;
    let (_reading, downloads) = old_downloads(&cache_directory().join("downloads"), used_since)?;
    let active = running.len().max(leases.len());
    let listed = listing(
        &location, &rules, active, &planned, &scratch, &downloads, &database,
    );
    print!("{listed}");
    if !arguments.remove {
        if !planned.is_empty() || !scratch.is_empty() || !downloads.is_empty() {
            println!("Nothing was removed; add -remove to remove it.");
        }
        return Ok(());
    }
    remote_attempts_finished(&cache_directory().join("scratch"))?;
    let removed = runtime
        .block_on(remove_guarded(
            &store,
            &planned,
            maintenance.as_ref().expect("removal holds maintenance"),
        ))
        .map_err(|error| broken(&error))?;
    for folder in &scratch {
        runtime
            .block_on(
                maintenance
                    .as_ref()
                    .expect("removal holds maintenance")
                    .check(),
            )
            .map_err(|error| broken(&error))?;
        std::fs::remove_dir_all(&folder.path)
            .map_err(|error| failure(format!("iunctus: {}: {error}", folder.path.display())))?;
        runtime
            .block_on(
                maintenance
                    .as_ref()
                    .expect("removal holds maintenance")
                    .check(),
            )
            .map_err(|error| broken(&error))?;
    }
    println!(
        "Removed {}, {}{} and {}.",
        count(removed.results, "result"),
        count(removed.objects, "file"),
        space(planned.freed()),
        count(scratch.len(), "scratch folder"),
    );
    for download in &downloads {
        let removed = match download.path.is_dir() {
            true => std::fs::remove_dir_all(&download.path),
            false => std::fs::remove_file(&download.path),
        };
        removed
            .map_err(|error| failure(format!("iunctus: {}: {error}", download.path.display())))?;
    }
    if !downloads.is_empty() {
        let size = downloads.iter().map(|download| download.size).sum();
        println!(
            "Removed {} ({}).",
            count(downloads.len(), "download"),
            bytes(size)
        );
    }
    Ok(())
}

/// The locations in the store that published results link to: the links that runs made and
/// that still point there. Removing those files would break the results, so they stay.
fn linked_results(
    database: &Database,
    store: &content_store::Store,
) -> Result<BTreeSet<String>, Failure> {
    let deliveries =
        (database.all_deliveries()).map_err(|error| failure(format!("iunctus: {error}")))?;
    let mut linked = BTreeSet::new();
    for delivery in deliveries {
        let target = Path::new(&delivery.target);
        let link = std::fs::symlink_metadata(target).is_ok_and(|found| found.is_symlink());
        let pointed = std::fs::canonicalize(target).ok();
        if link
            && pointed.is_some()
            && pointed == std::fs::canonicalize(&delivery.location).ok()
            && let Some(location) = store.location_of(&delivery.location)
        {
            linked.insert(location);
        }
    }
    Ok(linked)
}

/// Old copies of remote files and of contents of a store elsewhere, which runs download again,
/// and parts of crashed downloads; with the lock that keeps runs from reading downloads
/// meanwhile. While a run may read them, none go.
fn old_downloads(
    folder: &Path,
    used_since: Option<SystemTime>,
) -> Result<(Option<std::fs::File>, Vec<Scratch>), Failure> {
    let broken = |error: std::io::Error| failure(format!("iunctus: {}: {error}", folder.display()));
    let Some(reading) = storage::hold_downloads(folder).map_err(broken)? else {
        return Ok((None, Vec::new()));
    };
    let contents = folder.join(task_runner::CONTENTS);
    let entries = std::fs::read_dir(folder).map_err(broken)?.flatten();
    let copies = std::fs::read_dir(&contents).into_iter().flatten().flatten();
    let mut old = Vec::new();
    for entry in entries.chain(copies) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(found) = entry.metadata() else {
            continue;
        };
        let modified = found.modified().ok();
        let recent = used_since.is_some_and(|since| modified.is_none_or(|time| time >= since));
        // Older versions kept copies of contents by their digest next to the others.
        let digest = name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
        let copy = name.starts_with("v2-") || name.ends_with(".metadata") || digest;
        let copy = copy || entry.path().parent() == Some(contents.as_path());
        // No process writes downloads while clean-up holds the folder alone.
        let leftover = name.starts_with(".tmp");
        if leftover || (copy && !recent) {
            let path = entry.path();
            let size = match found.is_dir() {
                true => folder_size(&path),
                false => found.len(),
            };
            old.push(Scratch { path, size });
        }
    }
    Ok((Some(reading), old))
}

/// The folders in the scratch folder where the TES, Kubernetes and SLURM executors keep the
/// folders of their attempts.
const EXECUTOR_FOLDERS: [&str; 3] = ["tes", "kubernetes", "slurm"];

fn remote_attempts_finished(scratch: &Path) -> Result<(), Failure> {
    let folders = EXECUTOR_FOLDERS.iter().map(|name| scratch.join(name));
    for folder in std::iter::once(scratch.to_owned()).chain(folders) {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(failure(format!("iunctus: {}: {error}", folder.display())));
            }
        };
        for entry in entries {
            let entry = entry
                .map_err(|error| failure(format!("iunctus: {}: {error}", folder.display())))?;
            if !entry
                .file_type()
                .map_err(|error| failure(format!("iunctus: {}: {error}", entry.path().display())))?
                .is_dir()
            {
                continue;
            }
            let record = entry.path().join("remote.json");
            if record
                .try_exists()
                .map_err(|error| failure(format!("iunctus: {}: {error}", record.display())))?
            {
                return Err(failure(format!(
                    "iunctus: the remote attempt recorded in {} may still use its inputs; \
                     reconcile it with its original executor settings before removing data",
                    record.display()
                )));
            }
        }
    }
    Ok(())
}

/// A folder of the scratch directory that clean-up removes.
struct Scratch {
    path: PathBuf,
    size: u64,
}

/// The ids of the runs that go on.
fn running_ids(
    running: &[run_database::Run],
    leases: &[content_store::Holder],
) -> BTreeSet<String> {
    let ids = running.iter().map(|run| run.id.clone());
    ids.chain(leases.iter().map(|holder| holder.run.clone()))
        .collect()
}

/// The scratch folders of runs that ended: the made-up files of simulated runs that no rule
/// keeps, and the folders of attempts and input copies that a stopped run left behind, of every executor. A folder of a run that
/// the run database does not know stays, as it may belong to another database.
fn scratch_folders(
    database: &Database,
    kept: &BTreeSet<String>,
    running: &BTreeSet<String>,
) -> Result<Vec<Scratch>, Failure> {
    let scratch = cache_directory().join("scratch");
    let known = |run: &str| database.run(run).ok().flatten().is_some();
    let mut folders = Vec::new();
    let simulated = scratch.join("simulator");
    for entry in std::fs::read_dir(&simulated)
        .into_iter()
        .flatten()
        .flatten()
    {
        let run = entry.file_name().to_string_lossy().into_owned();
        if !kept.contains(&run) && !running.contains(&run) && known(&run) {
            folders.push(entry.path());
        }
    }
    // The local executor keeps its attempts in the scratch folder, the others in a folder of
    // their own in it.
    let executors = EXECUTOR_FOLDERS.iter().map(|name| scratch.join(name));
    for folder in std::iter::once(scratch.clone()).chain(executors) {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // An attempt's folder is `<run>-<task>-<attempt>`, and a run id is
            // `<time>-<process>`.
            let run: Vec<&str> = name.splitn(3, '-').take(2).collect();
            let run = run.join("-");
            if name != "simulator" && !running.contains(&run) && known(&run) {
                folders.push(entry.path());
            }
        }
    }
    let snapshots = cache_directory().join("downloads/input-snapshots");
    folders.extend(ended_snapshots(&snapshots, running, &known));
    Ok(folders
        .into_iter()
        .map(|path| Scratch {
            size: folder_size(&path),
            path,
        })
        .collect())
}

/// The private copies of inputs that runs which ended left behind, one folder per run, and
/// the nameless ones of older versions of iunctus while no run goes on.
fn ended_snapshots(
    folder: &Path,
    running: &BTreeSet<String>,
    known: &dyn Fn(&str) -> bool,
) -> Vec<PathBuf> {
    let entries = std::fs::read_dir(folder).into_iter().flatten().flatten();
    let ended = |run: &str| match run.starts_with(".tmp") {
        true => running.is_empty(),
        false => !running.contains(run) && known(run),
    };
    entries
        .filter(|entry| ended(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect()
}

/// The size of the files below a folder.
fn folder_size(folder: &Path) -> u64 {
    let mut size = 0;
    let mut pending = vec![folder.to_owned()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            match entry.metadata() {
                Ok(metadata) if metadata.is_dir() => pending.push(entry.path()),
                Ok(metadata) => size += metadata.len(),
                Err(_) => {}
            }
        }
    }
    size
}

/// What the store holds, what the rules keep, and what would go, grouped by run.
fn listing(
    location: &str,
    rules: &[String],
    active: usize,
    planned: &Plan,
    scratch: &[Scratch],
    downloads: &[Scratch],
    database: &Database,
) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "The store {location} holds {} and {} ({}).",
        count(planned.stored_results, "result"),
        count(planned.stored_objects, "file"),
        bytes(planned.stored_bytes),
    );
    let mut keeps = rules.to_vec();
    match active {
        0 => {}
        1 => keeps.push("what the run that goes on needs".to_owned()),
        more => keeps.push(format!("what the {more} runs that go on need")),
    }
    match keeps.is_empty() {
        true => {
            let _ = writeln!(text, "It keeps nothing: no run goes on.");
        }
        false => {
            let _ = writeln!(text, "It keeps {}.", words(&keeps));
        }
    }
    if planned.is_empty() && scratch.is_empty() && downloads.is_empty() {
        let _ = writeln!(text, "Nothing else is there to remove.");
        return text;
    }
    let _ = writeln!(
        text,
        "It would remove {} and {}{}:",
        count(planned.results.len(), "result"),
        count(planned.objects.len(), "file"),
        space(planned.freed()),
    );
    // Results and files by the run that made them; contents that nothing needs apart.
    let mut by_run: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for result in &planned.results {
        let run = result.run.clone().unwrap_or_default();
        by_run.entry(run).or_default().0 += 1;
    }
    let mut contents = 0;
    for object in &planned.objects {
        match object.location.strip_prefix("runs/") {
            Some(rest) => {
                let run = rest.split('/').next().unwrap_or_default().to_owned();
                by_run.entry(run).or_default().1 += 1;
            }
            None => contents += 1,
        }
    }
    for (run, (results, files)) in &by_run {
        let about = match database.run(run).ok().flatten() {
            Some(found) => format!(" ({}, {})", runs::local_time(found.started), found.workflow),
            None if run.is_empty() => " (results that cannot be read)".to_owned(),
            None => String::new(),
        };
        let _ = writeln!(
            text,
            "  run {run}{about}: {}, {}",
            count(*results, "result"),
            count(*files, "file")
        );
    }
    if contents > 0 {
        let _ = writeln!(
            text,
            "  contents and directory manifests that nothing needs: {}",
            count(contents, "file")
        );
    }
    if !scratch.is_empty() {
        let size: u64 = scratch.iter().map(|folder| folder.size).sum();
        let _ = writeln!(
            text,
            "  scratch folders of runs that ended: {} ({})",
            count(scratch.len(), "folder"),
            bytes(size)
        );
    }
    if !downloads.is_empty() {
        let size: u64 = downloads.iter().map(|download| download.size).sum();
        let _ = writeln!(
            text,
            "  downloads that runs fetch again when they need them: {} ({})",
            count(downloads.len(), "download"),
            bytes(size)
        );
    }
    text
}

/// `a`, `a and b`, `a, b and c`.
fn words(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `1 result`, `2 results`.
fn count(number: usize, noun: &str) -> String {
    match number {
        1 => format!("1 {noun}"),
        _ => format!("{number} {noun}s"),
    }
}

/// ` (1.2 GB)` for the space that removing frees, nothing when it frees none.
fn space(freed: u64) -> String {
    match freed {
        0 => String::new(),
        freed => format!(" ({})", bytes(freed)),
    }
}

/// A size in bytes, kB, MB, GB or TB, with one decimal from kB on.
pub(crate) fn bytes(size: u64) -> String {
    let units = ["kB", "MB", "GB", "TB"];
    if size < 1000 {
        return count(size as usize, "byte");
    }
    let mut value = size as f64;
    let mut unit = "";
    for next in units {
        value /= 1000.0;
        unit = next;
        if value < 1000.0 {
            break;
        }
    }
    format!("{value:.1} {unit}")
}

/// A time like `10m` or `2h`, as `-keep-slower-than` takes it.
fn duration(time: Duration) -> String {
    let seconds = time.as_secs();
    for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60)] {
        if seconds >= size && seconds.is_multiple_of(size) {
            return format!("{}{unit}", seconds / size);
        }
    }
    format!("{seconds}s")
}

/// Milliseconds since the Unix epoch, the unit of times in the run database.
fn milliseconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// A time of the run database.
fn time(milliseconds: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(milliseconds.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_attempts_retained() {
        let folder = tempfile::tempdir().expect("scratch");
        std::fs::write(folder.path().join("scratch.note"), "note").expect("ordinary file");
        assert!(
            remote_attempts_finished(folder.path()).is_ok(),
            "no attempts"
        );
        for backend in EXECUTOR_FOLDERS {
            let attempt = folder.path().join(backend).join("run-task-1");
            std::fs::create_dir_all(attempt.join("work")).expect("attempt");
            std::fs::write(attempt.join("work/remote.json"), "output").expect("output");
            assert!(
                remote_attempts_finished(folder.path()).is_ok(),
                "ordinary output"
            );
            let record = attempt.join("remote.json");
            std::fs::write(&record, "{}").expect("attempt record");
            let error = remote_attempts_finished(folder.path()).expect_err("unresolved attempt");
            assert!(error.message.contains(&record.display().to_string()));
            assert!(attempt.join("work/remote.json").is_file());
            std::fs::remove_file(record).expect("confirmed completion");
            assert!(
                remote_attempts_finished(folder.path()).is_ok(),
                "completed attempt"
            );
        }
    }

    #[test]
    fn removes_ended_snapshots() {
        let folder = tempfile::tempdir().expect("snapshots");
        for name in ["ended", "running", "foreign", ".tmpOld"] {
            std::fs::create_dir(folder.path().join(name)).expect("a folder");
        }
        let known = |run: &str| run != "foreign";
        let found = |running: &BTreeSet<String>| {
            let ended = ended_snapshots(folder.path(), running, &known);
            let mut names: Vec<String> = (ended.iter())
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        assert_eq!(found(&BTreeSet::from(["running".to_owned()])), ["ended"]);
        assert_eq!(found(&BTreeSet::new()), [".tmpOld", "ended", "running"]);
    }

    #[test]
    fn removes_old_downloads() {
        let folder = tempfile::tempdir().expect("downloads");
        for name in ["v2-ab", "input-snapshots", task_runner::CONTENTS] {
            std::fs::create_dir(folder.path().join(name)).expect("a folder");
        }
        std::fs::write(folder.path().join("v2-ab/content"), "data").expect("a copy");
        let digest = "ab".repeat(32);
        let copy = format!("{}/{}", task_runner::CONTENTS, "cd".repeat(32));
        for name in ["v2-cd.metadata", "0123abcd", ".tmpPart", &digest, &copy] {
            std::fs::write(folder.path().join(name), "file 4\n").expect("a file");
        }
        let names = |used_since| {
            let (_, old) = old_downloads(folder.path(), used_since).expect("lists");
            let mut names: Vec<String> = (old.iter())
                .map(|download| {
                    download
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        };
        let mut all = [
            ".tmpPart",
            "v2-ab",
            "v2-cd.metadata",
            &digest,
            &"cd".repeat(32),
        ];
        all.sort();
        assert_eq!(names(None), all);
        let day = SystemTime::now() - Duration::from_secs(86_400);
        assert_eq!(names(Some(day)), [".tmpPart"], "recent downloads stay");
        let reading = std::fs::File::open(folder.path().join("in-use.lock")).expect("the lock");
        reading.lock_shared().expect("locks");
        assert!(names(None).is_empty(), "a process reads downloads");
    }

    #[test]
    fn keeps_linked_results() {
        let folder = tempfile::tempdir().expect("a folder");
        let store = content_store::Store::local(&folder.path().join("store")).expect("a store");
        let start = run_database::RunStart {
            id: "old".into(),
            resumes: None,
            workflow: "main.nf".into(),
            language: workflow_graph::Language::Nextflow,
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: None,
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        };
        let mut recorder = Database::open(&folder.path().join("runs.sqlite"))
            .and_then(|database| database.record(&start))
            .expect("records");
        let results = folder.path().join("results");
        std::fs::create_dir(&results).expect("a results folder");
        let mut outputs = Vec::new();
        for name in ["linked", "replaced", "copied"] {
            let location = format!("runs/old/T-1/1/work/{name}.txt");
            let path = store.local_path(&location).expect("a local store");
            std::fs::create_dir_all(path.parent().unwrap()).expect("folders");
            std::fs::write(&path, name).expect("an output");
            let target = results.join(format!("{name}.txt"));
            match name {
                "copied" => std::fs::write(&target, name).expect("a copy"),
                _ => std::os::unix::fs::symlink(&path, &target).expect("a link"),
            }
            recorder.delivered(&target.to_string_lossy(), &path.to_string_lossy());
            outputs.push(location);
        }
        recorder.close().expect("closes");
        // The user replaced one link with a file of their own.
        std::fs::remove_file(results.join("replaced.txt")).expect("removes the link");
        std::fs::write(results.join("replaced.txt"), "own").expect("writes");
        let database = Database::open(&folder.path().join("runs.sqlite")).expect("opens");
        let linked = linked_results(&database, &store).expect("finds links");
        assert_eq!(linked, BTreeSet::from([outputs[0].clone()]));
        let keep = Keep {
            locations: linked,
            ..Keep::default()
        };
        let runtime = workflow_graph::runtime().expect("a runtime");
        let planned = runtime.block_on(plan(&store, &keep)).expect("plans");
        let removed: BTreeSet<&str> = (planned.objects.iter())
            .map(|object| object.location.as_str())
            .collect();
        assert_eq!(
            removed,
            BTreeSet::from([outputs[1].as_str(), outputs[2].as_str()])
        );
    }

    #[test]
    fn formats_retention_units() {
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(999), "999 bytes");
        assert_eq!(bytes(1_234), "1.2 kB");
        assert_eq!(bytes(12_300_000_000), "12.3 GB");
        assert_eq!(duration(Duration::from_secs(600)), "10m");
        assert_eq!(duration(Duration::from_secs(90)), "90s");
        assert_eq!(duration(Duration::from_secs(7_200)), "2h");
        let items = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(words(&items), "a, b and c");
    }
}

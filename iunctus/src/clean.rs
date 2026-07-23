//! `iunctus clean`: removes from the store what no rule keeps, as `nextflow clean` removes work
//! folders, but by rule and never what a run still refers to. It lists what it would remove
//! first, and removes it only with `-remove`. It also removes the scratch folders that runs
//! which ended left behind.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use run_database::Database;
use task_runner::clean::{Keep, Plan, plan, remove};

use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, cache_directory, failure, runs};

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
    let Some(rules) = rules(arguments) else {
        return Err(failure(
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
    let keep = Keep {
        used_since,
        slower_than: arguments.keep_slower_than,
        runs: kept.clone(),
        locations: (references.locations.iter())
            .filter_map(|location| store.location_of(location))
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
    let active = running.len().max(leases.len());
    print!(
        "{}",
        listing(&location, &rules, active, &planned, &scratch, &database)
    );
    if !arguments.remove {
        if !planned.is_empty() || !scratch.is_empty() {
            println!("Nothing was removed; add -remove to remove it.");
        }
        return Ok(());
    }
    let removed = runtime
        .block_on(remove(&store, &planned))
        .map_err(|error| broken(&error))?;
    for folder in &scratch {
        std::fs::remove_dir_all(&folder.path)
            .map_err(|error| failure(format!("iunctus: {}: {error}", folder.path.display())))?;
    }
    println!(
        "Removed {}, {}{} and {}.",
        count(removed.results, "result"),
        count(removed.objects, "file"),
        space(planned.freed()),
        count(scratch.len(), "scratch folder"),
    );
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
/// keeps, and the folders of attempts that a stopped run left behind. A folder of a run that
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
    for entry in std::fs::read_dir(&scratch).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // An attempt's folder is `<run>-<task>-<attempt>`, and a run id is `<time>-<process>`.
        let run: Vec<&str> = name.splitn(3, '-').take(2).collect();
        let run = run.join("-");
        if name != "simulator" && !running.contains(&run) && known(&run) {
            folders.push(entry.path());
        }
    }
    Ok(folders
        .into_iter()
        .map(|path| Scratch {
            size: folder_size(&path),
            path,
        })
        .collect())
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
    if planned.is_empty() && scratch.is_empty() {
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
fn bytes(size: u64) -> String {
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
    fn sizes_and_times_read_well() {
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

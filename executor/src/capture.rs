//! Captures the outputs of a task as numbered files with an index, and puts them back in place.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use task_model::Digest;

use crate::job::{CONTROL, ExecutorError, Finished, Job};
use crate::manifest;

/// The control files that a captured attempt gives back, by their names in the control folder.
pub const RECORDS: &[&str] = &[
    "exit", "stdout", "stderr", "manifest", "start", "end", "usage", INDEX,
];
/// The control file that names the path of each numbered output file, one `<number> <path>`
/// per line.
pub const INDEX: &str = "objects";
/// The folder of an attempt where the numbered output files are put back before they move.
const TRANSFER: &str = "transfer";

/// The place of an attempt in a staging store: `attempts/<digest>`, with `control/<record>` and
/// `files/<number>` below it.
pub fn attempt_prefix(job: Job<'_>) -> Result<String, ExecutorError> {
    let key = job
        .spec
        .cache_key()
        .map_err(|error| ExecutorError::Unsupported(error.to_string()))?;
    Ok(format!(
        "attempts/{}",
        Digest::sha256(format!("{key}\0{}", job.name).as_bytes()).hex()
    ))
}

/// The wrapper `script` with an end that copies each output file to `<transfer>/<number>` and
/// lists it in the index, then exits with the wrapper's status.
pub fn capture(script: &str, transfer: &str) -> Result<String, ExecutorError> {
    let script = script.strip_suffix("exit 0\n").ok_or_else(|| {
        ExecutorError::Unsupported("the capture wrapper has no final exit".into())
    })?;
    let flatten = FLATTEN.replace("{transfer}", &crate::wrapper::quote(transfer));
    Ok(format!("{script}{flatten}\nexit \"$status\"\n"))
}

/// Where an object of an attempt goes in its scratch directory `root`, by its path below the
/// attempt prefix: a record in the control folder, or a numbered file; `None` for others.
pub fn target(root: &Path, relative: &str, records: &[&str]) -> Option<PathBuf> {
    if let Some(name) = relative
        .strip_prefix("control/")
        .filter(|name| records.contains(name))
    {
        return Some(root.join(CONTROL).join(name));
    }
    relative
        .strip_prefix("files/")
        .filter(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
        .map(|name| root.join(TRANSFER).join(name))
}

/// Moves the numbered files of a task that ended well to the paths of the index, and checks
/// that every output of the manifest is there.
pub async fn arrange(finished: &Finished) -> Result<(), ExecutorError> {
    let transfer = finished.root.join(TRANSFER);
    let text = tokio::fs::read_to_string(finished.root.join(CONTROL).join(INDEX)).await?;
    for line in text.lines() {
        let (number, path) = line
            .split_once(' ')
            .filter(|(number, _)| number.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| failed("the capture index is invalid"))?;
        let parsed = manifest::parse(&format!("file {path}"))?;
        let target = finished.local_path(parsed[0].path.trim_start_matches("./"));
        let source = transfer.join(number);
        if !source.is_file() {
            return Err(failed("a captured output file is missing"));
        }
        tokio::fs::create_dir_all(target.parent().expect("a parent")).await?;
        tokio::fs::rename(source, target).await?;
    }
    if std::fs::read_dir(&transfer).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(failed("captured output files have no index records"));
    }
    for file in &finished.files {
        if file.directory {
            tokio::fs::create_dir_all(finished.local_path(&file.path)).await?;
        } else if !finished.local_path(&file.path).is_file() {
            return Err(failed("an output of the manifest is missing"));
        }
    }
    Ok(())
}

fn failed(message: &str) -> ExecutorError {
    std::io::Error::other(format!("object staging failed: {message}")).into()
}

/// Copies the outputs in the outbox and in the copies at absolute paths to numbered files.
const FLATTEN: &str = r#"transfer={transfer}
[ -z "${BASH_VERSION:-}" ] || shopt -u dotglob
mkdir -p "$transfer" || exit 1
[ -f "$control/usage" ] || : > "$control/usage"
: > "$control/objects"
number=0
flatten() {
    local source="$1" path="$2" child name
    if [ -d "$source" ]; then
        [ ! -L "$source" ] || { echo 'iunctus: cannot capture directory links' >&2; exit 33; }
        for child in "$source"/* "$source"/.[!.]* "$source"/..?*; do
            [ -e "$child" ] || [ -L "$child" ] || continue
            name=${child##*/}
            flatten "$child" "$path/$name"
        done
    elif [ -f "$source" ]; then
        case "$path" in *'
'*) echo 'iunctus: cannot capture newline paths' >&2; exit 33 ;; esac
        number=$((number + 1))
        cp -L "$source" "$transfer/$number" || exit 1
        printf '%s %s\n' "$number" "$path" >> "$control/objects"
    else
        echo 'iunctus: an output is not a regular file' >&2
        exit 33
    fi
}
if [ "$status" -eq 0 ]; then
    [ ! -d "$outbox" ] || flatten "$outbox" .
    [ ! -d "$control/root" ] || flatten "$control/root" ''
fi
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn arranges_indexed_files() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let file = target(root, "files/1", RECORDS).unwrap();
        assert_eq!(
            target(root, "control/exit", RECORDS),
            Some(root.join("control/exit"))
        );
        assert_eq!(target(root, "control/wrapper.sh", RECORDS), None);
        assert_eq!(target(root, "files/../1", RECORDS), None);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.join(CONTROL)).unwrap();
        std::fs::write(&file, "out").unwrap();
        std::fs::write(root.join("control/objects"), "1 ./out/a.txt\n").unwrap();
        let finished = Finished {
            address: None,
            exit_code: Some(0),
            files: manifest::parse("file out/a.txt\ndirectory empty\n").unwrap(),
            root: root.to_owned(),
            usage: None,
        };
        arrange(&finished).await.unwrap();
        assert_eq!(std::fs::read(root.join("work/out/a.txt")).unwrap(), b"out");
        assert!(root.join("work/empty").is_dir());
        std::fs::write(root.join("control/objects"), "").unwrap();
        std::fs::write(root.join("transfer/2"), "stray").unwrap();
        assert!(arrange(&finished).await.is_err(), "a file without a record");
    }
}

//! Makes shell task outputs transferable as FILE objects with a layout manifest.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use executor::{ExecutorError, Job, wrapper};

use crate::backend::{CONTROL, HOME, OUTBOX};
use crate::staging::{container_path, unsupported};

pub(crate) const TRANSFER: &str = "/iunctus/transfer";
pub(crate) const INDEX: &str = "objects";
pub(crate) const RECORDS: &[&str] = &[
    "exit", "stdout", "stderr", "manifest", "start", "end", "usage", INDEX,
];

pub(crate) fn script(
    job: Job<'_>,
    work: &str,
    directories: &[String],
    programs: &[String],
) -> Result<String, ExecutorError> {
    for rule in &job.spec.outputs {
        container_path(&rule.pattern, work)?;
    }
    let folders = wrapper::Folders {
        work,
        control: CONTROL,
        home: HOME,
        outbox: OUTBOX,
        owner: "",
    };
    let mut script = wrapper::wrapper(job.spec, &folders);
    let mut prepare = String::new();
    for directory in directories {
        let path = container_path(directory, work)?;
        if super::staging::wildcard(&path)
            || path == super::backend::WRAPPER
            || path == CONTROL
            || path.starts_with(&format!("{CONTROL}/"))
            || path == TRANSFER
            || path.starts_with(&format!("{TRANSFER}/"))
        {
            return Err(unsupported(
                "TES input directories overlap the output capture folders",
            ));
        }
        prepare.push_str(&format!("mkdir -p {} || exit 1\n", wrapper::quote(&path)));
    }
    for program in programs {
        prepare.push_str(&format!("chmod +x {} || exit 1\n", wrapper::quote(program)));
    }
    script = script.replacen("#!/bin/sh\n", &format!("#!/bin/sh\n{prepare}"), 1);
    let script = script
        .strip_suffix("exit 0\n")
        .ok_or_else(|| unsupported("TES capture wrapper has no final exit"))?;
    Ok(format!("{script}{FLATTEN}\nexit 0\n"))
}

const FLATTEN: &str = r#"transfer=/iunctus/transfer
[ -z "${BASH_VERSION:-}" ] || shopt -u dotglob
mkdir -p "$transfer" || exit 1
[ -f "$control/usage" ] || : > "$control/usage"
: > "$control/objects"
number=0
flatten() {
    local source="$1" path="$2" child name
    if [ -d "$source" ]; then
        [ ! -L "$source" ] || { echo 'iunctus: TES cannot capture directory links' >&2; exit 33; }
        for child in "$source"/* "$source"/.[!.]* "$source"/..?*; do
            [ -e "$child" ] || [ -L "$child" ] || continue
            name=${child##*/}
            flatten "$child" "$path/$name"
        done
    elif [ -f "$source" ]; then
        case "$path" in *'
'*) echo 'iunctus: TES cannot capture newline paths' >&2; exit 33 ;; esac
        number=$((number + 1))
        cp -L "$source" "$transfer/$number" || exit 1
        printf '%s %s\n' "$number" "$path" >> "$control/objects"
    else
        echo 'iunctus: TES output is not a regular file' >&2
        exit 33
    fi
}
if [ "$status" -eq 0 ]; then
    [ ! -d "$outbox" ] || flatten "$outbox" .
    [ ! -d "$control/root" ] || flatten "$control/root" ''
fi
"#;

//! The Bash wrapper that runs a task in its work directory and collects its outputs
//! (design section 5.4). It only knows the neutral names of the specification.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use task_model::{EnvironmentValue, TaskSpec};

use crate::braces::expand_braces;

/// The names of the control files the wrapper writes, and of the folder that holds copies of
/// outputs at absolute paths.
pub const EXIT: &str = "exit";
pub const START: &str = "start";
pub const END: &str = "end";
pub const MANIFEST: &str = "manifest";
pub const STDOUT: &str = "stdout";
pub const STDERR: &str = "stderr";
pub const STDIN: &str = "stdin";
pub const ROOT: &str = "root";
/// The last line that the wrapper writes to its own standard output, which the container's
/// output shows, when it got to its end: it recorded the tool's exit code and its outputs.
pub const ENDED: &str = "uwe: the wrapper ended";
/// Writes the time in seconds since the Unix epoch: with microseconds from Bash 5 on, whose
/// `EPOCHREALTIME` needs no program, else whole seconds from `date`.
pub(crate) const NOW: &str = "echo \"${EPOCHREALTIME:-$(date +%s)}\"";
/// What the task used, as the cgroup of its container counts it: `cpu_usec`, `memory_peak`,
/// `read_bytes`, `write_bytes` and `oom_kills`, one per line with its number.
pub const USAGE: &str = "usage";

pub(crate) fn invalid_specification() -> String {
    "#!/bin/sh\necho 'uwe: invalid task specification' >&2\nexit 1\n".to_owned()
}

/// Where the wrapper of a task finds its folders, as absolute paths inside the container.
#[derive(Clone, Copy, Debug)]
pub struct Folders<'a> {
    /// The work directory of the task.
    pub work: &'a str,
    /// The control files of the wrapper.
    pub control: &'a str,
    /// The home folder of the task, which the wrapper makes. The task may write there, as
    /// tools like Jupyter, R and Java expect of `HOME`, also when the container runs as a user
    /// that the image does not know.
    pub home: &'a str,
    /// Where copies of the outputs go, for servers that give back only the directories a task
    /// names, like TES; empty for none.
    pub outbox: &'a str,
    /// The user and group, like `1000:100`, that get what the task made in the folder of the
    /// control folder at the end, for a container that may run as another user; empty for none.
    pub owner: &'a str,
}

/// The wrapper for a task in the given folders.
pub fn wrapper(spec: &TaskSpec, folders: &Folders<'_>) -> String {
    if spec.validate().is_err() {
        return invalid_specification();
    }
    let Folders {
        work,
        control,
        home,
        outbox,
        owner,
    } = *folders;
    // A task that runs as Kubernetes runs it starts where its image or its `workdir` says,
    // with the `HOME` of its user.
    let as_image = spec.run_as.is_some();
    let mut script = String::from("#!/bin/sh\n# The uwe task wrapper.\nset -u\n");
    script.push_str(BASH);
    script.push_str(&format!(
        "work={}\ncontrol={}\noutbox={}\n",
        quote(work),
        quote(control),
        quote(outbox)
    ));
    // Folders that the backend made already need no `mkdir` process.
    script.push_str("[ -d \"$work\" ] && [ -d \"$control\" ] || mkdir -p \"$work\" \"$control\"\n");
    match (as_image, &spec.workdir) {
        (false, _) => {
            script.push_str("cd \"$work\" || exit 1\n");
            script.push_str(&format!(
                "export HOME={}\n[ -d \"$HOME\" ] || mkdir -p \"$HOME\"\n",
                quote(home)
            ));
        }
        (true, Some(workdir)) => script.push_str(&start_in(workdir)),
        (true, None) => {}
    }
    for (name, value) in &spec.environment {
        let value = match value {
            EnvironmentValue::Literal(text) => quote(text),
            EnvironmentValue::WorkDirectory => "\"$work\"".to_owned(),
        };
        script.push_str(&format!("export {name}={value}\n"));
    }
    let arguments: Vec<String> = spec
        .arguments
        .iter()
        .map(|argument| quote(argument))
        .collect();
    let stdin = match spec.stdin {
        Some(_) => format!(" < \"$control/{STDIN}\""),
        None => String::new(),
    };
    script.push_str(&format!("{NOW} > \"$control/{START}\"\n"));
    script.push_str(&stoppable(&format!(
        "{} > \"$control/{STDOUT}\" 2> \"$control/{STDERR}\"{stdin}",
        arguments.join(" ")
    )));
    script.push_str(&format!(
        "{NOW} > \"$control/{END}\"\necho \"$status\" > \"$control/{EXIT}\"\n"
    ));
    script.push_str(USAGE_SCRIPT);
    script.push_str(COLLECT);
    script.push_str(&format!(": > \"$control/{MANIFEST}\"\n"));
    if as_image {
        // Relative output paths are relative to the work directory.
        script.push_str("cd \"$work\" || exit 1\n");
    }
    // A shell rejects an empty `then` block, so a task without outputs gets none.
    if !spec.outputs.is_empty() {
        script.push_str("if [ \"$status\" -eq 0 ]; then\n");
        for rule in &spec.outputs {
            for pattern in expand_braces(rule.pattern.trim_end_matches('/')) {
                script.push_str(&format!("    collect {}\n", quote(&pattern)));
            }
        }
        script.push_str("fi\n");
    }
    if !outbox.is_empty() {
        script.push_str("chmod -R a+rwX \"$work\" \"$outbox\" \"$control\" 2> /dev/null\n");
    }
    script.push_str(&hand_back(owner, "\"${control%/*}\""));
    script.push_str(&format!("echo {}\nexit 0\n", quote(ENDED)));
    script
}

/// Goes to a folder where a container starts, which Kubernetes makes when it is not there.
pub(crate) fn start_in(folder: &str) -> String {
    let folder = quote(folder);
    format!("[ -d {folder} ] || mkdir -p {folder}\ncd {folder} || exit 1\n")
}

/// Gives what the task made in `folder`, a shell word, to `owner`, like `1000:100`, as root
/// with the capability `CHOWN`; else, as another user, lets every user change it. Nothing for
/// an empty owner.
pub(crate) fn hand_back(owner: &str, folder: &str) -> String {
    match owner.is_empty() {
        true => String::new(),
        false => format!(
            "chown -hR {} {folder} 2> /dev/null || chmod -R a+rwX {folder} 2> /dev/null\n",
            quote(owner)
        ),
    }
}

/// The exit code of a container whose task ran out of time, as `timeout` gives it.
pub const TIMED_OUT: i32 = 124;

/// The lines that run `command` and set `status`. The command gets TERM at the time of its
/// task, `seconds`, as the job around it may run longer, and `status` is then [`TIMED_OUT`].
pub fn timed(command: &str, seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return format!("{command}\nstatus=$?\n");
    };
    format!(
        "{command} &\npid=$!\ntrap 'kill -TERM \"$pid\" 2> /dev/null' TERM INT\n\
         ( sleep {seconds} && kill -TERM \"$pid\" 2> /dev/null && exit {TIMED_OUT} ) &\n\
         timer=$!\nwait \"$pid\"\nstatus=$?\nkill \"$timer\" 2> /dev/null\n\
         wait \"$timer\" 2> /dev/null\n\
         [ $? -ne {TIMED_OUT} ] || status={TIMED_OUT}\n"
    )
}

/// Runs a command so that a stop reaches it, and sets `status` to its exit code.
pub fn stoppable(command: &str) -> String {
    format!(
        "exec 3<&0\n<&3 3<&- {command} &\npid=$!\n\
         trap 'stopped=1; kill -TERM \"$pid\" 2> /dev/null' TERM INT HUP\n\
         wait \"$pid\"\nstatus=$?\nexec 3<&-\n"
    )
}

/// The wrapper starts in `sh`, which every image with a shell has, and goes on in Bash where
/// the image has it, wherever its `PATH` has it: Bash collects outputs with `**` and hidden
/// files, as Nextflow's globs do. In plain `sh`, as in Alpine, the patterns are those of `sh`.
pub(crate) const BASH: &str = r#"if [ -z "${BASH_VERSION:-}" ] && command -v bash > /dev/null 2>&1; then
    exec bash "$0"
fi
"#;

/// Records CPU, peak memory, block I/O and memory kills from the private cgroup using shell
/// builtins.
pub(crate) const USAGE_SCRIPT: &str = r#"cgroup=/sys/fs/cgroup
if [ -r "$cgroup/memory.peak" ]; then
    {
        read -r peak < "$cgroup/memory.peak"
        echo "memory_peak $peak"
        if [ -r "$cgroup/memory.events" ]; then
            while read -r key value; do
                [ "$key" = oom_kill ] && echo "oom_kills $value"
            done < "$cgroup/memory.events"
        fi
        if [ -r "$cgroup/cpu.stat" ]; then
            while read -r key value; do
                [ "$key" = usage_usec ] && echo "cpu_usec $value"
            done < "$cgroup/cpu.stat"
        fi
        if [ -r "$cgroup/io.stat" ]; then
            read_bytes=0 write_bytes=0
            while read -r line; do
                for word in $line; do
                    case $word in
                        rbytes=*) read_bytes=$((read_bytes + ${word#rbytes=})) ;;
                        wbytes=*) write_bytes=$((write_bytes + ${word#wbytes=})) ;;
                    esac
                done
            done < "$cgroup/io.stat"
            echo "read_bytes $read_bytes"
            echo "write_bytes $write_bytes"
        fi
    } > "$control/usage" 2> /dev/null
fi
"#;

/// Adds each file and directory that a pattern matches to the manifest, as `file <path>` or
/// `directory <path>`, without a process per file.
pub(crate) const COLLECT: &str = r#"if [ -n "${BASH_VERSION:-}" ]; then
    shopt -s globstar nullglob dotglob
fi
collect() {
    local IFS='
' match
    for match in $1; do
        [ -e "$match" ] || [ -L "$match" ] || continue
        match=${match#./}
        case $match in
            /*) [ -e "$control/root$match" ] || {
                    mkdir -p "$control/root${match%/*}" && cp -RLp "$match" "$control/root$match"
                } ;;
            *) [ -z "$outbox" ] || [ -e "$outbox/$match" ] || {
                    mkdir -p "$outbox/$(dirname "$match")" && cp -RL "$match" "$outbox/$match"
                } ;;
        esac
        if [ -d "$match" ]; then
            printf 'directory %s\n' "$match" >> "$control/manifest"
        elif [ -f "$match" ]; then
            printf 'file %s\n' "$match" >> "$control/manifest"
        fi
    done
}
"#;

/// A shell word in single quotes.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

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
    // Only a container has a cgroup of the task's own, which nf-cgroup-metrics reports.
    let sampled = spec.image.is_some();
    if sampled {
        script.push_str(MEMORY_WATCH);
        script.push_str("memory_watch &\nwatch=$!\n");
    }
    script.push_str(&format!("{NOW} > \"$control/{START}\"\n"));
    script.push_str(&stoppable(&format!(
        "{} > \"$control/{STDOUT}\" 2> \"$control/{STDERR}\"{stdin}",
        arguments.join(" ")
    )));
    script.push_str(&format!(
        "{NOW} > \"$control/{END}\"\necho \"$status\" > \"$control/{EXIT}\"\n"
    ));
    if sampled {
        script.push_str("kill \"$watch\" 2> /dev/null\nwait \"$watch\" 2> /dev/null\n");
    }
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
sampled=
if [ -r "$control/usage" ]; then
    while IFS= read -r line; do
        sampled="$sampled$line
"
    done < "$control/usage"
fi
if [ -r "$cgroup/memory.peak" ]; then
    {
        printf '%s' "$sampled"
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

/// Samples the memory of the task's cgroup once per second while the task runs, as
/// nf-cgroup-metrics 1.0.1 does: cgroup v2, else v1, at the path that `/proc/self/cgroup` names,
/// else at the root. It keeps the largest values in the usage file, in bytes.
pub(crate) const MEMORY_WATCH: &str = r#"memory_find() {
    memory= kind=
    if [ -f "$1/cgroup.controllers" ]; then
        while IFS= read -r line; do
            case $line in 0::*) memory="$1${line#0::}" ;; esac
        done 2> /dev/null < "$2"
        [ -r "$memory/memory.current" ] || memory=$1
        [ -r "$memory/memory.current" ] && kind=2
    elif [ -d "$1/memory" ]; then
        while IFS= read -r line; do
            case $line in *:memory:*) memory="$1/memory${line#*:memory:}" ;; esac
        done 2> /dev/null < "$2"
        [ -r "$memory/memory.usage_in_bytes" ] || memory=$1/memory
        [ -r "$memory/memory.usage_in_bytes" ] && kind=1
    fi
    [ -n "$kind" ]
}
memory_sample() {
    case $kind in
        2)
            read -r current < "$memory/memory.current" || return 1
            peak=$current
            [ -r "$memory/memory.peak" ] && read -r peak < "$memory/memory.peak"
            anon=0
            while read -r key value; do
                [ "$key" = anon ] && anon=$value
            done < "$memory/memory.stat"
            ;;
        1)
            read -r current < "$memory/memory.usage_in_bytes" || return 1
            peak=$current
            [ -r "$memory/memory.max_usage_in_bytes" ] &&
                read -r peak < "$memory/memory.max_usage_in_bytes"
            anon=0
            while read -r key value; do
                case $key in rss | rss_huge) anon=$((anon + value)) ;; esac
            done < "$memory/memory.stat"
            ;;
        *) return 1 ;;
    esac
    [ "$current" -gt "${vmem:-0}" ] && vmem=$current
    [ "$anon" -gt "${rss:-0}" ] && rss=$anon
    [ "$peak" -gt "${peak_vmem:-0}" ] && peak_vmem=$peak
    return 0
}
memory_watch() {
    trap 'exit 0' TERM
    memory_find /sys/fs/cgroup /proc/self/cgroup || return 0
    command -v sleep > /dev/null 2>&1 || return 0
    total=0
    while read -r key value unit; do
        [ "$key" = MemTotal: ] && total=$value
    done 2> /dev/null < /proc/meminfo
    while memory_sample 2> /dev/null; do
        printf 'vmem %s\nrss %s\npeak_vmem %s\nmemory_total %s\n' \
            "$vmem" "$rss" "$peak_vmem" "$((total * 1024))" > "$control/usage"
        sleep 1 &
        wait $!
    done
}
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::MEMORY_WATCH;

    /// Writes each file below `root`.
    fn lay_out(root: &Path, files: &[(&str, &str)]) {
        for (name, text) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes folders");
            std::fs::write(path, text).expect("writes");
        }
    }

    /// What two samples of the cgroup files in `root` give, `then` changing them in between:
    /// the cgroup version, the folder and the largest values.
    fn sampled(root: &Path, then: &str) -> String {
        let script = format!(
            "{MEMORY_WATCH}memory_find \"$1/sys\" \"$1/cgroup\" || exit 3\nmemory_sample\n\
             {then}\nmemory_sample\necho \"$kind ${{memory#$1}} $vmem $rss $peak_vmem\"\n"
        );
        let output = std::process::Command::new("sh")
            .args(["-c", &script, "sh"])
            .arg(root)
            .output()
            .expect("runs sh");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
            + &format!("{:?}", output.status.code())
    }

    #[test]
    fn samples_cgroup_v2() {
        let root = tempfile::tempdir().expect("a temporary directory");
        lay_out(
            root.path(),
            &[
                ("sys/cgroup.controllers", "memory\n"),
                ("sys/task/memory.current", "4096\n"),
                ("sys/task/memory.peak", "8192\n"),
                ("sys/task/memory.stat", "anon 1024\nfile 3072\n"),
                ("cgroup", "0::/task\n"),
            ],
        );
        let then = "echo 2048 > \"$memory/memory.current\"\n\
                    printf 'anon 1536\\nfile 512\\n' > \"$memory/memory.stat\"";
        let got = sampled(root.path(), then);
        assert_eq!(got, "2 /sys/task 4096 1536 8192Some(0)");
    }

    #[test]
    fn samples_cgroup_v1() {
        let root = tempfile::tempdir().expect("a temporary directory");
        // The path of a private cgroup namespace is missing, so the root counts.
        lay_out(
            root.path(),
            &[
                ("sys/memory/memory.usage_in_bytes", "3000\n"),
                ("sys/memory/memory.max_usage_in_bytes", "5000\n"),
                (
                    "sys/memory/memory.stat",
                    "cache 100\nrss 1000\nrss_huge 2048\ntotal_rss 9999\n",
                ),
                ("cgroup", "12:pids:/docker/x\n4:memory:/docker/x\n"),
            ],
        );
        let then = "echo 2000 > \"$memory/memory.usage_in_bytes\"";
        let got = sampled(root.path(), then);
        assert_eq!(got, "1 /sys/memory 3000 3048 5000Some(0)");
    }

    #[test]
    fn skips_missing_cgroup() {
        let root = tempfile::tempdir().expect("a temporary directory");
        lay_out(
            root.path(),
            &[("sys/cgroup.controllers", ""), ("cgroup", "0::/gone\n")],
        );
        assert_eq!(sampled(root.path(), ":"), "Some(3)");
    }
}

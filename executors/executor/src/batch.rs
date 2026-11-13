//! What the cloud batch executors share: where a job finds the uwe helper, and the job command
//! that stages the inputs in, runs the wrapper of the task and sends the outputs back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use config_file::{ConfigError, Table};

use crate::wrapper::timed;

/// The folder of the container where each job makes the folder of its attempt, by default.
pub const TASK_FOLDER: &str = "/tmp/uwe";

/// Where a job finds the static uwe binary that stages the files of its task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Helper {
    /// A path on the hosts, like a folder of their machine image; the job mounts its folder
    /// read-only.
    Path(String),
    /// An address that the job downloads with `curl` or `wget`, and the SHA-256 of the file.
    Download { url: String, sha256: String },
}

/// Where the jobs get the uwe binary: `helper_path` on the hosts, or `helper_url` with its
/// `helper_sha256`.
pub fn helper(table: &Table<'_>) -> Result<Option<Helper>, ConfigError> {
    let (path, url) = (table.text("helper_path")?, table.text("helper_url")?);
    match (path, url, table.text("helper_sha256")?) {
        (None, None, None) => Ok(None),
        (Some(path), None, None) if path.starts_with('/') => Ok(Some(Helper::Path(path))),
        (Some(_), None, None) => Err(table.invalid("helper_path", "must be an absolute path")),
        (None, Some(url), Some(sha256))
            if sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            let sha256 = sha256.to_ascii_lowercase();
            Ok(Some(Helper::Download { url, sha256 }))
        }
        (None, Some(_), _) => Err(table.invalid(
            "helper_sha256",
            "must be the SHA-256 of the file at helper_url, as 64 hex digits",
        )),
        _ => Err(table.invalid(
            "helper_path",
            "takes no helper_url; name the binary on the hosts or the address to download",
        )),
    }
}

/// Where the job gets the helper.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    /// A path that the job definition mounts from the host.
    Path(&'a str),
    /// An address to download with `curl` or `wget`, the SHA-256 of the file, and the machine
    /// it runs on, as `uname -m` names it, when that is known.
    Download {
        url: &'a str,
        sha256: &'a str,
        machine: Option<&'a str>,
    },
    /// A file that the job got before it started, like an Azure Batch resource file, with the
    /// SHA-256 and machine as for a download.
    Fetched {
        path: &'a str,
        sha256: &'a str,
        machine: Option<&'a str>,
    },
}

/// Gets the helper `$3` with the SHA-256 `$4` next to the folder `$1` of the attempt.
const DOWNLOAD: &str = "helper=\"$1.uwe\"\n{ curl -fsSL -o \"$helper\" \"$3\" || wget -q -O \
                        \"$helper\" \"$3\"; } && echo \"$4  $helper\" | sha256sum -c - > \
                        /dev/null && chmod +x \"$helper\" || { echo \"uwe: cannot get the \
                        helper; the image needs curl or wget and sha256sum\" >&2; exit 125; }\n";

/// Checks the helper `$3` that the job got against its SHA-256 `$4`.
const FETCHED: &str = "helper=\"$3\"\necho \"$4  $helper\" | sha256sum -c - > /dev/null || { echo \
                       \"uwe: the helper does not match its SHA-256; the image needs sha256sum\" \
                       >&2; exit 125; }\n";

/// The words of the job command for the folder `root` of the attempt in the container, the
/// plan at `plan`, and a task that may run `seconds`.
pub fn command(root: &str, plan: &str, source: Source<'_>, seconds: Option<u64>) -> Vec<String> {
    let mut script = String::from("mkdir -p \"$1\" || exit 125\n");
    let mut words = vec![root.to_owned(), plan.to_owned()];
    match source {
        Source::Path(path) => {
            script.push_str("helper=\"$3\"\n");
            words.push(path.to_owned());
        }
        Source::Download {
            url,
            sha256,
            machine,
        } => {
            script.push_str(&same_machine(machine));
            script.push_str(DOWNLOAD);
            words.extend([url.to_owned(), sha256.to_owned()]);
        }
        Source::Fetched {
            path,
            sha256,
            machine,
        } => {
            script.push_str(&same_machine(machine));
            script.push_str(FETCHED);
            words.extend([path.to_owned(), sha256.to_owned()]);
        }
    }
    let run = timed("sh \"$1/control/wrapper.sh\"", seconds).replace('\n', "\n    ");
    script.push_str(&format!(
        "if \"$helper\" __stage in \"$1\" \"$2\"; then\n    {}\nelse\n    status=125\nfi\n\
         \"$helper\" __stage out \"$1\" || exit 125\nexit \"$status\"\n",
        run.trim_end()
    ));
    let mut command = vec!["sh".to_owned(), "-c".to_owned(), script, "uwe".to_owned()];
    command.extend(words);
    command
}

/// A line that stops the job on a host of another machine than `machine`, as a static binary
/// runs only on its own; empty when the machine is not known.
fn same_machine(machine: Option<&str>) -> String {
    machine.map_or(String::new(), |machine| {
        format!(
            "[ \"$(uname -m)\" = {machine} ] || {{ echo \"uwe: the helper runs on {machine}, but \
             this host is $(uname -m)\" >&2; exit 125; }}\n"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_through_helper() {
        let words = command("/tmp/uwe/a", "https://plan", Source::Path("/opt/uwe"), None);
        assert_eq!(words[..2], ["sh", "-c"]);
        assert_eq!(
            words[3..],
            ["uwe", "/tmp/uwe/a", "https://plan", "/opt/uwe"]
        );
        let expected = "mkdir -p \"$1\" || exit 125\nhelper=\"$3\"\n\
            if \"$helper\" __stage in \"$1\" \"$2\"; then\n    \
            sh \"$1/control/wrapper.sh\"\n    status=$?\nelse\n    status=125\nfi\n\
            \"$helper\" __stage out \"$1\" || exit 125\nexit \"$status\"\n";
        assert_eq!(words[2], expected);
        let source = Source::Download {
            url: "https://staging/uwe",
            sha256: "ab",
            machine: Some("x86_64"),
        };
        let words = command("/tmp/uwe/a", "https://plan", source, Some(60));
        assert_eq!(words[6..], ["https://staging/uwe", "ab"]);
        assert!(
            words[2].contains("[ \"$(uname -m)\" = x86_64 ]"),
            "{}",
            words[2]
        );
        assert!(words[2].contains("sha256sum -c -"), "{}", words[2]);
        assert!(words[2].contains("sleep 60"), "{}", words[2]);
        let source = Source::Fetched {
            path: "./uwe",
            sha256: "cd",
            machine: None,
        };
        let words = command("/tmp/uwe/a", "plan.json", source, None);
        assert_eq!(words[6..], ["./uwe", "cd"]);
        assert!(words[2].contains("helper=\"$3\"\necho \"$4  $helper\" | sha256sum"));
        assert!(!words[2].contains("uname"), "{}", words[2]);
    }
}

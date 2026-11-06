//! The command of a job: it gets the uwe helper, stages the inputs in, runs the wrapper of the
//! task within its time, and sends the outputs and control records back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use executor::wrapper::timed;

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
}

/// Gets the helper `$3` with the SHA-256 `$4` next to the folder `$1` of the attempt.
const DOWNLOAD: &str = "helper=\"$1.uwe\"\n{ curl -fsSL -o \"$helper\" \"$3\" || wget -q -O \
                        \"$helper\" \"$3\"; } && echo \"$4  $helper\" | sha256sum -c - > \
                        /dev/null && chmod +x \"$helper\" || { echo \"uwe: cannot get the \
                        helper; the image needs curl or wget and sha256sum\" >&2; exit 125; }\n";

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
            // A downloaded binary runs only on its own machine.
            if let Some(machine) = machine {
                script.push_str(&format!(
                    "[ \"$(uname -m)\" = {machine} ] || {{ echo \"uwe: the helper runs on \
                     {machine}, but this host is $(uname -m)\" >&2; exit 125; }}\n"
                ));
            }
            script.push_str(DOWNLOAD);
            words.extend([url.to_owned(), sha256.to_owned()]);
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
    }
}

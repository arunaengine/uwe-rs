//! nf-lakefs 0.7.0: `lakefs://` paths through the lakeFS provider of storage, branches made on
//! demand, and commits by `lakefsCommit` and at the end of a successful run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["lakefsCommit"],
    call: Some(call),
    scopes: &["lakefs"],
    observer: Some(activate),
    ..Plugin::new("nf-lakefs", Kind::FileSystem, State::Implemented)
};

/// Hands the `lakefs` scope to the provider of `lakefs://` paths. With `autoCreateBranch` or
/// `autoCommit` and a `lakefs://` `outputDir`, an observer makes and commits its branch.
fn activate(
    config: &Config,
    _: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let text = |key| {
        (config.get(&["lakefs", key]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    let flag = |key| text(key).is_some_and(|text| text == "true");
    if text("transferMode").is_some_and(|mode| mode == "physical_path") {
        tracing::warn!(
            "lakefs.transferMode = 'physical_path' is not supported by uwe; data moves through \
             presigned addresses"
        );
    }
    let create = flag("autoCreateBranch");
    storage::lakefs::use_scope(storage::lakefs::Options {
        endpoint: text("apiUrl"),
        access_key: text("accessKey"),
        secret_key: text("secretKey"),
        create_branches: create,
        branch_source: text("autoCreateBranchSource"),
        timeout: text("readTimeout").and_then(|text| duration(&text)),
    });
    let output = (config.get(&["outputDir"]))
        .map(|value| value.to_string())
        .filter(|output| output.starts_with("lakefs://"));
    let commit = flag("autoCommit");
    Ok(output
        .filter(|_| create || commit)
        .map(|output| -> Box<dyn Observer> {
            Box::new(Branch {
                output,
                create,
                commit,
                message: None,
            })
        }))
}

/// `lakefsCommit(path, message)`: commits the branch of the path and returns the commit id.
fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    let thrown = |message: String| Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    };
    match (name, arguments.as_slice()) {
        ("lakefsCommit", [path, message]) => {
            let id = storage::lakefs::commit(&path.to_string(), &message.to_string());
            id.map(|id| Value::string(id.as_str()))
                .map_err(|error| thrown(error.to_string()))
        }
        ("lakefsCommit", _) => Err(thrown(
            "lakefsCommit takes a lakefs:// path and a commit message".to_owned(),
        )),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-lakefs`"),
            span: Default::default(),
        }),
    }
}

/// The branch of `outputDir`: made when the run starts, committed when it succeeds.
struct Branch {
    output: String,
    create: bool,
    commit: bool,
    /// `params.lakefs_commit_message`, else uwe's message with the run name.
    message: Option<String>,
}

impl Observer for Branch {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => {
                let message = start.params["lakefs_commit_message"].as_str();
                self.message = Some(match message {
                    Some(message) => message.to_owned(),
                    None => format!("Run {}", start.run_name),
                });
                if self.create
                    && let Err(error) = storage::lakefs::create_branch(&self.output)
                {
                    tracing::warn!("{error}");
                }
            }
            Event::RunEnded(end) if end.success && self.commit => {
                let message = self.message.clone().unwrap_or_default();
                if let Err(error) = storage::lakefs::commit(&self.output, &message) {
                    tracing::warn!("{error}");
                }
            }
            _ => {}
        }
    }
}

/// A duration like `60s`, `500ms` or `2m`.
fn duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let number: u64 = text[..split].parse().ok()?;
    match text[split..].trim() {
        "ms" => Some(Duration::from_millis(number)),
        "s" | "sec" => Some(Duration::from_secs(number)),
        "m" | "min" => Some(Duration::from_secs(number * 60)),
        "h" => Some(Duration::from_secs(number * 3600)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_durations() {
        assert_eq!(duration("60s"), Some(Duration::from_secs(60)));
        assert_eq!(duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(duration("soon"), None);
    }
}

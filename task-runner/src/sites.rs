//! Selects the execution site near the largest number of a task's input files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use executor::{Backend, ExecutorError, Finished, Job};
use tokio_util::sync::CancellationToken;
use workflow_graph::Task;

/// A site: its name, and the address prefixes or folders of the data near it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Site {
    pub name: String,
    pub near: Vec<String>,
}

/// Which site runs each task. Without sites, every task runs on the default server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteRule {
    /// In the order of the config file, which breaks ties.
    pub sites: Vec<Site>,
}

impl SiteRule {
    /// The site near most of the input files of a task: an input is near the site with the
    /// longest prefix that its address or path starts with. A tie goes to the site named
    /// first. `None` for a task near no site.
    pub fn site(&self, task: &Task) -> Option<&str> {
        let mut counts = vec![0_usize; self.sites.len()];
        for input in task.inputs.iter().filter(|input| input.content.is_none()) {
            if let Some(site) = self.nearest(&input.source) {
                counts[site] += 1;
            }
        }
        let most = counts.iter().copied().max().filter(|most| *most > 0)?;
        let first = counts.iter().position(|count| *count == most)?;
        Some(&self.sites[first].name)
    }

    /// The site with the longest prefix that `place` starts with.
    fn nearest(&self, place: &str) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None;
        for (index, site) in self.sites.iter().enumerate() {
            for prefix in site.near.iter().filter(|prefix| starts_with(place, prefix)) {
                if best.is_none_or(|(_, length)| prefix.len() > length) {
                    best = Some((index, prefix.len()));
                }
            }
        }
        best.map(|(index, _)| index)
    }
}

/// Whether `place` starts with `prefix`, where the prefix ends at a name: `s3://hh` is a
/// prefix of `s3://hh/a.txt`, not of `s3://hh-data/a.txt`.
fn starts_with(place: &str, prefix: &str) -> bool {
    match place.strip_prefix(prefix) {
        Some(rest) => prefix.ends_with('/') || rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// A backend for each site, and a default one: each task runs on the backend of the site that
/// its placement names (`Placement::executor`), the others on the default one.
pub struct Sites<B> {
    pub default: B,
    pub sites: BTreeMap<String, B>,
}

impl<B> Sites<B> {
    /// Only the default backend.
    pub fn one(default: B) -> Self {
        Sites {
            default,
            sites: BTreeMap::new(),
        }
    }

    /// The backend of a task's site.
    fn of(&self, job: &Job<'_>) -> &B {
        let site = job.placement.executor.as_ref();
        site.and_then(|site| self.sites.get(site))
            .unwrap_or(&self.default)
    }
}

impl<B: Backend> Backend for Sites<B> {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        self.default.pin(image).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        self.of(&job).run(job, cancel).await
    }

    fn hold(&self) {
        self.default.hold();
        for backend in self.sites.values() {
            backend.hold();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use task_model::{FORMAT, Placement, TaskSpec};
    use workflow_graph::Input;

    fn rule() -> SiteRule {
        let site = |name: &str, near: &[&str]| Site {
            name: name.into(),
            near: near.iter().map(|prefix| (*prefix).to_owned()).collect(),
        };
        SiteRule {
            sites: vec![
                site("hamburg", &["s3://hh", "/mnt/hamburg/"]),
                site("heidelberg", &["s3://hd/", "s3://hh/imaging/"]),
            ],
        }
    }

    fn task(sources: &[&str]) -> Task {
        let inputs = (sources.iter())
            .map(|source| Input {
                path: "in".into(),
                source: (*source).into(),
                ..Input::default()
            })
            .collect();
        Task {
            inputs,
            ..Task::default()
        }
    }

    #[test]
    fn selects_nearest_site() {
        let rule = rule();
        assert_eq!(
            rule.site(&task(&["s3://hh/a", "s3://hd/b", "s3://hd/c"])),
            Some("heidelberg")
        );
        assert_eq!(rule.site(&task(&["/mnt/hamburg/a.txt"])), Some("hamburg"));
        // The longest prefix wins, and a prefix ends at a name.
        assert_eq!(
            rule.site(&task(&["s3://hh/imaging/x.tif"])),
            Some("heidelberg")
        );
        assert_eq!(rule.site(&task(&["s3://hh-other/a"])), None);
        // A tie goes to the site named first; a task near no site has none.
        assert_eq!(
            rule.site(&task(&["s3://hd/b", "s3://hh/a"])),
            Some("hamburg")
        );
        assert_eq!(rule.site(&task(&["https://example.org/a"])), None);
        assert_eq!(SiteRule::default().site(&task(&["s3://hh/a"])), None);
    }

    /// Remembers the jobs it was given, and fails them.
    #[derive(Default)]
    struct Named(Mutex<Vec<String>>);

    impl Backend for Named {
        async fn pin(&self, _image: &str) -> Result<Option<String>, ExecutorError> {
            Ok(None)
        }

        async fn run(
            &self,
            job: Job<'_>,
            _cancel: &CancellationToken,
        ) -> Result<Finished, ExecutorError> {
            self.0.lock().expect("a lock").push(job.name.to_owned());
            Err(ExecutorError::Cancelled)
        }
    }

    #[test]
    fn uses_site_backend() {
        let sites = Sites {
            default: Named::default(),
            sites: [("hamburg".to_owned(), Named::default())].into(),
        };
        let spec = TaskSpec {
            run_as: None,
            format: FORMAT,
            namespace: String::new(),
            image: None,
            arguments: Vec::new(),
            workdir: None,
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        for (name, site) in [
            ("a", Some("hamburg")),
            ("b", None),
            ("c", Some("elsewhere")),
        ] {
            let placement = Placement {
                executor: site.map(str::to_owned),
                ..Placement::default()
            };
            let job = Job {
                name,
                spec: &spec,
                staging: &Default::default(),
                placement: &placement,
            };
            let _ = runtime.block_on(sites.run(job, &CancellationToken::new()));
        }
        assert_eq!(*sites.sites["hamburg"].0.lock().expect("a lock"), ["a"]);
        assert_eq!(*sites.default.0.lock().expect("a lock"), ["b", "c"]);
    }
}

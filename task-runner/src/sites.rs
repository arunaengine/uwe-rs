//! Selects the execution site of a task: the one it names, else one that can run it near the
//! largest number of its input files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use executor::orphans::Orphan;
use executor::{Backend, ExecutorError, Finished, Job};
use tokio_util::sync::CancellationToken;
use workflow_graph::Task;

/// A site: its name, the data near it, and what its backend can run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Site {
    pub name: String,
    /// The address prefixes or folders of the data near the site.
    pub near: Vec<String>,
    /// The storage endpoints near the site, as storage providers name the endpoints that hold
    /// the bytes of a file, like the ids of Aruna data proxies.
    pub endpoints: Vec<String>,
    /// Whether the site runs tasks that ask for GPUs.
    pub gpus: bool,
    /// The most memory a task of the site may ask for, in bytes; `None` for no limit.
    pub max_memory: Option<u64>,
    /// The most CPUs a task of the site may ask for; `None` for no limit.
    pub max_cpus: Option<f64>,
}

impl Site {
    /// Whether the site can run the task, by the GPUs, memory and CPUs that it asks for.
    pub fn fits(&self, task: &Task) -> bool {
        let resources = &task.resources;
        (self.gpus || resources.gpus.unwrap_or(0) == 0)
            && (self.max_memory)
                .is_none_or(|most| resources.memory_bytes.is_none_or(|bytes| bytes <= most))
            && (self.max_cpus).is_none_or(|most| resources.cpus.is_none_or(|cpus| cpus <= most))
    }
}

/// Which site runs each task. Without sites, every task runs on the default backend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteRule {
    /// In the order of the config file, which breaks ties.
    pub sites: Vec<Site>,
    /// What the default backend can run: a task that it cannot run and that is near no site
    /// goes to the first site that can. Its name and data are not used.
    pub default: Site,
}

impl SiteRule {
    /// The site of a task: the one it names, which must be a site of the rule, else the
    /// site near most of its inputs.
    pub fn choose<'a>(&'a self, task: &'a Task) -> Result<Option<&'a str>, String> {
        self.choose_near(task, &HashMap::new())
    }

    /// The site of a task as [`SiteRule::choose`] gives it, where `endpoints` names the
    /// storage endpoints that hold each remote input, by its address.
    pub fn choose_near<'a>(
        &'a self,
        task: &'a Task,
        endpoints: &HashMap<String, Vec<String>>,
    ) -> Result<Option<&'a str>, String> {
        match &task.site {
            Some(name) => match self.sites.iter().find(|site| site.name == *name) {
                Some(site) if site.fits(task) => Ok(Some(name)),
                Some(_) => Err(format!(
                    "the task is placed at {name}, which cannot give it the GPUs, memory or \
                     CPUs that it asks for"
                )),
                None => Err(format!(
                    "the task is placed at {name}, a site this run lacks"
                )),
            },
            None => match self.site_near(task, endpoints) {
                Some(site) => Ok(Some(site)),
                None if self.sites.is_empty() || self.default.fits(task) => Ok(None),
                None => Err(
                    "no site of the run, nor its executor, can give the task the GPUs, memory \
                     or CPUs that it asks for"
                        .to_owned(),
                ),
            },
        }
    }

    /// Whether choosing needs the storage endpoints of the inputs.
    pub fn uses_endpoints(&self) -> bool {
        self.sites.iter().any(|site| !site.endpoints.is_empty())
    }

    /// The site near most of the input files of a task: an input is near the site with the
    /// longest prefix that its address or path starts with. A tie goes to the site named
    /// first. `None` for a task near no site.
    pub fn site(&self, task: &Task) -> Option<&str> {
        self.site_near(task, &HashMap::new())
    }

    /// The site that can run a task near most of its inputs, by prefix or by the endpoints
    /// that hold them; else the first that can run a task that the default backend cannot.
    fn site_near(&self, task: &Task, endpoints: &HashMap<String, Vec<String>>) -> Option<&str> {
        let mut counts = vec![0_usize; self.sites.len()];
        for input in task.inputs.iter().filter(|input| input.content.is_none()) {
            let held = endpoints.get(&input.source).map_or(&[][..], Vec::as_slice);
            if let Some(site) = self.nearest(task, &input.source, held) {
                counts[site] += 1;
            }
        }
        if let Some(most) = counts.iter().copied().max().filter(|most| *most > 0) {
            let first = counts.iter().position(|count| *count == most)?;
            return Some(&self.sites[first].name);
        }
        if self.default.fits(task) {
            return None;
        }
        let first = self.sites.iter().find(|site| site.fits(task))?;
        Some(&first.name)
    }

    /// The site that can run `task` with the longest prefix that `place` starts with, else
    /// the first whose endpoints hold it, from the nearest endpoint on.
    fn nearest(&self, task: &Task, place: &str, held: &[String]) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None;
        for (index, site) in self.sites.iter().enumerate() {
            if !site.fits(task) {
                continue;
            }
            for prefix in site.near.iter().filter(|prefix| starts_with(place, prefix)) {
                if best.is_none_or(|(_, length)| prefix.len() > length) {
                    best = Some((index, prefix.len()));
                }
            }
        }
        if let Some((index, _)) = best {
            return Some(index);
        }
        held.iter().find_map(|endpoint| {
            (self.sites.iter())
                .position(|site| site.fits(task) && site.endpoints.contains(endpoint))
        })
    }
}

/// The endpoints that hold each remote input of a task, as its storage provider tells them.
/// An input whose provider does not tell has none.
pub async fn input_endpoints(
    storage: &storage::Storage,
    task: &Task,
) -> HashMap<String, Vec<String>> {
    let mut found = HashMap::new();
    let remote = (task.inputs.iter())
        .filter(|input| input.content.is_none() && input.source.contains("://"));
    for input in remote {
        if let Ok(Some(details)) = storage.describe(&input.source).await
            && !details.endpoints.is_empty()
        {
            found.insert(input.source.clone(), details.endpoints);
        }
    }
    found
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

    /// The backend of the site of the task pins its image, as it runs the task.
    async fn pin_at(
        &self,
        image: &str,
        site: Option<&str>,
    ) -> Result<Option<String>, ExecutorError> {
        match site.and_then(|site| self.sites.get(site)) {
            Some(backend) => backend.pin(image).await,
            None => self.default.pin(image).await,
        }
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

    async fn release(&self) {
        self.default.release().await;
        for backend in self.sites.values() {
            backend.release().await;
        }
    }

    /// Only when every site fetches direct sources, as a task may go to any of them.
    fn direct_sources(&self) -> bool {
        self.default.direct_sources() && self.sites.values().all(Backend::direct_sources)
    }

    fn supports_direct(&self, reference: &storage::DirectRef) -> bool {
        self.default.supports_direct(reference)
            && (self.sites.values()).all(|site| site.supports_direct(reference))
    }

    /// The jobs that the run left on any site, each once when sites share their journals.
    async fn orphans(&self, run: &str) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = self.default.orphans(run).await?;
        for backend in self.sites.values() {
            found.extend(backend.orphans(run).await?);
        }
        Ok(once(found))
    }

    async fn stop_orphans(&self, run: &str) -> Result<Vec<Orphan>, ExecutorError> {
        let mut found = self.default.stop_orphans(run).await?;
        for backend in self.sites.values() {
            found.extend(backend.stop_orphans(run).await?);
        }
        Ok(once(found))
    }
}

/// The orphans without a second report of an attempt. Sites that share journals each report
/// every attempt; the site that owns one tells more than one that keeps it or is unsure.
fn once(found: Vec<Orphan>) -> Vec<Orphan> {
    use executor::orphans::Outcome;
    let unsure = |orphan: &Orphan| matches!(orphan.outcome, Outcome::Unclear(_) | Outcome::Kept(_));
    let mut kept: Vec<Orphan> = Vec::new();
    let mut at = BTreeMap::new();
    for orphan in found {
        match at.get(&orphan.attempt) {
            Some(&index) if unsure(&kept[index]) && !unsure(&orphan) => kept[index] = orphan,
            Some(_) => {}
            None => {
                at.insert(orphan.attempt.clone(), kept.len());
                kept.push(orphan);
            }
        }
    }
    kept
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
            ..Site::default()
        };
        SiteRule {
            sites: vec![
                site("hamburg", &["s3://hh", "/mnt/hamburg/"]),
                site("heidelberg", &["s3://hd/", "s3://hh/imaging/"]),
            ],
            ..SiteRule::default()
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

    #[test]
    fn keeps_named_site() {
        let rule = rule();
        let mut placed = task(&["s3://hh/a"]);
        placed.site = Some("heidelberg".into());
        assert_eq!(rule.choose(&placed), Ok(Some("heidelberg")));
        placed.site = Some("elsewhere".into());
        let error = rule.choose(&placed).expect_err("no such site");
        assert_eq!(
            error,
            "the task is placed at elsewhere, a site this run lacks"
        );
        let empty = SiteRule::default();
        assert!(
            empty.choose(&placed).is_err(),
            "a run without sites never falls back"
        );
        let near = task(&["s3://hh/a"]);
        assert_eq!(rule.choose(&near), Ok(Some("hamburg")));
    }

    #[test]
    fn refuses_unfit_sites() {
        let small = Site {
            name: "small".into(),
            max_cpus: Some(2.0),
            ..Site::default()
        };
        let rule = SiteRule {
            sites: vec![small],
            default: Site {
                max_cpus: Some(4.0),
                ..Site::default()
            },
        };
        let asking = |cpus: f64, site: Option<&str>| Task {
            resources: workflow_graph::Resources {
                cpus: Some(cpus),
                ..Default::default()
            },
            site: site.map(str::to_owned),
            ..Task::default()
        };
        let error = rule
            .choose(&asking(3.0, Some("small")))
            .expect_err("too many CPUs");
        assert!(error.contains("cannot give it"), "{error}");
        assert_eq!(rule.choose(&asking(1.0, Some("small"))), Ok(Some("small")));
        assert_eq!(
            rule.choose(&asking(3.0, None)),
            Ok(None),
            "the executor runs it"
        );
        let error = rule.choose(&asking(8.0, None)).expect_err("no site fits");
        assert!(error.contains("no site of the run"), "{error}");
        // Without sites, the executor runs every task.
        assert_eq!(SiteRule::default().choose(&asking(8.0, None)), Ok(None));
    }

    #[test]
    fn pins_at_site() {
        let sites = Sites {
            default: Named::default(),
            sites: [("hamburg".to_owned(), Named(Mutex::default(), "hh"))].into(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let pinned = |site| runtime.block_on(sites.pin_at("debian", site)).unwrap();
        assert_eq!(pinned(Some("hamburg")).as_deref(), Some("debian@hh"));
        assert_eq!(pinned(None).as_deref(), Some("debian@default"));
        assert_eq!(pinned(Some("elsewhere")).as_deref(), Some("debian@default"));
    }

    /// Remembers the jobs it was given, and fails them; it pins an image with its own mark.
    struct Named(Mutex<Vec<String>>, &'static str);

    impl Default for Named {
        fn default() -> Self {
            Named(Mutex::default(), "default")
        }
    }

    impl Backend for Named {
        async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
            Ok(Some(format!("{image}@{}", self.1)))
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
            output_manifest: None,
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

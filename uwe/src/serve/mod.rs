//! The studio of `uwe serve` and `uwe page`: the backend that starts, follows and stops runs
//! for the page and for GA4GH WES clients, with the tool library of the combiner.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod active;
mod composer;
mod discover;
mod launch;

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use run_database::RunStore;
use run_page::{
    Capabilities, ComposerRequest, Described, ExportRequest, Exported, ImportRequest,
    LaunchRequest, Launched, Refusal, Reply, RunStarted, ScanRequest, Studio, SuggestRequest,
    ToolFilter, WorkflowEntry,
};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::Failure;
use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::tools::ToolLibrary;
use active::Registry;

/// The languages that a run of the studio can be in, as `-lang` names them.
const LANGUAGES: [&str; 7] = [
    "nextflow",
    "snakemake",
    "cwl",
    "argo",
    "wdl",
    "galaxy",
    "wir",
];

/// How many runs of the studio may go on at once unless the options say otherwise.
const RUN_LIMIT: usize = 64;

/// What the studio serves, and what its runs use.
#[derive(Clone)]
pub struct Options {
    /// Files and folders whose tools the library holds from the start.
    pub tools: Vec<PathBuf>,
    /// Folders whose workflows the launcher lists; imports find the tools they name there.
    pub workflows: Vec<PathBuf>,
    /// The uwe config file of the runs and of the presets.
    pub config_file: Option<PathBuf>,
    /// The run database that the runs write and the page reads.
    pub database: PathBuf,
    /// The cache store of the runs, like `-store`.
    pub store: Option<String>,
    /// The run store of a host instead of `database`.
    pub run_store: Option<Arc<dyn RunStore>>,
    /// The cache folder of the runs, like `[cache] folder`; `None` uses that of uwe.
    pub cache_folder: Option<PathBuf>,
    /// Makes the setup of each run, instead of reading the config files.
    pub setup: Option<Arc<dyn Fn() -> Setup + Send + Sync>>,
    /// Where the combiner writes the composed workflows that it runs and lowers.
    pub folder: PathBuf,
    /// How many runs may go on at once; the studio refuses more until one ends.
    pub run_limit: usize,
}

impl Options {
    pub fn new(database: PathBuf, folder: PathBuf) -> Options {
        Options {
            tools: Vec::new(),
            workflows: Vec::new(),
            config_file: None,
            database,
            store: None,
            run_store: None,
            cache_folder: None,
            setup: None,
            folder,
            run_limit: RUN_LIMIT,
        }
    }
}

/// The studio of one server: the runs it started, each on a thread of its own, and the tool
/// library. Construction reads the tools and starts nothing; `stop` ends the runs.
pub struct Backend {
    options: Options,
    /// The tool library as the last change left it. A change edits a copy and swaps it in,
    /// so that the page reads the library without waiting for a scan.
    library: Arc<RwLock<Arc<ToolLibrary>>>,
    /// Lets one change at a time copy the library, so that no change loses another.
    changing: Arc<Mutex<()>>,
    registry: Arc<Registry>,
}

impl Backend {
    pub fn new(options: Options) -> Backend {
        let mut library = ToolLibrary::new();
        for path in &options.tools {
            if path.is_dir() {
                library.scan(path);
            } else if let Err(reason) = library.add_file(path) {
                tracing::warn!("uwe: {} holds no tools: {reason}", path.display());
            }
        }
        Backend {
            options,
            library: Arc::new(RwLock::new(Arc::new(library))),
            changing: Arc::default(),
            registry: Arc::default(),
        }
    }

    /// Asks every run that the studio started to stop, and waits until each is recorded. The
    /// number of runs it waited for.
    pub fn stop(&self) -> usize {
        self.registry.stop()
    }

    /// The tool library as it is now.
    fn read(&self) -> Arc<ToolLibrary> {
        current(&self.library)
    }

    /// The folders that an isolated server lets runs and the library reach; `None` for any.
    fn roots(&self) -> Option<Vec<PathBuf>> {
        let isolation = self.options.setup.as_ref()?().isolation?;
        Some(
            isolation
                .roots
                .into_iter()
                .chain(isolation.launch)
                .collect(),
        )
    }

    /// The run store that the runs of the studio write.
    fn store(&self) -> Result<Arc<dyn RunStore>, Refusal> {
        if let Some(store) = &self.options.run_store {
            return Ok(store.clone());
        }
        let opened = run_database::SqliteStore::open(&self.options.database);
        Ok(Arc::new(
            opened.map_err(|error| Refusal::failed(error.to_string()))?,
        ))
    }

    /// Changes a copy of the library on a thread for blocking work, as scans read many files,
    /// and swaps it in when the change succeeds.
    async fn change(
        &self,
        work: impl FnOnce(&mut ToolLibrary) -> Result<Value, Refusal> + Send + 'static,
    ) -> Result<Value, Refusal> {
        let (library, changing) = (self.library.clone(), self.changing.clone());
        let done = tokio::task::spawn_blocking(move || {
            let _turn = changing.lock().unwrap_or_else(PoisonError::into_inner);
            let mut copy = ToolLibrary::clone(&current(&library));
            let answer = work(&mut copy)?;
            *library.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(copy);
            Ok(answer)
        });
        done.await
            .map_err(|error| Refusal::failed(error.to_string()))?
    }
}

/// The library that `shared` holds now, also after a thread panicked while it swapped it.
fn current(shared: &RwLock<Arc<ToolLibrary>>) -> Arc<ToolLibrary> {
    shared
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

impl Studio for Backend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            executors: crate::setup::EXECUTORS
                .iter()
                .map(|name| name.to_string())
                .collect(),
            languages: LANGUAGES.iter().map(|name| name.to_string()).collect(),
        }
    }

    fn workflows(&self) -> Reply<'_, Vec<WorkflowEntry>> {
        let folders = self.options.workflows.clone();
        let config = self.options.config_file.clone();
        Box::pin(async move {
            let found = tokio::task::spawn_blocking(move || {
                discover::workflows(&folders, config.as_deref())
            });
            found
                .await
                .map_err(|error| Refusal::failed(error.to_string()))
        })
    }

    fn describe(&self, request: LaunchRequest) -> Reply<'_, Described> {
        Box::pin(async move { launch::describe(self, &request).await })
    }

    fn validate(&self, request: LaunchRequest) -> Reply<'_> {
        Box::pin(async move { launch::validate(self, &request).await })
    }

    fn launch(&self, request: LaunchRequest) -> Reply<'_, RunStarted> {
        Box::pin(launch::launch(self, request))
    }

    fn repeat(&self, run: String, changes: LaunchRequest) -> Reply<'_, RunStarted> {
        Box::pin(async move { launch::repeat(self, &run, changes).await })
    }

    fn cancel(&self, run: &str) -> bool {
        self.registry.cancel(run)
    }

    fn launched(&self, run: &str) -> Option<Launched> {
        self.registry.launched(run)
    }

    fn launches(&self) -> Vec<Launched> {
        self.registry.launches()
    }

    fn follow(&self, run: &str) -> Option<broadcast::Receiver<Value>> {
        self.registry.follow(run)
    }

    fn tools(&self, filter: ToolFilter) -> Reply<'_> {
        Box::pin(async move { composer::tools(&self.read(), &filter) })
    }

    fn tool(&self, id: String, version: String) -> Reply<'_> {
        Box::pin(async move { composer::tool(&self.read(), &id, &version) })
    }

    fn scan(&self, request: ScanRequest) -> Reply<'_> {
        let roots = self.roots();
        let work = move |library: &mut ToolLibrary| {
            let path = Path::new(&request.path);
            if roots.as_ref().is_some_and(|roots| !contained(path, roots)) {
                return Err(Refusal::invalid(OUTSIDE));
            }
            composer::scan(library, &request)
        };
        Box::pin(self.change(work))
    }

    fn suggest(&self, request: SuggestRequest) -> Reply<'_> {
        Box::pin(async move { composer::suggest(&self.read(), &request) })
    }

    fn check(&self, request: ComposerRequest) -> Reply<'_> {
        Box::pin(async move { composer::check(&self.read(), &request) })
    }

    fn export(&self, request: ExportRequest) -> Reply<'_, Exported> {
        Box::pin(async move { composer::export(&self.read(), &self.options.folder, &request) })
    }

    fn import(&self, request: ImportRequest) -> Reply<'_> {
        let folders = self.options.workflows.clone();
        let roots = self.roots();
        let work = move |library: &mut ToolLibrary| {
            composer::import(library, (&folders, roots.as_deref()), &request)
        };
        Box::pin(self.change(work))
    }

    fn run(&self, request: ComposerRequest) -> Reply<'_, RunStarted> {
        Box::pin(async move {
            let target = composer::prepared(&self.read(), &self.options.folder, &request)?;
            launch::launch(self, composed_launch(request, &target)).await
        })
    }
}

/// Why an isolated server refuses a path that a request names.
const OUTSIDE: &str = "the path is outside the folders that this server lets runs reach";

/// Whether a path, which may not exist yet, is inside one of `roots` once its links resolve.
fn contained(path: &Path, roots: &[PathBuf]) -> bool {
    if !path.is_absolute() || (path.components()).any(|part| part == Component::ParentDir) {
        return false;
    }
    let (mut existing, mut missing) = (path, Vec::new());
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                existing = parent;
            }
            _ => return false,
        }
    }
    let Ok(mut resolved) = existing.canonicalize() else {
        return false;
    };
    resolved.extend(missing.iter().rev());
    (roots.iter()).any(|root| {
        root.canonicalize()
            .is_ok_and(|root| resolved.starts_with(root))
    })
}

/// The launch request of a composed workflow written to `target`, with its inputs as values.
fn composed_launch(request: ComposerRequest, target: &Path) -> LaunchRequest {
    LaunchRequest {
        target: Some(target.to_string_lossy().into_owned()),
        values: Some(request.inputs),
        executor: request.executor,
        no_network: request.no_network,
        no_cache: request.no_cache,
        default_image: request.default_image,
        output_dir: request.output_dir,
        ..LaunchRequest::default()
    }
}

/// `uwe serve`: the page with the launcher, the task explorer and the combiner, and the GA4GH
/// WES API, until Ctrl-C; then it stops the runs it started and waits for their records.
pub(crate) fn command(arguments: &Arguments) -> Result<(), Failure> {
    let backend = Arc::new(Backend::new(options(arguments, true)?));
    let studio: Arc<dyn Studio> = backend.clone();
    let server = crate::page::serve(arguments, "/", Some(studio), false)?;
    eprintln!("uwe: GA4GH WES at {}/ga4gh/wes/v1", server.url());
    eprintln!("uwe: press Ctrl-C to stop the server");
    server.serve_until_interrupted();
    let stopped = backend.stop();
    if stopped > 0 {
        eprintln!("uwe: waited for {stopped} runs to stop");
    }
    Ok(())
}

/// The options of the studio from `--tools`, `--workflows` and the options of runs. `uwe serve`
/// lists the workflows of the current folder when no `--workflows` names others.
pub(crate) fn options(arguments: &Arguments, serving: bool) -> Result<Options, Failure> {
    let mut workflows = arguments.workflows.clone();
    if workflows.is_empty() && serving {
        let here = std::env::current_dir().map_err(|error| Failure::setup(error.to_string()))?;
        workflows.push(here);
    }
    let cache = (arguments.cache_folder.clone()).unwrap_or_else(crate::cache_directory);
    let mut options = Options::new(crate::runs::database_path(arguments), cache.join("studio"));
    options.tools.clone_from(&arguments.tools);
    options.workflows = workflows;
    options.config_file.clone_from(&arguments.config_file);
    options.store.clone_from(&arguments.store);
    options.cache_folder.clone_from(&arguments.cache_folder);
    options.run_store = arguments.run_store.as_ref().map(|host| host.0.clone());
    Ok(options)
}

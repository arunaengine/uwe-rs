//! Loads a script and the scripts it includes: parses, lowers and links them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::lower::{Imports, lower_script};
use nextflow_evaluator::lowered::{DefinitionKind, Library, LibraryClass, Link, Module};
use nextflow_parser::{LineIndex, parse_script};

/// Why a script could not be loaded: every problem, each with its file and position.
#[derive(Debug)]
pub struct LoadError {
    pub messages: Vec<String>,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.messages.join("\n"))
    }
}

impl std::error::Error for LoadError {}

/// Loads scripts, each file once.
#[derive(Default)]
pub struct Loader {
    modules: HashMap<PathBuf, Rc<Module>>,
    /// Legacy syntax: the scripts that includes with `addParams(...)` or `params(...)` load
    /// again, as each such include runs a script of its own, by path, and by the including
    /// script and the position of the include.
    copies: HashMap<(PathBuf, PathBuf, u32), Rc<Module>>,
    /// The modules that stand for plugins, by plugin id.
    plugins: HashMap<Rc<str>, Rc<Module>>,
    /// The scripts that port plugin functions, by plugin id.
    plugin_scripts: HashMap<Rc<str>, Rc<Module>>,
    /// The plugins whose script is being loaded; that script may include their native functions.
    porting: HashSet<Rc<str>>,
    loading: HashSet<PathBuf>,
    /// The scripts that could not be loaded, whose errors are already reported.
    failed: HashSet<PathBuf>,
    errors: Vec<String>,
    /// The Groovy classes of the `lib/` directory next to the main script, which every script
    /// can use.
    library: Rc<Library>,
    /// The names of the classes of `library` that are not nested in another.
    library_names: Vec<Rc<str>>,
    /// The directory of the main script, `projectDir`.
    project: PathBuf,
    /// Whether a script has legacy syntax, which only `NXF_SYNTAX_PARSER=v1` runs.
    legacy: bool,
}

impl Loader {
    /// Loads a script file with everything it includes.
    pub fn load(&mut self, path: &Path) -> Result<Rc<Module>, LoadError> {
        if let Some(directory) = path.parent() {
            self.project = std::path::absolute(directory).unwrap_or(directory.to_owned());
            self.load_library(&directory.join("lib"));
        }
        let module = self.load_file(path);
        match module {
            Some(module) if self.errors.is_empty() => Ok(module),
            _ => Err(LoadError {
                messages: std::mem::take(&mut self.errors),
            }),
        }
    }

    /// Loads the main script, then the script at `scope` again with `extra` code after its
    /// text, which sees the functions and includes of that script.
    pub fn load_extended(
        &mut self,
        main: &Path,
        scope: &Path,
        extra: &str,
    ) -> Result<Rc<Module>, LoadError> {
        self.load(main)?;
        let path = std::path::absolute(scope).unwrap_or_else(|_| scope.to_owned());
        let text = std::fs::read_to_string(&path).map_err(|error| LoadError {
            messages: vec![format!("{}: {error}", path.display())],
        })?;
        let module = self.lower(&format!("{text}\n{extra}"), &path);
        let module = Rc::new(module.ok_or_else(|| LoadError {
            messages: std::mem::take(&mut self.errors),
        })?);
        if let Some(directory) = path.parent() {
            *module.directory.borrow_mut() = Some(directory.to_string_lossy().into());
        }
        self.add_classes(&module);
        self.link(&module, &path);
        match self.errors.is_empty() {
            true => Ok(module),
            false => Err(LoadError {
                messages: std::mem::take(&mut self.errors),
            }),
        }
    }

    fn load_file(&mut self, path: &Path) -> Option<Rc<Module>> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        if let Some(module) = self.modules.get(&path) {
            return Some(module.clone());
        }
        if self.failed.contains(&path) {
            return None;
        }
        if !self.loading.insert(path.clone()) {
            self.errors
                .push(format!("{}: the script includes itself", path.display()));
            return None;
        }
        let module = self.read(&path);
        self.loading.remove(&path);
        let Some(module) = module else {
            // Another include of the script does not report its errors again.
            self.failed.insert(path);
            return None;
        };
        let module = Rc::new(module);
        if let Some(directory) = path.parent() {
            *module.directory.borrow_mut() = Some(directory.to_string_lossy().into());
        }
        self.add_classes(&module);
        self.link(&module, &path);
        self.modules.insert(path, module.clone());
        Some(module)
    }

    /// A script loaded again for an include with params, once for each include.
    fn copy(&mut self, path: &Path, include: (PathBuf, u32)) -> Option<Rc<Module>> {
        let key = (path.to_owned(), include.0, include.1);
        if let Some(module) = self.copies.get(&key) {
            return Some(module.clone());
        }
        let module = Rc::new(self.read(path)?);
        if let Some(directory) = path.parent() {
            *module.directory.borrow_mut() = Some(directory.to_string_lossy().into());
        }
        self.add_classes(&module);
        self.link(&module, path);
        self.copies.insert(key, module.clone());
        Some(module)
    }

    /// Gives a script that defines classes a library of its own: the classes of `lib/` and
    /// its own, which only its code can name, as in Groovy.
    fn add_classes(&mut self, module: &Rc<Module>) {
        if module.classes.is_empty() {
            return;
        }
        let library = Library::default();
        let mut classes = library.classes.borrow_mut();
        classes.extend(
            self.library
                .classes
                .borrow()
                .iter()
                .map(|(name, class)| (name.clone(), class.clone())),
        );
        for def in &module.classes {
            let class = LibraryClass {
                def: def.clone(),
                module: module.clone(),
                statics: std::cell::RefCell::new(None),
                values: std::cell::RefCell::new(None),
            };
            classes.insert(def.name.clone(), Rc::new(class));
        }
        drop(classes);
        *module.library.borrow_mut() = Some(Rc::new(library));
    }

    fn read(&mut self, path: &Path) -> Option<Module> {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                self.errors.push(format!("{}: {error}", path.display()));
                return None;
            }
        };
        let module = self.lower(&source, path)?;
        self.legacy |= !module.legacy.is_empty();
        Some(module)
    }

    /// Whether a loaded script has legacy syntax, so that Nextflow 26.04.6 runs it only with
    /// `NXF_SYNTAX_PARSER=v1`, which also reads command-line params as numbers and booleans.
    pub fn legacy(&self) -> bool {
        self.legacy
    }

    /// Loads the Groovy classes of a `lib/` directory, as Nextflow adds the directory to its
    /// class path. Their names are known before any code is lowered, so that classes and scripts
    /// can use each other.
    fn load_library(&mut self, directory: &Path) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "groovy")
            })
            .collect();
        files.sort();
        let mut parsed = Vec::new();
        for file in files {
            let source = match std::fs::read_to_string(&file) {
                Ok(source) => source,
                Err(error) => {
                    self.errors.push(format!("{}: {error}", file.display()));
                    continue;
                }
            };
            let tree = parse_script(&source).tree;
            for item in &tree.items {
                if let nextflow_parser::ast::Item::Class(class) = item {
                    self.library_names.push(class.name.text.as_str().into());
                }
            }
            parsed.push((file, source));
        }
        for (file, source) in parsed {
            let Some(module) = self.lower(&source, &file) else {
                continue;
            };
            let module = Rc::new(module);
            if let Some(directory) = file.parent() {
                *module.directory.borrow_mut() = Some(directory.to_string_lossy().into());
            }
            let mut classes = self.library.classes.borrow_mut();
            for def in &module.classes {
                let class = LibraryClass {
                    def: def.clone(),
                    module: module.clone(),
                    statics: std::cell::RefCell::new(None),
                    values: std::cell::RefCell::new(None),
                };
                classes.insert(def.name.clone(), Rc::new(class));
            }
        }
    }

    /// The script that ports functions of a plugin, loaded once.
    fn plugin_script(&mut self, plugin: &str, source: &str) -> Option<Rc<Module>> {
        if let Some(module) = self.plugin_scripts.get(plugin) {
            return Some(module.clone());
        }
        if !self.porting.insert(plugin.into()) {
            return None;
        }
        let path = PathBuf::from(format!("plugin/{plugin}.nf"));
        let module = self.lower(source, &path);
        self.porting.remove(plugin);
        let module = Rc::new(module?);
        self.link(&module, &path);
        self.plugin_scripts.insert(plugin.into(), module.clone());
        Some(module)
    }

    /// Parses and lowers the source of a script.
    fn lower(&mut self, source: &str, path: &Path) -> Option<Module> {
        let lines = LineIndex::new(source);
        let located = |error: &nextflow_parser::Diagnostic| {
            format!("{}:{}", path.display(), lines.locate(error))
        };
        let parsed = parse_script(source);
        if !parsed.errors.is_empty() {
            match nextflow_evaluator::lower::dsl1_error(&parsed.tree) {
                Some(error) => self.errors.push(located(&error)),
                None => self.errors.extend(parsed.errors.iter().map(located)),
            }
            return None;
        }
        let mut imports = Includes {
            loader: self,
            directory: path.parent().unwrap_or(Path::new("/")).to_owned(),
        };
        match lower_script(&parsed.tree, source, &mut imports) {
            Ok(mut module) => {
                module.legacy.extend(parsed.legacy);
                *module.source.borrow_mut() = Some(source.into());
                *module.path.borrow_mut() = Some(path.to_string_lossy().into());
                *module.library.borrow_mut() = Some(self.library.clone());
                Some(module)
            }
            Err(errors) => {
                self.errors.extend(errors.iter().map(located));
                None
            }
        }
    }

    /// Links each include of a module to the module and name it refers to. A conditional
    /// include takes effect when its statement runs.
    fn link(&mut self, module: &Rc<Module>, path: &Path) {
        let directory = path.parent().unwrap_or(Path::new("/")).to_owned();
        for (index, include) in module.includes.iter().enumerate() {
            let target = match include.source.strip_prefix("plugin/") {
                // A function that a script ports runs as script code.
                Some(plugin) => {
                    let script = self
                        .plugin_scripts
                        .get(plugin)
                        .filter(|script| script.function_names.contains_key(&include.name))
                        .cloned();
                    script.unwrap_or_else(|| {
                        let native = self.plugins.entry(plugin.into()).or_insert_with(|| {
                            Rc::new(Module {
                                plugin: Some(plugin.into()),
                                ..Module::default()
                            })
                        });
                        native.clone()
                    })
                }
                None => {
                    let Some(target) = include_path(&self.project, &directory, &include.source)
                    else {
                        continue;
                    };
                    let target = match include.params {
                        true => self.copy(&target, (path.to_owned(), include.span.start)),
                        false => self.modules.get(&target).cloned(),
                    };
                    let Some(target) = target else {
                        continue;
                    };
                    target
                }
            };
            let link = Link {
                module: target,
                name: include.name.clone(),
            };
            if include.conditional {
                module.conditional_links.borrow_mut().insert(index, link);
            } else {
                module
                    .links
                    .borrow_mut()
                    .insert(include.alias.clone(), link);
            }
        }
    }
}

/// Resolves includes relative to their script: an existing .nf, a directory main.nf, or the name.
/// Plugins have no file; legacy sources may start with project or script directory expressions.
fn include_path(project: &Path, directory: &Path, source: &str) -> Option<PathBuf> {
    if source.starts_with("plugin/") {
        return None;
    }
    let launch = std::env::current_dir().unwrap_or_default();
    let mut source = source.to_owned();
    for (name, value) in [
        ("projectDir", project),
        ("baseDir", project),
        ("moduleDir", directory),
        ("launchDir", launch.as_path()),
    ] {
        source = source.replace(&format!("${{{name}}}"), &value.to_string_lossy());
    }
    let path = directory.join(&source);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let extended = path.with_file_name(format!("{name}.nf"));
    let path = if !source.ends_with(".nf") && (extended.exists() || !path.exists()) {
        extended
    } else if path.is_dir() {
        path.join("main.nf")
    } else {
        path
    };
    Some(std::path::absolute(&path).unwrap_or(path))
}

/// The includes of one script, loaded through the loader.
struct Includes<'a> {
    loader: &'a mut Loader,
    directory: PathBuf,
}

impl Imports for Includes<'_> {
    fn classes(&mut self) -> Vec<Rc<str>> {
        self.loader.library_names.clone()
    }

    fn kind(&mut self, source: &str, name: &str) -> Result<DefinitionKind, String> {
        if let Some(plugin) = source.strip_prefix("plugin/") {
            if let Some(script) = nextflow_plugins::script(plugin) {
                self.loader.plugin_script(plugin, script);
            }
            if self.loader.porting.contains(plugin) {
                return Ok(DefinitionKind::Function);
            }
            return match nextflow_plugins::functions(plugin) {
                Some(functions) if functions.contains(&name) => Ok(DefinitionKind::Function),
                Some(_) => Err(format!(
                    "`{name}` is not a function of the plugin `{plugin}`"
                )),
                None => Err(format!(
                    "Plugins are not supported yet: {source} ({})",
                    nextflow_plugins::catalogue::status(plugin)
                )),
            };
        }
        let Some(path) = include_path(&self.loader.project, &self.directory, source) else {
            return Err(format!("Plugins are not supported yet: {source}"));
        };
        if !path.exists() {
            return Err(format!(
                "the included file {} does not exist",
                path.display()
            ));
        }
        let Some(module) = self.loader.load_file(&path) else {
            return Err(format!("Cannot include '{source}', which has errors"));
        };
        if module.function_names.contains_key(name) {
            return Ok(DefinitionKind::Function);
        }
        if module
            .processes
            .iter()
            .any(|process| &*process.name == name)
        {
            return Ok(DefinitionKind::Process);
        }
        if module
            .workflows
            .iter()
            .any(|workflow| workflow.name.as_deref() == Some(name))
        {
            return Ok(DefinitionKind::Workflow);
        }
        Err(format!(
            "Included name '{name}' is not defined in module '{}'",
            path.display()
        ))
    }
}

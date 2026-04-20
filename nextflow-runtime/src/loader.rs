//! Loads a script and the scripts it includes: parses, lowers and links them.
// Copyright (c) 2026 The iunctus Contributors
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
    /// The modules that stand for plugins, by plugin id.
    plugins: HashMap<Rc<str>, Rc<Module>>,
    /// The scripts that port plugin functions, by plugin id.
    plugin_scripts: HashMap<Rc<str>, Rc<Module>>,
    loading: HashSet<PathBuf>,
    /// The scripts that could not be loaded, whose errors are already reported.
    failed: HashSet<PathBuf>,
    errors: Vec<String>,
    /// The Groovy classes of the `lib/` directory next to the main script, which every script
    /// can use.
    library: Rc<Library>,
    /// The names of the classes of `library` that are not nested in another.
    library_names: Vec<Rc<str>>,
}

impl Loader {
    /// Loads a script file with everything it includes.
    pub fn load(&mut self, path: &Path) -> Result<Rc<Module>, LoadError> {
        if let Some(directory) = path.parent() {
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
        self.link(&module, &path);
        self.modules.insert(path, module.clone());
        Some(module)
    }

    fn read(&mut self, path: &Path) -> Option<Module> {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                self.errors.push(format!("{}: {error}", path.display()));
                return None;
            }
        };
        self.lower(&source, path)
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
        let module = Rc::new(self.lower(source, Path::new(&format!("plugin/{plugin}.nf")))?);
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
            self.errors.extend(parsed.errors.iter().map(located));
            return None;
        }
        let mut imports = Includes {
            loader: self,
            directory: path.parent().unwrap_or(Path::new("/")).to_owned(),
        };
        match lower_script(&parsed.tree, source, &mut imports) {
            Ok(module) => {
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

    /// Links each include of a module to the module and name it refers to.
    fn link(&mut self, module: &Rc<Module>, path: &Path) {
        let directory = path.parent().unwrap_or(Path::new("/")).to_owned();
        for include in &module.includes {
            if let Some(plugin) = include.source.strip_prefix("plugin/") {
                // A function that a script ports runs as script code.
                let script = self
                    .plugin_scripts
                    .get(plugin)
                    .filter(|script| script.function_names.contains_key(&include.name))
                    .cloned();
                let target = script.unwrap_or_else(|| {
                    let native = self.plugins.entry(plugin.into()).or_insert_with(|| {
                        Rc::new(Module {
                            plugin: Some(plugin.into()),
                            ..Module::default()
                        })
                    });
                    native.clone()
                });
                let link = Link {
                    module: target,
                    name: include.name.clone(),
                };
                module
                    .links
                    .borrow_mut()
                    .insert(include.alias.clone(), link);
                continue;
            }
            let Some(target) = include_path(&directory, &include.source) else {
                continue;
            };
            if let Some(target) = self.modules.get(&target).cloned() {
                let link = Link {
                    module: target,
                    name: include.name.clone(),
                };
                module
                    .links
                    .borrow_mut()
                    .insert(include.alias.clone(), link);
            }
        }
    }
}

/// The file an include source names: relative to the including script, with `.nf` added, or
/// `main.nf` for a directory. Plugins have no file.
fn include_path(directory: &Path, source: &str) -> Option<PathBuf> {
    if source.starts_with("plugin/") {
        return None;
    }
    let path = directory.join(source);
    let path = if path.is_dir() {
        path.join("main.nf")
    } else if path.extension().is_some_and(|extension| extension == "nf") {
        path
    } else {
        path.with_extension("nf")
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
            return match nextflow_plugins::functions(plugin) {
                Some(functions) if functions.contains(&name) => Ok(DefinitionKind::Function),
                Some(_) => Err(format!(
                    "`{name}` is not a function of the plugin `{plugin}`"
                )),
                None => Err(format!("Plugins are not supported yet: {source}")),
            };
        }
        let Some(path) = include_path(&self.directory, source) else {
            return Err(format!("Plugins are not supported yet: {source}"));
        };
        if !path.exists() {
            return Err(format!("Invalid include source: '{}'", path.display()));
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

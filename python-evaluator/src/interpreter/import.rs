//! `import` and `from ... import`: modules of the host, stand-ins for library modules, and
//! `.py` files found on the module path.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use ruff_python_ast as ast;

use super::{Frame, Interpreter, Source};
use crate::error::Error;
use crate::host::FileKind;
use crate::value::{Module, Value};

impl Interpreter {
    pub(super) fn exec_import(
        &mut self,
        frame: &Frame,
        import: &ast::StmtImport,
    ) -> Result<(), Error> {
        for alias in &import.names {
            let name = alias.name.id.as_str();
            let module = self.import_module(name)?;
            match &alias.asname {
                Some(asname) => self.bind(frame, asname.id.as_str(), module),
                None => {
                    let top = name.split('.').next().unwrap_or(name);
                    let top_module = self.import_module(top)?;
                    self.bind(frame, top, top_module);
                }
            }
        }
        Ok(())
    }

    pub(super) fn exec_import_from(
        &mut self,
        frame: &Frame,
        import: &ast::StmtImportFrom,
    ) -> Result<(), Error> {
        let module_name = import
            .module
            .as_ref()
            .map(|name| name.id.as_str())
            .unwrap_or("");
        let full = if import.level > 0 {
            let package = self.package_of(frame, import.level)?;
            match (package.is_empty(), module_name.is_empty()) {
                (true, _) => module_name.to_owned(),
                (false, true) => package,
                (false, false) => format!("{package}.{module_name}"),
            }
        } else {
            module_name.to_owned()
        };
        let module = self.import_module(&full)?;
        for alias in &import.names {
            let name = alias.name.id.as_str();
            if name == "*" {
                self.import_all(frame, &module)?;
                continue;
            }
            // A submodule of a package in the container is not an attribute before it is
            // imported.
            let found = match self.find_attribute(&module, name) {
                Err(error) if error.is("AttributeError") && is_package(&module) => None,
                found => found?,
            };
            let value = match found {
                Some(value) => value,
                None => match self.import_module(&format!("{full}.{name}")) {
                    Ok(submodule) => submodule,
                    Err(error) if error.is("ModuleNotFoundError") => {
                        if crate::modules::is_native(&full) {
                            return Err(Error::unsupported(format!(
                                "the library function {full}.{name}"
                            )));
                        }
                        return Err(Error::new(
                            "ImportError",
                            format!("cannot import name '{name}' from '{full}'"),
                        ));
                    }
                    Err(error) => return Err(error),
                },
            };
            let bound = alias
                .asname
                .as_ref()
                .map_or(name, |asname| asname.id.as_str());
            self.bind(frame, bound, value);
        }
        Ok(())
    }

    fn import_all(&mut self, frame: &Frame, module: &Value) -> Result<(), Error> {
        let Value::Module(module) = module.plain() else {
            return Ok(());
        };
        let names: Vec<String> = match module.globals.get("__all__") {
            Some(all) => self
                .iterate(&all)?
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect(),
            None => module
                .globals
                .vars
                .borrow()
                .keys()
                .filter(|name| !name.starts_with('_'))
                .map(|name| name.to_string())
                .collect(),
        };
        for name in names {
            if let Some(value) = module.globals.get(&name) {
                self.bind(frame, &name, value);
            }
        }
        Ok(())
    }

    /// The package that a relative import of the given level starts from.
    fn package_of(&self, frame: &Frame, level: u32) -> Result<String, Error> {
        let name = frame
            .globals
            .get("__name__")
            .and_then(|name| name.as_str().map(str::to_owned))
            .unwrap_or_default();
        let is_package = frame
            .globals
            .get("__file__")
            .and_then(|file| file.as_str().map(|file| file.ends_with("__init__.py")))
            .unwrap_or(false);
        let mut parts: Vec<&str> = name.split('.').collect();
        if !is_package {
            parts.pop();
        }
        for _ in 1..level {
            if parts.pop().is_none() {
                return Err(Error::new(
                    "ImportError",
                    "attempted relative import beyond top-level package",
                ));
            }
        }
        Ok(parts.join("."))
    }

    /// Imports a module by its full dotted name.
    pub fn import_module(&mut self, name: &str) -> Result<Value, Error> {
        if let Some(module) = self.modules.get(name) {
            return Ok(module.clone());
        }
        let not_found = || Error::new("ModuleNotFoundError", format!("No module named '{name}'"));
        if name.is_empty() {
            return Err(not_found());
        }
        if let Some(factory) = self.host_modules.get(name).cloned() {
            let module = factory(self)?;
            self.register_submodule(name, module.clone());
            return Ok(module);
        }
        if let Some(module) = crate::modules::native(self, name)? {
            self.register_submodule(name, module.clone());
            return Ok(module);
        }
        // A submodule of a package: import the package first.
        let mut search: Vec<PathBuf> = self
            .iterate(&self.sys_path.clone())?
            .iter()
            .filter_map(|entry| entry.as_str().map(PathBuf::from))
            .collect();
        if let Some((parent, _)) = name.rsplit_once('.') {
            let wanted = self.packages.want(name);
            let parent_module = self.import_module(parent);
            if wanted {
                self.packages.forget_wanted();
            }
            let parent_module = parent_module?;
            if let Some(module) = self.modules.get(name) {
                return Ok(module.clone());
            }
            if is_package(&parent_module) {
                return self.import_package(name);
            }
            match parent_module.plain() {
                Value::Module(module) => match module.globals.get("__path__") {
                    Some(Value::Str(path)) => search = vec![PathBuf::from(&*path)],
                    _ => return Err(not_found()),
                },
                _ => return Err(not_found()),
            }
        }
        let leaf = name.rsplit('.').next().unwrap_or(name);
        for directory in search {
            let directory = self.host.resolve(&directory.to_string_lossy());
            let file = directory.join(format!("{leaf}.py"));
            let package = directory.join(leaf);
            let init = package.join("__init__.py");
            if self
                .host
                .stat(&file)
                .is_some_and(|stat| stat.kind == FileKind::File)
            {
                return self.load_file_module(name, &file, None);
            }
            if self
                .host
                .stat(&init)
                .is_some_and(|stat| stat.kind == FileKind::File)
            {
                return self.load_file_module(name, &init, Some(&package));
            }
            if self
                .host
                .stat(&package)
                .is_some_and(|stat| stat.kind == FileKind::Directory)
            {
                // A namespace package.
                let globals = self.new_globals(name, None);
                globals.set("__path__", Value::str(package.to_string_lossy().as_ref()));
                let module = Value::Module(Rc::new(Module {
                    name: name.into(),
                    globals,
                }));
                self.register_module(name, module.clone());
                return Ok(module);
            }
        }
        // A module of the standard library without a stand-in is a gap of iunctus; another
        // module comes from a package, which runs in the Python container (decision 19).
        if crate::modules::is_standard_library(name) {
            return Err(Error::unsupported(format!(
                "the standard library module {name}"
            )));
        }
        // A folder or file of the working directory that is not on the module path is the
        // workflow's own module, not a package of PyPI: the import fails as in Python.
        let cwd = self.host.cwd();
        if [cwd.join(name), cwd.join(format!("{name}.py"))]
            .iter()
            .any(|path| self.host.stat(path).is_some())
        {
            return Err(not_found());
        }
        self.import_package(name)
    }

    fn load_file_module(
        &mut self,
        name: &str,
        file: &std::path::Path,
        package: Option<&std::path::Path>,
    ) -> Result<Value, Error> {
        let path = file.to_string_lossy().into_owned();
        let bytes = self
            .host
            .read(file)
            .map_err(|error| Error::os_error(&error, &path))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let suite = Rc::new(Interpreter::parse(&path, &text)?);
        let globals = self.new_globals(name, Some(&path));
        if let Some(package) = package {
            globals.set("__path__", Value::str(package.to_string_lossy().as_ref()));
            globals.set("__package__", Value::str(name));
        } else {
            let package = name.rsplit_once('.').map_or("", |(package, _)| package);
            globals.set("__package__", Value::str(package));
        }
        let module = Value::Module(Rc::new(Module {
            name: name.into(),
            globals: globals.clone(),
        }));
        // Registered before it runs, so that circular imports find it.
        self.register_module(name, module.clone());
        let frame = Frame::module(globals, Source::new(&path, &text));
        self.retained.push(suite.clone());
        if let Err(error) = self.run(&suite, &frame) {
            self.modules.remove(name);
            return Err(error);
        }
        Ok(module)
    }

    /// Registers a module of the host or a stand-in, which is also an attribute of its
    /// package, as in Python: after `import urllib.error`, `urllib.error` works.
    fn register_submodule(&mut self, name: &str, module: Value) {
        self.modules.insert(name.to_owned(), module.clone());
        if let Some((parent, leaf)) = name.rsplit_once('.')
            && let Ok(Value::Module(parent)) = self.import_module(parent)
            && parent.globals.get(leaf).is_none()
        {
            parent.globals.set(leaf, module);
        }
    }

    fn register_module(&mut self, name: &str, module: Value) {
        self.modules.insert(name.to_owned(), module.clone());
        if let Some((parent, leaf)) = name.rsplit_once('.')
            && let Some(Value::Module(parent)) = self.modules.get(parent)
        {
            parent.globals.set(leaf, module);
        }
    }
}

/// Whether a module is a package module in the Python container.
fn is_package(module: &Value) -> bool {
    module
        .as_native::<crate::packages::PackageObject>()
        .is_some()
}

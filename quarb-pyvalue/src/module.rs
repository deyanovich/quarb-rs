//! A Python module mounted as an environment (spec: Environment
//! Mounts, `py=python:./helpers.py`; Environment Commands): the
//! module's functions as commands under the mount's alias, with the
//! native framing. The input is the first positional parameter
//! unless `$_` places it among the operands or in an option's value
//! (`py:round(2)` is `round(x, 2)`, `py:sub("^get_"; ""; $_)` is
//! `sub("^get_", "", x)`, `py:f(--data=$_)` is `f(data=x)`); the
//! operands are the positional arguments in order, the options the
//! keyword arguments; a raised exception is a failed call, null
//! after `|` and a refusal after `@|`, where the one call over the
//! stream answers one thread per returned value. The module is
//! imported once at mount time, from a file or by its import path,
//! and its synopses are read from the functions' signatures as a
//! reflected library's are. An environment reaches outside the
//! query by definition, so nothing in the module is withheld for
//! reading a file.

use crate::reflect::Reflective;
use pyo3::prelude::*;
use quarb::Value;
use quarb::library::{CommandLibrary, LibCall, LibFailure, Synopsis};

/// One mounted module: a command library under the mount's alias.
pub struct ModuleEnvironment {
    alias: String,
    /// The target as mounted, for messages.
    target: String,
    /// The module's name in `sys.modules`: the import path, or a
    /// name of the mount's own for a module file.
    root: &'static str,
    reflect: Reflective,
}

impl ModuleEnvironment {
    /// Mount `target` under `alias`: a module file (`./helpers.py`,
    /// imported from its path) or an import path (`pkg.helpers`,
    /// imported from the Python path, the active virtualenv's
    /// included). `Err` names what did not import, the module's own
    /// error in it.
    pub fn mount(alias: &str, target: &str) -> Result<ModuleEnvironment, String> {
        let is_file = target.ends_with(".py");
        // A module file is registered under a name of the mount's
        // own, derived from its path, so two mounts of one file (a
        // session remounting, two aliases) share the loaded module
        // and two files never collide.
        let root: &'static str = if is_file {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            target.hash(&mut h);
            Box::leak(format!("quarb_env_{:016x}", h.finish()).into_boxed_str())
        } else {
            Box::leak(target.to_string().into_boxed_str())
        };
        // One mount at a time, the lock taken outside the
        // interpreter so a mount that yields the GIL while its
        // module runs cannot wait on a thread that holds both.
        static MOUNTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _one = MOUNTING.lock().unwrap_or_else(|e| e.into_inner());
        Python::attach(|py| -> Result<(), String> {
            crate::foreign::extend_sys_path(py).map_err(|e| e.to_string())?;
            let fail = |e: PyErr| format!("'{alias}=python:{target}': {}", e.value(py));
            if is_file {
                let modules = py
                    .import("sys")
                    .map_err(fail)?
                    .getattr("modules")
                    .map_err(fail)?;
                if modules.contains(root).map_err(fail)? {
                    // Loaded by an earlier mount of the same file.
                    return Ok(());
                }
                let util = py.import("importlib.util").map_err(fail)?;
                let spec = util
                    .call_method1("spec_from_file_location", (root, target))
                    .map_err(fail)?;
                if spec.is_none() {
                    return Err(format!("'{alias}=python:{target}': not a module file"));
                }
                let module = util
                    .call_method1("module_from_spec", (&spec,))
                    .map_err(fail)?;
                // Run first, register after: a reader never meets
                // the module half-made; the module's own relative
                // imports do not need the entry (a file mount is a
                // top-level module).
                spec.getattr("loader")
                    .map_err(fail)?
                    .call_method1("exec_module", (&module,))
                    .map_err(fail)?;
                modules.set_item(root, &module).map_err(fail)?;
            } else {
                py.import(root).map_err(fail)?;
            }
            Ok(())
        })?;
        Ok(ModuleEnvironment {
            alias: alias.to_string(),
            target: target.to_string(),
            root,
            reflect: Reflective::module(root),
        })
    }

    /// The module's name in a message reads as the mount.
    fn relabel(&self, m: String) -> String {
        let m = m.replace(self.root, &format!("python:{}", self.target));
        format!("environment '{}': {m}", self.alias)
    }
}

impl std::fmt::Debug for ModuleEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleEnvironment")
            .field("alias", &self.alias)
            .field("target", &self.target)
            .finish()
    }
}

impl CommandLibrary for ModuleEnvironment {
    fn prefix(&self) -> &str {
        &self.alias
    }

    /// Read from the module as the names are met: none declared.
    fn commands(&self) -> &[Synopsis] {
        &[]
    }

    fn resolve(&self, name: &str) -> Result<&Synopsis, String> {
        self.reflect.resolve(name).map_err(|m| self.relabel(m))
    }

    fn call(&self, name: &str, call: LibCall) -> Result<Value, LibFailure> {
        let syn = self.resolve(name).map_err(LibFailure::Unknown)?;
        self.reflect.call(syn, name, call)
    }
}

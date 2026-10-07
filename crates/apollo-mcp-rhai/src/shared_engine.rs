use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use rhai::EvalAltResult;

use crate::engine::RhaiEngine;

/// A cheaply cloneable handle to the Rhai engine shared by every hook call site.
///
/// Hooks run against an [`Arc`] snapshot taken from the handle, so no lock is held while a
/// script runs. A script that blocks on an HTTP call therefore cannot stall other tool calls,
/// and reloading scripts cannot interrupt hooks that are already in flight.
#[derive(Clone)]
pub struct SharedRhaiEngine {
    engine: Arc<RwLock<Arc<RhaiEngine>>>,
    script_dir: Arc<Path>,
}

impl SharedRhaiEngine {
    /// A handle with no scripts loaded. No hooks are defined until [`Self::reload`] succeeds.
    pub fn new(script_dir: impl AsRef<Path>) -> Self {
        let script_dir = script_dir.as_ref();
        let engine = RhaiEngine::new(script_dir);

        Self::from_engine(script_dir, engine)
    }

    /// Loads the scripts in `script_dir`. A missing main script leaves the engine without hooks,
    /// since scripting is optional.
    pub fn load(script_dir: impl AsRef<Path>) -> Result<Self, Box<EvalAltResult>> {
        let script_dir = script_dir.as_ref();
        let mut engine = RhaiEngine::new(script_dir);
        engine.load_from_path()?;

        Ok(Self::from_engine(script_dir, engine))
    }

    /// The engine to run hooks against.
    ///
    /// The snapshot is independent of later reloads, so a hook keeps running against the scripts
    /// it started with.
    pub(crate) fn current(&self) -> Arc<RhaiEngine> {
        Arc::clone(&self.engine.read())
    }

    /// Replaces the scripts with a freshly compiled copy from disk.
    ///
    /// The engine is only swapped in once the new scripts compile and their top level runs
    /// without error, so a failure leaves the previous scripts in place. A missing main script
    /// counts as a failure, which keeps the hooks loaded through the delete-then-create that
    /// some editors use to save a file.
    pub fn reload(&self) -> Result<(), Box<EvalAltResult>> {
        // Compile outside the lock so that in-flight hooks keep running.
        let engine = RhaiEngine::compile_from_path(&self.script_dir)?;

        *self.engine.write() = Arc::new(engine);
        Ok(())
    }

    fn from_engine(script_dir: &Path, engine: RhaiEngine) -> Self {
        Self {
            engine: Arc::new(RwLock::new(Arc::new(engine))),
            script_dir: Arc::from(script_dir),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_script(
        script_dir: impl AsRef<Path>,
        script: &str,
    ) -> Result<Self, Box<EvalAltResult>> {
        let script_dir = script_dir.as_ref();
        let mut engine = RhaiEngine::new(script_dir);
        engine.load_from_string(script)?;

        Ok(Self::from_engine(script_dir, engine))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rhai_script(base: &std::path::Path, content: &str) {
        let rhai_dir = base.join("rhai");
        std::fs::create_dir_all(&rhai_dir).expect("Should create rhai dir");
        std::fs::write(rhai_dir.join("main.rhai"), content).expect("Should write script");
    }

    fn write_rhai_module(base: &std::path::Path, module_name: &str, content: &str) {
        let rhai_dir = base.join("rhai");
        std::fs::create_dir_all(&rhai_dir).expect("Should create rhai dir");
        std::fs::write(rhai_dir.join(format!("{module_name}.rhai")), content)
            .expect("Should write module");
    }

    fn seeded(script_dir: &std::path::Path) -> SharedRhaiEngine {
        SharedRhaiEngine::from_script(script_dir, "fn original() { 1 }").expect("Should compile")
    }

    #[test]
    fn load_should_succeed_when_script_file_missing() {
        let dir = tempfile::tempdir().expect("Should create temp dir");

        let engine = SharedRhaiEngine::load(dir.path().join("rhai")).expect("Should load");

        assert!(!engine.current().ast_has_function("original"));
    }

    #[test]
    fn reload_should_preserve_state_when_script_file_missing() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        let engine = seeded(&dir.path().join("rhai"));

        assert!(engine.reload().is_err());
        assert!(engine.current().ast_has_function("original"));
    }

    #[test]
    fn reload_should_load_new_script_from_disk() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        write_rhai_script(dir.path(), "fn reloaded() { 99 }");
        let engine = seeded(&dir.path().join("rhai"));

        engine.reload().expect("Should reload successfully");

        assert!(engine.current().ast_has_function("reloaded"));
    }

    #[test]
    fn reload_should_remove_old_functions_after_loading_new_script() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        write_rhai_script(dir.path(), "fn reloaded() { 99 }");
        let engine = seeded(&dir.path().join("rhai"));

        engine.reload().expect("Should reload successfully");

        assert!(!engine.current().ast_has_function("original"));
    }

    #[test]
    fn reload_should_preserve_state_on_compile_error() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        write_rhai_script(dir.path(), "this is not valid {{{");
        let engine = seeded(&dir.path().join("rhai"));

        let result = engine.reload();

        assert!(result.is_err());
        assert!(engine.current().ast_has_function("original"));
    }

    #[test]
    fn reload_should_preserve_state_on_runtime_error() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        write_rhai_script(dir.path(), r#"throw "init error";"#);
        let engine = seeded(&dir.path().join("rhai"));

        let result = engine.reload();

        assert!(result.is_err());
        assert!(engine.current().ast_has_function("original"));
    }

    #[test]
    fn reload_should_pick_up_changes_to_imported_modules() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        write_rhai_module(dir.path(), "helpers", "fn helper_value() { 1 }");
        write_rhai_script(
            dir.path(),
            r#"import "helpers" as h; fn get_value() { h::helper_value() }"#,
        );

        let engine = SharedRhaiEngine::load(dir.path().join("rhai")).expect("Should load");

        let result = engine
            .current()
            .execute_hook("get_value", ())
            .expect("Should not error");
        assert_eq!(result.unwrap().as_int().unwrap(), 1);

        // Update the module file and reload
        write_rhai_module(dir.path(), "helpers", "fn helper_value() { 42 }");
        engine.reload().expect("Should reload successfully");

        let result = engine
            .current()
            .execute_hook("get_value", ())
            .expect("Should not error");
        assert_eq!(result.unwrap().as_int().unwrap(), 42);
    }

    #[test]
    fn reload_should_reset_scope_with_new_globals() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        write_rhai_script(dir.path(), "let new_var = 200;\nfn get_new() { new_var }");
        let engine = SharedRhaiEngine::from_script(dir.path().join("rhai"), "let old_var = 100;")
            .expect("Should compile");

        engine.reload().expect("Should reload successfully");

        let result = engine
            .current()
            .execute_hook("get_new", ())
            .expect("Should not error");
        assert_eq!(result.unwrap().as_int().unwrap(), 200);
    }

    #[test]
    fn current_should_keep_running_scripts_taken_before_a_reload() {
        let dir = tempfile::tempdir().expect("Should create temp dir");
        write_rhai_script(dir.path(), "fn reloaded() { 99 }");
        let engine = seeded(&dir.path().join("rhai"));

        let before = engine.current();
        engine.reload().expect("Should reload successfully");

        assert!(before.ast_has_function("original"));
    }
}

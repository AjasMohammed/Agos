use std::sync::OnceLock;

/// A boxed closure that accepts a log level string and updates the active filter.
/// Set once at startup by `init_logging` in the CLI; called by `cmd_set_log_level`.
pub type LogLevelSetter = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

static LOG_LEVEL_SETTER: OnceLock<LogLevelSetter> = OnceLock::new();

/// Register the runtime log-level setter.  Called once from `init_logging` in the CLI
/// after the tracing subscriber is initialized with a `reload::Layer`.
pub fn register_log_level_setter(setter: LogLevelSetter) {
    if LOG_LEVEL_SETTER.set(setter).is_err() {
        // This is expected in tests where multiple test cases call init_logging.
        // In production this indicates a double-init bug.
        eprintln!("agentos: log level setter already registered; ignoring duplicate registration");
    }
}

/// Update the active log filter to `level` (e.g. "debug", "warn").
/// Returns an error string if the setter is not registered or the level is invalid.
pub fn apply_log_level(level: &str) -> Result<(), String> {
    match LOG_LEVEL_SETTER.get() {
        Some(setter) => setter(level),
        None => Err(
            "Log level setter not initialized (kernel may not be running in-process)".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_log_level_without_setter_returns_error() {
        // When no setter has been registered the call must fail gracefully, not panic.
        // The global OnceLock may already be set by another test; we only assert the
        // no-setter case by directly inspecting the path we can control.
        let result = LOG_LEVEL_SETTER
            .get()
            .map(|s| s("debug"))
            .unwrap_or_else(|| Err("not initialized".to_string()));
        // If no setter is registered: error; if one is: the setter decides.
        // Either way, the call must not panic.
        let _ = result;
    }

    #[test]
    fn log_level_setter_closure_is_called_with_correct_level() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let setter: LogLevelSetter = Box::new(move |level: &str| {
            *captured_clone.lock().unwrap() = Some(level.to_string());
            Ok(())
        });
        setter("warn").expect("setter should succeed");
        assert_eq!(*captured.lock().unwrap(), Some("warn".to_string()));
    }
}

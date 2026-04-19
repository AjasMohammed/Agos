use agentos_tools::{
    script_tool::{ScriptParser, ScriptRegistry, ScriptTool},
    ToolRunner,
};
use notify::{
    Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use tracing::{info, warn};

/// Watches `$data_dir/scripts/` for new, modified, and deleted script files.
///
/// When a file is created or modified it is parsed for `@agentos tool:` annotations.
/// If valid annotations are found, a [`ScriptTool`] is built and registered with
/// [`ToolRunner::register_dynamic`]. When the file is deleted the corresponding
/// dynamic tool is unregistered.
///
/// This turns the scripts directory into a live, reactive package registry:
/// **drop a file = install a tool; delete a file = uninstall a tool**.
///
/// # Language support
///
/// Any executable that reads `$AGENTOS_INPUT` (JSON) and writes JSON to stdout
/// is supported. Language detection is automatic from the shebang line or file
/// extension. Supported: `sh`, `bash`, `python`, `node`, `ruby`, `lua`, `perl`,
/// `php`, `r` — and any compiled binary.
///
/// # Security
///
/// Scripts are executed inside a `bwrap` sandbox with the same constraints as
/// `shell-exec`: read-only root filesystem, writable `data_dir` only, no network
/// unless `@network: true` or `network.outbound:x` permission is declared.
///
/// # Event processing
///
/// The `notify` callback is intentionally minimal — it only sends a path over a
/// channel. All filesystem I/O (reading annotation headers, building `ScriptTool`)
/// is done on a dedicated background thread (`script-watcher`), so slow filesystems
/// or large files cannot stall inotify event delivery.
pub struct ScriptWatcher {
    scripts_dir: PathBuf,
    registry: Arc<Mutex<ScriptRegistry>>,
    // Wrapped in Option so Drop can explicitly drop the watcher (closing the tx
    // channel) BEFORE joining the event thread. Without this, the join would
    // block forever because the thread is still waiting on the open channel.
    watcher: Option<RecommendedWatcher>,
    // Background thread — Option so Drop can take() and join it after watcher drops.
    event_thread: Option<std::thread::JoinHandle<()>>,
}

/// Events forwarded from the notify callback to the background processing thread.
enum ScriptEvent {
    /// File created or modified — re-parse and (re-)register.
    Upsert(PathBuf),
    /// File removed — unregister if it was a tool.
    Remove(PathBuf),
}

impl ScriptWatcher {
    /// Start watching `scripts_dir`. Existing scripts are loaded immediately.
    /// Returns `Err` if the directory cannot be watched.
    pub fn start(scripts_dir: PathBuf, tool_runner: Arc<ToolRunner>) -> anyhow::Result<Self> {
        // Create the scripts directory if it doesn't exist yet.
        std::fs::create_dir_all(&scripts_dir).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create scripts directory {}: {}",
                scripts_dir.display(),
                e
            )
        })?;

        // Canonicalize so all path keys in ScriptRegistry match what notify emits.
        // On systems where scripts_dir contains a symlink component, notify events
        // carry the resolved path; without canonicalization the registry lookup fails
        // and deleted scripts are never unregistered.
        let scripts_dir = scripts_dir.canonicalize().unwrap_or(scripts_dir);

        let registry = Arc::new(Mutex::new(ScriptRegistry::new()));

        // Load all scripts that already exist in the directory at boot.
        // (Synchronous, but at boot time this is acceptable for typical script counts.)
        Self::scan_existing(&scripts_dir, &tool_runner, &registry);

        // Channel: notify callback → background thread.
        // The callback sends path events cheaply; the thread does all file I/O.
        let (tx, rx) = mpsc::channel::<ScriptEvent>();

        let registry_thread = registry.clone();
        let runner_thread = tool_runner;
        let event_thread = std::thread::Builder::new()
            .name("script-watcher".into())
            .spawn(move || {
                for event in rx {
                    match event {
                        ScriptEvent::Upsert(path) => {
                            // is_file() is safe to call here (background thread, not callback).
                            if path.is_file() {
                                Self::try_register(&runner_thread, &registry_thread, &path);
                            }
                        }
                        ScriptEvent::Remove(path) => {
                            // File is already gone — just consult the registry by path.
                            Self::try_unregister(&runner_thread, &registry_thread, &path);
                        }
                    }
                }
                // Channel sender dropped (ScriptWatcher dropped) — thread exits cleanly.
            })
            .map_err(|e| anyhow::anyhow!("Failed to spawn ScriptWatcher event thread: {}", e))?;

        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<Event>| match result {
                Ok(event) => {
                    // Keep the callback minimal: just forward path(s) to the background thread.
                    // No file I/O here — that happens on the event_thread.
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) => {
                            for path in event.paths {
                                if !Self::is_editor_artifact(&path) {
                                    let _ = tx.send(ScriptEvent::Upsert(path));
                                }
                            }
                        }
                        EventKind::Remove(_) => {
                            for path in event.paths {
                                if !Self::is_editor_artifact(&path) {
                                    let _ = tx.send(ScriptEvent::Remove(path));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    warn!(error = %e, "ScriptWatcher: notify error");
                }
            },
            NotifyConfig::default(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create ScriptWatcher: {}", e))?;

        watcher
            .watch(&scripts_dir, RecursiveMode::NonRecursive)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to watch scripts directory {}: {}",
                    scripts_dir.display(),
                    e
                )
            })?;

        info!(
            dir = %scripts_dir.display(),
            "ScriptWatcher: active — drop scripts here to add tools at runtime"
        );

        Ok(Self {
            scripts_dir,
            registry,
            watcher: Some(watcher),
            event_thread: Some(event_thread),
        })
    }

    /// Signal shutdown and join the background event thread.
    ///
    /// Drops the internal watcher (closing the inotify channel and the `tx` end),
    /// then joins the background thread. After this returns, no further tool
    /// registrations or unregistrations will occur.
    ///
    /// The `Drop` impl calls this automatically, so explicit calls are only needed
    /// when the caller needs synchronous confirmation that the thread has exited
    /// (e.g. kernel shutdown ordering).
    pub fn shutdown(&mut self) {
        // Drop the watcher first to close the tx channel, then join the thread.
        drop(self.watcher.take());
        if let Some(handle) = self.event_thread.take() {
            let _ = handle.join();
        }
    }

    /// The directory being watched.
    pub fn scripts_dir(&self) -> &Path {
        &self.scripts_dir
    }

    /// List all currently registered script tools as (name, version, path) tuples.
    /// Used by `agentos script list`.
    pub fn list_scripts(&self) -> Vec<(String, String, String)> {
        // Snapshot the registry under the lock, then release before doing any
        // filesystem I/O (ScriptParser::parse reads from disk). Holding the mutex
        // during I/O would block the background event thread on slow filesystems.
        // Safe to recover from poison: registry is a plain HashMap with no invariants.
        let snapshot: Vec<(PathBuf, String)> = {
            let reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            reg.entries()
                .map(|(path, name)| (path.clone(), name.clone()))
                .collect()
        };

        snapshot
            .into_iter()
            .map(|(path, name)| {
                // Re-parse to get version — only reads the annotation header (≤60 lines).
                // If the file was deleted between the snapshot and here, parse returns Err
                // and version falls back to "?" gracefully (no panic).
                let version = ScriptParser::parse(&path)
                    .ok()
                    .flatten()
                    .map(|a| a.version)
                    .unwrap_or_else(|| "?".to_string());
                (name, version, path.display().to_string())
            })
            .collect()
    }

    /// Force re-parse and re-register a script by name.
    /// Used by `agentos script reload <name>`.
    pub fn reload_by_name(&self, name: &str, tool_runner: &ToolRunner) -> bool {
        // Fast path: look up the known path for this name from the registry.
        // This avoids scanning the entire directory for the common case where
        // the script was previously loaded.
        let known_path: Option<PathBuf> = {
            // Safe to recover from poison: registry is a plain HashMap.
            let reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            let path = reg
                .entries()
                .find(|(_, n)| n.as_str() == name)
                .map(|(p, _)| p.clone());
            path
        };

        if let Some(path) = known_path {
            return Self::try_register(tool_runner, &self.registry, &path).is_some();
        }

        // Slow path: the script may have been dropped but not yet indexed by the
        // watcher (e.g., the watcher event is still in flight). Scan the directory.
        if let Ok(entries) = std::fs::read_dir(&self.scripts_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                match ScriptParser::parse(&p) {
                    Ok(Some(ann)) if ann.name == name => {
                        return Self::try_register(tool_runner, &self.registry, &p).is_some();
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            path = %p.display(),
                            "ScriptWatcher: parse error during reload scan"
                        );
                    }
                    _ => {}
                }
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    fn scan_existing(
        scripts_dir: &Path,
        tool_runner: &ToolRunner,
        registry: &Arc<Mutex<ScriptRegistry>>,
    ) {
        let entries = match std::fs::read_dir(scripts_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, dir = %scripts_dir.display(), "ScriptWatcher: failed to read scripts directory at boot");
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                Self::try_register(tool_runner, registry, &path);
            }
        }
    }

    /// Parse `path`, build a `ScriptTool`, and call `register_dynamic`.
    ///
    /// If the annotation name changed for the same path (user edited `@agentos tool:`
    /// in place), the old tool name is unregistered first to prevent ghost tools.
    ///
    /// Returns the new tool name on success.
    fn try_register(
        tool_runner: &ToolRunner,
        registry: &Arc<Mutex<ScriptRegistry>>,
        path: &Path,
    ) -> Option<String> {
        // Canonicalize path so registry keys always match notify event paths.
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path = path.as_path();

        let annotations = match ScriptParser::parse(path) {
            Ok(Some(a)) => a,
            Ok(None) => return None, // No @agentos tool: annotation — silently skip.
            Err(e) => {
                warn!(error = %e, path = %path.display(), "ScriptWatcher: annotation parse error");
                return None;
            }
        };

        let new_name = annotations.name.clone();

        // C2: Refuse to shadow a built-in static tool. The static tool always
        // wins execution (runner checks static first), so registering would
        // create a misleading entry that never actually runs.
        if tool_runner.has_static_tool(&new_name) {
            warn!(
                tool_name = %new_name,
                path = %path.display(),
                "ScriptWatcher: refusing to register script — a built-in tool with this name already exists"
            );
            return None;
        }

        // W11: Build the new ScriptTool BEFORE touching the registry or unregistering
        // the old tool. If construction fails, the previously-registered tool remains
        // available — no capability gap.
        let tool = match ScriptTool::new(path.to_path_buf(), annotations) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "ScriptWatcher: failed to build ScriptTool");
                return None;
            }
        };

        // LOCK ORDERING: ScriptRegistry lock is always released before ToolRunner's
        // dynamic_tools lock is acquired. Never hold both locks simultaneously.
        // Only now: unregister the old name if the annotation was renamed in-place.
        let prev_name = registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .name_for(path)
            .map(str::to_owned);
        if let Some(ref prev) = prev_name {
            if prev != &new_name {
                tool_runner.unregister_dynamic(prev);
                info!(
                    old_name = %prev,
                    new_name = %new_name,
                    path = %path.display(),
                    "ScriptWatcher: annotation name changed — unregistered old tool"
                );
            }
        }

        tool_runner.register_dynamic(Box::new(tool));
        registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(path.to_path_buf(), new_name.clone());
        info!(tool_name = %new_name, path = %path.display(), "ScriptWatcher: registered script tool");
        Some(new_name)
    }

    fn try_unregister(
        tool_runner: &ToolRunner,
        registry: &Arc<Mutex<ScriptRegistry>>,
        path: &Path,
    ) {
        // File is already gone so canonicalize may fail — fall back to the raw path.
        // Both forms are tried: the registry was recorded with a canonicalized path
        // (from try_register), so the canonical form should match.
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        // Acquire the lock once and try both path forms within the same critical section.
        // The double-lock pattern (or_else re-acquiring) is avoided to prevent unnecessary
        // lock churn — a single guard is sufficient since both lookups are read-then-remove.
        let name = {
            let mut reg = registry.lock().unwrap_or_else(|e| e.into_inner());
            // Prefer the canonicalized form (matches how try_register recorded the path).
            // Fall back to the raw path when canonicalize failed (file already deleted).
            reg.remove_path(&canonical)
                .or_else(|| reg.remove_path(path))
        };

        if let Some(name) = name {
            tool_runner.unregister_dynamic(&name);
            info!(
                tool_name = %name,
                path = %path.display(),
                "ScriptWatcher: unregistered script tool"
            );
        }
    }

    /// Returns true for editor temp/swap files that should be ignored by the watcher.
    /// These are created transiently during saves and never represent real scripts.
    fn is_editor_artifact(path: &Path) -> bool {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => return true, // Non-UTF8 name — skip.
        };
        name.starts_with('.')        // hidden files (.swp, .vim~, etc.)
            || name.starts_with('#') // Emacs lock files (#foo.sh#)
            || name.ends_with('~')   // Emacs backup files
            || name.ends_with(".swp")
            || name.ends_with(".swo")
            || name.ends_with(".swpx") // Neovim swap v2
            || name.ends_with(".tmp")
            || name.ends_with(".bak")
    }
}

impl Drop for ScriptWatcher {
    fn drop(&mut self) {
        // Step 1: explicitly drop the watcher, which closes the `tx` channel end.
        // The background thread's `for event in rx` loop then reaches end-of-channel
        // and exits. This MUST happen before the join, otherwise the join blocks
        // forever waiting for a thread that is still waiting on an open channel.
        drop(self.watcher.take());

        // Step 2: join the thread — now guaranteed to exit promptly.
        if let Some(handle) = self.event_thread.take() {
            let _ = handle.join();
        }
    }
}

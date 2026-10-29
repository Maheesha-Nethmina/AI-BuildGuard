use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
        Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::{checks, status};

/// Wait this long after the last event before reporting a batch.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Directories we never react to, wherever they appear in the path.
const IGNORED_DIRS: [&str; 6] = ["node_modules", ".git", "dist", "build", "target", ".next"];

/// Only these file types can affect type-checking or linting.
const RELEVANT_EXTENSIONS: [&str; 10] = [
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "css", "scss", "html",
];

/// Ever-increasing change counter shared by the watcher, the checker and the status manager.
pub(crate) static REVISION: AtomicU64 = AtomicU64::new(0);

/// Holds the active watcher. Dropping it stops watching.
#[derive(Default)]
pub(crate) struct WatcherState(pub(crate) Mutex<Option<RecommendedWatcher>>);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChangeBatch {
    revision: u64,
    files: Vec<String>,
    at_ms: u64,
}

fn is_relevant(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };

    for component in relative.components() {
        if let Component::Normal(name) = component {
            if let Some(name) = name.to_str() {
                if IGNORED_DIRS.contains(&name) {
                    return false;
                }
            }
        }
    }

    if relative.file_name().and_then(|n| n.to_str()) == Some("package-lock.json") {
        return false;
    }

    matches!(
        relative.extension().and_then(|e| e.to_str()),
        Some(ext) if RELEVANT_EXTENSIONS.contains(&ext)
    )
}

/// Collects raw file events, waits until they go quiet, then emits one batch,
/// tells the status manager, and asks the checkers to run. The thread ends by
/// itself when the watcher (and its sender) is dropped.
fn spawn_debouncer(app: AppHandle, root: PathBuf, rx: mpsc::Receiver<PathBuf>) {
    let project_path = root.to_string_lossy().to_string();

    thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch: BTreeSet<PathBuf> = BTreeSet::new();
            batch.insert(first);

            loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok(path) => {
                        batch.insert(path);
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }

            let files: Vec<String> = batch
                .iter()
                .filter_map(|p| p.strip_prefix(&root).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .collect();

            let revision = REVISION.fetch_add(1, Ordering::SeqCst) + 1;
            let at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let _ = app.emit(
                "project-changed",
                ChangeBatch {
                    revision,
                    files,
                    at_ms,
                },
            );

            // The status manager learns about the change first, so any result for an
            // older revision is treated as out of date from this moment on.
            status::on_change(&app, revision);
            checks::request_check(&app, project_path.clone(), revision);
        }
    });
}

fn try_start_watcher(app: &AppHandle, project_path: &str) -> Result<(), String> {
    let root = PathBuf::from(project_path);
    if !root.is_dir() {
        return Err(format!("Project folder not found: {project_path}"));
    }

    let (tx, rx) = mpsc::channel::<PathBuf>();
    let filter_root = root.clone();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                for path in event.paths {
                    if is_relevant(&filter_root, &path) {
                        let _ = tx.send(path);
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;

    // Root: only direct children (package.json, tsconfig.json, vite.config.ts, ...).
    watcher
        .watch(&root, RecursiveMode::NonRecursive)
        .map_err(|e| e.to_string())?;

    // Source folder: everything inside, recursively.
    let src = root.join("src");
    if src.is_dir() {
        watcher
            .watch(&src, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
    }

    spawn_debouncer(app.clone(), root, rx);

    // Replacing the old watcher drops it, which stops the old watching.
    let state = app.state::<WatcherState>();
    let mut active = state.0.lock().map_err(|e| e.to_string())?;
    *active = Some(watcher);
    Ok(())
}

fn clear_watcher(app: &AppHandle) {
    let state = app.state::<WatcherState>();
    if let Ok(mut active) = state.0.lock() {
        *active = None;
    }
}

/// Starts watching `project_path`, tells the status manager whether that worked,
/// and requests a first check so the dashboard has an answer without a file change.
pub(crate) fn start_watcher(app: &AppHandle, project_path: &str) -> Result<(), String> {
    let result = try_start_watcher(app, project_path);

    if result.is_err() {
        // Never keep watching the previous project after a failed switch.
        clear_watcher(app);
    }

    status::set_project(app, Some(project_path.to_string()), result.is_ok());

    if result.is_ok() {
        checks::request_check(
            app,
            project_path.to_string(),
            REVISION.load(Ordering::SeqCst),
        );
    }

    result
}
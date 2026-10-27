use std::{
    collections::BTreeSet,
    fs,
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
use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_dialog::DialogExt;

// ---------------------------------------------------------------------------
// Settings (persisted in the app data folder)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Settings {
    project_path: Option<String>,
}

fn settings_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

fn load_settings(app: &AppHandle) -> Settings {
    settings_file(app)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_settings(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_file(app)?;
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// File watcher
// ---------------------------------------------------------------------------

/// Wait this long after the last event before reporting a batch.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Directories we never react to, wherever they appear in the path.
const IGNORED_DIRS: [&str; 6] = ["node_modules", ".git", "dist", "build", "target", ".next"];

/// Only these file types can affect type-checking or linting.
const RELEVANT_EXTENSIONS: [&str; 10] = [
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "css", "scss", "html",
];

/// Ever-increasing change counter. Step 5 uses it to discard stale check results.
static REVISION: AtomicU64 = AtomicU64::new(0);

/// Holds the active watcher. Dropping it stops watching.
#[derive(Default)]
struct WatcherState(Mutex<Option<RecommendedWatcher>>);

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

/// Collects raw file events, waits until they go quiet, then emits one batch.
/// The thread ends by itself when the watcher (and its sender) is dropped.
fn spawn_debouncer(app: AppHandle, root: PathBuf, rx: mpsc::Receiver<PathBuf>) {
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
        }
    });
}

fn start_watcher(app: &AppHandle, project_path: &str) -> Result<(), String> {
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

// ---------------------------------------------------------------------------
// Window helpers
// ---------------------------------------------------------------------------

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// ---------------------------------------------------------------------------
// Commands (the doorway between the UI and Rust)
// ---------------------------------------------------------------------------

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Settings {
    load_settings(&app)
}

#[tauri::command]
fn get_watcher_status(state: State<WatcherState>) -> bool {
    state.0.lock().map(|active| active.is_some()).unwrap_or(false)
}

// Async so the blocking folder dialog never runs on the main thread.
#[tauri::command]
async fn select_project(app: AppHandle) -> Result<Settings, String> {
    let current = load_settings(&app);

    let picked = app.dialog().file().blocking_pick_folder();
    let Some(folder) = picked else {
        // User cancelled the dialog: keep whatever was saved before.
        return Ok(current);
    };

    let path = folder.into_path().map_err(|e| e.to_string())?;

    if !path.join("package.json").is_file() {
        return Err(
            "This folder has no package.json. Please choose the root folder of a frontend project."
                .to_string(),
        );
    }

    let path_text = path.to_string_lossy().to_string();
    let settings = Settings {
        project_path: Some(path_text.clone()),
    };
    save_settings(&app, &settings)?;
    start_watcher(&app, &path_text)?;
    Ok(settings)
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(WatcherState::default())
        .setup(|app| {
            let open_item = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("AI BuildGuard")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            // Resume watching the saved project on launch.
            if let Some(path) = load_settings(app.handle()).project_path {
                if let Err(err) = start_watcher(app.handle(), &path) {
                    eprintln!("Could not start file watcher: {err}");
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it; the app keeps running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            get_settings,
            get_watcher_status,
            select_project
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
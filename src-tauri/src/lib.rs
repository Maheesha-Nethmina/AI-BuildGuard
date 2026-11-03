mod checks;
mod settings;
mod status;
mod tray;
mod types;
mod watcher;

use std::sync::{atomic::Ordering, mpsc, Mutex};

use tauri::{AppHandle, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

use checks::{CheckRequest, CheckResult, CheckerState};
use settings::{load_settings, save_settings, Settings};
use status::{StatusSnapshot, StatusState};
use watcher::{start_watcher, WatcherState, REVISION};

// ---------------------------------------------------------------------------
// Commands (the doorway between the UI and Rust)
// ---------------------------------------------------------------------------

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

#[tauri::command]
fn get_status(app: AppHandle) -> StatusSnapshot {
    status::current_snapshot(&app)
}

#[tauri::command]
fn get_last_check(app: AppHandle) -> Option<CheckResult> {
    status::last_check(&app)
}

#[tauri::command]
fn run_check(app: AppHandle) -> Result<(), String> {
    let settings = load_settings(&app);
    let Some(project_path) = settings.project_path else {
        return Err("Select a project first.".to_string());
    };
    checks::request_check(&app, project_path, REVISION.load(Ordering::SeqCst));
    Ok(())
}

// Async so the blocking folder dialog never runs on the main thread.
#[tauri::command]
async fn select_project(app: AppHandle) -> Result<Settings, String> {
    let current = load_settings(&app);

    let picked = app.dialog().file().blocking_pick_folder();
    let Some(folder) = picked else {
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

#[tauri::command]
fn clear_project(
    app: AppHandle,
    watcher_state: State<WatcherState>,
    status_state: State<StatusState>,
) -> Result<Settings, String> {
    // 1. Overwrite settings with no project
    let settings = Settings {
        project_path: None,
    };
    save_settings(&app, &settings)?;

    // 2. Stop the background file watcher
    if let Ok(mut active) = watcher_state.0.lock() {
        *active = None;
    }

    // 3. Reset the internal status state back to the default (NoProject)
    if let Ok(mut snap) = status_state.0.lock() {
        if let Ok(default_snap) = StatusState::default().0.into_inner() {
            *snap = default_snap;
        }
    }

    Ok(settings)
}

#[tauri::command]
fn open_in_vscode(path: String, line: u32, column: u32) -> Result<(), String> {
    use std::process::Command;
    let arg = format!("{}:{}:{}", path, line, column);
    
    let mut cmd = Command::new(if cfg!(windows) { "code.cmd" } else { "code" });
    cmd.arg("--goto").arg(&arg);
    
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    
    cmd.spawn().map_err(|e| format!("Could not launch VS Code: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn run_production_build_command(project_path: String) -> Result<crate::types::ProductionBuildResult, String> {
    checks::run_production_build(&project_path)
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (check_tx, check_rx) = mpsc::channel::<CheckRequest>();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(WatcherState::default())
        .manage(StatusState::default())
        .manage(CheckerState(Mutex::new(check_tx)))
        .setup(move |app| {
            tray::build_tray(app)?;

            match app
                .notification()
                .builder()
                .title("BuildGuard test")
                .body("If you see this, Windows toasts work")
                .show()
            {
                Ok(_) => eprintln!("startup test toast: show() Ok"),
                Err(err) => eprintln!("startup test toast failed: {err}"),
            }

            checks::spawn_checker(app.handle().clone(), check_rx);

            if let Some(path) = load_settings(app.handle()).project_path {
                if let Err(err) = start_watcher(app.handle(), &path) {
                    eprintln!("Could not start file watcher: {err}");
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            get_settings,
            get_watcher_status,
            get_status,
            get_last_check,
            run_check,
            select_project,
            clear_project,
            open_in_vscode,
            run_production_build_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
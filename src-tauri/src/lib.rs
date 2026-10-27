use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
        Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
// Settings
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
// Diagnostics Data Model
// ---------------------------------------------------------------------------

const CHECK_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RAW_OUTPUT_CHARS: usize = 20_000;
static RUN_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
enum Outcome {
    Passed,
    Errors,
    Unavailable,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Diagnostic {
    file: String,
    line: u32,
    column: u32,
    severity: String, // "error" | "warning"
    code: String,
    message: String,
    source: String, // "typescript" | "eslint"
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CheckResult {
    run_id: u64,
    revision: u64,
    project_path: String,
    outcome: Outcome,
    diagnostics: Vec<Diagnostic>,
    message: Option<String>,
    raw_output: String,
    duration_ms: u64,
    exit_code: Option<i32>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CheckStarted {
    run_id: u64,
    revision: u64,
}

struct CheckRequest {
    project_path: String,
    revision: u64,
}

struct CheckerState(Mutex<mpsc::Sender<CheckRequest>>);

#[derive(Default)]
struct LastCheckState(Mutex<Option<CheckResult>>);

fn request_check(app: &AppHandle, project_path: String, revision: u64) {
    let state = app.state::<CheckerState>();
    if let Ok(sender) = state.0.lock() {
        let _ = sender.send(CheckRequest {
            project_path,
            revision,
        });
    }
}

fn truncate_output(text: String) -> String {
    if text.chars().count() <= MAX_RAW_OUTPUT_CHARS {
        return text;
    }
    let truncated: String = text.chars().take(MAX_RAW_OUTPUT_CHARS).collect();
    format!("{truncated}\n... output truncated ...")
}

// ---------------------------------------------------------------------------
// TypeScript Parser
// ---------------------------------------------------------------------------

fn parse_tsc_line(line: &str) -> Option<Diagnostic> {
    if let Some(rest) = line.strip_prefix("error TS") {
        let (code, message) = rest.split_once(':')?;
        return Some(Diagnostic {
            file: "(project)".to_string(),
            line: 0,
            column: 0,
            severity: "error".to_string(),
            code: format!("TS{}", code.trim()),
            message: message.trim().to_string(),
            source: "typescript".to_string(),
        });
    }

    let marker = line.find("): error TS")?;
    let location = &line[..marker];
    let rest = &line[marker + "): ".len()..];

    let open = location.rfind('(')?;
    let file = &location[..open];
    let (line_text, column_text) = location[open + 1..].split_once(',')?;
    let (head, message) = rest.split_once(": ")?;
    let code = head.strip_prefix("error ")?;

    Some(Diagnostic {
        file: file.to_string(),
        line: line_text.trim().parse().ok()?,
        column: column_text.trim().parse().ok()?,
        severity: "error".to_string(),
        code: code.to_string(),
        message: message.trim().to_string(),
        source: "typescript".to_string(),
    })
}

fn parse_tsc_output(output: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            if let Some(last) = diagnostics.last_mut() {
                last.message.push('\n');
                last.message.push_str(line.trim());
            }
            continue;
        }
        if let Some(diagnostic) = parse_tsc_line(line) {
            diagnostics.push(diagnostic);
        }
    }
    diagnostics
}

// ---------------------------------------------------------------------------
// ESLint Parser (JSON Output)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EslintMessage {
    rule_id: Option<String>,
    severity: u8, // 1 = warning, 2 = error
    message: String,
    line: Option<u32>,
    column: Option<u32>,
}

#[derive(Deserialize)]
struct EslintFileResult {
    file_path: String,
    messages: Vec<EslintMessage>,
}

fn parse_eslint_json(json_str: &str, root_path: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let Ok(results) = serde_json::from_str::<Vec<EslintFileResult>>(json_str) else {
        return diagnostics;
    };

    for file_res in results {
        let rel_file = Path::new(&file_res.file_path)
            .strip_prefix(root_path)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| file_res.file_path.clone());

        for msg in file_res.messages {
            let severity = if msg.severity == 2 { "error" } else { "warning" };
            diagnostics.push(Diagnostic {
                file: rel_file.clone(),
                line: msg.line.unwrap_or(0),
                column: msg.column.unwrap_or(0),
                severity: severity.to_string(),
                code: msg.rule_id.unwrap_or_else(|| "eslint".to_string()),
                message: msg.message,
                source: "eslint".to_string(),
            });
        }
    }
    diagnostics
}

// ---------------------------------------------------------------------------
// Combined Diagnostic Runner
// ---------------------------------------------------------------------------

fn run_diagnostics(request: &CheckRequest, run_id: u64) -> CheckResult {
    let started = Instant::now();
    let root = PathBuf::from(&request.project_path);
    let mut all_diagnostics = Vec::new();
    let mut raw_logs = String::new();

    // 1. Run TypeScript Check
    let tsc_script = root.join("node_modules").join("typescript").join("bin").join("tsc");
    if tsc_script.is_file() && root.join("tsconfig.json").is_file() {
        let mut cmd = Command::new("node");
        cmd.arg(&tsc_script)
            .args(["-b", "--pretty", "false"])
            .current_dir(&root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }

        if let Ok(child) = cmd.spawn() {
            if let Ok(output) = child.wait_with_output() {
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                raw_logs.push_str("--- TypeScript Output ---\n");
                raw_logs.push_str(&combined);
                raw_logs.push('\n');
                all_diagnostics.extend(parse_tsc_output(&combined));
            }
        }
    }

    // 2. Run ESLint Check
    let eslint_script = root.join("node_modules").join("eslint").join("bin").join("eslint.js");
    if eslint_script.is_file() {
        let mut cmd = Command::new("node");
        cmd.arg(&eslint_script)
            .args(["src", "--format", "json"])
            .current_dir(&root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }

        if let Ok(child) = cmd.spawn() {
            if let Ok(output) = child.wait_with_output() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                raw_logs.push_str("--- ESLint Output ---\n");
                raw_logs.push_str(&stdout);
                raw_logs.push('\n');
                all_diagnostics.extend(parse_eslint_json(&stdout, &root));
            }
        }
    }

    let outcome = if all_diagnostics.iter().any(|d| d.severity == "error") {
        Outcome::Errors
    } else {
        Outcome::Passed
    };

    CheckResult {
        run_id,
        revision: request.revision,
        project_path: request.project_path.clone(),
        outcome,
        diagnostics: all_diagnostics,
        message: None,
        raw_output: truncate_output(raw_logs),
        duration_ms: started.elapsed().as_millis() as u64,
        exit_code: Some(0),
    }
}

fn spawn_checker(app: AppHandle, rx: mpsc::Receiver<CheckRequest>) {
    thread::spawn(move || {
        while let Ok(mut request) = rx.recv() {
            while let Ok(newer) = rx.try_recv() {
                request = newer;
            }

            let run_id = RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = app.emit("check-started", CheckStarted { run_id, revision: request.revision });

            let result = run_diagnostics(&request, run_id);

            {
                let state = app.state::<LastCheckState>();
                if let Ok(mut last) = state.0.lock() {
                    *last = Some(result.clone());
                }
            }

            let _ = app.emit("check-finished", result);
        }
    });
}

// ---------------------------------------------------------------------------
// File Watcher
// ---------------------------------------------------------------------------

const DEBOUNCE: Duration = Duration::from_millis(500);
const IGNORED_DIRS: [&str; 6] = ["node_modules", ".git", "dist", "build", "target", ".next"];
const RELEVANT_EXTENSIONS: [&str; 10] = [
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "css", "scss", "html",
];

static REVISION: AtomicU64 = AtomicU64::new(0);

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

fn spawn_debouncer(app: AppHandle, root: PathBuf, rx: mpsc::Receiver<PathBuf>) {
    let project_path = root.to_string_lossy().to_string();

    thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch: BTreeSet<PathBuf> = BTreeSet::new();
            batch.insert(first);

            loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok(path) => { batch.insert(path); }
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
                ChangeBatch { revision, files, at_ms },
            );

            request_check(&app, project_path.clone(), revision);
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

    watcher.watch(&root, RecursiveMode::NonRecursive).map_err(|e| e.to_string())?;
    let src = root.join("src");
    if src.is_dir() {
        watcher.watch(&src, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
    }

    spawn_debouncer(app.clone(), root, rx);

    {
        let state = app.state::<WatcherState>();
        let mut active = state.0.lock().map_err(|e| e.to_string())?;
        *active = Some(watcher);
    }

    request_check(app, project_path.to_string(), REVISION.load(Ordering::SeqCst));
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// ---------------------------------------------------------------------------
// Commands
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
fn get_last_check(state: State<LastCheckState>) -> Option<CheckResult> {
    state.0.lock().ok().and_then(|last| (*last).clone())
}

#[tauri::command]
fn run_check(app: AppHandle) -> Result<(), String> {
    let settings = load_settings(&app);
    let Some(project_path) = settings.project_path else {
        return Err("Select a project first.".to_string());
    };
    request_check(&app, project_path, REVISION.load(Ordering::SeqCst));
    Ok(())
}

#[tauri::command]
async fn select_project(app: AppHandle) -> Result<Settings, String> {
    let current = load_settings(&app);
    let picked = app.dialog().file().blocking_pick_folder();
    let Some(folder) = picked else {
        return Ok(current);
    };

    let path = folder.into_path().map_err(|e| e.to_string())?;
    if !path.join("package.json").is_file() {
        return Err("This folder has no package.json. Please choose the root folder of a frontend project.".to_string());
    }

    let path_text = path.to_string_lossy().to_string();
    let settings = Settings { project_path: Some(path_text.clone()) };
    save_settings(&app, &settings)?;
    start_watcher(&app, &path_text)?;
    Ok(settings)
}

// ---------------------------------------------------------------------------
// App Entry Point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (check_tx, check_rx) = mpsc::channel::<CheckRequest>();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(WatcherState::default())
        .manage(LastCheckState::default())
        .manage(CheckerState(Mutex::new(check_tx)))
        .setup(move |app| {
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

            spawn_checker(app.handle().clone(), check_rx);

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
            get_last_check,
            run_check,
            select_project
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
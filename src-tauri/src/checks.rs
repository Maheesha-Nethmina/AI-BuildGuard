use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::status;

/// A single checker that takes longer than this is stopped and reported as unavailable.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// Raw tool output stored for the UI is capped to this many characters.
const MAX_RAW_OUTPUT_CHARS: usize = 20_000;

/// Config files that tell us a project uses ESLint.
const ESLINT_CONFIG_FILES: &[&str] = &[
    "eslint.config.js",
    "eslint.config.mjs",
    "eslint.config.cjs",
    "eslint.config.ts",
    "eslint.config.mts",
    "eslint.config.cts",
    ".eslintrc",
    ".eslintrc.js",
    ".eslintrc.cjs",
    ".eslintrc.json",
    ".eslintrc.yaml",
    ".eslintrc.yml",
];

/// Ever-increasing id of each check run.
static RUN_ID: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Outcome {
    Passed,
    Warnings,
    Errors,
    Unavailable,
    Skipped,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Diagnostic {
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) column: u32,
    pub(crate) severity: String,
    pub(crate) source: String,
    pub(crate) code: String,
    pub(crate) message: String,
}

/// What one checker (TypeScript or ESLint) reported for one run.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckerReport {
    name: String,
    outcome: Outcome,
    message: Option<String>,
    raw_output: String,
    exit_code: Option<i32>,
    duration_ms: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckResult {
    pub(crate) run_id: u64,
    pub(crate) revision: u64,
    pub(crate) project_path: String,
    pub(crate) outcome: Outcome,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) reports: Vec<CheckerReport>,
    pub(crate) duration_ms: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CheckStarted {
    run_id: u64,
    revision: u64,
}

pub(crate) struct CheckRequest {
    project_path: String,
    revision: u64,
}

/// The output of one checker: its report plus the problems it found.
struct CheckerRun {
    report: CheckerReport,
    diagnostics: Vec<Diagnostic>,
}

/// Sender used by the watcher and the UI to ask the worker for a check.
pub(crate) struct CheckerState(pub(crate) Mutex<mpsc::Sender<CheckRequest>>);

pub(crate) fn request_check(app: &AppHandle, project_path: String, revision: u64) {
    let state = app.state::<CheckerState>();
    if let Ok(sender) = state.0.lock() {
        let _ = sender.send(CheckRequest {
            project_path,
            revision,
        });
    }
}

// ---------------------------------------------------------------------------
// Shared helpers for running Node-based tools
// ---------------------------------------------------------------------------

struct ProcessOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timed_out: bool,
}

fn read_pipe<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).to_string()
    })
}

/// Runs `node <script> <args>` inside `cwd`. No shell is involved: the program,
/// the arguments and the working directory are passed separately.
fn run_node_script(script: &Path, args: &[&str], cwd: &Path) -> Result<ProcessOutput, String> {
    let mut command = Command::new("node");
    command
        .arg(script)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Do not flash a console window on Windows.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }

    let mut child = command.spawn().map_err(|err| {
        format!("Could not start Node.js. Is it installed and on PATH? ({err})")
    })?;

    // Read both pipes on their own threads so a full pipe can never block the tool.
    let stdout_reader = child.stdout.take().map(read_pipe);
    let stderr_reader = child.stderr.take().map(read_pipe);

    let deadline = Instant::now() + CHECK_TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break None,
        }
    };

    let stdout = stdout_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    Ok(ProcessOutput {
        stdout,
        stderr,
        exit_code: status.and_then(|s| s.code()),
        timed_out,
    })
}

fn truncate_output(text: String) -> String {
    if text.chars().count() <= MAX_RAW_OUTPUT_CHARS {
        return text;
    }
    let truncated: String = text.chars().take(MAX_RAW_OUTPUT_CHARS).collect();
    format!("{truncated}\n... output truncated ...")
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

fn make_report(
    name: &str,
    started: Instant,
    outcome: Outcome,
    message: Option<String>,
    raw_output: String,
    exit_code: Option<i32>,
) -> CheckerReport {
    CheckerReport {
        name: name.to_string(),
        outcome,
        message,
        raw_output,
        exit_code,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

/// A run with no problems and no raw output, only an outcome and a message.
fn simple_run(name: &str, started: Instant, outcome: Outcome, message: String) -> CheckerRun {
    CheckerRun {
        report: make_report(name, started, outcome, Some(message), String::new(), None),
        diagnostics: Vec::new(),
    }
}

fn crashed_run(name: &str) -> CheckerRun {
    simple_run(
        name,
        Instant::now(),
        Outcome::Unavailable,
        format!("The {name} check crashed unexpectedly."),
    )
}

// ---------------------------------------------------------------------------
// TypeScript checker
// ---------------------------------------------------------------------------

/// Parses one tsc line such as:
/// `src/App.tsx(12,5): error TS2322: Type 'string' is not assignable to type 'number'.`
fn parse_tsc_line(line: &str) -> Option<Diagnostic> {
    // Errors without a file: "error TS5083: Cannot read file ..."
    if let Some(rest) = line.strip_prefix("error TS") {
        let (code, message) = rest.split_once(':')?;
        return Some(Diagnostic {
            file: "(project)".to_string(),
            line: 0,
            column: 0,
            severity: "error".to_string(),
            source: "typescript".to_string(),
            code: format!("TS{}", code.trim()),
            message: message.trim().to_string(),
        });
    }

    let marker = line.find("): error TS")?;
    let location = &line[..marker];
    let rest = &line[marker + "): ".len()..]; // "error TS2322: message"

    let open = location.rfind('(')?;
    let file = &location[..open];
    let (line_text, column_text) = location[open + 1..].split_once(',')?;

    let (head, message) = rest.split_once(": ")?; // head = "error TS2322"
    let code = head.strip_prefix("error ")?;

    Some(Diagnostic {
        file: file.to_string(),
        line: line_text.trim().parse().ok()?,
        column: column_text.trim().parse().ok()?,
        severity: "error".to_string(),
        source: "typescript".to_string(),
        code: code.to_string(),
        message: message.trim().to_string(),
    })
}

fn parse_tsc_output(output: &str) -> Vec<Diagnostic> {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Indented lines continue the previous message.
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

fn run_typescript_check(root: &Path) -> CheckerRun {
    let started = Instant::now();
    let tsc_script = root
        .join("node_modules")
        .join("typescript")
        .join("bin")
        .join("tsc");

    if !root.join("tsconfig.json").is_file() {
        return simple_run(
            "typescript",
            started,
            Outcome::Unavailable,
            "No tsconfig.json found in the project root.".to_string(),
        );
    }

    if !tsc_script.is_file() {
        return simple_run(
            "typescript",
            started,
            Outcome::Unavailable,
            "TypeScript is not installed in this project. Run npm install in the project folder."
                .to_string(),
        );
    }

    // `tsc -b` is what the Vite template's own build runs, so results match the real build.
    let output = match run_node_script(&tsc_script, &["-b", "--pretty", "false"], root) {
        Ok(output) => output,
        Err(message) => {
            return simple_run("typescript", started, Outcome::Unavailable, message);
        }
    };

    let combined = format!("{}{}", output.stdout, output.stderr);
    let diagnostics = parse_tsc_output(&combined);
    let raw_output = truncate_output(combined);

    if output.timed_out {
        return CheckerRun {
            report: make_report(
                "typescript",
                started,
                Outcome::Unavailable,
                Some(format!(
                    "The TypeScript check timed out after {} seconds.",
                    CHECK_TIMEOUT.as_secs()
                )),
                raw_output,
                None,
            ),
            diagnostics: Vec::new(),
        };
    }

    if !diagnostics.is_empty() {
        return CheckerRun {
            report: make_report(
                "typescript",
                started,
                Outcome::Errors,
                None,
                raw_output,
                output.exit_code,
            ),
            diagnostics,
        };
    }

    if output.exit_code == Some(0) {
        return CheckerRun {
            report: make_report(
                "typescript",
                started,
                Outcome::Passed,
                None,
                raw_output,
                output.exit_code,
            ),
            diagnostics: Vec::new(),
        };
    }

    let code_text = output
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    CheckerRun {
        report: make_report(
            "typescript",
            started,
            Outcome::Unavailable,
            Some(format!(
                "TypeScript exited with code {code_text} but no errors could be read. See the raw output."
            )),
            raw_output,
            output.exit_code,
        ),
        diagnostics: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// ESLint checker
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EslintFileResult {
    file_path: String,
    #[serde(default)]
    messages: Vec<EslintMessage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EslintMessage {
    #[serde(default)]
    rule_id: Option<String>,
    #[serde(default)]
    severity: u8,
    #[serde(default)]
    message: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    column: u32,
}

fn has_eslint_config(root: &Path) -> bool {
    ESLINT_CONFIG_FILES
        .iter()
        .any(|name| root.join(name).is_file())
}

fn relative_display_path(root: &Path, file_path: &str) -> String {
    let path = Path::new(file_path);
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

fn run_eslint_check(root: &Path) -> CheckerRun {
    let started = Instant::now();
    let script = root
        .join("node_modules")
        .join("eslint")
        .join("bin")
        .join("eslint.js");

    // Not using ESLint is fine: skipping never makes the project look unhealthy.
    if !script.is_file() {
        return simple_run(
            "eslint",
            started,
            Outcome::Skipped,
            "ESLint is not installed in this project, so lint checks were skipped.".to_string(),
        );
    }

    if !has_eslint_config(root) {
        return simple_run(
            "eslint",
            started,
            Outcome::Skipped,
            "No ESLint configuration file found, so lint checks were skipped.".to_string(),
        );
    }

    // Read-only: never --fix, never --cache (that would write files into the project).
    let output = match run_node_script(&script, &["--format", "json", "."], root) {
        Ok(output) => output,
        Err(message) => {
            return simple_run("eslint", started, Outcome::Unavailable, message);
        }
    };

    let raw_output = truncate_output(format!("{}{}", output.stderr, output.stdout));

    if output.timed_out {
        return CheckerRun {
            report: make_report(
                "eslint",
                started,
                Outcome::Unavailable,
                Some(format!(
                    "The ESLint check timed out after {} seconds.",
                    CHECK_TIMEOUT.as_secs()
                )),
                raw_output,
                None,
            ),
            diagnostics: Vec::new(),
        };
    }

    // ESLint exits 0 (clean) or 1 (lint errors found). Anything else means it failed to run.
    if !matches!(output.exit_code, Some(0) | Some(1)) {
        let code_text = output
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let detail = first_line(&output.stderr);
        return CheckerRun {
            report: make_report(
                "eslint",
                started,
                Outcome::Unavailable,
                Some(format!(
                    "ESLint failed to run (exit code {code_text}). {detail}"
                )),
                raw_output,
                output.exit_code,
            ),
            diagnostics: Vec::new(),
        };
    }

    let files: Vec<EslintFileResult> = match serde_json::from_str(&output.stdout) {
        Ok(files) => files,
        Err(err) => {
            return CheckerRun {
                report: make_report(
                    "eslint",
                    started,
                    Outcome::Unavailable,
                    Some(format!("Could not read ESLint's JSON output: {err}")),
                    raw_output,
                    output.exit_code,
                ),
                diagnostics: Vec::new(),
            };
        }
    };

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    for file in files {
        let display_path = relative_display_path(root, &file.file_path);
        for message in file.messages {
            let severity = if message.severity >= 2 {
                "error"
            } else {
                "warning"
            };
            diagnostics.push(Diagnostic {
                file: display_path.clone(),
                line: message.line,
                column: message.column,
                severity: severity.to_string(),
                source: "eslint".to_string(),
                code: message.rule_id.unwrap_or_else(|| "eslint".to_string()),
                message: message.message,
            });
        }
    }

    let outcome = if diagnostics.iter().any(|d| d.severity == "error") {
        Outcome::Errors
    } else if !diagnostics.is_empty() {
        Outcome::Warnings
    } else {
        Outcome::Passed
    };

    CheckerRun {
        report: make_report("eslint", started, outcome, None, raw_output, output.exit_code),
        diagnostics,
    }
}

// ---------------------------------------------------------------------------
// Running all checkers
// ---------------------------------------------------------------------------

fn overall_outcome(reports: &[CheckerReport], diagnostics: &[Diagnostic]) -> Outcome {
    if diagnostics.iter().any(|d| d.severity == "error") {
        return Outcome::Errors;
    }
    // A checker that should have run but could not means we cannot call the project healthy.
    if reports
        .iter()
        .any(|r| matches!(r.outcome, Outcome::Unavailable))
    {
        return Outcome::Unavailable;
    }
    if diagnostics.iter().any(|d| d.severity == "warning") {
        return Outcome::Warnings;
    }
    Outcome::Passed
}

fn run_checks(request: &CheckRequest, run_id: u64) -> CheckResult {
    let started = Instant::now();
    let root = PathBuf::from(&request.project_path);

    // Both checkers run at the same time, so the total time is the slower one.
    let (typescript, eslint) = thread::scope(|scope| {
        let typescript_handle = scope.spawn(|| run_typescript_check(&root));
        let eslint_handle = scope.spawn(|| run_eslint_check(&root));
        (
            typescript_handle
                .join()
                .unwrap_or_else(|_| crashed_run("typescript")),
            eslint_handle
                .join()
                .unwrap_or_else(|_| crashed_run("eslint")),
        )
    });

    let mut diagnostics = typescript.diagnostics;
    diagnostics.extend(eslint.diagnostics);

    // Errors first, then warnings; within each, by file and position.
    diagnostics.sort_by(|a, b| {
        let rank = |d: &Diagnostic| -> u8 {
            if d.severity == "error" {
                0
            } else {
                1
            }
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.column.cmp(&b.column))
    });

    let reports = vec![typescript.report, eslint.report];
    let outcome = overall_outcome(&reports, &diagnostics);

    CheckResult {
        run_id,
        revision: request.revision,
        project_path: request.project_path.clone(),
        outcome,
        diagnostics,
        reports,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

/// One worker runs checks one at a time. Requests that pile up while a check is
/// running are merged: only the newest one is run next. The status manager is
/// told when a run starts and finishes.
pub(crate) fn spawn_checker(app: AppHandle, rx: mpsc::Receiver<CheckRequest>) {
    thread::spawn(move || {
        while let Ok(mut request) = rx.recv() {
            while let Ok(newer) = rx.try_recv() {
                request = newer;
            }

            let run_id = RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;

            status::on_check_started(&app, run_id, &request.project_path);
            let _ = app.emit(
                "check-started",
                CheckStarted {
                    run_id,
                    revision: request.revision,
                },
            );

            let result = run_checks(&request, run_id);

            let _ = app.emit("check-finished", result.clone());
            status::on_check_finished(&app, result);
        }
    });
}
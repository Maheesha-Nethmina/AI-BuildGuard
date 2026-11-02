use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusKind {
    NoProject,
    NotWatching,
    Initializing,
    Checking,
    Healthy,
    Warnings,
    Errors,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSnapshot {
    pub kind: StatusKind,
    pub label: String,
    pub error_count: usize,
    pub warning_count: usize,
    pub revision: usize,
    pub checked_revision: Option<usize>,
    pub project_path: Option<String>,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            kind: StatusKind::NoProject,
            label: "No project".to_string(),
            error_count: 0,
            warning_count: 0,
            revision: 0,
            checked_revision: None,
            project_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub run_id: usize,
    pub revision: usize,
    pub project_path: String,
    pub outcome: String,
    pub diagnostics: Vec<Diagnostic>,
    pub reports: Vec<CheckerReport>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub severity: String,
    pub source: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckerReport {
    pub name: String,
    pub outcome: String,
    pub message: Option<String>,
    pub raw_output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}
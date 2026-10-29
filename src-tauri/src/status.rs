use std::{path::Path, sync::Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::{
    checks::{CheckResult, Outcome},
    tray,
};

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) enum StatusKind {
    NoProject,
    NotWatching,
    Initializing,
    Checking,
    Healthy,
    Warnings,
    Errors,
    Unavailable,
}

/// Everything the UI and the tray need to know, computed in one place.
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusSnapshot {
    pub(crate) kind: StatusKind,
    pub(crate) label: String,
    pub(crate) error_count: usize,
    pub(crate) warning_count: usize,
    /// Latest file-change revision seen.
    pub(crate) revision: u64,
    /// Revision the last accepted check result belongs to.
    pub(crate) checked_revision: Option<u64>,
    pub(crate) project_path: Option<String>,
}

fn count_label(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

// ---------------------------------------------------------------------------
// Pure status logic (no Tauri types, easy to test)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct StatusManager {
    project_path: Option<String>,
    watching: bool,
    latest_revision: u64,
    running_run: Option<u64>,
    last_result: Option<CheckResult>,
    /// Last status that was not transient (Healthy, Warnings, Errors, Unavailable).
    /// Used to decide whether a new Errors status deserves a notification.
    settled: Option<StatusKind>,
}

impl StatusManager {
    pub(crate) fn set_project(&mut self, path: Option<String>, watching: bool) {
        if self.project_path != path {
            // A different project: nothing known about the old one applies.
            self.last_result = None;
            self.settled = None;
            self.running_run = None;
        }
        self.project_path = path;
        self.watching = watching;
    }

    pub(crate) fn on_change(&mut self, revision: u64) {
        self.latest_revision = self.latest_revision.max(revision);
    }

    pub(crate) fn on_check_started(&mut self, run_id: u64, project_path: &str) {
        if self.project_path.as_deref() == Some(project_path) {
            self.running_run = Some(run_id);
        }
    }

    pub(crate) fn on_check_finished(&mut self, result: CheckResult) {
        if self.running_run == Some(result.run_id) {
            self.running_run = None;
        }

        // A result for another project must never be shown for this one.
        if self.project_path.as_deref() != Some(result.project_path.as_str()) {
            return;
        }

        // An older run must never overwrite a newer one.
        if let Some(previous) = &self.last_result {
            if result.run_id < previous.run_id {
                return;
            }
        }

        self.last_result = Some(result);
    }

    pub(crate) fn last_result(&self) -> Option<CheckResult> {
        self.last_result.clone()
    }

    pub(crate) fn snapshot(&self) -> StatusSnapshot {
        let make = |kind: StatusKind,
                    label: String,
                    errors: usize,
                    warnings: usize,
                    checked: Option<u64>| StatusSnapshot {
            kind,
            label,
            error_count: errors,
            warning_count: warnings,
            revision: self.latest_revision,
            checked_revision: checked,
            project_path: self.project_path.clone(),
        };

        if self.project_path.is_none() {
            return make(StatusKind::NoProject, "No project".to_string(), 0, 0, None);
        }

        if !self.watching {
            return make(StatusKind::NotWatching, "Not watching".to_string(), 0, 0, None);
        }

        let Some(result) = &self.last_result else {
            return if self.running_run.is_some() {
                make(StatusKind::Checking, "Checking…".to_string(), 0, 0, None)
            } else {
                make(StatusKind::Initializing, "Starting…".to_string(), 0, 0, None)
            };
        };

        let errors = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == "error")
            .count();
        let warnings = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == "warning")
            .count();
        let checked = Some(result.revision);

        // Files changed after this result's check started, or a check is running:
        // the result describes an older state, so it cannot be reported as current.
        let is_stale = result.revision < self.latest_revision;
        if self.running_run.is_some() || is_stale {
            return make(StatusKind::Checking, "Checking…".to_string(), errors, warnings, checked);
        }

        match result.outcome {
            Outcome::Errors => make(
                StatusKind::Errors,
                count_label(errors, "error"),
                errors,
                warnings,
                checked,
            ),
            Outcome::Unavailable => make(
                StatusKind::Unavailable,
                "Checker unavailable".to_string(),
                errors,
                warnings,
                checked,
            ),
            Outcome::Warnings => make(
                StatusKind::Warnings,
                count_label(warnings, "warning"),
                errors,
                warnings,
                checked,
            ),
            Outcome::Passed | Outcome::Skipped => make(
                StatusKind::Healthy,
                "No errors".to_string(),
                errors,
                warnings,
                checked,
            ),
        }
    }

    /// Returns true when this snapshot should produce a notification:
    /// errors that appear after a settled, non-error status. The first settled
    /// status after launch or a project switch only sets the baseline.
    pub(crate) fn take_notification(&mut self, snapshot: &StatusSnapshot) -> bool {
        match snapshot.kind {
            StatusKind::Healthy
            | StatusKind::Warnings
            | StatusKind::Errors
            | StatusKind::Unavailable => {
                let previous = self.settled.replace(snapshot.kind);
                snapshot.kind == StatusKind::Errors
                    && matches!(previous, Some(kind) if kind != StatusKind::Errors)
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri glue: apply a change, then update the UI, the tray and notifications
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct StatusState(pub(crate) Mutex<StatusManager>);

fn update<F: FnOnce(&mut StatusManager)>(app: &AppHandle, mutate: F) {
    let state = app.state::<StatusState>();

    let (snapshot, changed, notify) = {
        let Ok(mut manager) = state.0.lock() else {
            return;
        };
        let before = manager.snapshot();
        mutate(&mut *manager);
        let after = manager.snapshot();
        let notify = manager.take_notification(&after);
        let changed = before != after;
        (after, changed, notify)
    };

    // The lock is released before anything slow (events, tray, notifications) happens.
    if changed {
        let _ = app.emit("status-changed", &snapshot);
        tray::update_tray(app, &snapshot);
    }

    if notify {
        send_notification(app, &snapshot);
    }
}

fn send_notification(app: &AppHandle, snapshot: &StatusSnapshot) {
    let project_name = snapshot
        .project_path
        .as_deref()
        .map(Path::new)
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("your project");

    eprintln!("send_notification called: {} in {}", snapshot.label, project_name);

    match app
        .notification()
        .builder()
        .title("BuildGuard found build errors")
        .body(format!("{} in {}", snapshot.label, project_name))
        .show()
    {
        Ok(_) => eprintln!("notification show() returned Ok"),
        Err(err) => eprintln!("Notification failed: {err}"),
    }
}

pub(crate) fn set_project(app: &AppHandle, project_path: Option<String>, watching: bool) {
    update(app, |manager| manager.set_project(project_path, watching));
}

pub(crate) fn on_change(app: &AppHandle, revision: u64) {
    update(app, |manager| manager.on_change(revision));
}

pub(crate) fn on_check_started(app: &AppHandle, run_id: u64, project_path: &str) {
    update(app, |manager| manager.on_check_started(run_id, project_path));
}

pub(crate) fn on_check_finished(app: &AppHandle, result: CheckResult) {
    update(app, |manager| manager.on_check_finished(result));
}

pub(crate) fn current_snapshot(app: &AppHandle) -> StatusSnapshot {
    let state = app.state::<StatusState>();
    let snapshot = match state.0.lock() {
        Ok(manager) => manager.snapshot(),
        Err(_) => StatusManager::default().snapshot(),
    };
    snapshot
}

pub(crate) fn last_check(app: &AppHandle) -> Option<CheckResult> {
    let state = app.state::<StatusState>();
    let result = match state.0.lock() {
        Ok(manager) => manager.last_result(),
        Err(_) => None,
    };
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::Diagnostic;

    const PROJECT: &str = "C:/project";

    fn diagnostic(severity: &str) -> Diagnostic {
        Diagnostic {
            file: "src/App.tsx".to_string(),
            line: 1,
            column: 1,
            severity: severity.to_string(),
            source: "typescript".to_string(),
            code: "TS0000".to_string(),
            message: "test".to_string(),
        }
    }

    fn result(
        project: &str,
        run_id: u64,
        revision: u64,
        outcome: Outcome,
        diagnostics: Vec<Diagnostic>,
    ) -> CheckResult {
        CheckResult {
            run_id,
            revision,
            project_path: project.to_string(),
            outcome,
            diagnostics,
            reports: Vec::new(),
            duration_ms: 0,
        }
    }

    fn manager() -> StatusManager {
        let mut manager = StatusManager::default();
        manager.set_project(Some(PROJECT.to_string()), true);
        manager
    }

    /// Mimics what `update` does after each change.
    fn settle(manager: &mut StatusManager) -> bool {
        let snapshot = manager.snapshot();
        manager.take_notification(&snapshot)
    }

    #[test]
    fn no_project_and_not_watching_are_reported() {
        let manager = StatusManager::default();
        assert_eq!(manager.snapshot().kind, StatusKind::NoProject);

        let mut manager = StatusManager::default();
        manager.set_project(Some(PROJECT.to_string()), false);
        assert_eq!(manager.snapshot().kind, StatusKind::NotWatching);
    }

    #[test]
    fn starts_as_initializing_then_checking() {
        let mut manager = manager();
        assert_eq!(manager.snapshot().kind, StatusKind::Initializing);

        manager.on_check_started(1, PROJECT);
        assert_eq!(manager.snapshot().kind, StatusKind::Checking);
    }

    #[test]
    fn healthy_only_after_a_clean_check_of_the_latest_revision() {
        let mut manager = manager();
        manager.on_change(1);
        manager.on_check_started(1, PROJECT);
        assert_ne!(manager.snapshot().kind, StatusKind::Healthy);

        manager.on_check_finished(result(PROJECT, 1, 1, Outcome::Passed, vec![]));
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.kind, StatusKind::Healthy);
        assert_eq!(snapshot.checked_revision, Some(1));
    }

    #[test]
    fn result_older_than_the_latest_change_is_never_current() {
        let mut manager = manager();
        manager.on_change(1);
        manager.on_check_started(1, PROJECT);
        manager.on_check_finished(result(
            PROJECT,
            1,
            1,
            Outcome::Errors,
            vec![diagnostic("error")],
        ));
        assert_eq!(manager.snapshot().kind, StatusKind::Errors);

        // A file changes: the old Errors result must stop being reported as current.
        manager.on_change(2);
        assert_eq!(manager.snapshot().kind, StatusKind::Checking);

        manager.on_check_started(2, PROJECT);
        manager.on_check_finished(result(PROJECT, 2, 2, Outcome::Passed, vec![]));
        assert_eq!(manager.snapshot().kind, StatusKind::Healthy);
    }

    #[test]
    fn result_for_another_project_is_ignored() {
        let mut manager = manager();
        manager.on_check_started(1, PROJECT);
        manager.on_check_finished(result(
            "C:/other",
            1,
            0,
            Outcome::Errors,
            vec![diagnostic("error")],
        ));

        assert_eq!(manager.snapshot().kind, StatusKind::Initializing);
        assert!(manager.last_result().is_none());
    }

    #[test]
    fn older_run_cannot_overwrite_a_newer_run() {
        let mut manager = manager();
        manager.on_change(5);
        manager.on_check_finished(result(
            PROJECT,
            5,
            5,
            Outcome::Errors,
            vec![diagnostic("error")],
        ));
        manager.on_check_finished(result(PROJECT, 3, 5, Outcome::Passed, vec![]));

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.kind, StatusKind::Errors);
        assert_eq!(snapshot.error_count, 1);
    }

    #[test]
    fn unavailable_checker_is_never_reported_as_healthy() {
        let mut manager = manager();
        manager.on_change(1);
        manager.on_check_finished(result(PROJECT, 1, 1, Outcome::Unavailable, vec![]));
        assert_eq!(manager.snapshot().kind, StatusKind::Unavailable);
    }

    #[test]
    fn warnings_are_reported_separately_from_errors() {
        let mut manager = manager();
        manager.on_change(1);
        manager.on_check_finished(result(
            PROJECT,
            1,
            1,
            Outcome::Warnings,
            vec![diagnostic("warning")],
        ));

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.kind, StatusKind::Warnings);
        assert_eq!(snapshot.warning_count, 1);
        assert_eq!(snapshot.error_count, 0);
    }

    #[test]
    fn first_result_after_launch_only_sets_the_baseline() {
        let mut manager = manager();
        manager.on_change(1);
        manager.on_check_finished(result(
            PROJECT,
            1,
            1,
            Outcome::Errors,
            vec![diagnostic("error")],
        ));
        assert!(!settle(&mut manager));
    }

    #[test]
    fn notifies_when_errors_appear_but_not_for_repeats() {
        let mut manager = manager();

        // Clean baseline.
        manager.on_change(1);
        manager.on_check_started(1, PROJECT);
        assert!(!settle(&mut manager));
        manager.on_check_finished(result(PROJECT, 1, 1, Outcome::Passed, vec![]));
        assert!(!settle(&mut manager));

        // Errors appear: one notification.
        manager.on_change(2);
        manager.on_check_started(2, PROJECT);
        manager.on_check_finished(result(
            PROJECT,
            2,
            2,
            Outcome::Errors,
            vec![diagnostic("error")],
        ));
        assert!(settle(&mut manager));

        // Errors, then Checking, then Errors again: no repeat.
        manager.on_change(3);
        manager.on_check_started(3, PROJECT);
        assert!(!settle(&mut manager));
        manager.on_check_finished(result(
            PROJECT,
            3,
            3,
            Outcome::Errors,
            vec![diagnostic("error"), diagnostic("error")],
        ));
        assert!(!settle(&mut manager));

        // Fixed, then broken again: notifies again.
        manager.on_change(4);
        manager.on_check_started(4, PROJECT);
        manager.on_check_finished(result(PROJECT, 4, 4, Outcome::Passed, vec![]));
        assert!(!settle(&mut manager));
        manager.on_change(5);
        manager.on_check_started(5, PROJECT);
        manager.on_check_finished(result(
            PROJECT,
            5,
            5,
            Outcome::Errors,
            vec![diagnostic("error")],
        ));
        assert!(settle(&mut manager));
    }
}
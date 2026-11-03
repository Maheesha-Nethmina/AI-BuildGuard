import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { onAction } from "@tauri-apps/plugin-notification";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";

type Settings = {
  projectPath: string | null;
};

type ChangeBatch = {
  revision: number;
  files: string[];
  atMs: number;
};

type Outcome = "passed" | "warnings" | "errors" | "unavailable" | "skipped";

type Diagnostic = {
  file: string;
  line: number;
  column: number;
  severity: string;
  source: string;
  code: string;
  message: string;
};

type CheckerReport = {
  name: string;
  outcome: Outcome;
  message: string | null;
  rawOutput: string;
  exitCode: number | null;
  durationMs: number;
};

type CheckResult = {
  runId: number;
  revision: number;
  projectPath: string;
  outcome: Outcome;
  diagnostics: Diagnostic[];
  reports: CheckerReport[];
  durationMs: number;
};

type StatusKind =
  | "noProject"
  | "notWatching"
  | "initializing"
  | "checking"
  | "healthy"
  | "warnings"
  | "errors"
  | "unavailable";

// Computed in Rust (status.rs). The UI only displays it.
type StatusSnapshot = {
  kind: StatusKind;
  label: string;
  errorCount: number;
  warningCount: number;
  revision: number;
  checkedRevision: number | null;
  projectPath: string | null;
};

const INITIAL_STATUS: StatusSnapshot = {
  kind: "noProject",
  label: "No project",
  errorCount: 0,
  warningCount: 0,
  revision: 0,
  checkedRevision: null,
  projectPath: null,
};

const PILL_CLASS: Record<StatusKind, string> = {
  noProject: "pill pill-idle",
  notWatching: "pill pill-warning",
  initializing: "pill pill-watching",
  checking: "pill pill-watching",
  healthy: "pill pill-success",
  warnings: "pill pill-warning",
  errors: "pill pill-danger",
  unavailable: "pill pill-warning",
};

const MAX_LOG_ENTRIES = 50;

const CHECKER_LABELS: Record<string, string> = {
  typescript: "TypeScript",
  eslint: "ESLint",
};

const OUTCOME_TEXT: Record<Outcome, string> = {
  passed: "Passed",
  warnings: "Warnings",
  errors: "Errors",
  unavailable: "Unavailable",
  skipped: "Skipped",
};

function checkerLabel(name: string): string {
  return CHECKER_LABELS[name] ?? name;
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString();
}

function formatSeconds(ms: number): string {
  return `${(ms / 1000).toFixed(1)}s`;
}

// ---- UI only: tooltip texts (plain-language help for every card and item) ----
const STATUS_TIPS: Record<StatusKind, string> = {
  noProject: "No project is selected yet. Choose a project folder to start monitoring.",
  notWatching:
    "A project is selected, but file watching is not active, so changes will not be detected automatically.",
  initializing: "BuildGuard is starting up and running its first check.",
  checking: "A check is running right now. Results update when it finishes.",
  healthy: "Both checkers finished and found no errors or warnings.",
  warnings: "No build-breaking errors, but some warnings were found.",
  errors: "The project has errors that need to be fixed.",
  unavailable: "A checker could not run, so the project cannot be confirmed healthy.",
};

const OUTCOME_TIPS: Record<Outcome, string> = {
  passed: "This checker ran and found nothing wrong.",
  warnings: "This checker found warnings. They do not break the build but are worth reviewing.",
  errors: "This checker found errors that need to be fixed.",
  unavailable: "This checker could not run (for example, it is not installed in the project).",
  skipped: "This checker was skipped for this run.",
};

const CHECKER_TIPS: Record<string, string> = {
  typescript:
    "TypeScript compiler check. Finds type mistakes, missing names and syntax errors in your code.",
  eslint:
    "ESLint check. Finds code-quality problems such as unused variables and risky patterns.",
};

// ---- UI only: tiny inline icon set (no extra dependencies) ----
const ICON_PATHS: Record<string, string> = {
  grid: "M3 3h7v7H3zM14 3h7v7h-7zM14 14h7v7h-7zM3 14h7v7H3z",
  list: "M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01",
  checks: "M9 11l3 3L22 4M21 12v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11",
  activity: "M22 12h-4l-3 9L9 3l-3 9H2",
  layers: "M12 2 2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5",
  error: "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM12 8v4M12 16h.01",
  warning:
    "M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0zM12 9v4M12 17h.01",
  clock: "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM12 6v6l4 2",
  folder: "M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z",
  play: "M5 3l14 9-14 9V3z",
  shield: "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10zM9 12l2 2 4-4",
  code: "M16 18l6-6-6-6M8 6l-6 6 6 6",
  info: "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM12 16v-4M12 8h.01",
};

function Icon({ name, size = 18 }: { name: string; size?: number }) {
  return (
    <svg
      className="icon"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.9"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={ICON_PATHS[name]} />
    </svg>
  );
}

// Small "i" button placed next to a card title. Hover or focus shows the help text.
function Hint({ text }: { text: string }) {
  return (
    <button type="button" className="hint" data-tip={text} aria-label={text}>
      <Icon name="info" size={15} />
    </button>
  );
}

// One shared tooltip for the whole app. Any element with a data-tip attribute
// shows its text on hover or keyboard focus. It is positioned with "fixed" so it
// is never clipped by scrolling lists or cards.
type TipState = { text: string; x: number; y: number; below: boolean };

function TooltipLayer() {
  const [tip, setTip] = useState<TipState | null>(null);

  useEffect(() => {
    const HALF_WIDTH = 130;
    const MARGIN = 8;

    function showFor(target: EventTarget | null) {
      const el =
        target instanceof Element ? (target.closest("[data-tip]") as HTMLElement | null) : null;
      const text = el?.dataset.tip;
      if (!el || !text) {
        setTip(null);
        return;
      }
      const rect = el.getBoundingClientRect();
      const x = Math.min(
        Math.max(rect.left + rect.width / 2, HALF_WIDTH + MARGIN),
        window.innerWidth - HALF_WIDTH - MARGIN,
      );
      const below = rect.top < 100;
      const y = below ? rect.bottom + MARGIN : rect.top - MARGIN;
      setTip((prev) =>
        prev && prev.text === text && prev.x === x && prev.y === y
          ? prev
          : { text, x, y, below },
      );
    }

    const onOver = (e: MouseEvent) => showFor(e.target);
    const onFocus = (e: FocusEvent) => showFor(e.target);
    const hide = () => setTip(null);

    document.addEventListener("mouseover", onOver);
    document.addEventListener("focusin", onFocus);
    document.addEventListener("focusout", hide);
    document.documentElement.addEventListener("mouseleave", hide);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("blur", hide);

    return () => {
      document.removeEventListener("mouseover", onOver);
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("focusout", hide);
      document.documentElement.removeEventListener("mouseleave", hide);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("blur", hide);
    };
  }, []);

  if (!tip) return null;

  return (
    <div
      role="tooltip"
      className={tip.below ? "tooltip tooltip-below" : "tooltip tooltip-above"}
      style={{ left: tip.x, top: tip.y }}
    >
      {tip.text}
    </div>
  );
}

// Subscribes to a Tauri event. The "cancelled" flag prevents a duplicate
// listener when React StrictMode mounts, unmounts and re-mounts in development.
function useTauriEvent<T>(eventName: string, handler: (payload: T) => void) {
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<T>(eventName, (event) => handler(event.payload)).then((stop) => {
      if (cancelled) {
        stop();
      } else {
        unlisten = stop;
      }
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [eventName]);
}

function App() {
  const [settings, setSettings] = useState<Settings>({ projectPath: null });
  const [status, setStatus] = useState<StatusSnapshot>(INITIAL_STATUS);
  const [changes, setChanges] = useState<ChangeBatch[]>([]);
  const [totalBatches, setTotalBatches] = useState(0);
  const [result, setResult] = useState<CheckResult | null>(null);
  const [error, setError] = useState("");
  const [greetMsg, setGreetMsg] = useState("");
  const [name, setName] = useState("");

  async function refresh() {
    const [loadedSettings, snapshot] = await Promise.all([
      invoke<Settings>("get_settings"),
      invoke<StatusSnapshot>("get_status"),
    ]);
    setSettings(loadedSettings);
    setStatus(snapshot);
  }

  useEffect(() => {
    refresh().catch((err) => setError(String(err)));
    invoke<CheckResult | null>("get_last_check")
      .then((last) => {
        if (last) setResult(last);
      })
      .catch((err) => setError(String(err)));
  }, []);

  // Listen for notification clicks to open/focus the app
  useEffect(() => {
    type NotificationListener = Awaited<ReturnType<typeof onAction>>;
    let unlistenFn: NotificationListener | undefined;

    const setupListener = async () => {
      unlistenFn = await onAction(async (_event) => {
        const appWindow = getCurrentWindow();

        // These commands are asynchronous in Tauri v2 and must be awaited
        await appWindow.unminimize();
        await appWindow.show();
        await appWindow.setFocus();
      });
    };

    setupListener();

    return () => {
      if (unlistenFn) {
        unlistenFn.unregister();
      }
    };
  }, []);

  useTauriEvent<StatusSnapshot>("status-changed", (snapshot) => {
    setStatus(snapshot);
  });

  useTauriEvent<CheckResult>("check-finished", (finished) => {
    setResult(finished);
  });

  useTauriEvent<ChangeBatch>("project-changed", (batch) => {
    setChanges((previous) => [batch, ...previous].slice(0, MAX_LOG_ENTRIES));
    setTotalBatches((count) => count + 1);
  });

  async function chooseProject() {
    setError("");
    const previousPath = settings.projectPath;
    try {
      const updated = await invoke<Settings>("select_project");
      if (updated.projectPath !== previousPath) {
        setChanges([]);
        setTotalBatches(0);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      await refresh();
    }
  }

  async function runCheck() {
    setError("");
    try {
      await invoke("run_check");
    } catch (err) {
      setError(String(err));
    }
  }

  async function greet() {
    setGreetMsg(await invoke<string>("greet", { name }));
  }

  const hasProject = settings.projectPath !== null;

  // Only show details that belong to the current project.
  const currentResult =
    result && result.projectPath === status.projectPath ? result : null;

  const isChecking = status.kind === "checking";
  const showCounts =
    status.kind === "healthy" || status.kind === "warnings" || status.kind === "errors";

  // Display-only helpers
  const problemCount = currentResult ? currentResult.diagnostics.length : 0;

  return (
    <div className="shell">
      <TooltipLayer />

      {/* ---------- Sidebar ---------- */}
      <aside className="sidebar">
        <div
          className="brand"
          data-tip="AI BuildGuard watches your project and reports TypeScript and ESLint problems every time you save a file."
        >
          <div className="brand-mark">
            <Icon name="shield" size={20} />
          </div>
          <div>
            <h1>BuildGuard</h1>
            <p className="subtitle">AI build monitor</p>
          </div>
        </div>

        <nav className="nav">
          <a
            href="#overview"
            className="nav-link"
            data-tip="Go to the top: selected project, buttons and summary numbers."
          >
            <Icon name="grid" />
            <span>Overview</span>
          </a>
          <a
            href="#problems"
            className="nav-link"
            data-tip="Go to the list of errors and warnings found in your code."
          >
            <Icon name="list" />
            <span>Problems</span>
            {problemCount > 0 && <span className="nav-count">{problemCount}</span>}
          </a>
          <a
            href="#checkers"
            className="nav-link"
            data-tip="Go to the tools (TypeScript, ESLint) that scan your project, with their raw output."
          >
            <Icon name="checks" />
            <span>Checkers</span>
          </a>
          <a
            href="#activity"
            className="nav-link"
            data-tip="Go to the log of files that changed in your project."
          >
            <Icon name="activity" />
            <span>Activity</span>
            {totalBatches > 0 && <span className="nav-count nav-count-muted">{totalBatches}</span>}
          </a>
        </nav>

        <div
          className="sidebar-foot"
          data-tip="Overall health of the monitored project right now."
        >
          <span className="foot-label">Status</span>
          <span className={PILL_CLASS[status.kind]} data-tip={STATUS_TIPS[status.kind]}>
            <span className="pill-dot" />
            {status.label}
          </span>
        </div>
      </aside>

      {/* ---------- Main ---------- */}
      <main className="main">
        <header className="page-head" id="overview">
          <div>
            <h2 className="page-title">Overview</h2>
            <p className="page-sub">Live type and lint status for your project.</p>
          </div>
          <div className="actions">
            {hasProject && (
              <button
                type="button"
                className="btn btn-primary"
                onClick={runCheck}
                data-tip="Run TypeScript and ESLint on the project right now, without waiting for a file change."
              >
                <Icon name="play" size={14} />
                Run check
              </button>
            )}
            <button
              type="button"
              className="btn"
              onClick={chooseProject}
              data-tip={
                hasProject
                  ? "Pick a different project folder to monitor. Activity history is cleared."
                  : "Pick the project folder you want BuildGuard to monitor."
              }
            >
              <Icon name="folder" size={15} />
              {hasProject ? "Change project" : "Select project folder"}
            </button>
          </div>
        </header>

        {error && (
          <p className="error" data-tip="Something went wrong. This is the error message returned by the app.">
            {error}
          </p>
        )}

        <section
          className="card project-card"
          data-tip="The folder BuildGuard is watching. Saving any file inside it triggers a new check."
        >
          <div className="project-icon">
            <Icon name="folder" size={20} />
          </div>
          <div className="project-body">
            <span className="card-kicker">Monitored project</span>
            <p className={hasProject ? "path" : "path path-empty"}>
              {settings.projectPath ?? "No project selected yet."}
            </p>
          </div>
        </section>

        <section className="stats">
          <div
            className="stat"
            data-tip="Counter that increases each time BuildGuard detects a new batch of file changes. Shows — until the first change."
          >
            <div className="stat-icon stat-icon-blue">
              <Icon name="layers" />
            </div>
            <div className="stat-text">
              <span className="stat-label">Revision</span>
              <span className="stat-value">{status.revision > 0 ? status.revision : "—"}</span>
            </div>
          </div>
          <div
            className="stat"
            data-tip="How many groups of file saves were detected since this project was selected. One save of one or more files = one batch."
          >
            <div className="stat-icon stat-icon-violet">
              <Icon name="activity" />
            </div>
            <div className="stat-text">
              <span className="stat-label">Change batches</span>
              <span className="stat-value">{totalBatches}</span>
            </div>
          </div>
          <div
            className="stat"
            data-tip="Number of errors in the last finished check. Errors break the build, so fix these first."
          >
            <div className="stat-icon stat-icon-red">
              <Icon name="error" />
            </div>
            <div className="stat-text">
              <span className="stat-label">Errors</span>
              <span className="stat-value">{showCounts ? status.errorCount : "—"}</span>
            </div>
          </div>
          <div
            className="stat"
            data-tip="Number of warnings in the last finished check. Warnings do not break the build but are worth reviewing."
          >
            <div className="stat-icon stat-icon-amber">
              <Icon name="warning" />
            </div>
            <div className="stat-text">
              <span className="stat-label">Warnings</span>
              <span className="stat-value">{showCounts ? status.warningCount : "—"}</span>
            </div>
          </div>
          <div
            className="stat"
            data-tip="How long the most recent full check (TypeScript and ESLint together) took to finish."
          >
            <div className="stat-icon stat-icon-green">
              <Icon name="clock" />
            </div>
            <div className="stat-text">
              <span className="stat-label">Last check</span>
              <span className="stat-value stat-small">
                {currentResult ? formatSeconds(currentResult.durationMs) : "—"}
              </span>
            </div>
          </div>
        </section>

        <section className="card" id="problems">
          <div className="card-head">
            <div className="title-wrap">
              <h3>Problems</h3>
              <Hint text="Every error and warning found in your code. Each row shows the tool that found it, the error code, and the file, line and column to fix." />
            </div>
            {isChecking ? (
              <span className="muted updating" data-tip="A new check is running. The list below will refresh when it finishes.">
                <span className="spinner" /> Updating…
              </span>
            ) : (
              currentResult && (
                <span className="muted" data-tip="Total number of problems in the latest check.">
                  {problemCount} found
                </span>
              )
            )}
          </div>

          {!currentResult && (
            <p className="empty">
              {hasProject ? "No check has finished yet." : "Select a project to start."}
            </p>
          )}

          {status.kind === "healthy" && (
            <p
              className="success-note"
              data-tip="Great news: both checkers ran successfully and reported nothing to fix."
            >
              <Icon name="shield" size={16} /> No errors or warnings found.
            </p>
          )}

          {status.kind === "unavailable" && (
            <div
              className="notice"
              data-tip="At least one checker failed to start, so the results may be incomplete."
            >
              <strong>A checker could not run.</strong>
              <p>The project cannot be reported as healthy until it can. See the checkers below.</p>
            </div>
          )}

          {currentResult && currentResult.diagnostics.length > 0 && (
            <ul className={isChecking ? "problems dimmed" : "problems"}>
              {currentResult.diagnostics.map((d, index) => (
                <li
                  key={`${d.source}:${d.file}:${d.line}:${d.column}:${d.code}:${index}`}
                  className={d.severity === "error" ? "problem" : "problem problem-warning"}
                >
                  <div className="problem-head">
                    <span
                      className="badge badge-source"
                      data-tip={`Reported by ${checkerLabel(d.source)}.`}
                    >
                      {checkerLabel(d.source)}
                    </span>
                    <span
                      className={
                        d.severity === "error" ? "badge badge-error" : "badge badge-warning"
                      }
                      data-tip={
                        d.severity === "error"
                          ? `Error code ${d.code}. Severity: error, this must be fixed.`
                          : `Rule ${d.code}. Severity: warning, worth reviewing.`
                      }
                    >
                      {d.code}
                    </span>
                    <span
                      className="problem-loc"
                      data-tip={
                        d.line > 0
                          ? `File ${d.file}, line ${d.line}, column ${d.column}. Open it in your editor to fix.`
                          : `File ${d.file}. No specific line was reported.`
                      }
                    >
                      {d.file}
                      {d.line > 0 ? `:${d.line}:${d.column}` : ""}
                    </span>
                  </div>
                  <p className="problem-msg">{d.message}</p>
                </li>
              ))}
            </ul>
          )}
        </section>

        {currentResult && (
          <section className="card" id="checkers">
            <div className="card-head">
              <div className="title-wrap">
                <h3>Checkers</h3>
                <Hint text="The tools that scan your project. Each one reports its own result, run time and raw output." />
              </div>
              <span
                className="muted"
                data-tip="The change revision these results belong to."
              >
                Revision {currentResult.revision}
              </span>
            </div>
            <ul className="reports">
              {currentResult.reports.map((report) => (
                <li key={report.name} className="report">
                  <div className="report-head">
                    <span
                      className="report-icon"
                      data-tip={CHECKER_TIPS[report.name] ?? `${checkerLabel(report.name)} checker.`}
                    >
                      <Icon name="code" size={15} />
                    </span>
                    <span
                      className="report-name"
                      data-tip={CHECKER_TIPS[report.name] ?? `${checkerLabel(report.name)} checker.`}
                    >
                      {checkerLabel(report.name)}
                    </span>
                    <span
                      className={`outcome outcome-${report.outcome}`}
                      data-tip={OUTCOME_TIPS[report.outcome]}
                    >
                      {OUTCOME_TEXT[report.outcome]}
                    </span>
                    <span
                      className="muted report-time"
                      data-tip="How long this checker took to run."
                    >
                      {formatSeconds(report.durationMs)}
                    </span>
                  </div>
                  {report.message && <p className="report-message">{report.message}</p>}
                  <details className="raw">
                    <summary data-tip="Show the exact text the tool printed. Useful for debugging or sharing with a teammate.">
                      Raw output
                    </summary>
                    <pre>{report.rawOutput || "(no output)"}</pre>
                  </details>
                </li>
              ))}
            </ul>
          </section>
        )}

        <section className="card" id="activity">
          <div className="card-head">
            <div className="title-wrap">
              <h3>Activity</h3>
              <Hint text="A live log of files that changed in your project. Each entry triggers a new check." />
            </div>
            <span
              className="muted"
              data-tip="Only the most recent batches are kept to keep the app fast."
            >
              Latest {MAX_LOG_ENTRIES} batches
            </span>
          </div>
          {changes.length === 0 ? (
            <p className="empty">
              {hasProject
                ? "Waiting for file changes. Save a file in your project."
                : "Select a project to start watching."}
            </p>
          ) : (
            <ul className="log">
              {changes.map((batch) => (
                <li key={batch.revision} className="log-item">
                  <div className="log-meta">
                    <span
                      className="badge"
                      data-tip={`Change batch number ${batch.revision}. Higher numbers are newer.`}
                    >
                      #{batch.revision}
                    </span>
                    <span className="muted" data-tip="Time when these file changes were detected.">
                      {formatTime(batch.atMs)}
                    </span>
                  </div>
                  <ul className="files">
                    {batch.files.map((file) => (
                      <li key={file} data-tip="A file that was saved or changed in this batch.">
                        {file}
                      </li>
                    ))}
                  </ul>
                </li>
              ))}
            </ul>
          )}
        </section>

        <details className="card dev">
          <summary data-tip="Extra tools for developers. You can ignore this section.">
            Developer tools
          </summary>
          <form
            className="row"
            onSubmit={(e) => {
              e.preventDefault();
              greet();
            }}
          >
            <input
              id="greet-input"
              onChange={(e) => setName(e.currentTarget.value)}
              placeholder="Enter a name..."
              data-tip="Type any name to test that the app can talk to its Rust backend."
            />
            <button
              type="submit"
              className="btn"
              data-tip="Sends the name to the backend and shows its reply."
            >
              Greet
            </button>
          </form>
          {greetMsg && <p className="muted">{greetMsg}</p>}
        </details>
      </main>
    </div>
  );
}

export default App;
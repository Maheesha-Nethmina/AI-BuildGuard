import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

type Settings = {
  projectPath: string | null;
};

type ChangeBatch = {
  revision: number;
  files: string[];
  atMs: number;
};

type Diagnostic = {
  file: string;
  line: number;
  column: number;
  severity: string;
  code: string;
  message: string;
  source: string;
};

type CheckResult = {
  runId: number;
  revision: number;
  projectPath: string;
  outcome: "passed" | "errors" | "unavailable";
  diagnostics: Diagnostic[];
  message: string | null;
  rawOutput: string;
  durationMs: number;
  exitCode: number | null;
};

type Pill = { className: string; text: string };

const MAX_LOG_ENTRIES = 50;

function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString();
}

// Subscribes to a Tauri event and cleans up properly to handle React StrictMode
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
  const [watching, setWatching] = useState(false);
  const [changes, setChanges] = useState<ChangeBatch[]>([]);
  const [totalBatches, setTotalBatches] = useState(0);
  const [latestRevision, setLatestRevision] = useState(0);
  const [result, setResult] = useState<CheckResult | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const [greetMsg, setGreetMsg] = useState("");
  const [name, setName] = useState("");

  async function refresh() {
    const [loadedSettings, isWatching] = await Promise.all([
      invoke<Settings>("get_settings"),
      invoke<boolean>("get_watcher_status"),
    ]);
    setSettings(loadedSettings);
    setWatching(isWatching);
  }

  useEffect(() => {
    refresh().catch((err) => setError(String(err)));
    invoke<CheckResult | null>("get_last_check")
      .then((last) => {
        if (last) setResult(last);
      })
      .catch((err) => setError(String(err)));
  }, []);

  useTauriEvent<ChangeBatch>("project-changed", (batch) => {
    setChanges((previous) => [batch, ...previous].slice(0, MAX_LOG_ENTRIES));
    setTotalBatches((count) => count + 1);
    setLatestRevision((current) => Math.max(current, batch.revision));
  });

  useTauriEvent<{ runId: number; revision: number }>("check-started", () => {
    setChecking(true);
  });

  useTauriEvent<CheckResult>("check-finished", (finished) => {
    setResult(finished);
    setChecking(false);
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

  // Only trust a result that belongs to the current project.
  const currentResult =
    result && result.projectPath === settings.projectPath ? result : null;
  // A result older than the latest file change is out of date.
  const isStale = currentResult !== null && currentResult.revision < latestRevision;
  const isChecking = checking || isStale;
  const errorCount = currentResult?.diagnostics.length ?? 0;

  function getPill(): Pill {
    if (!hasProject) return { className: "pill pill-idle", text: "No project" };
    if (!watching) return { className: "pill pill-warning", text: "Not watching" };
    if (isChecking) return { className: "pill pill-watching", text: "Checking…" };
    if (currentResult?.outcome === "errors") {
      return {
        className: "pill pill-danger",
        text: errorCount === 1 ? "1 error" : `${errorCount} errors`,
      };
    }
    if (currentResult?.outcome === "unavailable") {
      return { className: "pill pill-warning", text: "Checker unavailable" };
    }
    if (currentResult?.outcome === "passed") {
      return { className: "pill pill-success", text: "No errors" };
    }
    return { className: "pill pill-watching", text: "Watching" };
  }

  const pill = getPill();

  return (
    <main className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brand-mark">BG</div>
          <div>
            <h1>AI BuildGuard</h1>
            <p className="subtitle">Build error monitor</p>
          </div>
        </div>
        <span className={pill.className}>
          <span className="pill-dot" />
          {pill.text}
        </span>
      </header>

      <section className="card">
        <div className="card-head">
          <h2>Monitored project</h2>
          <div className="actions">
            {hasProject && (
              <button type="button" className="btn" onClick={runCheck}>
                Run check
              </button>
            )}
            <button type="button" className="btn" onClick={chooseProject}>
              {hasProject ? "Change project" : "Select project folder"}
            </button>
          </div>
        </div>
        <p className={hasProject ? "path" : "path path-empty"}>
          {settings.projectPath ?? "No project selected yet."}
        </p>
        {error && <p className="error">{error}</p>}
      </section>

      <section className="stats">
        <div className="stat">
          <span className="stat-label">Revision</span>
          <span className="stat-value">{latestRevision > 0 ? latestRevision : "—"}</span>
        </div>
        <div className="stat">
          <span className="stat-label">Change batches</span>
          <span className="stat-value">{totalBatches}</span>
        </div>
        <div className="stat">
          <span className="stat-label">Errors</span>
          <span className="stat-value">
            {currentResult && currentResult.outcome !== "unavailable" && !isChecking
              ? errorCount
              : "—"}
          </span>
        </div>
        <div className="stat">
          <span className="stat-label">Last check</span>
          <span className="stat-value stat-small">
            {currentResult ? `${(currentResult.durationMs / 1000).toFixed(1)}s` : "—"}
          </span>
        </div>
      </section>

      <section className="card">
        <div className="card-head">
          <h2>Problems</h2>
          {isChecking && <span className="muted">Updating…</span>}
        </div>

        {!currentResult && (
          <p className="empty">
            {hasProject ? "No check has finished yet." : "Select a project to start."}
          </p>
        )}

        {currentResult?.outcome === "passed" && (
          <p className={isChecking ? "success-note dimmed" : "success-note"}>
            TypeScript found no errors.
          </p>
        )}

        {currentResult?.outcome === "unavailable" && (
          <div className="notice">
            <strong>The checker could not run.</strong>
            <p>{currentResult.message}</p>
          </div>
        )}

        {currentResult?.outcome === "errors" && (
          <ul className={isChecking ? "problems dimmed" : "problems"}>
            {currentResult.diagnostics.map((d, index) => (
              <li key={`${d.file}:${d.line}:${d.column}:${d.code}:${index}`} className="problem">
                <div className="problem-head">
                  <span className="badge badge-error">{d.code}</span>
                  <span className="problem-loc">
                    {d.file}
                    {d.line > 0 ? `:${d.line}:${d.column}` : ""}
                  </span>
                </div>
                <p className="problem-msg">{d.message}</p>
              </li>
            ))}
          </ul>
        )}

        {currentResult && (
          <details className="raw">
            <summary>Raw output (revision {currentResult.revision})</summary>
            <pre>{currentResult.rawOutput || "(no output)"}</pre>
          </details>
        )}
      </section>

      <section className="card">
        <div className="card-head">
          <h2>Activity</h2>
          <span className="muted">Latest {MAX_LOG_ENTRIES} batches</span>
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
                  <span className="badge">#{batch.revision}</span>
                  <span className="muted">{formatTime(batch.atMs)}</span>
                </div>
                <ul className="files">
                  {batch.files.map((file) => (
                    <li key={file}>{file}</li>
                  ))}
                </ul>
              </li>
            ))}
          </ul>
        )}
      </section>

      <details className="card dev">
        <summary>Developer tools</summary>
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
          />
          <button type="submit" className="btn">
            Greet
          </button>
        </form>
        {greetMsg && <p className="muted">{greetMsg}</p>}
      </details>
    </main>
  );
}

export default App;
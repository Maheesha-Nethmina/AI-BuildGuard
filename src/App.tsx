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

const MAX_LOG_ENTRIES = 50;

function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString();
}

function App() {
  const [settings, setSettings] = useState<Settings>({ projectPath: null });
  const [watching, setWatching] = useState(false);
  const [changes, setChanges] = useState<ChangeBatch[]>([]);
  const [totalBatches, setTotalBatches] = useState(0);
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
  }, []);

  useEffect(() => {
    // The "cancelled" flag prevents a duplicate listener when React
    // StrictMode mounts, unmounts and re-mounts this effect in development.
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<ChangeBatch>("project-changed", (event) => {
      setChanges((previous) =>
        [event.payload, ...previous].slice(0, MAX_LOG_ENTRIES),
      );
      setTotalBatches((count) => count + 1);
    }).then((stop) => {
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
  }, []);

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

  async function greet() {
    setGreetMsg(await invoke<string>("greet", { name }));
  }

  const hasProject = settings.projectPath !== null;
  const latest = changes[0];

  let pillClass = "pill pill-idle";
  let pillText = "No project";
  if (hasProject && watching) {
    pillClass = "pill pill-watching";
    pillText = "Watching";
  } else if (hasProject) {
    pillClass = "pill pill-warning";
    pillText = "Not watching";
  }

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
        <span className={pillClass}>
          <span className="pill-dot" />
          {pillText}
        </span>
      </header>

      <section className="card">
        <div className="card-head">
          <h2>Monitored project</h2>
          <button type="button" className="btn" onClick={chooseProject}>
            {hasProject ? "Change project" : "Select project folder"}
          </button>
        </div>
        <p className={hasProject ? "path" : "path path-empty"}>
          {settings.projectPath ?? "No project selected yet."}
        </p>
        {error && <p className="error">{error}</p>}
      </section>

      <section className="stats">
        <div className="stat">
          <span className="stat-label">Revision</span>
          <span className="stat-value">{latest ? latest.revision : "—"}</span>
        </div>
        <div className="stat">
          <span className="stat-label">Change batches</span>
          <span className="stat-value">{totalBatches}</span>
        </div>
        <div className="stat">
          <span className="stat-label">Last change</span>
          <span className="stat-value stat-small">
            {latest ? formatTime(latest.atMs) : "—"}
          </span>
        </div>
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
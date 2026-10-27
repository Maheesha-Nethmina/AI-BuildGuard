# AI BuildGuard

**AI BuildGuard** is a background build error monitor for frontend developers that runs fast diagnostics and surfaces real-time project status through a Windows system-tray dashboard.

It monitors a selected local frontend project, runs configured diagnostics when relevant files change, and updates a system-tray status indicator. By surfacing errors directly on the desktop, AI BuildGuard reduces the friction of repeatedly switching to a terminal to discover common build, TypeScript, and linting problems.

## 🚀 Features

- **Targeted Background Monitoring:** Watches relevant source and configuration files while ignoring dependency and build folders by default.
- **System-Tray Integration:** Updates the tray icon to reflect the current status, including initializing, checking, healthy, and errors found.
- **Desktop Notifications:** Sends notifications when meaningful diagnostic state transitions occur.
- **Fast Diagnostics:** Integrates existing tools such as the TypeScript compiler and ESLint when configured in the selected project.
- **Controlled Execution and Debouncing:** Coalesces rapid file-save events to avoid unnecessary diagnostic runs and executes checks through controlled child processes.
- **Error Dashboard:** Displays diagnostics with severity, message, source, file path, line, column, and the latest check timestamp.
- **Editor Navigation:** Allows users to click a diagnostic and open the corresponding file location directly in VS Code.
- **Independent Build Validation:** Provides a manual action to run a production build and view its result separately from fast diagnostics.
- **Local-First Architecture:** Keeps project monitoring and diagnostic processing on the local machine by default.
- **No Paid AI Dependency:** Provides core monitoring and diagnostic functionality without requiring an LLM API or paid backend.

## 🛠️ Technology Stack

| Component | Technology |
|---|---|
| Desktop framework | Tauri 2 |
| Native backend | Rust |
| User interface | React |
| Frontend build tool | Vite |
| Programming language | TypeScript |
| File monitoring and process management | Rust-based backend |
| Diagnostics | TypeScript compiler and ESLint, when configured |

### Initial Project Support

The initial version targets **React + Vite + TypeScript** projects. Diagnostic availability depends on the tools and scripts configured in the selected project.

## 💻 Recommended IDE Setup

For development, the following tools are recommended:

- [Visual Studio Code](https://code.visualstudio.com/)
- [Tauri VS Code Extension](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode)
- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## 📋 Prerequisites

Before installing AI BuildGuard, ensure the following tools are available on your development machine:

- [Node.js](https://nodejs.org/) — Version 18 or higher recommended.
- npm — Included with Node.js installations.
- [Rust and Cargo](https://www.rust-lang.org/tools/install) — Required for the Tauri native backend.
- [Visual Studio Code](https://code.visualstudio.com/) — Required for the editor navigation feature.

A compatible Windows development environment is required to develop and build the Windows desktop application.

## 📦 Getting Started

### 1. Clone the Repository

Replace `your-username` with the actual GitHub username or organization that owns the repository.

```bash
git clone https://github.com/your-username/ai-buildguard.git
cd ai-buildguard
```

### 2. Install Dependencies

Install the frontend dependencies:

```bash
npm install
```

Ensure Rust, Cargo, and the required Tauri system dependencies are installed before starting development.

### 3. Run in Development Mode

Start AI BuildGuard in development mode:

```bash
npm run tauri dev
```

This command starts the Vite development server and launches the Tauri desktop application.

### 4. Build for Production

Build the application for production:

```bash
npm run tauri build
```

This compiles the application and generates the distributable packages supported by the configured Tauri build targets.

The generated Windows installers and bundles are typically located under:

```text
src-tauri/target/release/bundle/
```

The exact output files depend on the bundle targets configured in `src-tauri/tauri.conf.json`.

## 🔍 How It Works

AI BuildGuard is designed to provide fast feedback while frontend developers work on their projects.

1. **Select a Project:** Choose the local frontend project to monitor.
2. **Watch File Changes:** Monitor relevant source and configuration files while excluding common dependency and build directories by default.
3. **Run Diagnostics:** Trigger configured checks when relevant files change.
4. **Process Results:** Collect diagnostic messages and associated file locations from supported tools.
5. **Update Project Status:** Reflect the latest diagnostic state in the system tray and dashboard.
6. **Navigate to Errors:** Open a reported issue at its corresponding location in VS Code.
7. **Validate Production Builds:** Run a production build manually when a more comprehensive validation is needed.

## 🔒 Security and Privacy

AI BuildGuard follows a local-first approach to project monitoring and diagnostics.

- **Local Execution:** Project paths, source code, and diagnostic data remain local by default.
- **No Telemetry Upload:** This release does not upload telemetry to an external service.
- **Controlled Processes:** Supported diagnostic commands are invoked with controlled arguments. Build scripts are executed through an explicit user action.
- **No Paid AI Dependency:** Core MVP functionality works without an LLM API or paid backend.
- **Future AI Features:** Any future AI-powered functionality will be strictly opt-in.

**Security note:** Only monitor projects and execute build scripts from trusted sources. Local execution reduces external data exposure but does not eliminate risks associated with untrusted project scripts or dependencies.

## 🎯 Project Goals

AI BuildGuard aims to make frontend development more efficient by:

- Reducing the need to switch between the editor and terminal for routine diagnostics.
- Detecting common development issues soon after relevant file changes.
- Providing a clear, persistent overview of project health.
- Making diagnostic messages easier to locate and resolve.
- Keeping the core development feedback workflow lightweight and local.


**AI BuildGuard — Catch development errors early, without leaving your workflow.**

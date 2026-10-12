# mlue Architecture — Brick 0: Application Foundation

## 1. Overview

**mlue** is a local-first desktop application that exposes supported AI models and services through personal API keys. Its architecture isolates local network access, desktop presentation, and asynchronous model communication within a lightweight native container.

Tagline: *Your AI. Your API.*

## 2. Technology Stack

- **Desktop Framework**: Tauri 2
- **Backend & Core Runtime**: Rust (edition 2024), Tokio async runtime
- **Embedded HTTP Server**: Axum 0.8 + Tower-HTTP
- **IPC & Serialization**: Tauri IPC, Serde, Serde JSON
- **Frontend Presentation**: React 19, TypeScript, Vite 8, Tailwind CSS v4

## 3. Embedded Runtime Lifecycle

```
[ Tauri Desktop Launch ]
          │
          ├──> Initialize AppState (state = Starting, endpoint = http://127.0.0.1:4782)
          ├──> Register IPC command: `get_runtime_status`
          ├──> Spawn Tokio async runtime task: `run_server`
          │           │
          │           ├──> Validate host is loopback (127.0.0.1)
          │           ├──> Attempt TcpListener::bind("127.0.0.1:4782")
          │           │           │
          │           │           ├──[ Success ]──> AppState (state = Running)
          │           │           │                 Serve Axum routes (/health)
          │           │           │                 Await graceful shutdown signal
          │           │           │
          │           │           └──[ Failure ]──> AppState (state = Error, error = details)
          │           │                             Log failure, keep UI alive
          │           │
          ├──> Open Native WebView Window (App.tsx)
          │           │
          │           └──> Invoke `get_runtime_status` via Tauri IPC
          │                Display actual state & endpoints to user
          │
[ Application Exit Requested ]
          │
          └──> Send shutdown signal via Tokio watch channel
               Axum drains in-flight requests & terminates cleanly
               AppState transitions to Stopped
```

### 3.1 Server Startup & Error Isolation
1. **Binding Address**: The embedded HTTP server binds strictly to `127.0.0.1:4782`. Any attempt to bind to `0.0.0.0` or external network adapters is explicitly rejected.
2. **Port Conflict Resilience**: If port `4782` is already in use by another process or cannot be bound, the error is captured and stored in `AppState` with state `Error`. The server task terminates gracefully without calling `panic!` or crashing the Tauri desktop application. The frontend UI remains interactive and displays the precise OS bind error.
3. **No Dynamic Port Switching**: The server does not silently switch to an alternative port (e.g., 4783). mlue guarantees a predictable local API endpoint.

### 3.2 Inter-Process Communication (IPC)
The React frontend communicates with the Rust backend via Tauri 2 IPC:
- **Command**: `get_runtime_status`
- **Response Schema**:
  ```json
  {
    "state": "starting" | "running" | "error" | "stopped",
    "endpoint": "http://127.0.0.1:4782",
    "error": "Optional error message string or null"
  }
  ```
The frontend never assumes backend health based solely on whether Vite or the WebView finished loading.

### 3.3 HTTP API Surface (Brick 0)
- **`GET /health`**:
  Returns the service health status and crate version:
  ```json
  {
    "status": "ok",
    "service": "mlue",
    "version": "0.1.0"
  }
  ```
  The version is dynamically derived from `env!("CARGO_PKG_VERSION")`.

### 3.4 Controlled Shutdown
When the user closes the application or an OS termination signal is caught (`tauri::RunEvent::ExitRequested`), Tauri signals a `tokio::sync::watch::Sender<bool>`. The Axum server listener terminates cleanly via `.with_graceful_shutdown()`, releasing the socket and ensuring no zombie background processes persist.

### 3.5 Security Boundaries
- **Loopback Exclusivity**: Bound to `127.0.0.1`.
- **Restricted CORS**: Wildcard `*` CORS is rejected. Only standard local dev and Tauri origins (`http://localhost:1420`, `tauri://localhost`, `http://tauri.localhost`, `http://127.0.0.1:1420`) are permitted.
- **No Remote Administration or Shell Execution**: No endpoints exist in Brick 0 that execute system commands or traverse filesystem paths.

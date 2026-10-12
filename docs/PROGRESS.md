# mlue Development Progress

## Current Milestone: Brick 0 — Application Foundation

### 1. Implemented in Brick 0
- **Tauri 2 Desktop Application Scaffolding**: Configured native desktop shell with identifier `app.mlue.desktop` and product name `mlue`.
- **Frontend Stack**: React 19 + TypeScript + Vite 8 + Tailwind CSS v4 styling.
- **Embedded Rust HTTP Server**: Embedded Axum 0.8 server bound to loopback `127.0.0.1:4782`.
- **Health Check Endpoint**: `GET /health` returning `{"status": "ok", "service": "mlue", "version": "0.1.0"}` derived from `CARGO_PKG_VERSION`.
- **Tauri IPC Command**: `get_runtime_status` returning structured `{ state, endpoint, error }`.
- **Runtime State Machine**: Explicit states (`starting`, `running`, `error`, `stopped`) in thread-safe `AppState`.
- **Error Handling & Port Resilience**: Port collision and network bind errors captured gracefully; desktop UI remains active and shows clear diagnostic error message.
- **Controlled Graceful Shutdown**: Integrated with Tauri exit events using Tokio watch channels.
- **Security Protections**: Loopback-only binding check and strict CORS layer (no wildcard `*`).
- **Core Documentation**: `AGENTS.md`, `README.md`, `docs/ARCHITECTURE.md`, `docs/PROGRESS.md`.

### 2. Validation & Testing
- Rust toolchain: Installed and verified stable compiler (`rustc 1.99.0`, `cargo 1.99.0`).
- Frontend compilation: `npm run build` verified (`tsc` + Vite).
- Backend unit and integration tests:
  - Health handler HTTP 200 and schema response verification.
  - Non-loopback host binding rejection test.
  - Graceful port conflict handling test.
  - Runtime state transitions test.

### 3. Known Blockers
- None.

### 4. Outstanding Work (Next Milestones)
- **Brick 1**: Provider integration abstractions (model-provider agnostic adapter layer).
- **Brick 2**: Local inference detection and connection management.
- **Brick 3**: API key generation and management (SQLite persistence).
- **Brick 4**: OpenAI-compatible API reverse proxy surface (`/v1/chat/completions`).

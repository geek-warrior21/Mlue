# mlue Development Progress

## Current Milestone: Brick 1 — Ollama Auto-Discovery & Model Registry

---

### Milestone: Brick 1 — Ollama Auto-Discovery & Model Registry (Current)

#### 1. Implemented in Brick 1
- **Ollama Provider Adapter**: Created `OllamaProvider` connecting to `http://127.0.0.1:11434/api/tags` with 1.5s connect timeout and 3.0s request timeout.
- **Provider-Independent Data Models**: Defined normalized `ModelInfo` and `ModelCapabilities` preserving upstream model IDs and namespace tags without provider lock-in.
- **Discovery Abstraction**: Implemented `ModelDiscovery` async trait for extensible local and remote provider drivers.
- **In-Memory Model Registry**: Built thread-safe `ModelRegistry` with explicit states (`checking`, `ready`, `no_models`, `unreachable`, `error`), atomic snapshot replacement, and serialized refresh guards (`Mutex<()>`) without holding locks across network I/O.
- **Tauri IPC Commands**: Added `get_discovered_models` (cached snapshot) and `refresh_ollama` (active refresh) commands.
- **Non-blocking Startup Discovery**: Initial discovery launched asynchronously on app launch; desktop window renders immediately.
- **Frontend Local AI Section**: Extended `App.tsx` with dedicated Models UI supporting all 5 provider states, model metadata tags (parameters, quantization, file size), and a Refresh Models action.
- **Mock-Server Test Suite**: Added 9 automated backend test cases against ephemeral Axum mock servers validating all network error conditions, JSON parsing, namespace preservation, duplicate prevention, and offline invalidation.

#### 2. Validation & Testing
- `cargo fmt --check`: Clean (0 diffs).
- `cargo check`: Built cleanly in dev mode.
- `cargo test`: 15 passed, 0 failed across `server_tests.rs` (6) and `ollama_discovery_tests.rs` (9).
- `cargo clippy --all-targets -- -D warnings`: 0 warnings across all targets.
- `npm run build`: Production bundle built cleanly (TypeScript `tsc` + Vite).
- Desktop smoke launch: Application launched headlessly/natively and responded with HTTP 200 on `/health`.

#### 3. Known Blockers
- None.

#### 4. Outstanding Work (Next Milestones)
- **Brick 2**: Inference Driver Abstraction & Local Model Execution.
- **Brick 3**: API Key Generation & SQLite Credential Persistence.
- **Brick 4**: OpenAI-Compatible Reverse Proxy API (`/v1/chat/completions` & `/v1/models`).

---

### Milestone: Brick 0 — Application Foundation (Completed)

#### 1. Implemented in Brick 0
- **Tauri 2 Desktop Application Scaffolding**: Configured native desktop shell with identifier `app.mlue.desktop` and product name `mlue`.
- **Frontend Stack**: React 19 + TypeScript + Vite 8 + Tailwind CSS v4 styling.
- **Embedded Rust HTTP Server**: Embedded Axum 0.8 server bound to loopback `127.0.0.1:4782`.
- **Health Check Endpoint**: `GET /health` returning `{"status": "ok", "service": "mlue", "version": "0.1.0"}` derived from `CARGO_PKG_VERSION`.
- **Tauri IPC Command**: `get_runtime_status` returning structured `{ state, endpoint, error }`.
- **Runtime State Machine**: Explicit states (`starting`, `running`, `error`, `stopped`) in thread-safe `AppState`.
- **Error Handling & Port Resilience**: Port collision and network bind errors captured gracefully; desktop UI remains active.
- **Controlled Graceful Shutdown**: Integrated with Tauri exit events using Tokio watch channels.
- **Security Protections**: Loopback-only binding check and strict CORS layer (no wildcard `*`).
- **Core Documentation**: `AGENTS.md`, `README.md`, `docs/ARCHITECTURE.md`, `docs/PROGRESS.md`.

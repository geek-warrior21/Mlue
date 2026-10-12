# mlue

> **Your AI. Your API.**

mlue is a local-first desktop application that allows users to expose supported AI models and AI services through personal API keys.

---

## Current Milestones

### Brick 0 — Application Foundation
- **Native Tauri 2 Desktop Shell**: Cross-platform desktop application powered by Rust and modern web standards.
- **Embedded Axum HTTP Server**: Asynchronous HTTP server running inside the desktop process, listening strictly on `127.0.0.1:4782`.
- **Health Check Endpoint**: `GET /health` returning service metadata, status, and dynamic package versioning.
- **Tauri IPC Runtime Status**: Real-time status inspection (`starting`, `running`, `error`, `stopped`) queried via Tauri IPC command `get_runtime_status`.
- **Port Conflict Resilience**: Graceful error handling for port collisions without crashing the desktop user interface.
- **Controlled Lifecycle Shutdown**: Orderly asynchronous cleanup when the application exits.

### Brick 1 — Ollama Auto-Discovery & Model Registry
- **Ollama Service Discovery**: Automatic non-blocking detection of locally running Ollama instances on `http://127.0.0.1:11434`.
- **Provider-Agnostic Model Metadata**: Normalized `ModelInfo` and `ModelCapabilities` models preserving upstream model names, namespace tags, parameter counts, and quantization levels.
- **In-Memory Model Registry**: Concurrency-safe registry with atomic snapshot updates and offline state invalidation.
- **Tauri IPC Discovery Commands**:
  - `get_discovered_models`: Instant retrieval of in-memory model snapshot without network blocking.
  - `refresh_ollama`: User-triggered asynchronous discovery refresh with connection serialization.
- **Desktop UI Local Models Section**: Interactive visual status indicating `checking`, `ready` (with model cards), `no_models`, `unreachable`, or `error` with manual Refresh action.

---

## Prerequisites

- **Node.js**: v18+ (tested on v26+)
- **npm**: v9+ (tested on v11+)
- **Rust**: 1.80+ (tested on 1.99 stable)
- **C Compiler / Platform SDK**: Xcode Command Line Tools on macOS (`xcode-select --install`)

---

## Development Commands

### 1. Install Frontend Dependencies
```bash
npm install
```

### 2. Frontend Production Build
```bash
npm run build
```

### 3. Run Backend Checks and Tests
```bash
cd src-tauri
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

### 4. Launch Desktop Application (Development Mode)
```bash
npm run tauri dev
```

---

## Testing the Health Endpoint

When the application is running, test the embedded HTTP server:

```bash
curl -i http://127.0.0.1:4782/health
```

Expected response (`HTTP/1.1 200 OK`):

```json
{
  "status": "ok",
  "service": "mlue",
  "version": "0.1.0"
}
```

You can also test the endpoint directly from the desktop UI using the **Test** action next to the Health endpoint.

---

## What is NOT Implemented Yet (Planned for Future Bricks)

The following capabilities are deliberately out of scope for Brick 0 and Brick 1:

- `/v1/chat/completions` or `/v1/models` public reverse proxy
- Model execution or prompt generation
- API key generation and management
- Cloud provider connections (OpenAI, Gemini, Anthropic)
- SQLite database integration
- Public networking / remote tunnels (Tailscale)
- Model downloading/pulling workflows
- System tray integration

---

## Architecture & Guidelines

For details on the embedded runtime lifecycle and discovery registry, refer to [ARCHITECTURE.md](docs/ARCHITECTURE.md). For progress history, see [PROGRESS.md](docs/PROGRESS.md). For engineering invariants, see [AGENTS.md](AGENTS.md).

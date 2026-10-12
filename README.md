# mlue

> **Your AI. Your API.**

mlue is a local-first desktop application that allows users to expose supported AI models and AI services through personal API keys.

---

## Current Milestone: Brick 0 — Application Foundation

Brick 0 establishes the native application bootstrap, embedded server runtime, and inter-process communication foundation:

- **Native Tauri 2 Desktop Shell**: Cross-platform desktop application powered by Rust and modern web standards.
- **Embedded Axum HTTP Server**: Asynchronous HTTP server running inside the desktop process, listening strictly on `127.0.0.1:4782`.
- **Health Check Endpoint**: `GET /health` returning service metadata, status, and dynamic versioning.
- **Tauri IPC Runtime Status**: Real-time status inspection (`starting`, `running`, `error`, `stopped`) queried via Tauri IPC commands.
- **Port Conflict & Error Resilience**: Graceful error handling for port collisions without crashing the desktop user interface.
- **Controlled Lifecycle Shutdown**: Orderly asynchronous cleanup when the application exits.

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

The following capabilities are deliberately out of scope for Brick 0:

- AI model detection or local inference (Ollama, llama.cpp, etc.)
- Cloud provider connections (OpenAI, Gemini, Anthropic)
- API key generation and management
- OpenAI-compatible inference endpoints (`/v1/chat/completions`)
- Model routing or prompt proxying
- Database persistence (SQLite)
- Public networking / remote tunnels (Tailscale)
- Authentication workflows
- System tray integration

---

## Architecture

For details on the embedded runtime lifecycle and security model, refer to [ARCHITECTURE.md](docs/ARCHITECTURE.md). For engineering invariants and contribution guidelines, see [AGENTS.md](AGENTS.md).

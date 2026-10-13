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

### Brick 2A — Provider-Independent Inference & Ollama Test Playground
- **Provider-Independent Inference Abstraction**: Canonical `InferenceRequest`, `ChatMessage`, `MessageRole`, `UsageInfo`, and `InferenceResponse` types.
- **`ModelInference` Trait**: Extensible async execution interface implemented for the Ollama adapter.
- **Ollama Chat Execution**: Native `POST /api/chat` integration supporting text-only single-turn and multi-turn conversations, system prompts, and token usage metrics.
- **Capability Validation & Caching**: Pre-flight inspection via `POST /api/show` to verify chat capability and reject embedding-only models before execution, with snapshot-synchronized caching.
- **Desktop Tauri IPC Command**: `generate_test_completion` returning normalized results and duration without holding registry locks.
- **Desktop AI Playground**: Interactive UI in the desktop app with live model selection, prompt input, generation loading indicator, duration metrics, and structured error display.
- **Security Boundaries**: Strictly local execution; no unauthenticated public inference HTTP endpoints are exposed.

### Brick 2B — Real-Time Inference Streaming & Cancellation
- **Canonical Streaming Event Pipeline**: Provider-independent `InferenceEvent` (`Started`, `Delta`, `Completed`, `Cancelled`, `Error`) with guaranteed single terminal event semantics via atomic CAS `TerminalGuard`.
- **Tauri 2 IPC Channels**: Fast, direct token streaming via typed `Channel<InferenceEvent>` directly targeting the requesting window.
- **Incremental NDJSON Parser**: Stream parser handling byte fragments, split NDJSON objects, split multi-byte UTF-8 code points, blank lines, bounded buffering (64 KiB line limit), and premature EOF detection. User-facing content is extracted strictly from `message.content` without mixing thinking fields.
- **Real-Time Request Cancellation**: `cancel_test_completion` terminates in-flight inference, drops upstream Reqwest HTTP streams to abort the underlying network connection, releases concurrency permits, and marks status as `Cancelled`.
- **Concurrency & Semaphore Management**: Backend-wide semaphore (max 2 active inference jobs) with automatic permit reclamation.
- **Decoupled Timeout Policy**: Decoupled connection (5s), initial response (30s), idle token (15s), and overall generation (300s) timeouts.
- **React Playground Streaming Experience**: Real-time token rendering with throttled UI frame updates, live Stop Generation button, latency metrics, and component unmount cleanup.

---

## Prerequisites

- **Node.js**: v18+ (tested on v26+)
- **npm**: v9+ (tested on v11+)
- **Rust**: 1.80+ (tested on 1.99 stable)
- **C Compiler / Platform SDK**: Xcode Command Line Tools on macOS (`xcode-select --install`)
- **Ollama** (optional for live model testing): Locally installed and running on `http://127.0.0.1:11434`

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

---

## What is NOT Implemented Yet (Planned for Future Bricks)

The following capabilities are deliberately out of scope for Brick 2B:

- Public `/v1/chat/completions` or `/v1/models` HTTP endpoints (Brick 4)
- API key generation and management (Brick 3)
- SQLite database integration (Brick 3)
- Cloud provider connections (OpenAI, Gemini, Anthropic)
- Public networking / remote tunnels (Tailscale)
- Automatic model downloading or pulling workflows
- Agentic shell execution

---

## Architecture & Guidelines

For details on the embedded runtime lifecycle, discovery registry, and inference architecture, refer to [ARCHITECTURE.md](docs/ARCHITECTURE.md). For progress history, see [PROGRESS.md](docs/PROGRESS.md). For engineering invariants, see [AGENTS.md](AGENTS.md).

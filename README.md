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

### Brick 3A — Project Management, SQLite Persistence & Secure API Keys
- **Projects Ownership Model**: Hierarchical project entities (`mlue installation -> Project -> API Keys`) cleanly separated from future user/session layers.
- **Embedded SQLite Storage**: Database powered by `sqlx` (v0.8) with WAL mode, foreign keys, and automatic OS application data directory placement (`app.path().app_data_dir()`).
- **Versioned Migrations**: Automated schema migration runner executing transaction-safe migrations tracked in `_schema_migrations`.
- **CSPRNG API Key Generation**: `mlue_sk_<secret>` generated with 256 bits of OS entropy (`rand::rngs::OsRng`) and URL-safe base64 encoding.
- **Secret Hygiene & Constant-Time Verification**: Plaintext keys are never stored in SQLite and are presented only once upon generation. Stored digests use SHA-256 and are authenticated using `subtle::ConstantTimeEq`. Routine listings return only metadata with non-sensitive prefixes.
- **Model Access Policy Framework**: Granular permission models (`local_only` default, `selected_models` whitelist) blocking access to non-local providers.
- **Desktop Administrative Control Plane**: Dedicated Tauri IPC commands for Project CRUD, Key Generation, Key Listing, and Revocation; zero administration endpoints are exposed over public HTTP.
- **Desktop UI**: Comprehensive Project & Key Management dashboard with creation, editing, deletion, safe key generation modal, and key revocation controls.

### Brick 3B-1 — Authenticated HTTP Inference & First External API Call
- **Loopback-Bound OpenAI-Compatible API**: Embedded Axum HTTP endpoints at `http://127.0.0.1:4782/v1`.
- **Bearer Token Authentication**: Enforces `Authorization: Bearer mlue_sk_<secret>` with SHA-256 verification and cascade deletion checking.
- **Fail-Closed Model Access Policy**: Validates requests against trusted in-memory model registry metadata and enforces `local_only` or `selected_models` policies.
- **Deterministic Auto-Routing**: `model: "auto"` routes deterministically to eligible, chat-capable local models.
- **Endpoints**:
  - `GET /v1/models`: Returns list of models permitted under the authenticating key.
  - `POST /v1/chat/completions`: Non-streaming chat completion executing against local providers (e.g. Ollama) and returning canonical OpenAI envelopes.
- **Resource Limits & Concurrency**: Enforces 256 KiB body limit, 50 message cap, 64 KiB single message cap, 128 KiB total content cap, and global semaphore concurrency control.
- **Desktop UI Experience**: One-time key generation modal presents endpoint URL, copyable secret, and instant Python SDK copy-paste example.

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

## Testing Authenticated HTTP Inference

### 1. Test Server Health Check
```bash
curl -i http://127.0.0.1:4782/health
```

### 2. Generate an API Key
Launch mlue, open the **Projects & API Keys** tab, select or create a project, and click **Generate Key**. Copy the plaintext secret (`mlue_sk_...`).

### 3. List Permitted Models
```bash
curl -X GET http://127.0.0.1:4782/v1/models \
  -H "Authorization: Bearer mlue_sk_YOUR_SECRET"
```

### 4. Run a Chat Completion via cURL
```bash
curl -X POST http://127.0.0.1:4782/v1/chat/completions \
  -H "Authorization: Bearer mlue_sk_YOUR_SECRET" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [
      { "role": "user", "content": "Hello mlue! Are you working?" }
    ]
  }'
```

### 5. Run via OpenAI Python SDK
```python
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:4782/v1",
    api_key="mlue_sk_YOUR_SECRET",
)

response = client.chat.completions.create(
    model="auto",
    messages=[{"role": "user", "content": "What is 2 + 2?"}],
    stream=False,
)

print(response.choices[0].message.content)
```

---

## What is NOT Implemented Yet (Planned for Future Bricks)

The following capabilities are deliberately out of scope for Brick 3B-1:

- Server-Sent Events (SSE) HTTP Streaming for `/v1/chat/completions` (`stream: true`)
- Cloud provider connections (OpenAI, Gemini, Anthropic)
- Public networking / remote tunnels (Tailscale / wireguard)
- Automatic model downloading or pulling workflows
- Agentic shell execution

---

## Architecture & API Reference

For detailed documentation, refer to:
- [API Reference](docs/API.md): Endpoints, error codes, and integration patterns.
- [Architecture](docs/ARCHITECTURE.md): Embedded runtime lifecycle, IPC, and security boundaries.
- [Progress Log](docs/PROGRESS.md): Chronological brick milestone history.
- [Engineering Guidelines](AGENTS.md): Architectural invariants and standards.

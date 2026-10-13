# mlue Development Progress

## Current Milestone: Brick 2B — Real-Time Inference Streaming & Cancellation

---

### Milestone: Brick 2B — Real-Time Inference Streaming & Cancellation (Completed)

#### 1. Implemented in Brick 2B
- **Canonical Streaming Event Contract**: Defined `InferenceEvent` enum (`Started`, `Delta`, `Completed`, `Cancelled`, `Error`) with tagged JSON serialization in `src-tauri/src/inference/types.rs`.
- **Streaming Model Inference Trait**: Extended `ModelInference` trait in `src-tauri/src/inference/mod.rs` with `stream(&self, request)` returning `InferenceEventStream` (`Pin<Box<dyn Stream<Item = Result<InferenceEvent, InferenceError>> + Send>>`) alongside non-streaming `generate()`.
- **Ollama Native Streaming Driver**: Implemented `stream_with_upstream` in `src-tauri/src/providers/ollama.rs` calling `POST /api/chat` with `"stream": true`.
- **Incremental NDJSON Parser**: Robust byte-stream parser that handles multiple NDJSON objects per chunk, split NDJSON across chunks, UTF-8 code points split across chunk boundaries (via `\n` boundary slicing), blank line skipping, bounded buffering (64 KiB line limit), premature EOF detection, and extraction of `prompt_eval_count`, `eval_count`, and `done_reason`. User text is extracted strictly from `message.content` without mixing in reasoning fields.
- **Tauri 2 IPC Channels**: Implemented `start_test_stream` command using `tauri::ipc::Channel<InferenceEvent>`. Channels deliver events directly to the requesting window without global event bus clutter. Requests are registered before acknowledging start to prevent cancellation races.
- **Active Inference & Cancellation Manager**: Built `InferenceManager` in `src-tauri/src/inference/manager.rs` managing active requests with `tokio_util::sync::CancellationToken` and `TerminalGuard` (atomic CAS guaranteeing exactly one terminal event: `Completed`, `Cancelled`, or `Error`). Dropping the upstream Reqwest response stream on cancellation immediately aborts the underlying HTTP socket connection.
- **Concurrency Control & Semaphore**: Integrated concurrency management using `tokio::sync::Semaphore` (max 2 concurrent Ollama tasks) with RAII permit reclamation across normal completion, cancellation, errors, and timeouts.
- **Decoupled Streaming Timeout Policy**: Replaced short timeouts with decoupled streaming policies: 5s connection, 30s initial response, 15s idle stream, and 300s overall generation timeout.
- **Tightened Input Boundaries**: Validated max 50 chat messages, 64 KiB single message, 128 KiB total bytes before provider network calls.
- **React Desktop Playground**: Enhanced `src/App.tsx` with streaming mode, real-time incremental rendering with 20ms throttled UI batching, Stop Generation button, instant cancellation status transition, clear badges (`Streaming...`, `Generation stopped`, `Completed`, `Inference error`), and component unmount cancellation cleanup.
- **Automated Mock-Server Test Suite**: Implemented 20 dedicated integration tests in `tests/streaming_tests.rs` covering all 20 required scenarios from Section 14 (multi-delta, single-chunk NDJSON, split NDJSON across chunks, split UTF-8 character, token accumulation, completion finish reason, usage extraction, malformed JSON, HTTP error before stream, network disconnect mid-stream, premature EOF without `done: true`, cancellation before first token, cancellation after multiple tokens, upstream HTTP connection drop verification, single terminal event guarantee via `TerminalGuard`, permit release on cancellation, simultaneous request isolation, concurrent non-blocking registry reads, request limit violations, and non-streaming regression tests).

#### 2. Validation & Testing
- `cargo fmt --check`: Clean (0 diffs).
- `cargo check`: Passed.
- `cargo test`: **55 passed, 0 failed** across `streaming_tests.rs` (22), `inference_tests.rs` (17), `ollama_discovery_tests.rs` (10), and `server_tests.rs` (6).
- `cargo clippy --all-targets -- -D warnings`: 0 warnings across all targets.
- `npm run build`: Production bundle built cleanly (TypeScript `tsc` + Vite).
- `npm run tauri build -- --debug`: Successfully packaged macOS native application (`mlue.app`, 43.32 MiB) and installer bundle (`mlue_0.1.0_aarch64.dmg`, 10.20 MiB).

#### 3. Known Blockers
- None.

#### 4. Outstanding Work (Next Milestones)
- **Brick 3**: API Key Generation, SQLite Credential Persistence & Public API Authentication.
- **Brick 4**: OpenAI-Compatible Reverse Proxy API (`/v1/chat/completions` & `/v1/models`).

---

### Milestone: Brick 2A — Provider-Independent Inference & Ollama Test Playground (Completed)

#### 1. Implemented in Brick 2A
- **Provider-Independent Inference Contracts**: Defined canonical `InferenceRequest`, `ChatMessage`, `MessageRole`, `UsageInfo`, and `InferenceResponse` in `src-tauri/src/inference/types.rs`.
- **Model Inference Trait**: Implemented `ModelInference` async trait in `src-tauri/src/inference/mod.rs` with blanket `Arc<T>` implementation.
- **Ollama Inference Driver**: Extended `OllamaProvider` with `POST /api/chat` integration, support for system/user/assistant message roles, usage token extraction (`prompt_eval_count`, `eval_count`), exact upstream model identifier targeting, and a dedicated 60-second inference timeout decoupled from the 3-second discovery timeout.
- **Capability Inspection & Caching**: Implemented capability validation via Ollama's `POST /api/show`. Embedding-only models (e.g. BERT architectures, `nomic-embed-text`) are detected and rejected with structured `UnsupportedCapability` errors before any chat request is issued. Results are cached in thread-safe storage and automatically invalidated on registry refresh.
- **Concurrency & Lock Safety**: Registry read locks are dropped immediately after model resolution; no shared locks are held across model generation network I/O, allowing concurrent inference requests without blocking registry reads.
- **Security & Resource Bounds**: Implemented strict validation limits (max 100 messages, 500k characters per message, 1M total characters), loopback exclusivity, zero public HTTP inference exposure, and zero prompt logging.
- **Desktop Tauri IPC Command**: Implemented `generate_test_completion` accepting canonical request structures and returning normalized responses with elapsed duration and usage tokens.
- **Structured Error Handling**: Implemented typed `InferenceError` and `InferenceErrorPayload` with user-friendly messages for all failure modes (unreachable, missing model, unsupported capability, timeout, HTTP 4xx/5xx, malformed JSON, empty response, load failures).
- **Desktop AI Playground**: Built a complete interactive test playground in `src/App.tsx` featuring real-time model selection from the live registry, prompt textarea with Cmd+Enter submission, loading spinner, duplicate submission prevention, duration display, token metrics, copy-to-clipboard, and responsive error banners.
- **Comprehensive Automated Tests**: Created `tests/inference_tests.rs` with 14 test cases against mock Axum servers covering single-turn chat, multi-turn chat, system messages, exact model names, response normalization, usage metrics, missing models, embedding capability rejection, HTTP 500, malformed JSON, timeouts, cache invalidation, and concurrent read non-blocking.
- **Baseline Fix**: Resolved Brick 1 startup discovery auto-transition issue by emitting `models-updated` Tauri events and polling as a fallback.

#### 2. Validation & Testing
- `cargo fmt --check`: Clean (0 diffs).
- `cargo check`: Passed.
- `cargo test`: 33 passed, 0 failed across `inference_tests.rs` (17), `ollama_discovery_tests.rs` (10), and `server_tests.rs` (6).
- `cargo clippy --all-targets -- -D warnings`: 0 warnings across all targets.
- `npm run build`: Production bundle built cleanly (TypeScript `tsc` + Vite).
- `tauri build --debug`: Successfully packaged macOS native application (`mlue.app`) and installer bundle (`mlue_0.1.0_aarch64.dmg`).
- Desktop smoke launch: Native binary booted cleanly and responded with HTTP 200 on `/health`.

#### 3. Known Blockers
- None.

#### 4. Outstanding Work (Next Milestones)
- **Brick 2B**: Streaming Inference (SSE) & Token-by-Token Playground Output.
- **Brick 3**: API Key Generation, SQLite Credential Persistence & Public API Authentication.
- **Brick 4**: OpenAI-Compatible Reverse Proxy API (`/v1/chat/completions` & `/v1/models`).

---

### Milestone: Brick 1 — Ollama Auto-Discovery & Model Registry (Completed)

#### 1. Implemented in Brick 1
- **Ollama Provider Adapter**: Created `OllamaProvider` connecting to `http://127.0.0.1:11434/api/tags` with 1.5s connect timeout and 3.0s request timeout.
- **Provider-Independent Data Models**: Defined normalized `ModelInfo` and `ModelCapabilities` preserving upstream model IDs and namespace tags without provider lock-in.
- **Discovery Abstraction**: Implemented `ModelDiscovery` async trait for extensible local and remote provider drivers.
- **In-Memory Model Registry**: Built thread-safe `ModelRegistry` with explicit states (`checking`, `ready`, `no_models`, `unreachable`, `error`), atomic snapshot replacement, and serialized refresh guards (`Mutex<()>`) without holding locks across network I/O.
- **Tauri IPC Commands**: Added `get_discovered_models` (cached snapshot) and `refresh_ollama` (active refresh) commands.
- **Non-blocking Startup Discovery**: Initial discovery launched asynchronously on app launch; desktop window renders immediately.
- **Frontend Local AI Section**: Extended `App.tsx` with dedicated Models UI supporting all 5 provider states, model metadata tags (parameters, quantization, file size), and a Refresh Models action.
- **Mock-Server Test Suite**: Added 10 automated backend test cases against ephemeral Axum mock servers validating all network error conditions, JSON parsing, namespace preservation, duplicate prevention, and offline invalidation.

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

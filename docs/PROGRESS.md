# mlue Development Progress

## Current Milestone: Brick 3A — Project Management, SQLite Persistence & Secure API Keys

---

### Milestone: Brick 3A — Project Management, SQLite Persistence & Secure API Keys (Completed)

#### 1. Implemented in Brick 3A
- **Prerequisite Audit & Closure of Brick 2B**: Completed exhaustive audit of 7 critical requirements (cancellable queuing, single terminal event, request cleanup, permit release, closed channel teardown, cross-window isolation, zero unauthenticated endpoints) and committed locally as `2c30036`: `feat: add streaming inference and cancellation`.
- **Embedded SQLite Storage Layer**: Built persistent SQLite storage using `sqlx` (v0.8) with WAL journal mode, 5-second busy timeout, and automatic database directory resolution in the OS application data path (`app.path().app_data_dir()`, e.g. `~/Library/Application Support/app.mlue.desktop/mlue.db`). Persistent files are never placed relative to the process working directory.
- **Versioned Schema Migration Engine**: Created migration runner tracking applied migrations in `_schema_migrations` with `PRAGMA foreign_keys = ON` and transaction safety. Migration 1 initializes `projects`, `api_keys`, `settings` tables and performance indexes (`idx_api_keys_project_id`, `idx_api_keys_key_hash`).
- **Project Management Service (`ProjectService`)**: Transactional CRUD operations for Projects (UUID v4 identifiers, name validation 1-100 characters, optional descriptions up to 500 characters, timestamps, and confirmed cascading deletion of projects and owned keys). Fully decoupled from Axum and Tauri UI layers.
- **Cryptographic API Key Service (`ApiKeyService`)**:
  - Secure random key generation using operating system CSPRNG (`rand::rngs::OsRng`) producing 256 bits of entropy.
  - Canonical key formatting: `mlue_sk_<url_safe_base64_secret>`.
  - Non-sensitive prefix extraction (`mlue_sk_` + first 8 characters) for safe UI identification.
  - SHA-256 cryptographic hashing for persistence in SQLite; plaintext secret is NEVER stored in database, configuration, or logs.
  - Constant-time verification using `subtle::ConstantTimeEq` against timing side-channel attacks.
  - Key metadata type (`ApiKeyMetadata`) that strictly omits `key_hash` or secret strings from routine listing queries.
  - One-time plaintext secret carrier (`CreatedApiKey`) returned only upon initial generation.
  - Key revocation enforcement (`revoked_at` timestamp) preventing revoked keys from authenticating.
  - Usage tracking (`last_used_at` timestamp) updated on successful verification.
- **Model Access Policy Enforcement**:
  - `AccessPolicy` model supporting `local_only` (default) and `selected_models` (explicit whitelist).
  - Strict enforcement: `local_only` permits local/Ollama models (`ollama/*`, `local/*`) and rejects external provider identifiers (`openai/*`, `anthropic/*`, `gemini/*`, `cloud/*`).
  - Model alias `auto` resolves to permitted models.
- **Tauri IPC Administrative Control Plane**:
  - Exposed desktop-only commands: `create_project`, `list_projects`, `update_project`, `delete_project`, `create_api_key`, `list_api_keys`, `revoke_api_key`.
  - Zero administrative or CRUD endpoints exposed on the embedded Axum HTTP server; desktop UI is the sole authorized control plane.
- **Desktop React UI for Projects & Credentials**:
  - Added dedicated **Projects & API Keys** interface in `src/App.tsx`.
  - Project list, creation modal, rename modal, and confirmed delete modal.
  - Key listing table displaying name, prefix (`mlue_sk_...`), policy badge (`Local Models Only`, `Selected Models`), status badge (`Active`, `Revoked`), creation date, and last used date.
  - Generate Key modal with name input and policy selector.
  - One-Time Secret Copy Modal with explicit warning: *"Copy this key now. For security, mlue won't show it again."* Plaintext secret is cleared upon dismissal.
  - Revoke key confirmation modal.
  - Preserved existing Runtime Status, Ollama Discovery, and Test Playground views.
- **Automated Test Suite**:
  - Implemented 18 comprehensive integration tests in `tests/persistence_and_auth_tests.rs` covering all required scenarios: first-run DB init, migration execution, project CRUD, cascade deletion, multi-key per project, cross-project key isolation, formatting validation, correct key auth, incorrect key rejection, revoked key rejection, revocation persistence across DB re-open, absence of plaintext in DB rows, safe listing responses, access policy checks, non-local model rejection, and error recovery/integrity.
  - 100% of tests run in isolated in-memory or temporary-file SQLite databases.

#### 2. Validation & Testing
- `cargo fmt --check`: Clean (0 diffs).
- `cargo check`: Passed.
- `cargo test`: **73 passed, 0 failed** across:
  - `persistence_and_auth_tests.rs`: 18 passed
  - `streaming_tests.rs`: 22 passed
  - `inference_tests.rs`: 17 passed
  - `ollama_discovery_tests.rs`: 10 passed
  - `server_tests.rs`: 6 passed
- `cargo clippy --all-targets -- -D warnings`: 0 warnings across all targets.
- `npm run build`: Production bundle built cleanly (TypeScript `tsc` + Vite).
- `npm run tauri build -- --debug`: Successfully packaged macOS native application (`mlue.app`, 50.72 MiB) and installer bundle (`mlue_0.1.0_aarch64.dmg`, 12.09 MiB).
- **Live Runtime Verification**: Started desktop binary, verified creation of `mlue.db` (WAL mode, schema version 1 applied) in `~/Library/Application Support/app.mlue.desktop/`, and verified loopback server `/health` returns HTTP 200.

#### 3. Known Blockers
- None.

#### 4. Outstanding Work (Next Milestones)
- **Brick 3B**: Public HTTP Authentication Middleware & Route Protection.
- **Brick 4**: OpenAI-Compatible Reverse Proxy API (`/v1/chat/completions` & `/v1/models`).

---

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

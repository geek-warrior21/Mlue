# mlue Architecture — System Design & Runtime Lifecycle

## 1. Overview

**mlue** is a local-first desktop application that exposes supported AI models and services through personal API keys. Its architecture isolates local network access, desktop presentation, and asynchronous model communication within a lightweight native container.

Tagline: *Your AI. Your API.*

## 2. Technology Stack

- **Desktop Framework**: Tauri 2
- **Backend & Core Runtime**: Rust (edition 2024), Tokio async runtime
- **Embedded HTTP Server**: Axum 0.8 + Tower-HTTP
- **IPC & Serialization**: Tauri IPC, Serde, Serde JSON
- **HTTP Client**: Reqwest 0.12 with separate timeouts for discovery and inference
- **Frontend Presentation**: React 19, TypeScript, Vite 8, Tailwind CSS v4

---

## 3. Embedded Runtime Lifecycle (Brick 0)

```
[ Tauri Desktop Launch ]
          │
          ├──> Initialize AppState (state = Starting, endpoint = http://127.0.0.1:4782)
          ├──> Initialize ModelRegistry (status = Checking)
          ├──> Register IPC commands:
          │      • `get_runtime_status`
          │      • `get_discovered_models`
          │      • `refresh_ollama`
          │      • `generate_test_completion` (Brick 2A)
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
          ├──> Spawn Tokio async task: initial Ollama model discovery
          │           │
          │           └──> Query Ollama GET /api/tags -> Update ModelRegistry -> Emit `models-updated`
          │
          ├──> Open Native WebView Window (App.tsx)
          │           │
          │           ├──> Invoke `get_runtime_status`
          │           ├──> Invoke `get_discovered_models`
          │           └──> Listen for `models-updated` Tauri event
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
The React frontend communicates with the Rust backend via Tauri 2 IPC commands:
- **`get_runtime_status`**: Returns `{ state, endpoint, error }`.
- **`get_discovered_models`**: Returns the current in-memory model registry snapshot without triggering network calls.
- **`refresh_ollama`**: Asynchronously triggers Ollama discovery and returns the updated registry snapshot.
- **`generate_test_completion`**: Invokes desktop-only model inference via provider-independent structures without exposing any public HTTP endpoint.

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
- **Loopback Exclusivity**: Bound strictly to `127.0.0.1`.
- **Restricted CORS**: Wildcard `*` CORS is rejected. Only standard local dev and Tauri origins (`http://localhost:1420`, `tauri://localhost`, `http://tauri.localhost`, `http://127.0.0.1:1420`) are permitted.
- **No Remote Administration or Shell Execution**: No endpoints exist that execute system commands or traverse filesystem paths.
- **No Unauthenticated Public Inference**: Model inference is accessible strictly via internal Tauri IPC (`generate_test_completion`), not via the Axum HTTP server.

---

## 4. Local Model Discovery & In-Memory Registry (Brick 1)

Brick 1 introduces auto-discovery for local AI engines, beginning with an official adapter for Ollama, abstracted behind a provider-independent model registry.

### 4.1 Normalized Model Abstraction
All AI models discovered from local or future remote engines are normalized into provider-independent data structures:

```rust
pub struct ModelInfo {
    pub id: String,                  // e.g. "ollama/qwen3:8b"
    pub provider_id: String,         // e.g. "ollama"
    pub upstream_id: String,         // e.g. "qwen3:8b"
    pub display_name: String,        // e.g. "qwen3:8b"
    pub capabilities: ModelCapabilities,
    pub size_bytes: Option<u64>,
    pub parameter_size: Option<String>,
    pub quantization_level: Option<String>,
    pub modified_at: Option<String>,
}
```

Capabilities (`chat`, `streaming`, `tools`, `vision`) use `Option<bool>`. Unknown capabilities remain `None` and are not assumed without inspection.

### 4.2 Discovery Trait
The `ModelDiscovery` trait enables pluggable adapter drivers for Ollama, LM Studio, vLLM, and external APIs without altering internal registry structures:

```rust
pub trait ModelDiscovery: Send + Sync {
    fn provider_id(&self) -> &str;
    fn discover(&self) -> impl Future<Output = Result<Vec<ModelInfo>, DiscoveryError>> + Send;
}
```

### 4.3 Ollama Provider Adapter
The Ollama adapter communicates with Ollama's local HTTP API:
- **Default Endpoint**: `http://127.0.0.1:11434`
- **Discovery Endpoint**: `GET /api/tags`
- **Timeouts**: Strict 1.5s connect timeout and 3.0s total discovery timeout to prevent UI degradation when Ollama is stopped or unresponsive.
- **Upstream ID Preservation**: Exact model names, namespaces, and tags (e.g. `library/mistral:7b-instruct-q4`) are preserved in `upstream_id` and formatted as `ollama/<upstream_id>` in `id`.
- **Inference Isolation**: No GPU or inference calls are made during discovery.

### 4.4 In-Memory Model Registry
The `ModelRegistry` maintains a thread-safe cache of detected providers and models:
- **Provider Status**: `checking`, `ready`, `no_models`, `unreachable`, `error`.
- **Concurrency Safety**: Registry read/write locks (`RwLock`) are never held across asynchronous HTTP network requests.
- **Serialized Refreshes**: A dedicated `Mutex<()>` serializes concurrent refresh invocations to prevent race conditions or out-of-order state overwrites.
- **Offline Invalidation**: When Ollama becomes unreachable or returns an error, the registry replaces the snapshot atomically and clears the active model list, guaranteeing that offline models are never falsely presented as available.

---

## 5. Provider-Independent Inference & Ollama Adapter (Brick 2A)

Brick 2A introduces provider-independent internal request/response contracts, capability validation, and actual local AI inference execution via Ollama.

### 5.1 Canonical Request & Response Types
Inference contracts are strictly provider-agnostic, with zero Ollama-specific leaks:

```rust
pub enum MessageRole {
    System,
    User,
    Assistant,
}

pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

pub struct InferenceRequest {
    pub model_id: String,            // Canonical model ID, e.g. "ollama/qwen3:8b"
    pub messages: Vec<ChatMessage>,
}

pub struct UsageInfo {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

pub struct InferenceResponse {
    pub model_id: String,
    pub content: String,
    pub finish_reason: Option<String>,
    pub usage: Option<UsageInfo>,
    pub duration_ms: Option<u64>,
}
```

### 5.2 Provider Inference Trait
The `ModelInference` trait establishes a unified execution interface for all present and future model drivers:

```rust
pub trait ModelInference: Send + Sync {
    fn generate(
        &self,
        request: InferenceRequest,
    ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send;
}
```

### 5.3 Ollama Inference Implementation
- **Native Endpoint**: `POST http://127.0.0.1:11434/api/chat`
- **Payload**:
  ```json
  {
    "model": "<upstream_model_name>",
    "messages": [
      { "role": "system", "content": "..." },
      { "role": "user", "content": "..." }
    ],
    "stream": false
  }
  ```
- **Timeout Management**: Separate 60-second inference timeout (`DEFAULT_INFERENCE_TIMEOUT`), fully decoupled from the 3-second discovery timeout.
- **Message Roles**: Maps `MessageRole::System` -> `"system"`, `MessageRole::User` -> `"user"`, `MessageRole::Assistant` -> `"assistant"`.
- **Usage Extraction**: Parses `prompt_eval_count` and `eval_count` into normalized `UsageInfo`.
- **Finish Reason**: Preserves `done_reason` or falls back to `"stop"` when `done == true`.

### 5.4 Capability Inspection Strategy
Before running text generation, mlue verifies that the target model actually supports completion:
1. Queries Ollama's documented `POST /api/show` endpoint with `{"model": "<upstream_model>"}`.
2. Inspects `capabilities` (`chat`, `completion`, `embedding`), `details.family`, `model_info["general.architecture"]`, and `template`.
3. If the model is embedding-only (e.g. `nomic-embed-text`, `bge-m3`, BERT architecture) and lacks chat/completion, mlue rejects the request with structured `InferenceError::UnsupportedCapability`.
4. **Caching & Invalidation**: Results are cached in an `Arc<RwLock<HashMap<String, ModelCapabilities>>>` keyed by model name, preventing redundant HTTP roundtrips on consecutive requests. The cache is automatically cleared whenever the model snapshot is refreshed via `clear_capabilities_cache()`.
5. **No Blind Trust**: Unknown capabilities remain `None` and are never silently treated as verified chat capabilities.

### 5.5 Model Selection & Execution Pipeline
When an inference request arrives at `ModelRegistry::execute_inference`:
1. **Resource Controls**: Validates maximum message count (100), single message length (500,000 chars), and total content size (1,000,000 chars).
2. **Model Validation**: Checks registry snapshot for the canonical `model_id`. Drops read lock immediately.
3. **Provider Availability**: Verifies the provider is in a non-unreachable state.
4. **Upstream Resolution**: Extracts the exact upstream model identifier (e.g., `qwen3:8b`).
5. **Capability Check**: Ensures text-completion capability via cached inspection.
6. **Execution & Normalization**: Issues network request through `OllamaProvider::generate_with_upstream` with zero registry locks held across the call.

### 5.6 Error Handling Architecture
All errors are mapped into strongly-typed `InferenceError` variants and converted into `InferenceErrorPayload` for desktop IPC presentation:
- `PROVIDER_UNREACHABLE`: "Ollama couldn't be reached. Start Ollama and try again."
- `MODEL_NOT_FOUND`: "Selected model is not installed or available."
- `UNSUPPORTED_CAPABILITY`: "This model doesn't support text chat."
- `TIMEOUT`: "The AI model took too long to respond. Try again."
- `HTTP_ERROR`: "AI service error (HTTP {code})."
- `MALFORMED_RESPONSE`: "Received an invalid response from the AI provider."
- `EMPTY_RESPONSE`: "The AI model returned an empty response. Try again."
- `INVALID_REQUEST`: "Invalid inference request."
- `MODEL_LOAD_FAILURE`: "The AI model failed to load or run. Please verify your system resources."

Raw user prompts and provider internals are never leaked in error payloads or default logs.

### 5.7 Current Limitations (Brick 2A)
- Public HTTP inference (`/v1/chat/completions`) is intentionally disabled; inference is accessible exclusively via Tauri desktop IPC.
- API key authentication and multi-user quota enforcement are scheduled for Brick 3.

---

## 6. Real-Time Inference Streaming & Cancellation (Brick 2B)

Brick 2B introduces token-by-token streaming from local Ollama into the desktop Test Playground using Tauri 2 IPC Channels, provider-independent streaming event contracts, atomic cancellation mechanics, decoupled timeouts, and concurrency controls.

### 6.1 Streaming Pipeline

```
React Playground (App.tsx)
      │
      │ 1. Tauri Channel callback registered
      │ 2. invoke('start_test_stream', { request_id, model_id, messages, on_event })
      ▼
Tauri IPC Command Handler (lib.rs)
      │
      │ 3. Validate input boundaries (50 msgs, 64 KiB/msg, 128 KiB total)
      │ 4. Acquire concurrency permit (InferenceManager semaphore: max 2 tasks)
      │ 5. Register active CancellationToken & TerminalGuard
      ▼
InferenceManager & ModelRegistry (registry/mod.rs)
      │
      │ 6. Resolve upstream model, check capabilities (cached, locks dropped)
      ▼
Ollama Streaming Driver (providers/ollama.rs)
      │
      │ 7. POST http://127.0.0.1:11434/api/chat with {"stream": true}
      │ 8. Receive raw byte chunks from Reqwest response stream
      ▼
Incremental NDJSON Parser (providers/ollama.rs)
      │
      │ • Bounded byte-buffer (max 64 KiB line limit)
      │ • Slices on '\n' boundary (guarantees split UTF-8 safety)
      │ • Parses Ollama chat chunk: message.content (ignores separate thinking fields)
      │ • Detects premature EOF without done: true
      │ • Extracts token usage (prompt_eval_count, eval_count)
      ▼
Canonical Stream Events (InferenceEvent)
      │
      │ [Started, Delta*, Completed | Cancelled | Error]
      ▼
TerminalGuard (inference/manager.rs)
      │
      │ • Atomic CAS: guarantees exactly one terminal event per request
      ▼
Tauri 2 IPC Channel (Channel<InferenceEvent>)
      │
      │ • Direct per-window IPC channel delivery (no global event bus)
      ▼
React Desktop Playground (App.tsx)
        • Incremental token rendering with throttled state batching (20ms)
        • Real-time Stop Generation button triggering invoke('cancel_test_completion')
```

### 6.2 Canonical Streaming Event Contract

Streaming events are strictly provider-agnostic and serialized with a tagged `type` field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InferenceEvent {
    Started {
        request_id: String,
        model_id: String,
    },
    Delta {
        request_id: String,
        content: String,
    },
    Completed {
        request_id: String,
        model_id: String,
        usage: Option<UsageInfo>,
        duration_ms: u64,
        finish_reason: Option<String>,
    },
    Cancelled {
        request_id: String,
    },
    Error {
        request_id: String,
        code: String,
        message: String,
    },
}
```

#### Event Invariants:
1. **Identifier Attribution**: Every event carries the unique `request_id`.
2. **Exactly One Terminal Event**: Every request concludes with exactly one terminal event: `Completed`, `Cancelled`, or `Error`.
3. **No Post-Terminal Emission**: Once a terminal event is sent, no further `Delta` or status events are emitted for that request.
4. **Chronological Ordering**: Chronological token ordering is strictly preserved.
5. **No Provider Leaks**: Provider-specific fields (e.g. Ollama's internal timestamp numbers or chunk headers) are transformed into canonical fields.

### 6.3 Native Ollama Incremental NDJSON Parser

Ollama returns newline-delimited JSON (`\n` separated JSON objects). The stream parser (`providers/ollama.rs`):
- **Bounded Buffering**: Accumulates incoming byte chunks into a `Vec<u8>` buffer with a hard 64 KiB ceiling per incomplete line. If an upstream chunk exceeds 64 KiB without a newline, it fails with `MalformedResponse`.
- **Split UTF-8 Handling**: Because multi-byte UTF-8 sequences never contain the ASCII newline byte `0x0A` (`\n`), slicing incoming bytes strictly on `\n` boundaries before UTF-8 decoding guarantees that multi-byte code points split across network packet boundaries assemble without corruption.
- **Message Content Extraction**: User-facing text is extracted exclusively from `message.content`. Any non-message or separate reasoning/thinking fields are not mixed into the displayed text delta.
- **Usage & Metadata Extraction**: When Ollama emits `done: true`, `prompt_eval_count` and `eval_count` are captured into `UsageInfo`, and `done_reason` is captured into `finish_reason`.
- **Premature EOF Detection**: If the upstream byte stream ends without emitting a `done: true` object, the parser emits an explicit `InferenceError::MalformedResponse("Premature EOF: stream closed without completion event")`.

### 6.4 Concurrency Control & Semaphore

- `InferenceManager` maintains an internal `tokio::sync::Semaphore` bounding active concurrent inference jobs across the desktop application (default limit: 2 concurrent tasks).
- Permits are acquired before registration and automatically returned on normal completion, cancellation, provider errors, timeouts, or channel teardown via RAII.
- Frontend playground restricts concurrent submissions per window; backend rejects excess jobs with `InferenceError::InvalidRequest("Maximum concurrent inference jobs reached")`.

### 6.5 Cancellation Architecture & Upstream Connection Abort

1. **Initiation**: The frontend calls `cancel_test_completion({ request_id })`.
2. **Handle Lookup**: `InferenceManager` locates the active request's `CancellationToken` in its concurrent registry.
3. **Cancellation Trigger**: `token.cancel()` is triggered.
4. **Upstream Abort**: In the async worker task, `tokio::select!` branches on `token.cancelled()`. The active `response.bytes_stream()` is immediately dropped. Dropping the Reqwest response stream terminates the underlying TCP socket connection to Ollama immediately.
5. **Permit Release**: The RAII concurrency permit is dropped.
6. **Single Terminal Event Enforcement**: A `TerminalGuard` uses atomic compare-and-swap (`AtomicU8`) to guarantee that `Cancelled` is emitted if and only if neither `Completed` nor `Error` has already fired.
7. **Cleanup**: Active request tracking is purged from `InferenceManager`.

### 6.6 Decoupled Timeout Policy

To support long-running generative tasks while preventing hung workers, timeouts are strictly decoupled:
- **Connection Timeout**: 5 seconds (establishing initial TCP connection to Ollama).
- **Initial Response Timeout**: 30 seconds (waiting for Ollama to load the model into memory/VRAM and emit the first byte chunk).
- **Idle Stream Timeout**: 15 seconds (maximum allowable silence between consecutive token chunks).
- **Overall Generation Timeout**: 300 seconds (5 minutes total generation limit).

Cancellation signals interrupt timeout waits immediately via Tokio select.

### 6.7 Input Boundary Protections

All inference requests are validated before acquiring concurrency permits or making network calls:
- Maximum chat messages: 50.
- Maximum single message content: 64 KiB (65,536 bytes UTF-8).
- Maximum total combined message content: 128 KiB (131,072 bytes UTF-8).

### 6.8 Known Limitations (Brick 2B)
- Public HTTP inference (`/v1/chat/completions`) remains disabled; inference is strictly accessible via internal desktop Tauri IPC.
- Model downloading/pulling workflows are out of scope.

---

## 7. Project Management, SQLite Persistence & Secure API Keys (Brick 3A)

### 7.1 Project Ownership Hierarchy

`mlue` organizes model consumption into distinct administrative **Projects**. A Project represents a distinct application, agent, or integration that consumes local AI capabilities through dedicated API credentials:

```text
mlue Desktop Installation
    │
    ├── Project A (e.g. "My Study Bot")
    │      ├── API Key 1 (mlue_sk_..., policy: local_only)
    │      └── API Key 2 (mlue_sk_..., policy: selected_models)
    │
    └── Project B (e.g. "Website AI Backend")
           └── API Key 3 (mlue_sk_..., policy: local_only)
```

#### Future Extension Hierarchy
The architecture intentionally keeps projects separate from future user-level abstractions:
```text
Project
   ├── API Keys
   ├── End Users   [future milestone]
   ├── Sessions    [future milestone]
   └── Memories    [future milestone]
```

### 7.2 Database Location & Storage Engine

- **Persistence Layer**: Embedded SQLite powered by `sqlx` (v0.8) with connection pooling (`SqlitePool`).
- **OS-Standard Path**: Database files are stored exclusively in the operating system's canonical application data directory obtained via Tauri 2's `app.path().app_data_dir()` API (e.g., `~/Library/Application Support/app.mlue.desktop/mlue.db` on macOS). Persistent files are never placed relative to the process working directory or temporary folders.
- **Journal & Performance Configuration**:
  - `PRAGMA journal_mode = WAL` (Write-Ahead Logging for high concurrency without blocking reads).
  - `PRAGMA synchronous = NORMAL`.
  - `PRAGMA foreign_keys = ON` (strict relational integrity enforcement).
  - `PRAGMA busy_timeout = 5000` (5-second lock queueing).

### 7.3 Schema & Versioned Migration Engine

Schema management uses a migration runner that tracks applied migrations in `_schema_migrations`. Migrations are executed within database transactions during application initialization before any IPC handlers or HTTP servers accept traffic:

#### Schema Tables

1. **`projects`**:
   - `id`: TEXT PRIMARY KEY (stable UUID v4)
   - `name`: TEXT NOT NULL (1 to 100 characters)
   - `description`: TEXT NULL (up to 500 characters)
   - `created_at`: TEXT NOT NULL (ISO 8601 UTC)
   - `updated_at`: TEXT NOT NULL (ISO 8601 UTC)

2. **`api_keys`**:
   - `id`: TEXT PRIMARY KEY (stable UUID v4)
   - `project_id`: TEXT NOT NULL (Foreign Key referencing `projects(id)` ON DELETE CASCADE)
   - `name`: TEXT NOT NULL (1 to 100 characters)
   - `key_prefix`: TEXT NOT NULL (first 16 characters for safe identification, e.g. `mlue_sk_a1b2c3d4...`)
   - `key_hash`: TEXT NOT NULL UNIQUE (SHA-256 digest in hex)
   - `access_policy`: TEXT NOT NULL (JSON-serialized `AccessPolicy`)
   - `enabled`: BOOLEAN NOT NULL DEFAULT 1
   - `created_at`: TEXT NOT NULL (ISO 8601 UTC)
   - `last_used_at`: TEXT NULL (ISO 8601 UTC)
   - `revoked_at`: TEXT NULL (ISO 8601 UTC)

3. **`settings`**:
   - `key`: TEXT PRIMARY KEY
   - `value`: TEXT NOT NULL

4. **Indexes**:
   - `idx_api_keys_project_id` on `api_keys(project_id)`
   - `idx_api_keys_key_hash` on `api_keys(key_hash)`

### 7.4 Cryptographic API-Key Generation & Secret Hygiene

- **Secret Format**: `mlue_sk_<random_secret>`
- **Entropy**: Generated using operating system cryptographically secure random number generator (`rand::rngs::OsRng`), producing 32 random bytes (256 bits of entropy).
- **Encoding**: URL-safe base64 without padding (`URL_SAFE_NO_PAD`).
- **One-Time Plaintext Secret Exposure**:
  - The plaintext secret exists in memory only during creation and is returned once in `CreatedApiKey.full_key`.
  - Plaintext keys are **never stored** in SQLite, configuration files, or logs.
  - The desktop UI immediately presents a copy modal with the explicit notice: *"Copy this key now. For security, mlue won't show it again."*
  - The UI clears the one-time secret state immediately upon dismissal.
- **Cryptographic Digest**: SQLite stores only the hex-encoded SHA-256 digest (`key_hash`).
- **Constant-Time Verification**: Key authentication hashes candidate keys and compares against stored digests using `subtle::ConstantTimeEq` to prevent timing side-channel attacks.
- **Metadata Protection**: `ApiKeyMetadata` strictly exposes `key_prefix` (e.g., `mlue_sk_8f7b3a1...`) and omits the hash or any secret material.

### 7.5 Access Policies & Permission Enforcement

Every API key is bound to an `AccessPolicy`:
1. `local_only` (Default):
   - Restricts key usage to local models (e.g. `ollama/*`, `local/*`).
   - Strictly denies any external or remote provider model identifiers (e.g. `openai/*`, `anthropic/*`, `gemini/*`, `cloud/*`).
   - The alias `auto` resolves to permitted local models.
2. `selected_models`:
   - Restricts key usage to an explicit whitelist of registered model IDs (e.g. `["ollama/qwen3:8b", "ollama/llama3:8b"]`).
   - Keys cannot invoke unlisted models even if hosted locally.

### 7.6 Service Layer Separation

- **`ProjectService`**: Handles project CRUD, input validation, and cascade deletion. Does not depend on Axum or Tauri UI types.
- **`ApiKeyService`**: Handles CSPRNG key generation, SHA-256 digest calculation, key verification, last-used updates, key revocation, and policy evaluation. Does not depend on Tauri UI components.
- **Storage Layer**: Manages SQLite connection pool lifecycle, migration runners, and transaction safety.
- **Tauri IPC Command Layer**: Thin adapters exposing:
  - `create_project`
  - `list_projects`
  - `update_project`
  - `delete_project`
  - `create_api_key`
  - `list_api_keys`
  - `revoke_api_key`

### 7.7 Security Boundaries
- **Administrative Control Plane**: All project and key management commands run strictly over internal Tauri IPC. Zero administrative or management endpoints are exposed over Axum HTTP.
- **No HTTP `/admin` Endpoint**: The desktop application is the sole administrative surface.

---

## 8. Authenticated HTTP API Layer & OpenAI Compatibility (Brick 3B-1)

Brick 3B-1 exposes local AI models through an authenticated, OpenAI-compatible HTTP interface running inside the embedded Axum server on `127.0.0.1:4782`.

```
[ External App / cURL / Python SDK ]
               │
               ├──> HTTP POST http://127.0.0.1:4782/v1/chat/completions
               │    Headers: Authorization: Bearer mlue_sk_<secret>
               │
               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Axum Embedded HTTP Server (127.0.0.1:4782)                                  │
│                                                                             │
│ 1. `require_api_key` Middleware:                                            │
│    • Validate `Bearer mlue_sk_...` header format                            │
│    • Extract secret -> SHA-256 hash -> Constant-time SQLite verification   │
│    • Verify parent Project exists and is not deleted                        │
│    • Verify key is not revoked (`revoked_at IS NULL`)                       │
│    • Spawn debounced `last_used_at` update (max once / 60s)                 │
│    • Attach `AuthenticatedKey` to request extensions                        │
│                                                                             │
│ 2. Custom Extractor `AppJson<T>`:                                           │
│    • Enforce 256 KiB maximum request body size (HTTP 413)                   │
│    • Map JSON parse errors to OpenAI error envelope (HTTP 400 invalid_json) │
│                                                                             │
│ 3. `chat_completions_handler`:                                              │
│    • Reject `stream: true` (HTTP 400 unsupported_parameter)                 │
│    • Validate message boundaries (max 50, 64 KiB single, 128 KiB total)     │
│    • Validate roles (system, user, assistant only)                          │
│    • Fail-closed model access policy check via ModelRegistry metadata:      │
│      - If model == "auto": deterministically resolve first eligible model   │
│      - If specific model: verify registry presence, provider, and policy    │
│    • Acquire inference concurrency permit from Semaphore                    │
│    • Dispatch canonical InferenceRequest to ModelInference adapter          │
│    • Drop permit upon completion or failure                                 │
│    • Wrap canonical response into OpenAI `chat.completion` envelope         │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Local Provider Engine (e.g. Ollama @ 127.0.0.1:11434)                       │
│ • POST /api/chat                                                            │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 8.1 Authentication Middleware (`require_api_key`)
- **Header Parsing**: Rejects missing or malformed headers with HTTP 401 `missing_auth_header` or `invalid_auth_header`.
- **Database Lookup & Timing Resistance**: Computes the SHA-256 digest of the bearer secret and queries `api_keys` joined with `projects`. Verification uses `subtle::ConstantTimeEq`. Keys of deleted projects or revoked keys are rejected immediately with HTTP 401 `invalid_api_key` or `revoked_api_key`.
- **Debounced Asynchronous Usage Tracking**: Updates `last_used_at` in the SQLite database at most once every 60 seconds per key. The database update is dispatched via `tokio::spawn` in the background and never blocks request latency.
- **Context Injection**: Successful authentication attaches `AuthenticatedKey` (`id`, `project_id`, `name`, `key_prefix`, `policy`) into Axum request extensions for downstream handlers.

### 8.2 Fail-Closed Model Access Policy Enforcement
Model authorization never relies on arbitrary client-provided model name strings or substring guessing:
1. **Registry Verification**: The model must exist in the active `ModelRegistry`.
2. **Provider Identity**: The model's owning provider must be verified as active and trusted.
3. **Policy Checking**:
   - `local_only`: Only permits models belonging to verified local providers (e.g., `ollama`).
   - `selected_models`: Verifies that the model's exact identifier is present in the key's explicit allowlist.
4. **Deterministic `auto` Resolution**: Resolves to the first available chat-capable model that satisfies the key's access policy. Returns HTTP 400 `no_models_available` if no permitted model exists.

### 8.3 Concurrency & Resource Protections
- **Global Concurrency Semaphore**: HTTP inference requests acquire an `OwnedSemaphorePermit` from `InferenceManager`. If max concurrent tasks (2) are running, requests immediately return HTTP 503 `capacity_exceeded` without hanging or leaking permits.
- **Payload Limits**: Axum enforces a 256 KiB body limit. Payloads exceeding this receive HTTP 413 `request_too_large`.
- **Message Content Boundaries**: Requests with >50 messages, >64 KiB single message, or >128 KiB total content are rejected before reaching upstream providers.

### 8.4 OpenAI Compatibility
- **`GET /v1/models`**: Returns `{ "object": "list", "data": [ ... ] }` containing all models permitted for the caller's key.
- **`POST /v1/chat/completions`**: Accepts OpenAI-formatted requests and returns `{ "id": "chatcmpl_mlue_...", "object": "chat.completion", "choices": [ ... ], "usage": { ... } }`.
- **Error Envelopes**: All error responses return standard OpenAI `{ "error": { "message", "type", "param", "code" } }` structures with appropriate HTTP status codes.

---

## 9. OpenAI-Compatible HTTP Streaming, Disconnect Cancellation & Security Hardening (Brick 3B-2)

Brick 3B-2 upgrades the authenticated `POST /v1/chat/completions` endpoint with full Server-Sent Events (SSE) streaming (`stream: true`), disconnect cancellation, backpressure preservation, and HTTP perimeter security hardening.

```
[ External Client / OpenAI SDK ]
             │
             ├──> POST http://127.0.0.1:4782/v1/chat/completions
             │    Headers: Authorization: Bearer mlue_sk_...
             │    Body: { "model": "auto", "stream": true, "stream_options": { "include_usage": true } }
             │
             ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Embedded Axum HTTP Boundary (127.0.0.1:4782)                                │
│                                                                             │
│ 1. Security Middleware (`validate_http_security`):                          │
│    • Validate Host header in [127.0.0.1, localhost, 127.0.0.1:4782]         │
│      -> Reject untrusted Host with HTTP 400 invalid_host                    │
│    • Validate Origin header (if present) against trusted local origins      │
│      -> Reject untrusted browser Origin with HTTP 403 untrusted_origin      │
│                                                                             │
│ 2. Parameter Rejection:                                                     │
│    • Unsupported generation fields (temperature, max_tokens, tools, etc.)   │
│      captured via #[serde(flatten)] unsupported_fields                      │
│      -> Reject with HTTP 400 unsupported_parameter                          │
│                                                                             │
│ 3. Fail-Closed Model Authorization & Concurrency Permit:                    │
│    • Acquire OwnedSemaphorePermit from InferenceManager (capacity bounded) │
│    • Resolve authorized model via ModelRegistry (no locks held across I/O)  │
│                                                                             │
│ 4. SSE Stream Pipeline (`SseStream`):                                       │
│    • CancellationToken + StreamGuard registered in InferenceManager         │
│    • Poll InferenceEventStream directly (zero unbounded intermediate queues)│
│    • Format chunks: role -> delta* -> stop -> [usage] -> data: [DONE]\n\n   │
│                                                                             │
│ 5. Disconnect Cancellation:                                                 │
│    • Client terminates connection / closes TCP socket                       │
│    • Axum drops response body -> StreamGuard dropped                        │
│    • CancellationToken cancelled -> Upstream Ollama TCP socket dropped      │
│    • Concurrency permit released & request untracked                        │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Local Provider (Ollama @ 127.0.0.1:11434)                                   │
│ • POST /api/chat with {"stream": true}                                      │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 9.1 Server-Sent Events (SSE) Protocol Implementation
- **Content-Type**: `text/event-stream; charset=utf-8` with `Cache-Control: no-cache` and `Connection: keep-alive`.
- **Zero-Unbounded-Queue Backpressure**: `SseStream` implements `futures_util::Stream<Item = Result<axum::body::Bytes, Infallible>>` by directly polling the underlying provider `InferenceEventStream`. It does not use intermediate Tokio unbounded channels (`mpsc::unbounded_channel`), preserving true TCP backpressure from the client to the provider.
- **Chunk Formatting**:
  - `Started`: Yields initial role chunk `{"role":"assistant"}`.
  - `Delta`: Yields content chunk `{"content":"..."}`.
  - `Completed`: Yields terminal finish chunk `{"finish_reason":"stop"}`.
  - `include_usage`: If requested, yields an OpenAI-standard usage chunk with `choices: []` and populated `usage`.
  - Terminal Frame: Yields `data: [DONE]\n\n` exactly once at stream conclusion.
- **No IPC Routing**: HTTP streaming connects directly from the Axum handler to the provider stream and never traverses Tauri IPC channels, preventing desktop UI serialization overhead.

### 9.2 Client Disconnect Cancellation (`StreamGuard`)
When an external client disconnects (e.g. user hits Ctrl+C, closes browser, or cancels a stream):
1. Axum detects the closed TCP socket and drops the downstream response stream.
2. Dropping the stream invokes the `StreamGuard` destructor (`Drop`).
3. `StreamGuard::drop` immediately:
   - Calls `cancellation_token.cancel()`.
   - The upstream provider worker task detects cancellation and terminates the active HTTP request to Ollama, aborting unnecessary model computation.
   - Unregisters the request ID from `InferenceManager`.
   - Drops `OwnedSemaphorePermit`, returning the concurrency permit to the pool.

### 9.3 Security Boundary Hardening
1. **Host Header Validation**: Every incoming request must present a loopback host header (`127.0.0.1`, `localhost`, or port-qualified variants). Untrusted host headers receive HTTP 400 `invalid_host`.
2. **Origin Header Validation**: Browser cross-origin requests are strictly validated. In production builds, only exact packaged Tauri schemes (`tauri://localhost`, `http://tauri.localhost`) are permitted. In development mode (`debug_assertions`), exact Vite dev ports (`http://localhost:1420`, `http://127.0.0.1:1420`) are additionally permitted. Arbitrary localhost ports (e.g. `http://localhost:8080`) and external domains are rejected with HTTP 403 `untrusted_origin`. Standard non-browser CLI clients (cURL, Python SDK, LangChain) do not send `Origin` headers and remain unaffected.
3. **Strict Parameter Rejection**: To avoid silent hallucination or unsupported behavior, parameters not yet implemented (`temperature`, `top_p`, `max_tokens`, `tools`, `functions`) are detected via Serde and rejected with HTTP 400 `unsupported_parameter`.

---

## 10. First-Run Onboarding & One-Click API Verification (Brick 4A)

Brick 4A introduces a unified first-run user onboarding flow and an instant one-click HTTP API verification mechanism in the desktop frontend, validating end-to-end connectivity without bypassing any core security or networking boundaries.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ desktop UI (`App.tsx` — Onboarding & Verification Component)                │
│                                                                             │
│  [ Step 1: Model ]  ───>  [ Step 2: Project ]  ───>  [ Step 3: API Key ]    │
│  (Discovered Ollama)     (Select / Create)           (Generate Secret)      │
│                                                              │              │
│                                                              ▼              │
│                                                   [ Step 4: Test My API ]   │
└──────────────────────────────────────────────────────────────┬──────────────┘
                                                               │
                                         Real Browser `fetch`  │
                                         or `verify_http_api_call` (loopback)
                                         POST /v1/chat/completions
                                         Headers: Authorization, Content-Type
                                                               │
                                                               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Embedded Axum HTTP Boundary (http://127.0.0.1:4782)                         │
│                                                                             │
│ 1. CORS Preflight & Origin Validation (tauri:// in prod; 1420 in dev)       │
│ 2. Constant-Time API Key Hash Verification (`subtle::ConstantTimeEq`)       │
│ 3. Concurrency Semaphore & Model Registry Access Policy Evaluation          │
│ 4. SSE Stream or Non-Streaming JSON Response Generation                     │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Local Provider (Ollama Engine @ http://127.0.0.1:11434)                     │
│ • Real model inference with llama3.2:1b                                     │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 10.1 Architectural Invariants & Verification Guarantees
1. **Real HTTP Verification & Narrowly Scoped Loopback Self-Test Adapter**:
   - The "Test My API" button in the frontend initiates verification against `http://127.0.0.1:4782/v1/chat/completions`.
   - In environments where browser `window.fetch()` directly executes cross-origin requests, it does so cleanly.
   - For packaged macOS WebViews running under custom URI schemes (`tauri://localhost`), WebKit security policies can restrict direct cross-scheme fetch calls to loopback HTTP. To guarantee 100% test fidelity without bypassing authentication or routing, `mlue` provides a narrowly scoped Tauri IPC command `verify_http_api_call`.
   - This adapter makes a genuine HTTP request via `reqwest` to the loopback gateway with real Bearer headers, exercising the exact same Axum routing, API key SHA-256 verification, model access control, and SSE chunk parsers.
   - It strictly rejects non-loopback URLs and does NOT call internal inference engine IPCs directly.
2. **Idempotent Setup Without Auto-Duplication**:
   - The onboarding component does NOT perform automatic entity creation in `useEffect` or mount handlers.
   - If no project exists, the user is presented with a single explicit creation action. Subsequent reloads detect existing projects and bind to the active selection without duplicate database records.
3. **Secret Hygiene**:
   - Full plaintext keys (`mlue_sk_...`) exist only in transient React component state for testing and copying.
   - Plaintext keys are never written to SQLite, localStorage, or IPC logs.
4. **Live Latency & Token Diagnostics**:
   - Time-To-First-Token (TTFT) and total request duration are measured using high-precision performance timers (`performance.now()`).
   - Token metrics (`prompt_tokens`, `completion_tokens`, `total_tokens`) are parsed from streaming usage chunks and displayed in real time.
5. **Copy-Ready Developer Integration Examples**:
   - Generates fully populated, working snippets for Python (using official `openai` SDK v1.x with streaming and sync modes) and cURL (streaming SSE and sync JSON) with dynamic endpoints and key references.





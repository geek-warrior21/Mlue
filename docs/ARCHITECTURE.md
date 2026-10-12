# mlue Architecture — System Design & Runtime Lifecycle

## 1. Overview

**mlue** is a local-first desktop application that exposes supported AI models and services through personal API keys. Its architecture isolates local network access, desktop presentation, and asynchronous model communication within a lightweight native container.

Tagline: *Your AI. Your API.*

## 2. Technology Stack

- **Desktop Framework**: Tauri 2
- **Backend & Core Runtime**: Rust (edition 2024), Tokio async runtime
- **Embedded HTTP Server**: Axum 0.8 + Tower-HTTP
- **IPC & Serialization**: Tauri IPC, Serde, Serde JSON
- **HTTP Client**: Reqwest 0.12 with strict timeouts
- **Frontend Presentation**: React 19, TypeScript, Vite 8, Tailwind CSS v4

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
          │           └──> Query Ollama GET /api/tags -> Update ModelRegistry
          │
          ├──> Open Native WebView Window (App.tsx)
          │           │
          │           ├──> Invoke `get_runtime_status`
          │           └──> Invoke `get_discovered_models`
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
- **No Remote Administration or Shell Execution**: No endpoints exist that execute system commands or traverse filesystem paths.

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
- **Timeouts**: Strict 1.5s connect timeout and 3.0s total request timeout to prevent UI degradation when Ollama is stopped or unresponsive.
- **Upstream ID Preservation**: Exact model names, namespaces, and tags (e.g. `library/mistral:7b-instruct-q4`) are preserved in `upstream_id` and formatted as `ollama/<upstream_id>` in `id`.
- **Inference Isolation**: No GPU or inference calls are made during discovery.

### 4.4 In-Memory Model Registry
The `ModelRegistry` maintains a thread-safe cache of detected providers and models:
- **Provider Status**: `checking`, `ready`, `no_models`, `unreachable`, `error`.
- **Concurrency Safety**: Registry read/write locks (`RwLock`) are never held across asynchronous HTTP network requests.
- **Serialized Refreshes**: A dedicated `Mutex<()>` serializes concurrent refresh invocations to prevent race conditions or out-of-order state overwrites.
- **Offline Invalidation**: When Ollama becomes unreachable or returns an error, the registry replaces the snapshot atomically and clears the active model list, guaranteeing that offline models are never falsely presented as available.

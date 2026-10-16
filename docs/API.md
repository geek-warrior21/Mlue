# mlue Authenticated HTTP API Reference

mlue exposes a local-first, OpenAI-compatible HTTP interface powered by an embedded Rust Axum server running inside the desktop application. This document specifies the authentication protocol, API endpoints, error envelopes, constraints, and integration examples.

---

## 1. Base URL & Network Boundaries

- **Base URL**: `http://127.0.0.1:4782/v1`
- **Health Check**: `http://127.0.0.1:4782/health`
- **Loopback Exclusivity**: The embedded server binds strictly to IPv4 loopback (`127.0.0.1:4782`). It does not bind to `0.0.0.0` or expose open network surfaces to external networks or the internet.
- **Host Header Validation**: Requests must present a loopback host header (`127.0.0.1`, `localhost`, or `127.0.0.1:4782`). Untrusted hosts are rejected with HTTP 400 `invalid_host`.
- **Origin Header Validation**: Browser cross-origin requests are validated. Requests from untrusted origins are rejected with HTTP 403 `untrusted_origin`. In production, only exact packaged desktop application origins (`tauri://localhost`, `http://tauri.localhost`) are accepted. In development mode, exact Vite dev server origins (`http://localhost:1420`, `http://127.0.0.1:1420`) are additionally permitted. Arbitrary localhost browser origins (e.g. `http://localhost:8080`) are strictly rejected. Standard non-browser CLI clients (cURL, Python SDK, which omit `Origin`) are accepted. Wildcard CORS (`*`) is strictly prohibited.

---

## 2. Authentication

All endpoints under `/v1/*` require an active mlue API key passed in the standard HTTP `Authorization` header:

```http
Authorization: Bearer mlue_sk_<secret>
```

### 2.1 Key Lifecycle & Security Invariants
- **Generation**: Generated exclusively via the mlue desktop UI using OS CSPRNG (`rand::rngs::OsRng`) providing 256 bits of entropy.
- **One-Time Secret Presentation**: The plaintext key is displayed only once upon generation in the desktop modal. It is never stored in SQLite, log files, or IPC cache.
- **SHA-256 Storage & Constant-Time Verification**: mlue stores only the cryptographic SHA-256 hash. Key lookup and secret comparison use constant-time operations (`subtle::ConstantTimeEq`) to eliminate timing side-channels.
- **Project Scope**: Each key is linked to a Project. If a project is deleted, all associated keys are immediately rejected.
- **Instant Revocation**: Revoking a key in the desktop UI takes effect immediately on subsequent requests.
- **Debounced Usage Tracking**: Key usage updates the `last_used_at` timestamp in the database, debounced to a maximum of once every 60 seconds per key without blocking request processing.

---

## 3. Endpoints

### 3.1 Health Check (Unauthenticated)

Inspect the embedded server status and version without credentials:

```http
GET /health
```

#### Response (`200 OK`)
```json
{
  "status": "ok",
  "service": "mlue",
  "version": "0.1.0"
}
```

---

### 3.2 List Permitted Models

Retrieve the list of models currently registered and permitted for the authenticating API key:

```http
GET /v1/models
Authorization: Bearer mlue_sk_...
```

#### Response (`200 OK`)
```json
{
  "object": "list",
  "data": [
    {
      "id": "ollama/qwen3:8b",
      "object": "model",
      "created": 1711281600,
      "owned_by": "mlue",
      "permission": []
    },
    {
      "id": "ollama/llama3:8b",
      "object": "model",
      "created": 1711281600,
      "owned_by": "mlue",
      "permission": []
    }
  ]
}
```

*Note:* If an API key has a `selected_models` access policy, only the permitted models currently present and available in the provider registry are returned.

---

### 3.3 Create Chat Completion

Execute a chat completion request against a permitted local model in either non-streaming (`stream: false`) or streaming (`stream: true`) mode:

```http
POST /v1/chat/completions
Authorization: Bearer mlue_sk_...
Content-Type: application/json
```

#### Request Body
```json
{
  "model": "ollama/qwen3:8b",
  "messages": [
    { "role": "system", "content": "You are a concise programming assistant." },
    { "role": "user", "content": "Write a hello world function in Rust." }
  ],
  "stream": false
}
```

#### Request Parameters
| Parameter | Type | Required | Description |
|---|---|---|---|
| `model` | string | **Yes** | Model identifier (e.g., `ollama/qwen3:8b`, `qwen3:8b`), or `"auto"` for automatic model resolution. |
| `messages` | array | **Yes** | Ordered array of message objects. Each object must have `role` (`"system"`, `"user"`, or `"assistant"`) and `content` (string). |
| `stream` | boolean | No | Optional, defaults to `false`. When `true`, returns a Server-Sent Events (SSE) stream (`text/event-stream; charset=utf-8`). |
| `stream_options` | object | No | Optional. Supported options: `include_usage` (boolean). When `true` and `stream: true`, an additional chunk containing token usage is emitted prior to `[DONE]`. |

#### Request Limits & Validation Constraints
- **Payload Size**: Maximum request body size is **256 KiB** (enforced at HTTP layer). Payloads exceeding this limit receive HTTP 413 `request_too_large`.
- **Message Count**: Maximum **50 messages** per conversation request.
- **Single Message Size**: Maximum **64 KiB** per message content.
- **Total Messages Size**: Maximum **128 KiB** total text content across all messages.
- **Unsupported Roles**: Roles other than `system`, `user`, or `assistant` (e.g. `tool`, `function`) return HTTP 400 `unsupported_role`.
- **Strict Parameter Rejection**: Passing unsupported parameters (e.g. `temperature`, `top_p`, `max_tokens`, `tools`, `functions`, `tool_choice`, `function_call`) returns HTTP 400 `unsupported_parameter` with the name of the rejected parameter.

#### Non-Streaming Response (`200 OK`, `stream: false`)
```json
{
  "id": "chatcmpl_mlue_019234ab-5678-7123-8abc-def012345678",
  "object": "chat.completion",
  "created": 1711281605,
  "model": "ollama/qwen3:8b",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "fn main() {\n    println!(\"Hello, world!\");\n}"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 28,
    "completion_tokens": 14,
    "total_tokens": 42
  }
}
```

---

### 3.4 Streaming Chat Completion (`stream: true`)

When `stream: true` is passed, the endpoint returns an OpenAI-compatible Server-Sent Events (SSE) stream with `Content-Type: text/event-stream; charset=utf-8` and headers `Cache-Control: no-cache` and `Connection: keep-alive`.

#### Streaming Chunk Format
Each SSE data event is prefixed with `data: ` and formatted as a JSON `chat.completion.chunk` object, terminated with double newlines (`\n\n`):

1. **Initial Role Chunk**:
```sse
data: {"id":"chatcmpl_mlue_...","object":"chat.completion.chunk","created":1711281605,"model":"ollama/qwen3:8b","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

```

2. **Incremental Delta Chunks**:
```sse
data: {"id":"chatcmpl_mlue_...","object":"chat.completion.chunk","created":1711281605,"model":"ollama/qwen3:8b","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

```

3. **Terminal Completion Chunk**:
```sse
data: {"id":"chatcmpl_mlue_...","object":"chat.completion.chunk","created":1711281605,"model":"ollama/qwen3:8b","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

```

4. **Optional Usage Chunk** (when `stream_options.include_usage: true`):
```sse
data: {"id":"chatcmpl_mlue_...","object":"chat.completion.chunk","created":1711281605,"model":"ollama/qwen3:8b","choices":[],"usage":{"prompt_tokens":28,"completion_tokens":14,"total_tokens":42}}

```

5. **Terminal Done Event**:
```sse
data: [DONE]

```

#### Disconnect Cancellation Mechanics
If an external HTTP client closes the connection, aborts reading, or terminates early:
1. Axum detects the dropped socket and drops the downstream response stream.
2. The internal `StreamGuard` RAII handle triggers an immediate cancellation on the associated Tokio `CancellationToken`.
3. Upstream HTTP connection to the provider (Ollama) is immediately terminated, stopping unnecessary model compute.
4. Active request tracking is purged from `InferenceManager` and the concurrency semaphore permit is immediately returned.

---

## 4. Model Resolution & Fail-Closed Access Policy

### 4.1 Trusted Provider Identity
Access policies in mlue are enforced strictly using internal registry metadata, not by matching arbitrary model name prefixes.
- **`local_only` (Default Policy)**: Permitted only for models discovered from trusted local engines (e.g., Ollama).
- **`selected_models` Policy**: Permitted only for exact model identifiers explicitly granted in the key's allowlist.
- **Fail-Closed Protection**: Any model name not found in the verified `ModelRegistry` or belonging to an unauthorized provider returns HTTP 404 `model_not_found` or HTTP 403 `unauthorized_model`. Attempting to bypass checks using forged names (e.g. `ollama/nonexistent`) is strictly rejected.

### 4.2 Automatic Routing (`model: "auto"`)
When `model: "auto"` is requested:
1. mlue inspects all models currently registered in `ModelRegistry`.
2. Filters to models permitted by the authenticating key's `AccessPolicy`.
3. Filters to models verifying chat capability (rejects embedding-only models).
4. Deterministically selects the first eligible model.
5. If no permitted model is available, returns HTTP 400 `no_models_available`.

---

## 5. Error Envelope & Error Code Reference

All errors conform to the standard OpenAI JSON error envelope:

```json
{
  "error": {
    "message": "Human-readable explanation of what went wrong.",
    "type": "invalid_request_error",
    "param": "model",
    "code": "model_not_found"
  }
}
```

### Complete Error Code Catalog

| HTTP Status | Error Type | Error Code | Description |
|---|---|---|---|
| `400 Bad Request` | `invalid_request_error` | `invalid_host` | Untrusted or non-loopback `Host` header provided. |
| `400 Bad Request` | `invalid_request_error` | `invalid_json` | Request body is not well-formed JSON. |
| `400 Bad Request` | `invalid_request_error` | `invalid_request_error` | Missing required fields (`messages`, empty array, empty content). |
| `400 Bad Request` | `invalid_request_error` | `unsupported_parameter` | Unsupported generation parameter supplied (e.g. `temperature`, `max_tokens`, `tools`, `functions`). |
| `400 Bad Request` | `invalid_request_error` | `unsupported_role` | Message role is not `system`, `user`, or `assistant`. |
| `400 Bad Request` | `invalid_request_error` | `unsupported_capability` | Requested model is an embedding model or lacks chat capability. |
| `400 Bad Request` | `invalid_request_error` | `no_models_available` | `model: "auto"` could not find any eligible chat model under the key policy. |
| `401 Unauthorized` | `invalid_request_error` | `missing_auth_header` | `Authorization` header was not provided. |
| `401 Unauthorized` | `invalid_request_error` | `invalid_auth_header` | Header format is invalid (must be `Bearer mlue_sk_<secret>`). |
| `401 Unauthorized` | `invalid_request_error` | `invalid_api_key` | Secret key does not match any active key, or parent project was deleted. |
| `401 Unauthorized` | `invalid_request_error` | `revoked_api_key` | Key has been revoked. |
| `403 Forbidden` | `invalid_request_error` | `untrusted_origin` | Cross-origin browser request from untrusted `Origin` header rejected. |
| `403 Forbidden` | `invalid_request_error` | `unauthorized_model` | Model is not permitted under the key's access policy. |
| `404 Not Found` | `invalid_request_error` | `model_not_found` | Requested model is not discovered or registered. |
| `413 Payload Too Large` | `invalid_request_error` | `request_too_large` | Request body exceeds 256 KiB limit, or messages exceed limits (max 50, 64 KiB single, 128 KiB total). |
| `503 Service Unavailable` | `api_error` | `provider_offline` | Upstream provider (e.g. Ollama) is not running or unreachable. |
| `503 Service Unavailable` | `api_error` | `capacity_exceeded` | Maximum concurrent inference requests exceeded (semaphore limit reached). |
| `504 Gateway Timeout` | `api_error` | `timeout` | Upstream provider failed to generate a response within the timeout. |
| `502 Bad Gateway` | `api_error` | `provider_error` | Upstream provider returned an error or failed to load the model. |
| `500 Internal Error` | `api_error` | `internal_error` | Unexpected internal server failure. |

---

## 6. Integration Examples

### 6.1 Using cURL

#### List Models
```bash
curl -X GET http://127.0.0.1:4782/v1/models \
  -H "Authorization: Bearer mlue_sk_YOUR_SECRET_KEY"
```

#### Non-Streaming Chat Completion
```bash
curl -X POST http://127.0.0.1:4782/v1/chat/completions \
  -H "Authorization: Bearer mlue_sk_YOUR_SECRET_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [
      { "role": "user", "content": "Explain Rust ownership in two sentences." }
    ],
    "stream": false
  }'
```

#### Real-Time Streaming Chat Completion (SSE)
```bash
curl -N -X POST http://127.0.0.1:4782/v1/chat/completions \
  -H "Authorization: Bearer mlue_sk_YOUR_SECRET_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [
      { "role": "user", "content": "Count from 1 to 5 slowly." }
    ],
    "stream": true,
    "stream_options": {
      "include_usage": true
    }
  }'
```

---

### 6.2 Using the Official OpenAI Python SDK

Install the OpenAI client:
```bash
pip install openai
```

#### Non-Streaming Example
```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:4782/v1",
    api_key=os.environ.get("MLUE_API_KEY", "mlue_sk_YOUR_SECRET_KEY"),
)

# 1. List accessible models
models = client.models.list()
print("Discovered models:", [m.id for m in models.data])

# 2. Generate a non-streaming completion
response = client.chat.completions.create(
    model="auto",
    messages=[
        {"role": "system", "content": "You are a concise engineering assistant."},
        {"role": "user", "content": "What is the capital of France?"},
    ],
    stream=False,
)

print("Response:", response.choices[0].message.content)
print("Tokens used:", response.usage.total_tokens)
```

#### Real-Time Streaming Example
```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:4782/v1",
    api_key=os.environ.get("MLUE_API_KEY", "mlue_sk_YOUR_SECRET_KEY"),
)

# Generate a streaming completion
stream = client.chat.completions.create(
    model="auto",
    messages=[
        {"role": "user", "content": "Write a haiku about local AI sovereignty."},
    ],
    stream=True,
    stream_options={"include_usage": True},
)

for chunk in stream:
    if chunk.choices and chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="", flush=True)
    if chunk.usage:
        print(f"\n[Usage: {chunk.usage.total_tokens} tokens]")
print()
```

---

## 7. First-Run Onboarding & One-Click Verification Compatibility

As of Brick 4A, mlue includes an interactive, browser-level API verification test runner built into the desktop interface.

### 7.1 Web Origin & CORS Preflight Rules
When testing through the desktop WebView or a local browser frontend:
- **Preflight Request**: The browser dispatches an HTTP `OPTIONS /v1/chat/completions` request before the POST call.
- **Allowed Headers**: `Authorization`, `Content-Type`.
- **Allowed Methods**: `GET`, `POST`, `OPTIONS`.
- **Permitted Origins**: Loopback WebViews (`http://localhost:1420`, `http://127.0.0.1:1420`, `tauri://localhost`, `http://tauri.localhost`).
- **Forbidden Origins**: Any non-local or untrusted domain receives HTTP 403 `untrusted_origin`.

### 7.2 Native Verification Call Pattern
The one-click test button triggers an authentic HTTP POST using the Web API `fetch()`:
```javascript
const response = await fetch("http://127.0.0.1:4782/v1/chat/completions", {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${activeApiKey}`,
    "Content-Type": "application/json"
  },
  body: JSON.stringify({
    model: "auto",
    messages: [{ role: "user", content: "Explain data sovereignty in two sentences." }],
    stream: true,
    stream_options: { include_usage: true }
  })
});
```
This tests the full production HTTP gateway stack identically to third-party SDK calls.


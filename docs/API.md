# mlue Authenticated HTTP API Reference

mlue exposes a local-first, OpenAI-compatible HTTP interface powered by an embedded Rust Axum server running inside the desktop application. This document specifies the authentication protocol, API endpoints, error envelopes, constraints, and integration examples.

---

## 1. Base URL & Network Boundaries

- **Base URL**: `http://127.0.0.1:4782/v1`
- **Health Check**: `http://127.0.0.1:4782/health`
- **Loopback Exclusivity**: The embedded server binds strictly to IPv4 loopback (`127.0.0.1:4782`). It does not bind to `0.0.0.0` or expose open network surfaces to external networks or the internet.
- **CORS Policy**: Permissive wildcard CORS (`*`) is prohibited. Only local desktop and development origins are accepted.

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

Execute a non-streaming chat completion request against a permitted local model:

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
| `stream` | boolean | No | Must be `false` or omitted. HTTP streaming is not supported in this version. |

#### Request Limits & Validation Constraints
- **Payload Size**: Maximum request body size is **256 KiB** (enforced at HTTP layer). Payloads exceeding this limit receive HTTP 413 `request_too_large`.
- **Message Count**: Maximum **50 messages** per conversation request.
- **Single Message Size**: Maximum **64 KiB** per message content.
- **Total Messages Size**: Maximum **128 KiB** total text content across all messages.
- **Unsupported Roles**: Roles other than `system`, `user`, or `assistant` (e.g. `tool`, `function`) return HTTP 400 `unsupported_role`.
- **Unsupported Parameters**: Passing `tools`, `functions`, `tool_choice`, or `function_call` returns HTTP 400 `unsupported_parameter`.
- **Streaming Policy**: Specifying `"stream": true` returns HTTP 400 `unsupported_parameter` with a descriptive message that HTTP streaming is planned for a future milestone.

#### Response (`200 OK`)
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
| `400 Bad Request` | `invalid_request_error` | `invalid_json` | Request body is not well-formed JSON. |
| `400 Bad Request` | `invalid_request_error` | `invalid_request_error` | Missing required fields (`messages`, empty array, empty content). |
| `400 Bad Request` | `invalid_request_error` | `unsupported_parameter` | Unsupported options supplied (e.g. `stream: true`, `tools`, `functions`). |
| `400 Bad Request` | `invalid_request_error` | `unsupported_role` | Message role is not `system`, `user`, or `assistant`. |
| `400 Bad Request` | `invalid_request_error` | `unsupported_capability` | Requested model is an embedding model or lacks chat capability. |
| `400 Bad Request` | `invalid_request_error` | `no_models_available` | `model: "auto"` could not find any eligible chat model under the key policy. |
| `401 Unauthorized` | `invalid_request_error` | `missing_auth_header` | `Authorization` header was not provided. |
| `401 Unauthorized` | `invalid_request_error` | `invalid_auth_header` | Header format is invalid (must be `Bearer mlue_sk_<secret>`). |
| `401 Unauthorized` | `invalid_request_error` | `invalid_api_key` | Secret key does not match any active key, or parent project was deleted. |
| `401 Unauthorized` | `invalid_request_error` | `revoked_api_key` | Key has been revoked. |
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

#### Chat Completion (Specific Model)
```bash
curl -X POST http://127.0.0.1:4782/v1/chat/completions \
  -H "Authorization: Bearer mlue_sk_YOUR_SECRET_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "ollama/qwen3:8b",
    "messages": [
      { "role": "user", "content": "Explain Rust ownership in two sentences." }
    ]
  }'
```

#### Chat Completion (Automatic Model Routing)
```bash
curl -X POST http://127.0.0.1:4782/v1/chat/completions \
  -H "Authorization: Bearer mlue_sk_YOUR_SECRET_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [
      { "role": "user", "content": "Hello mlue!" }
    ]
  }'
```

---

### 6.2 Using the Official OpenAI Python SDK

Install the OpenAI client:
```bash
pip install openai
```

Execute an inference completion:
```python
import os
from openai import OpenAI

# Point client to the local mlue runtime
client = OpenAI(
    base_url="http://127.0.0.1:4782/v1",
    api_key=os.environ.get("MLUE_API_KEY", "mlue_sk_YOUR_SECRET_KEY"),
)

# 1. List accessible models
models = client.models.list()
print("Discovered models:", [m.id for m in models.data])

# 2. Generate a completion
response = client.chat.completions.create(
    model="auto",  # or a specific model like "ollama/qwen3:8b"
    messages=[
        {"role": "system", "content": "You are a concise engineering assistant."},
        {"role": "user", "content": "What is the capital of France?"},
    ],
    stream=False,
)

print("Response:", response.choices[0].message.content)
print("Tokens used:", response.usage.total_tokens)
```

use super::error::ApiError;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// Validates that incoming HTTP requests to the embedded server have trusted Host and Origin headers.
/// This protects against DNS rebinding attacks and unauthorized browser cross-origin requests.
pub async fn validate_host_and_origin(req: Request, next: Next) -> Result<Response, ApiError> {
    // 1. Validate Host header
    if let Some(host_val) = req.headers().get(axum::http::header::HOST) {
        let host_str = host_val.to_str().unwrap_or("");
        if !is_trusted_host(host_str) {
            return Err(ApiError::bad_request(
                format!(
                    "Invalid or untrusted Host header '{}'. mlue embedded server only accepts loopback requests ('127.0.0.1', 'localhost').",
                    host_str
                ),
                "invalid_host",
                Some("host".to_string()),
            ));
        }
    }

    // 2. Validate Origin header if present
    // Non-browser clients (curl, Python SDK, CLI tools) do not send an Origin header.
    // If an Origin header is present (browser cross-origin request), it MUST match our trusted local origins.
    if let Some(origin_val) = req.headers().get(axum::http::header::ORIGIN) {
        let origin_str = origin_val.to_str().unwrap_or("");
        if !is_trusted_origin(origin_str) {
            return Err(ApiError::forbidden(
                format!(
                    "Untrusted browser Origin '{}'. mlue embedded server only accepts trusted local application origins.",
                    origin_str
                ),
                "untrusted_origin",
                Some("origin".to_string()),
            ));
        }
    }

    Ok(next.run(req).await)
}

/// Checks whether the Host header represents a legitimate loopback address.
/// Accepts: 127.0.0.1, localhost, [::1], optionally with any port.
pub fn is_trusted_host(host: &str) -> bool {
    let host_clean = host.trim();
    if host_clean.is_empty() {
        return false;
    }

    // Extract the hostname part before any trailing :port
    let hostname = if host_clean.starts_with('[') {
        // IPv6 literal, e.g. [::1]:4782 or [::1]
        if let Some(end_bracket) = host_clean.find(']') {
            &host_clean[..=end_bracket]
        } else {
            return false;
        }
    } else if let Some(colon_pos) = host_clean.find(':') {
        &host_clean[..colon_pos]
    } else {
        host_clean
    };

    let hostname_lower = hostname.to_ascii_lowercase();
    hostname_lower == "127.0.0.1" || hostname_lower == "localhost" || hostname_lower == "[::1]"
}

/// Checks whether the Origin header represents a trusted local desktop / development origin.
/// In production, only the exact packaged Tauri webview origins ('tauri://localhost', 'http://tauri.localhost') are trusted.
/// In debug/development mode, the exact Vite dev server origins ('http://localhost:1420', 'http://127.0.0.1:1420') are additionally permitted.
/// Arbitrary localhost browser origins (e.g. 'http://localhost:8080') are rejected in all environments.
pub fn is_trusted_origin(origin: &str) -> bool {
    let origin_clean = origin.trim();
    if origin_clean.is_empty() {
        return false;
    }

    // Exact matches for known Tauri packaged schemes (all environments)
    if origin_clean == "tauri://localhost" || origin_clean == "http://tauri.localhost" {
        return true;
    }

    // In development mode only, permit exact Vite development server origins
    #[cfg(debug_assertions)]
    {
        if origin_clean == "http://localhost:1420" || origin_clean == "http://127.0.0.1:1420" {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_trusted_host() {
        assert!(is_trusted_host("127.0.0.1"));
        assert!(is_trusted_host("127.0.0.1:4782"));
        assert!(is_trusted_host("localhost"));
        assert!(is_trusted_host("localhost:4782"));
        assert!(is_trusted_host("LOCALHOST:8080"));
        assert!(is_trusted_host("[::1]"));
        assert!(is_trusted_host("[::1]:4782"));

        assert!(!is_trusted_host(""));
        assert!(!is_trusted_host("   "));
        assert!(!is_trusted_host("evil.com"));
        assert!(!is_trusted_host("attacker.com:4782"));
        assert!(!is_trusted_host("192.168.1.100"));
        assert!(!is_trusted_host("0.0.0.0"));
    }

    #[test]
    fn test_is_trusted_origin_packaged_tauri() {
        assert!(is_trusted_origin("tauri://localhost"));
        assert!(is_trusted_origin("http://tauri.localhost"));
    }

    #[test]
    fn test_is_trusted_origin_rejects_arbitrary_and_malicious() {
        assert!(!is_trusted_origin(""));
        assert!(!is_trusted_origin("http://evil.com"));
        assert!(!is_trusted_origin("https://malicious.org"));
        assert!(!is_trusted_origin("http://localhost:8080"));
        assert!(!is_trusted_origin("http://localhost:3000"));
        assert!(!is_trusted_origin("http://127.0.0.1:8000"));
        assert!(!is_trusted_origin("http://tauri.localhost.evil.com"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_is_trusted_origin_dev_mode() {
        assert!(is_trusted_origin("http://localhost:1420"));
        assert!(is_trusted_origin("http://127.0.0.1:1420"));
    }
}

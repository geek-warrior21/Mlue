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
pub fn is_trusted_origin_with_mode(origin: &str, is_debug: bool) -> bool {
    let origin_clean = origin.trim();
    if origin_clean.is_empty() {
        return false;
    }

    // Exact matches for known Tauri packaged schemes (all environments)
    if origin_clean == "tauri://localhost" || origin_clean == "http://tauri.localhost" {
        return true;
    }

    // In development mode only, permit exact Vite development server origins
    if is_debug
        && (origin_clean == "http://localhost:1420" || origin_clean == "http://127.0.0.1:1420")
    {
        return true;
    }

    false
}

pub fn is_trusted_origin(origin: &str) -> bool {
    is_trusted_origin_with_mode(origin, cfg!(debug_assertions))
}

/// Strictly validates destination URLs for internal HTTP self-test adapters.
/// Ensures the target is strictly the local mlue HTTP service:
/// - Scheme must be http://
/// - Host must be 127.0.0.1 or localhost
/// - Port must exactly match the expected mlue port (e.g. 4782)
/// - No userinfo (e.g. user:pass@)
/// - Path must be empty, "/", or exactly match the allowed API route (e.g. "/v1/chat/completions" or "/v1")
/// - No query string or URL fragment
pub fn validate_loopback_api_destination(
    raw_url: &str,
    expected_port: u16,
    allowed_route: &str,
) -> Result<reqwest::Url, String> {
    let clean = raw_url.trim();
    if clean.is_empty() {
        return Err("Destination URL cannot be empty".to_string());
    }

    let parsed = reqwest::Url::parse(clean)
        .map_err(|e| format!("Invalid destination URL '{}': {}", clean, e))?;

    // 1. Strict scheme: HTTP only (no https, ftp, file, gopher, etc.)
    if parsed.scheme() != "http" {
        return Err(format!(
            "Invalid scheme '{}'. Self-test adapter only permits 'http://'",
            parsed.scheme()
        ));
    }

    // 2. Strict userinfo check: no credentials allowed in URL
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "Self-test adapter forbids userinfo credentials in destination URL".to_string(),
        );
    }

    // 3. Strict host check: strictly loopback
    let host = parsed.host_str().unwrap_or("");
    if host != "127.0.0.1" && host != "localhost" {
        return Err(format!(
            "Untrusted destination host '{}'. Self-test adapter strictly requires loopback ('127.0.0.1' or 'localhost')",
            host
        ));
    }

    // 4. Strict port check: exact mlue service port only
    let port = parsed.port().unwrap_or(80);
    if port != expected_port {
        return Err(format!(
            "Destination port {} is rejected. Self-test adapter only permits exact mlue service port {}",
            port, expected_port
        ));
    }

    // 5. Strict path check: must be empty, "/", or match the allowed route or its parent "/v1"
    let clean_allowed = allowed_route.trim_matches('/');
    let clean_path = parsed.path().trim_matches('/');
    if !clean_path.is_empty() && clean_path != clean_allowed && clean_path != "v1" {
        return Err(format!(
            "Destination path '{}' is rejected. Self-test adapter only permits explicitly allowed route '/{}'",
            parsed.path(),
            clean_allowed
        ));
    }

    // 6. Strict query and fragment check
    if parsed.query().is_some() {
        return Err("Self-test adapter forbids query parameters in destination URL".to_string());
    }
    if parsed.fragment().is_some() {
        return Err("Self-test adapter forbids URL fragments in destination URL".to_string());
    }

    // Canonicalize to the exact allowed route
    let canonical = format!("http://{}:{}/{}", host, port, clean_allowed);
    reqwest::Url::parse(&canonical).map_err(|e| e.to_string())
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

    #[test]
    fn test_is_trusted_origin_release_mode_strictness() {
        // In release mode (is_debug = false):
        assert!(is_trusted_origin_with_mode("tauri://localhost", false));
        assert!(is_trusted_origin_with_mode("http://tauri.localhost", false));

        // Vite dev origins MUST be rejected in release mode!
        assert!(!is_trusted_origin_with_mode("http://localhost:1420", false));
        assert!(!is_trusted_origin_with_mode("http://127.0.0.1:1420", false));

        // Arbitrary origins MUST be rejected
        assert!(!is_trusted_origin_with_mode("http://localhost:8080", false));
        assert!(!is_trusted_origin_with_mode("http://evil.com", false));
    }

    #[test]
    fn test_is_trusted_origin_debug_mode() {
        assert!(is_trusted_origin_with_mode("http://localhost:1420", true));
        assert!(is_trusted_origin_with_mode("http://127.0.0.1:1420", true));
        assert!(is_trusted_origin_with_mode("tauri://localhost", true));
        assert!(is_trusted_origin_with_mode("http://tauri.localhost", true));

        assert!(!is_trusted_origin_with_mode("http://localhost:8080", true));
        assert!(!is_trusted_origin_with_mode("http://evil.com", true));
    }

    #[test]
    fn test_validate_loopback_api_destination_valid() {
        let route = "/v1/chat/completions";
        let valid_cases = [
            "http://127.0.0.1:4782",
            "http://127.0.0.1:4782/",
            "http://127.0.0.1:4782/v1",
            "http://127.0.0.1:4782/v1/chat/completions",
            "http://localhost:4782",
            "http://localhost:4782/v1/chat/completions",
        ];

        for case in valid_cases {
            let res = validate_loopback_api_destination(case, 4782, route);
            assert!(res.is_ok(), "Expected valid for '{}': {:?}", case, res);
            assert_eq!(
                res.unwrap().as_str(),
                format!(
                    "http://{}:4782/v1/chat/completions",
                    if case.contains("localhost") {
                        "localhost"
                    } else {
                        "127.0.0.1"
                    }
                )
            );
        }
    }

    #[test]
    fn test_validate_loopback_api_destination_invalid() {
        let route = "/v1/chat/completions";

        // Bad scheme
        assert!(validate_loopback_api_destination("https://127.0.0.1:4782", 4782, route).is_err());
        assert!(validate_loopback_api_destination("ftp://127.0.0.1:4782", 4782, route).is_err());
        assert!(validate_loopback_api_destination("file:///etc/passwd", 4782, route).is_err());

        // Userinfo
        assert!(
            validate_loopback_api_destination("http://admin:pass@127.0.0.1:4782", 4782, route)
                .is_err()
        );
        assert!(
            validate_loopback_api_destination("http://user@127.0.0.1:4782", 4782, route).is_err()
        );

        // Non-loopback host
        assert!(validate_loopback_api_destination("http://192.168.1.1:4782", 4782, route).is_err());
        assert!(
            validate_loopback_api_destination("http://169.254.169.254:4782", 4782, route).is_err()
        );
        assert!(validate_loopback_api_destination("http://evil.com:4782", 4782, route).is_err());
        assert!(validate_loopback_api_destination("http://0.0.0.0:4782", 4782, route).is_err());

        // Arbitrary / wrong port
        assert!(validate_loopback_api_destination("http://127.0.0.1:8080", 4782, route).is_err());
        assert!(validate_loopback_api_destination("http://127.0.0.1:11434", 4782, route).is_err());
        assert!(validate_loopback_api_destination("http://127.0.0.1:3000", 4782, route).is_err());

        // Arbitrary path
        assert!(
            validate_loopback_api_destination("http://127.0.0.1:4782/admin", 4782, route).is_err()
        );
        assert!(
            validate_loopback_api_destination("http://127.0.0.1:4782/v1/models", 4782, route)
                .is_err()
        );
        assert!(
            validate_loopback_api_destination(
                "http://127.0.0.1:4782/v1/chat/completions/../../admin",
                4782,
                route
            )
            .is_err()
        );

        // Query & Fragment
        assert!(
            validate_loopback_api_destination(
                "http://127.0.0.1:4782/v1/chat/completions?admin=1",
                4782,
                route
            )
            .is_err()
        );
        assert!(
            validate_loopback_api_destination(
                "http://127.0.0.1:4782/v1/chat/completions#hash",
                4782,
                route
            )
            .is_err()
        );
    }
}

//! Shared HTTP client and error formatting for the LLM engines and image generation.

use std::sync::OnceLock;
use std::time::Duration;

/// Time allowed to establish a TCP/TLS connection to the API.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Upper bound for a whole request (send + model thinking + response). Generous because
/// extended thinking and image generation can legitimately take minutes, but bounded so
/// a dropped Wi-Fi connection can't hang a request forever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// How much of an error response body to include in the error message.
const ERROR_BODY_SNIPPET_CHARS: usize = 500;

/// One process-wide client so connections (and TLS sessions) are reused across requests
/// instead of building a new client for every call.
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|e| {
                log::warn!("Failed to build HTTP client with timeouts ({}); falling back to defaults", e);
                reqwest::Client::new()
            })
    })
}

/// Build an error for a non-success API response that keeps the useful part: the status
/// code plus the start of the body (where providers explain what went wrong, e.g. an
/// unknown model, a rate limit or an unsupported image).
pub fn api_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let body = body.trim();
    let mut snippet: String = body.chars().take(ERROR_BODY_SNIPPET_CHARS).collect();
    if body.chars().count() > ERROR_BODY_SNIPPET_CHARS {
        snippet.push('…');
    }
    if snippet.is_empty() {
        anyhow::anyhow!("API error {}", status)
    } else {
        anyhow::anyhow!("API error {}: {}", status, snippet)
    }
}

/// Read a response, returning its body text on success or an [`api_error`] otherwise.
pub async fn read_body(response: reqwest::Response) -> anyhow::Result<String> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("failed to read API response ({}): {}", status, e.without_url()))?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(api_error(status, &body))
    }
}

/// Strip the request URL from a reqwest error so it never ends up in logs or the UI.
pub fn send_error(e: reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        anyhow::anyhow!("API request timed out: {}", e.without_url())
    } else {
        anyhow::anyhow!("API request failed: {}", e.without_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_includes_status_and_body_snippet() {
        let err = api_error(reqwest::StatusCode::BAD_REQUEST, r#"{"error":{"message":"model not found"}}"#).to_string();
        assert!(err.contains("400"), "{}", err);
        assert!(err.contains("model not found"), "{}", err);
    }

    #[test]
    fn api_error_truncates_long_bodies() {
        let body = "x".repeat(5000);
        let err = api_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &body).to_string();
        assert!(err.len() < 600, "{}", err.len());
        assert!(err.ends_with('…'));
    }

    #[test]
    fn api_error_without_body() {
        assert_eq!(api_error(reqwest::StatusCode::UNAUTHORIZED, "  ").to_string(), "API error 401 Unauthorized");
    }

    #[test]
    fn client_is_shared() {
        assert!(std::ptr::eq(client(), client()));
    }
}

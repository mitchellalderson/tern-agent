//! Shared HTTP plumbing for provider clients.

use std::time::Duration;

use tern_core::types::ProviderError;

pub fn client() -> Result<reqwest::Client, ProviderError> {
    tern_core::net::http_client()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| ProviderError::Config(format!("http client: {e}")))
}

pub fn network(e: reqwest::Error) -> ProviderError {
    ProviderError::Network(root_cause(&e))
}

fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    let mut msg = e.to_string();
    while let Some(next) = cur.source() {
        cur = next;
        msg = format!("{msg}: {next}");
    }
    msg
}

/// Turn a non-2xx response into an error, keeping `retry-after`.
pub async fn check(resp: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(Duration::from_secs_f64);
    let mut body = resp.text().await.unwrap_or_default();
    body.truncate(2000);
    Err(ProviderError::Http {
        status: status.as_u16(),
        body,
        retry_after,
    })
}

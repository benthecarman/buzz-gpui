//! Finite reads and writes over HTTPS, authenticated with NIP-98.

use crate::origin::RelayOrigin;
use crate::socket::Filter;
use buzz_identity::Identity;
use nostr::prelude::Event;
use std::sync::Arc;
use std::time::Duration;

/// Larger responses are refused rather than buffered.
pub const MAX_RESPONSE: usize = 8 << 20;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("could not reach the relay: {0}")]
    Transport(String),
    /// The relay answered with an error status.
    #[error("relay answered {status}: {message}")]
    Status {
        status: u16,
        message: String,
        /// From `Retry-After`, when the relay is rate limiting.
        retry_after: Option<Duration>,
    },
    #[error("relay response is too large")]
    TooLarge,
    #[error("relay response is invalid: {0}")]
    Invalid(String),
}

#[derive(Clone)]
pub struct RelayHttp {
    client: reqwest::Client,
    origin: RelayOrigin,
    identity: Arc<Identity>,
}

impl RelayHttp {
    pub fn new(origin: RelayOrigin, identity: Arc<Identity>) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("build the HTTP client");
        Self {
            client,
            origin,
            identity,
        }
    }

    pub fn origin(&self) -> &RelayOrigin {
        &self.origin
    }

    /// `POST /query`. Events with bad signatures are dropped.
    pub async fn query(&self, filters: &[Filter]) -> Result<Vec<Event>, HttpError> {
        let body = serde_json::to_string(filters).expect("filters serialize");
        let bytes = self.post("/query", body).await?;
        let raw: Vec<serde_json::Value> = serde_json::from_slice(&bytes)
            .map_err(|error| HttpError::Invalid(error.to_string()))?;
        Ok(raw
            .into_iter()
            .filter_map(|value| serde_json::from_value::<Event>(value).ok())
            .filter(|event| event.verify().is_ok())
            .collect())
    }

    /// A signed `POST` with a JSON body. Never retried here: a write that
    /// failed in transport may still have reached the relay.
    pub async fn post(&self, path: &str, body: String) -> Result<Vec<u8>, HttpError> {
        let url = self.origin.endpoint(path);
        let auth = self
            .identity
            .http_auth(&url, "POST", Some(body.as_bytes()))
            .map_err(|error| HttpError::Invalid(error.to_string()))?;
        let response = self
            .client
            .post(&url)
            .header("Authorization", auth)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| HttpError::Transport(error.to_string()))?;
        read(response).await
    }

    /// `GET /` as NIP-11 relay information.
    pub async fn information(&self) -> Result<serde_json::Value, HttpError> {
        let response = self
            .client
            .get(self.origin.endpoint("/"))
            .header("Accept", "application/nostr+json")
            .send()
            .await
            .map_err(|error| HttpError::Transport(error.to_string()))?;
        let bytes = read(response).await?;
        serde_json::from_slice(&bytes).map_err(|error| HttpError::Invalid(error.to_string()))
    }
}

async fn read(mut response: reqwest::Response) -> Result<Vec<u8>, HttpError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    if response
        .content_length()
        .is_some_and(|length| length as usize > MAX_RESPONSE)
    {
        return Err(HttpError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| HttpError::Transport(error.to_string()))?
    {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err(HttpError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|e| e.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(200).collect());
        return Err(HttpError::Status {
            status: status.as_u16(),
            message,
            retry_after,
        });
    }
    Ok(bytes)
}

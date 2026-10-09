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
    /// Not sent: the relay asked us to back off.
    #[error("relay requests paused; retry in {}s", .retry_after.as_secs())]
    Paused { retry_after: Duration },
}

impl HttpError {
    /// Whether the request certainly never reached the relay.
    pub fn not_sent(&self) -> bool {
        matches!(self, Self::Paused { .. })
    }
}

#[derive(Clone)]
pub struct RelayHttp {
    client: reqwest::Client,
    origin: RelayOrigin,
    identity: Arc<Identity>,
    lane: Arc<Lane>,
}

/// Admission for every signed request to one relay from one identity
/// (`http-admission.ts`): at most 6 awaiting a response, and a relay quota
/// refusal pauses the whole lane so queued requests fail at once instead
/// of piling onto a relay that already said no.
struct Lane {
    slots: tokio::sync::Semaphore,
    paused_until: std::sync::Mutex<Option<tokio::time::Instant>>,
}

const LANE_SLOTS: usize = 6;
/// Pause assumed when a quota refusal gives no `retry in Ns` hint.
const DEFAULT_QUOTA_PAUSE: Duration = Duration::from_secs(60);
const MAX_QUOTA_PAUSE: Duration = Duration::from_secs(86_400);

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
            lane: Arc::new(Lane {
                slots: tokio::sync::Semaphore::new(LANE_SLOTS),
                paused_until: std::sync::Mutex::new(None),
            }),
        }
    }

    pub fn identity(&self) -> &Arc<Identity> {
        &self.identity
    }

    pub fn origin(&self) -> &RelayOrigin {
        &self.origin
    }

    /// `POST /query`. Events with bad signatures are dropped.
    pub async fn query(&self, filters: &[Filter]) -> Result<Vec<Event>, HttpError> {
        Ok(self
            .query_raw(filters)
            .await?
            .into_iter()
            .filter_map(|value| serde_json::from_value::<Event>(value).ok())
            .filter(|event| event.verify().is_ok())
            .collect())
    }

    /// `POST /query` returning the raw event array, for callers that verify
    /// it themselves.
    pub async fn query_raw(&self, filters: &[Filter]) -> Result<Vec<serde_json::Value>, HttpError> {
        let body = serde_json::to_string(filters).expect("filters serialize");
        let bytes = self.post("/query", body).await?;
        serde_json::from_slice(&bytes).map_err(|error| HttpError::Invalid(error.to_string()))
    }

    /// A signed `POST` with a JSON body. Never retried here: a write that
    /// failed in transport may still have reached the relay.
    pub async fn post(&self, path: &str, body: String) -> Result<Vec<u8>, HttpError> {
        self.check_paused()?;
        let _slot = self
            .lane
            .slots
            .acquire()
            .await
            .expect("lane semaphore stays open");
        // A pause may have started while this request waited for a slot.
        self.check_paused()?;
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
        let result = read(response).await;
        if let Err(HttpError::Status {
            status: 429,
            message,
            retry_after,
        }) = &result
            && message.starts_with("rate-limited: quota exceeded")
        {
            let pause = quota_pause(message)
                .or(*retry_after)
                .unwrap_or(DEFAULT_QUOTA_PAUSE)
                .min(MAX_QUOTA_PAUSE);
            *self.lane.paused_until.lock().expect("lane lock") =
                Some(tokio::time::Instant::now() + pause);
        }
        result
    }

    fn check_paused(&self) -> Result<(), HttpError> {
        let paused = *self.lane.paused_until.lock().expect("lane lock");
        match paused {
            Some(until) if until > tokio::time::Instant::now() => Err(HttpError::Paused {
                retry_after: until - tokio::time::Instant::now(),
            }),
            _ => Ok(()),
        }
    }

    /// `GET /` as NIP-11 relay information.
    pub async fn information(&self) -> Result<serde_json::Value, HttpError> {
        self.get_public("/").await
    }

    /// An unauthenticated `GET` of a JSON document, such as NIP-11 or the
    /// join policy. The relay only answers JSON on `/` with this `Accept`.
    pub async fn get_public(&self, path: &str) -> Result<serde_json::Value, HttpError> {
        let response = self
            .client
            .get(self.origin.endpoint(path))
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

/// `rate-limited: quota exceeded; retry in Ns` → N+1 seconds; the extra
/// second covers the relay's whole-second rounding.
fn quota_pause(message: &str) -> Option<Duration> {
    let seconds: u64 = message
        .rsplit_once("retry in ")?
        .1
        .strip_suffix('s')?
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds + 1))
}

#[cfg(test)]
mod tests {
    #[test]
    fn quota_hint_adds_a_second() {
        assert_eq!(
            super::quota_pause("rate-limited: quota exceeded; retry in 12s"),
            Some(std::time::Duration::from_secs(13))
        );
        assert_eq!(super::quota_pause("rate-limited: quota exceeded"), None);
    }
}

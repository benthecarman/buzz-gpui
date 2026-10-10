//! GIF search for one session (`gifs.ts`). Only HTTPS communities offer
//! it, as in the reference (`communityFromScope`).

use super::Session;
use crate::gifs::{self, Gif, SEARCH_PATH, UNAVAILABLE};
use crate::http::HttpError;

impl Session {
    /// Whether the relay advertises KLIPY search in NIP-11. Read fresh;
    /// callers keep the answer while the picker is open.
    pub async fn gif_search_available(&self) -> bool {
        if !self.origin().http().starts_with("https://") {
            return false;
        }
        let http = self.direct.http.clone();
        self.run(async move {
            let info = http
                .information()
                .await
                .map_err(|error| error.to_string())?;
            Ok(gifs::search_path(&info).is_some())
        })
        .await
        .unwrap_or(false)
    }

    /// One page of GIFs for `query`, trending when it is empty. The
    /// relay forwards it to the provider with its own key.
    pub async fn search_gifs(
        &self,
        query: &str,
        customer_id: &str,
        locale: &str,
    ) -> Result<Vec<Gif>, String> {
        if !self.origin().http().starts_with("https://") {
            return Err(UNAVAILABLE.into());
        }
        let http = self.direct.http.clone();
        let body = gifs::search_body(customer_id, locale, query);
        self.run(async move {
            let bytes = http
                .post(SEARCH_PATH, body)
                .await
                .map_err(|error| match error {
                    HttpError::Status {
                        status, message, ..
                    } if message.trim().is_empty() => format!("GIF request failed ({status})"),
                    HttpError::Status { message, .. } => message,
                    other => other.to_string(),
                })?;
            let response: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| "GIF search failed".to_string())?;
            gifs::parse(&response)
        })
        .await
    }
}

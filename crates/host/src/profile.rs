//! Publishing a new agent's profile (kind 0), signed with the agent's own
//! key, as buzz-app does (`src-tauri/src/agents/profile_http.rs`).
//!
//! The controller decides what the profile says; this only moves it to the
//! relay and checks it became current, since a replaceable event can be
//! accepted without winning.

use base64::Engine;
use buzz_agent_controller::{CreationProfile, Secret};
use serde_json::Value;

type Result<T> = std::result::Result<T, String>;

fn authorization(event: Value) -> Result<String> {
    let json = serde_json::to_vec(&event).map_err(|_| "Could not authorize profile request")?;
    Ok(format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD.encode(json)
    ))
}

async fn body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        return Err("Profile request refused; the profile stays pending".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Profile response unavailable; retry publication")?
    {
        if bytes.len() + chunk.len() > limit {
            return Err("Profile response is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn current(
    client: &reqwest::Client,
    profile: &CreationProfile,
    key: &Secret,
) -> Result<Vec<Value>> {
    let query = serde_json::to_vec(&serde_json::json!([
        { "kinds": [0], "authors": [&profile.pubkey], "limit": 5 }
    ]))
    .map_err(|_| "Could not encode profile query")?;
    let response = client
        .post(profile.query_url())
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            authorization(profile.authenticate_query(key, &query)?)?,
        )
        .header("x-auth-tag", &profile.auth)
        .body(query)
        .send()
        .await
        .map_err(|_| "Could not read the current profile; it stays pending")?;
    serde_json::from_slice(&body(response, 1024 * 1024).await?)
        .map_err(|_| "Invalid current profile response".into())
}

pub async fn publish(
    client: &reqwest::Client,
    profile: &CreationProfile,
    key: &Secret,
) -> Result<()> {
    let event = profile.event(key, &current(client, profile, key).await?)?;
    let event_id = event
        .get("id")
        .and_then(Value::as_str)
        .ok_or("Invalid profile")?
        .to_string();
    let bytes = serde_json::to_vec(&event).map_err(|_| "Could not encode profile")?;
    let response = client
        .post(&profile.url)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            authorization(profile.authenticate(key, &bytes)?)?,
        )
        .header("x-auth-tag", &profile.auth)
        .body(bytes)
        .send()
        .await
        .map_err(|_| "Profile publication unconfirmed; retry")?;
    let receipt: Value = serde_json::from_slice(&body(response, 16 * 1024).await?)
        .map_err(|_| "Invalid profile receipt; retry")?;
    if receipt.get("accepted").and_then(Value::as_bool) != Some(true)
        || receipt.get("event_id").and_then(Value::as_str) != Some(event_id.as_str())
    {
        return Err("The relay did not accept the profile; retry".into());
    }
    profile.confirm(&current(client, profile, key).await?, &event_id)
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("build the profile HTTP client")
}

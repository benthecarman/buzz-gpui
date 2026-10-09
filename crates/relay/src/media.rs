//! Media download (`docs/port/relay.md` §5n.6).
//!
//! Relay media (`<origin>/media/<sha256>[.ext]`) needs a fresh Blossom
//! `get` authorization on every request, which only this process can sign,
//! so the UI fetches it here rather than through a generic HTTP image
//! loader. Other `https://` URLs (profile pictures, custom emoji hosted
//! elsewhere) load unauthenticated, as the reference webview loads them.
//!
//! Fetches run on the session's tokio runtime and resolve to the whole
//! body. Recent bodies stay in a small in-memory cache, standing in for the
//! webview cache the reference relies on (`max-age=3600`).

use crate::origin::RelayOrigin;
use buzz_identity::{Identity, MediaVerb};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use url::Url;

/// Largest relay media body (`MAX_MEDIA` in the reference proxy).
const MAX_RELAY_MEDIA: usize = 100 * 1024 * 1024;
/// Largest body from another host. The UI only loads images from them.
const MAX_PUBLIC_MEDIA: usize = 25 * 1024 * 1024;
/// Bytes kept in the cache, and the largest body worth caching.
const CACHE_BUDGET: usize = 64 * 1024 * 1024;
const CACHE_ENTRY_LIMIT: usize = 16 * 1024 * 1024;
/// Downloads in flight at once, so a screen of avatars cannot starve the
/// relay connection.
const SLOTS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("This media URL is not supported")]
    Unsupported,
    #[error("The media is too large")]
    TooLarge,
    #[error("The media server answered {0}")]
    Status(u16),
    #[error("{0}")]
    Transport(String),
}

/// Media downloads for one community. Cheap to clone.
#[derive(Clone)]
pub struct Media(Arc<Inner>);

struct Inner {
    origin: RelayOrigin,
    identity: Arc<Identity>,
    runtime: tokio::runtime::Handle,
    /// Signed requests never follow redirects, so the authorization cannot
    /// leak to another host.
    relay: reqwest::Client,
    public: reqwest::Client,
    slots: Semaphore,
    cache: Mutex<Cache>,
}

impl Media {
    pub fn new(
        runtime: tokio::runtime::Handle,
        origin: RelayOrigin,
        identity: Arc<Identity>,
    ) -> Self {
        let relay = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("build the media client");
        let public = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("build the media client");
        Self(Arc::new(Inner {
            origin,
            identity,
            runtime,
            relay,
            public,
            slots: Semaphore::new(SLOTS),
            cache: Mutex::new(Cache::default()),
        }))
    }

    /// Whether `url` is this relay's media: same origin, path
    /// `/media/<64 lowercase hex>[.ext]` with `ext` `[a-z0-9]{1,8}` or
    /// `thumb.jpg`, and no query or fragment (`relay.rs` `media_url`).
    pub fn is_relay_media(&self, url: &str) -> bool {
        relay_media_url(&self.0.origin, url).is_some()
    }

    /// The relay's thumbnail for its own media, which avatars use
    /// (`mediaUrl(..., "small")`). `None` for any other URL.
    pub fn thumbnail(&self, url: &str) -> Option<String> {
        let url = relay_media_url(&self.0.origin, url)?;
        let name = url.path().strip_prefix("/media/")?;
        let hash = name.split('.').next()?;
        let mut thumb = url.clone();
        thumb.set_path(&format!("/media/{hash}.thumb.jpg"));
        Some(thumb.to_string())
    }

    pub(crate) fn origin(&self) -> &RelayOrigin {
        &self.0.origin
    }

    pub(crate) fn identity(&self) -> &Arc<Identity> {
        &self.0.identity
    }

    /// The client for this relay: no redirects, a 600 s timeout.
    pub(crate) fn relay_client(&self) -> &reqwest::Client {
        &self.0.relay
    }

    pub(crate) fn runtime(&self) -> &tokio::runtime::Handle {
        &self.0.runtime
    }

    /// The whole body of `url`: signed for this relay's media, plain for
    /// other `https://` URLs, refused for anything else.
    pub async fn fetch(&self, url: &str) -> Result<Arc<[u8]>, MediaError> {
        let target = if let Some(url) = relay_media_url(&self.0.origin, url) {
            Target::Relay(url)
        } else {
            let url = Url::parse(url).map_err(|_| MediaError::Unsupported)?;
            if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
                return Err(MediaError::Unsupported);
            }
            Target::Public(url)
        };
        let key = target.url().to_string();
        if let Some(bytes) = self.0.cache.lock().expect("media cache").get(&key) {
            return Ok(bytes);
        }
        let inner = self.0.clone();
        let bytes = self
            .0
            .runtime
            .spawn(async move { inner.download(target).await })
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))??;
        self.0
            .cache
            .lock()
            .expect("media cache")
            .insert(key, bytes.clone());
        Ok(bytes)
    }
}

enum Target {
    Relay(Url),
    Public(Url),
}

impl Target {
    fn url(&self) -> &Url {
        match self {
            Self::Relay(url) | Self::Public(url) => url,
        }
    }
}

impl Inner {
    async fn download(&self, target: Target) -> Result<Arc<[u8]>, MediaError> {
        let _slot = self.slots.acquire().await.expect("media slots stay open");
        let (request, limit) = match &target {
            Target::Relay(url) => {
                let auth = self
                    .identity
                    .media_auth(MediaVerb::Get, &server(url), None)
                    .map_err(|error| MediaError::Transport(error.to_string()))?;
                (
                    self.relay.get(url.as_str()).header("Authorization", auth),
                    MAX_RELAY_MEDIA,
                )
            }
            Target::Public(url) => (self.public.get(url.as_str()), MAX_PUBLIC_MEDIA),
        };
        let mut response = request
            .send()
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(MediaError::Status(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(MediaError::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
        {
            if bytes.len() + chunk.len() > limit {
                return Err(MediaError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes.into())
    }
}

/// `host[:port]` as the relay checks the Blossom `server` tag (`URL.host`).
pub(crate) fn server(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

fn relay_media_url(origin: &RelayOrigin, value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    let base = Url::parse(&origin.http()).ok()?;
    if url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let name = url.path().strip_prefix("/media/")?;
    let (hash, extension) = name.split_once('.').unwrap_or((name, ""));
    let extension_ok = extension.is_empty()
        || extension == "thumb.jpg"
        || (extension.len() <= 8
            && extension
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    let hash_ok = hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (hash_ok && extension_ok).then_some(url)
}

/// Least recently used bodies within [`CACHE_BUDGET`].
#[derive(Default)]
struct Cache {
    entries: HashMap<String, (Arc<[u8]>, u64)>,
    bytes: usize,
    tick: u64,
}

impl Cache {
    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|(bytes, used)| {
            *used = tick;
            bytes.clone()
        })
    }

    fn insert(&mut self, key: String, bytes: Arc<[u8]>) {
        if bytes.len() > CACHE_ENTRY_LIMIT {
            return;
        }
        self.tick += 1;
        self.bytes += bytes.len();
        if let Some((old, _)) = self.entries.insert(key, (bytes, self.tick)) {
            self.bytes -= old.len();
        }
        while self.bytes > CACHE_BUDGET {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some((old, _)) = self.entries.remove(&oldest) {
                self.bytes -= old.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn recognizes_only_this_relays_media() {
        let origin = RelayOrigin::parse("wss://relay.example.com").unwrap();
        let ok = |url: &str| relay_media_url(&origin, url).is_some();
        assert!(ok(&format!("https://relay.example.com/media/{HASH}.jpg")));
        assert!(ok(&format!("https://relay.example.com/media/{HASH}")));
        assert!(ok(&format!(
            "https://relay.example.com/media/{HASH}.thumb.jpg"
        )));
        assert!(!ok(&format!("https://other.example.com/media/{HASH}.jpg")));
        assert!(!ok(&format!("http://relay.example.com/media/{HASH}.jpg")));
        assert!(!ok(&format!(
            "https://relay.example.com/media/{HASH}.jpg?x=1"
        )));
        assert!(!ok(&format!("https://relay.example.com/media/{HASH}.JPG")));
        assert!(!ok("https://relay.example.com/media/abc.jpg"));
        assert!(!ok(&format!("https://relay.example.com/files/{HASH}.jpg")));

        let local = RelayOrigin::parse("ws://127.0.0.1:53000").unwrap();
        let url = format!("http://127.0.0.1:53000/media/{HASH}.png");
        assert_eq!(
            server(&relay_media_url(&local, &url).unwrap()),
            "127.0.0.1:53000"
        );
    }

    #[test]
    fn thumbnails_relay_media_only() {
        let media = Media::new(
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .handle()
                .clone(),
            RelayOrigin::parse("wss://relay.example.com").unwrap(),
            Arc::new(Identity::ephemeral()),
        );
        assert_eq!(
            media.thumbnail(&format!("https://relay.example.com/media/{HASH}.png")),
            Some(format!("https://relay.example.com/media/{HASH}.thumb.jpg"))
        );
        assert_eq!(media.thumbnail("https://example.com/a.png"), None);
    }

    #[test]
    fn cache_evicts_the_least_recently_used() {
        let mut cache = Cache::default();
        let big = || Arc::<[u8]>::from(vec![0u8; CACHE_ENTRY_LIMIT]);
        for key in ["a", "b", "c", "d"] {
            cache.insert(key.into(), big());
        }
        assert!(cache.get("a").is_some());
        cache.insert("e".into(), big());
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
        assert_eq!(cache.bytes, CACHE_BUDGET);
    }
}

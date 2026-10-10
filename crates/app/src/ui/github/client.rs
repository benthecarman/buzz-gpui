//! Public GitHub REST reads. Requests carry no credentials, and every
//! load gives up after 10 seconds (`AbortSignal.timeout(10_000)`).

use futures::future::BoxFuture;
use gpui_kit::{App, Global};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Largest response read; API pages are far smaller.
const MAX_BODY: usize = 16 * 1024 * 1024;
/// What a browser's `fetch` rejects with when the timeout signal fires.
pub const TIMED_OUT: &str = "The operation timed out.";
/// What a browser's `fetch` rejects with when the network fails.
const LOAD_FAILED: &str = "Load failed";

pub const FULL_JSON: &str = "application/vnd.github.full+json";
pub const JSON: &str = "application/vnd.github+json";

pub struct Response {
    pub status: u16,
    /// The `Link` header, for paging.
    pub link: Option<String>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub type Fetched = Result<Response, String>;

/// One GET. Implemented by the network client, the development fixture
/// directory and tests.
pub trait Fetch: Send + Sync + 'static {
    fn get(&self, url: String, accept: &'static str) -> BoxFuture<'static, Fetched>;
}

struct Network(reqwest::Client);

impl Fetch for Network {
    fn get(&self, url: String, accept: &'static str) -> BoxFuture<'static, Fetched> {
        let client = self.0.clone();
        Box::pin(async move {
            let mut response = client
                .get(&url)
                .header("Accept", accept)
                .send()
                .await
                .map_err(|_| LOAD_FAILED.to_string())?;
            let status = response.status().as_u16();
            let link = response
                .headers()
                .get("link")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| LOAD_FAILED.to_string())?
            {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(LOAD_FAILED.to_string());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Response { status, link, body })
        })
    }
}

/// Development only: `BUZZ_DEV_GITHUB_FIXTURES=<dir>` answers from files
/// instead of the network, so headless runs can show every state. A URL
/// maps to a key, its path and query after `api.github.com/` with every
/// other character as `_`; `<key>.json` is the body, `<key>.status` and
/// `<key>.link` override the status and `Link` header, and `<key>.hang`
/// never answers. Anything else is a 404.
struct Fixtures(PathBuf);

pub fn fixture_key(url: &str) -> String {
    url.trim_start_matches("https://api.github.com/")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

impl Fetch for Fixtures {
    fn get(&self, url: String, _: &'static str) -> BoxFuture<'static, Fetched> {
        let base = self.0.join(fixture_key(&url));
        Box::pin(async move {
            let read = |extension: &str| std::fs::read(base.with_extension(extension)).ok();
            if read("hang").is_some() {
                futures::future::pending::<()>().await;
            }
            let body = read("json");
            let status = read("status")
                .and_then(|s| String::from_utf8(s).ok())
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(if body.is_some() { 200 } else { 404 });
            let link = read("link")
                .and_then(|s| String::from_utf8(s).ok())
                .map(|s| s.trim().to_string());
            Ok(Response {
                status,
                link,
                body: body.unwrap_or_default(),
            })
        })
    }
}

/// The app's GitHub reader.
#[derive(Clone)]
pub struct GitHub {
    fetch: Arc<dyn Fetch>,
    runtime: tokio::runtime::Handle,
}

impl Global for GitHub {}

impl GitHub {
    pub fn global(cx: &mut App) -> Self {
        if let Some(github) = cx.try_global::<Self>() {
            return github.clone();
        }
        let runtime = crate::runtime::Runtime::handle(cx);
        let fetch: Arc<dyn Fetch> = match std::env::var_os("BUZZ_DEV_GITHUB_FIXTURES") {
            Some(dir) => Arc::new(Fixtures(dir.into())),
            None => Arc::new(Network(
                reqwest::Client::builder()
                    // GitHub refuses API requests without one.
                    .user_agent(concat!("buzz-gpui/", env!("CARGO_PKG_VERSION")))
                    .connect_timeout(TIMEOUT)
                    .build()
                    .expect("build the GitHub client"),
            )),
        };
        let github = Self { fetch, runtime };
        cx.set_global(github.clone());
        github
    }

    /// Runs a load on the network runtime under the 10 second limit. The
    /// load stops when the returned future is dropped.
    pub fn run<T: Send + 'static>(
        &self,
        load: impl FnOnce(Arc<dyn Fetch>) -> BoxFuture<'static, Result<T, String>>,
    ) -> impl Future<Output = Result<T, String>> + 'static {
        let load = load(self.fetch.clone());
        // The timer needs the runtime, so it starts inside the task.
        let task = self
            .runtime
            .spawn(async move { tokio::time::timeout(TIMEOUT, load).await });
        let task = AbortOnDrop(task);
        async move {
            match task.await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(TIMED_OUT.to_string()),
                Err(_) => Err(LOAD_FAILED.to_string()),
            }
        }
    }
}

/// Retiring a load aborts its request (`AbortController.abort()`).
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// `encodeURIComponent`.
pub fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    /// Answers from a closure and records every request.
    pub struct Stub {
        pub calls: Mutex<Vec<(String, &'static str)>>,
        answer: Box<dyn Fn(&str) -> Fetched + Send + Sync>,
    }

    impl Stub {
        pub fn new(answer: impl Fn(&str) -> Fetched + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                answer: Box::new(answer),
            })
        }

        pub fn urls(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(url, _)| url.clone())
                .collect()
        }
    }

    impl Fetch for Stub {
        fn get(&self, url: String, accept: &'static str) -> BoxFuture<'static, Fetched> {
            let answer = (self.answer)(&url);
            self.calls.lock().unwrap().push((url, accept));
            Box::pin(async move { answer })
        }
    }

    pub fn json(value: serde_json::Value) -> Fetched {
        Ok(Response {
            status: 200,
            link: None,
            body: value.to_string().into_bytes(),
        })
    }

    pub fn linked(value: serde_json::Value, link: &str) -> Fetched {
        Ok(Response {
            status: 200,
            link: Some(link.to_string()),
            body: value.to_string().into_bytes(),
        })
    }

    pub fn status(status: u16) -> Fetched {
        Ok(Response {
            status,
            link: None,
            body: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(encode_component("abc-1_2.~"), "abc-1_2.~");
        assert_eq!(encode_component("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn maps_urls_to_fixture_keys() {
        assert_eq!(
            fixture_key("https://api.github.com/repos/a/b/pulls/1/reviews?per_page=30&page=2"),
            "repos_a_b_pulls_1_reviews_per_page_30_page_2"
        );
    }
}

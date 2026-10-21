//! Hover cards on web links: the page's site name, title, description
//! and image from its OpenGraph (or plain `<title>`) tags. A page is read
//! only once its card opens, then kept for the session; a failed read is
//! tried again on a later hover.
//!
//! Message links are untrusted, so the reader reaches only the public
//! internet: names resolving to loopback, private, link-local or
//! carrier-grade NAT (Tailscale) addresses are refused, on every redirect
//! as on the first request. Requests carry no cookies or credentials.

use super::super::links::{self, LinkKind};
use buzz_ui::{ActiveTheme, Icon, MaterialStyled, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Pages are read up to `</head>`, and never past this.
const MAX_HTML: usize = 512 * 1024;
const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 400;
/// How long a failed read stays failed before a hover tries it again.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// The card's content width, as the Buzz link cards.
const WIDTH: f32 = 340.;
const IMAGE_HEIGHT: f32 = 160.;

/// What a page says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub site: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// An `https://` image, resolved against the page's final address.
    pub image: Option<String>,
}

impl Page {
    fn is_empty(&self) -> bool {
        self.title.is_none() && self.description.is_none() && self.image.is_none()
    }
}

/// Whether an address is on the public internet.
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                // Unique local fc00::/7 and link-local fe80::/10.
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                // Documentation 2001:db8::/32.
                || (first == 0x2001 && ip.segments()[1] == 0x0db8)
                // NAT64 64:ff9b::/96 can reach anything behind it.
                || (first == 0x0064 && ip.segments()[1] == 0xff9b))
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        // Carrier-grade NAT 100.64.0.0/10, where Tailscale lives.
        || (a == 100 && (b & 0xc0) == 64)
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking 198.18.0.0/15.
        || (a == 198 && (b & 0xfe) == 18)
        || a >= 240)
}

/// An address the reader may request: `http(s)`, no credentials, and an
/// IP-literal host only when it is public. Names are checked as they
/// resolve.
fn allowed(url: &url::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host != "localhost" && !host.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => public(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => public(IpAddr::V6(ip)),
        None => false,
    }
}

/// The system resolver, keeping only public addresses.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| public(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err("not a public address".into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// The app's page reader.
#[derive(Clone)]
struct Reader {
    http: reqwest::Client,
    runtime: tokio::runtime::Handle,
}

impl Global for Reader {}

impl Reader {
    fn global(cx: &mut App) -> Self {
        if let Some(reader) = cx.try_global::<Self>() {
            return reader.clone();
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "Mozilla/5.0 (compatible; buzz-gpui/",
                env!("CARGO_PKG_VERSION"),
                "; link preview)"
            ))
            .connect_timeout(TIMEOUT)
            .dns_resolver(Arc::new(PublicOnly))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if allowed(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .build()
            .expect("build the link preview client");
        let reader = Self {
            http,
            runtime: crate::runtime::Runtime::handle(cx),
        };
        cx.set_global(reader.clone());
        reader
    }

    /// Runs `load` on the network runtime under the time limit.
    fn run<T: Send + 'static>(
        &self,
        load: impl Future<Output = Result<T, String>> + Send + 'static,
    ) -> impl Future<Output = Result<T, SharedString>> + Send + 'static {
        let task = self
            .runtime
            .spawn(async move { tokio::time::timeout(TIMEOUT, load).await });
        async move {
            match task.await {
                Ok(Ok(result)) => result.map_err(SharedString::from),
                Ok(Err(_)) => Err("timed out".into()),
                Err(_) => Err("load failed".into()),
            }
        }
    }
}

/// GETs `url`, keeping at most `limit` bytes; `stop` ends the read early.
async fn get(
    http: &reqwest::Client,
    url: &str,
    accept: &str,
    limit: usize,
    stop: impl Fn(&[u8]) -> bool,
) -> Result<(reqwest::Response, Vec<u8>), String> {
    let parsed = url::Url::parse(url).map_err(|_| "not a URL".to_string())?;
    if !allowed(&parsed) {
        return Err("not a public address".into());
    }
    let mut response = http
        .get(parsed)
        .header("Accept", accept)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        let room = limit - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= limit || stop(&body) {
            break;
        }
    }
    Ok((response, body))
}

async fn load_page(http: reqwest::Client, url: String) -> Result<Page, String> {
    let (response, body) = get(
        &http,
        &url,
        "text/html,application/xhtml+xml",
        MAX_HTML,
        |body| contains_ignore_case(body, b"</head>"),
    )
    .await?;
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase())
        .is_none_or(|value| value.contains("html"));
    if !html {
        return Err("not a page".into());
    }
    let page = parse(&String::from_utf8_lossy(&body), response.url());
    if page.is_empty() {
        return Err("nothing to show".into());
    }
    Ok(page)
}

async fn load_image(http: reqwest::Client, url: String) -> Result<Arc<Image>, String> {
    let (_, bytes) = get(&http, &url, "image/*", MAX_IMAGE + 1, |_| false).await?;
    if bytes.len() > MAX_IMAGE {
        return Err("image too large".into());
    }
    match super::super::media::image_format(&bytes) {
        Some(ImageFormat::Svg) | None => Err("not a supported image".into()),
        Some(format) => Ok(Arc::new(Image::from_bytes(format, bytes))),
    }
}

fn contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Collapses runs of whitespace and cuts to `max` characters.
fn tidy(text: &str, max: usize) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= max {
        return Some(text);
    }
    let cut: String = text.chars().take(max).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// Reads a page's preview from its `<head>`: OpenGraph first, then
/// Twitter's tags, then `<title>` and `<meta name=description>`.
pub fn parse(html: &str, base: &url::Url) -> Page {
    let dom = html5ever::parse_document(RcDom::default(), Default::default()).one(html);
    let mut meta: Vec<(String, String)> = Vec::new();
    let mut title: Option<String> = None;
    // The DOM stays alive through the walk: dropping a node empties its
    // descendants' child lists, even those still held here.
    let mut pending: Vec<Handle> = vec![dom.document.clone()];
    while let Some(node) = pending.pop() {
        if let NodeData::Element { name, attrs, .. } = &node.data {
            let attrs = attrs.borrow();
            let attr = |key: &str| {
                attrs
                    .iter()
                    .find(|a| a.name.local.as_ref().eq_ignore_ascii_case(key))
                    .map(|a| a.value.to_string())
            };
            match name.local.as_ref() {
                "meta" => {
                    if let (Some(key), Some(content)) =
                        (attr("property").or_else(|| attr("name")), attr("content"))
                    {
                        meta.push((key.to_ascii_lowercase(), content));
                    }
                }
                "title" if title.is_none() => {
                    let mut text = String::new();
                    for child in node.children.borrow().iter() {
                        if let NodeData::Text { contents } = &child.data {
                            text.push_str(&contents.borrow());
                        }
                    }
                    title = Some(text);
                }
                // Nothing a preview reads lives in the body.
                "body" => continue,
                _ => {}
            }
        }
        pending.extend(node.children.borrow().iter().rev().cloned());
    }
    let first = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            meta.iter()
                .find(|(name, value)| name == key && !value.trim().is_empty())
                .map(|(_, value)| value.clone())
        })
    };
    let image = first(&["og:image:secure_url", "og:image", "og:image:url"])
        .or_else(|| first(&["twitter:image", "twitter:image:src"]))
        .and_then(|image| base.join(image.trim()).ok())
        .filter(|image| image.scheme() == "https" && allowed(image))
        .map(String::from);
    Page {
        site: first(&["og:site_name", "application-name"]).and_then(|s| tidy(&s, MAX_TITLE)),
        title: first(&["og:title", "twitter:title"])
            .or(title)
            .and_then(|t| tidy(&t, MAX_TITLE)),
        description: first(&["og:description", "twitter:description", "description"])
            .and_then(|d| tidy(&d, MAX_DESCRIPTION)),
        image,
    }
}

enum WebPage {}

impl Asset for WebPage {
    type Source = SharedString;
    type Output = Result<Arc<Page>, SharedString>;

    #[allow(clippy::manual_async_fn)]
    fn load(
        url: SharedString,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let reader = Reader::global(cx);
        async move {
            let load = load_page(reader.http.clone(), url.to_string());
            let page = reader.run(load).await;
            if let Err(error) = &page {
                tracing::debug!(%url, %error, "no link preview");
            }
            page.map(Arc::new)
        }
    }
}

enum WebImage {}

impl Asset for WebImage {
    type Source = SharedString;
    type Output = Result<Arc<Image>, SharedString>;

    #[allow(clippy::manual_async_fn)]
    fn load(
        url: SharedString,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let reader = Reader::global(cx);
        async move {
            let load = load_image(reader.http.clone(), url.to_string());
            reader.run(load).await
        }
    }
}

/// Failed reads waiting to be forgotten, by asset and source.
#[derive(Default)]
struct Retrying(Arc<Mutex<HashSet<(&'static str, String)>>>);

impl Global for Retrying {}

/// Forgets a failed asset after [`RETRY_AFTER`], so a later look reads it
/// again rather than keeping the failure for the session.
fn retry_later<A: Asset>(source: A::Source, key: String, window: &Window, cx: &mut App)
where
    A::Source: Send + 'static,
{
    let entry = (std::any::type_name::<A>(), key);
    let set = cx.default_global::<Retrying>().0.clone();
    if !set.lock().expect("retrying").insert(entry.clone()) {
        return;
    }
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(RETRY_AFTER).await;
            let _ = cx.update(|_, cx| cx.remove_asset::<A>(&source));
            set.lock().expect("retrying").remove(&entry);
        })
        .detach();
}

/// The page's image, `None` while it loads or when it fails.
fn image_source(url: SharedString) -> ImageSource {
    ImageSource::Custom(Arc::new(move |window, cx| {
        match window.use_asset::<WebImage>(&url, cx)? {
            Ok(image) => image.use_render_image(window, cx).map(Ok),
            Err(error) => {
                retry_later::<WebImage>(url.clone(), url.to_string(), window, cx);
                Some(Err(ImageCacheError::Asset(error)))
            }
        }
    }))
}

/// A card's surface; a click opens the link as clicking it does.
fn frame(
    id: SharedString,
    open: impl Fn(Modifiers, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let c = *cx.colors();
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(8.))
        .w(px(WIDTH))
        .max_h(px(360.))
        .overflow_hidden()
        .p(px(12.))
        .floating_surface(&c)
        .rounded(px(16.))
        .text_body()
        .text_color(c.text_standard)
        .cursor_pointer()
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            open(event.modifiers(), window, cx)
        })
}

/// The host a card names when the page names no site.
fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|h| h.trim_start_matches("www.").to_string())
        })
        .unwrap_or_else(|| url.to_string())
}

/// The card's top line: the link's service icon and the site's name.
fn site_line(url: &str, site: Option<&str>, cx: &App) -> Div {
    let c = cx.colors();
    let kind = links::link_kind(url).unwrap_or(LinkKind::Web);
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_body_sm()
        .text_color(c.text_subtle)
        .child(Icon::new(links::icon(kind, false)).size(13.))
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(site.map_or_else(|| host(url), str::to_string)),
        )
}

/// The card for a web link, reading the page the first time it shows;
/// `open` opens the link, as clicking it does.
pub fn card(
    id: SharedString,
    url: &str,
    open: impl Fn(Modifiers, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let c = *cx.colors();
    let source = SharedString::from(url.to_string());
    let page = window.use_asset::<WebPage>(&source, cx);
    if let Some(Err(_)) = &page {
        retry_later::<WebPage>(source.clone(), url.to_string(), window, cx);
    }
    let frame = frame(id, open, cx);
    let address = div()
        .text_body_sm()
        .text_color(c.text_subtle)
        .line_clamp(2)
        .text_ellipsis()
        .child(url.to_string());
    match page {
        None => frame
            .child(site_line(url, None, cx))
            .child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("Loading preview…"),
            )
            .into_any_element(),
        // No preview: the card is the address, as the tooltip was.
        Some(Err(_)) => frame
            .child(site_line(url, None, cx))
            .child(address)
            .into_any_element(),
        Some(Ok(page)) => frame
            .child(site_line(url, page.site.as_deref(), cx))
            .when_some(page.title.clone(), |d, title| {
                d.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(title),
                )
            })
            .when_some(page.description.clone(), |d, description| {
                d.child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .line_clamp(3)
                        .text_ellipsis()
                        .child(description),
                )
            })
            .when_some(page.image.clone(), |d, image| {
                d.child(
                    img(image_source(image.into()))
                        .w_full()
                        .h(px(IMAGE_HEIGHT))
                        .rounded(px(8.))
                        .object_fit(ObjectFit::Cover)
                        .with_fallback(|| div().into_any_element()),
                )
            })
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn base() -> url::Url {
        url::Url::parse("https://example.com/a/b").unwrap()
    }

    #[test]
    fn reads_opengraph_before_fallbacks() {
        let html = r#"<!doctype html><html><head>
            <title>Plain  title</title>
            <meta name="description" content="Plain description">
            <meta property="og:title" content=" Open
                Graph title ">
            <meta property="og:description" content="OG description">
            <meta property="og:site_name" content="Example">
            <meta property="og:image" content="/card.png">
            </head><body><meta property="og:title" content="ignored"></body></html>"#;
        assert_eq!(
            parse(html, &base()),
            Page {
                site: Some("Example".into()),
                title: Some("Open Graph title".into()),
                description: Some("OG description".into()),
                image: Some("https://example.com/card.png".into()),
            }
        );
    }

    #[test]
    fn falls_back_to_title_and_description() {
        let html = "<title>Just &amp; a title</title>\
            <meta name=description content='Words'>\
            <meta name=twitter:image content='http://example.com/x.png'>";
        let page = parse(html, &base());
        assert_eq!(page.title.as_deref(), Some("Just & a title"));
        assert_eq!(page.description.as_deref(), Some("Words"));
        // Only https images load.
        assert_eq!(page.image, None);
        assert_eq!(page.site, None);
    }

    #[test]
    fn cuts_long_text() {
        let long = "word ".repeat(200);
        let html = format!("<meta property=og:description content='{long}'>");
        let description = parse(&html, &base()).description.unwrap();
        assert!(description.ends_with('…'));
        assert!(description.chars().count() <= MAX_DESCRIPTION + 1);
    }

    #[test]
    fn refuses_addresses_off_the_public_internet() {
        for url in [
            "http://localhost:8080/",
            "http://app.localhost/",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.100.100.100/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:127.0.0.1]/",
            "https://user:pass@example.com/",
            "ftp://example.com/",
        ] {
            assert!(!allowed(&url::Url::parse(url).unwrap()), "{url}");
        }
        for url in [
            "https://example.com/",
            "http://93.184.216.34/",
            "https://[2606:4700::1111]/",
        ] {
            assert!(allowed(&url::Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn names_the_host_without_www() {
        assert_eq!(host("https://www.example.com/x"), "example.com");
        assert_eq!(host("https://docs.rs/"), "docs.rs");
    }
}

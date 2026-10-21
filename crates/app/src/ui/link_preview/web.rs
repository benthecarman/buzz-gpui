//! Web link embeds under messages: the page's site name, title,
//! description, image and theme colour from its OpenGraph (or plain
//! `<title>`) tags. A page is read when its message first shows, then kept
//! for the session; a failed read is tried once more a little later.
//!
//! Message links are untrusted, so the reader reaches only the public
//! internet: names resolving to loopback, private, link-local or
//! carrier-grade NAT (Tailscale) addresses are refused, on every redirect
//! as on the first request. Requests carry no cookies or credentials.

use buzz_ui::{ActiveTheme, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Pages are read up to `</head>`, and never past this.
const MAX_HTML: usize = 512 * 1024;
const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 400;
/// How long a failed read stays failed before it is tried once more.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Reads running at once, so a channel full of links queues its pages.
const MAX_READS: usize = 4;
/// An embed's width at most, with its bar and padding, and its large
/// image's width inside that.
const MAX_WIDTH: f32 = 432.;
const IMAGE_WIDTH: f32 = 400.;
const THUMBNAIL: f32 = 80.;

/// What a page says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub site: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// An `https://` image, resolved against the page's final address.
    pub image: Option<String>,
    /// Whether the image is a large one under the text (the default)
    /// rather than a thumbnail beside it (`twitter:card` `summary`).
    pub large: bool,
    /// The page's `theme-color`, as `0xRRGGBB`.
    pub color: Option<u32>,
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
    reads: Arc<tokio::sync::Semaphore>,
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
            reads: Arc::new(tokio::sync::Semaphore::new(MAX_READS)),
        };
        cx.set_global(reader.clone());
        reader
    }

    /// Runs `load` on the network runtime, at most [`MAX_READS`] at once,
    /// under the time limit.
    fn run<T: Send + 'static>(
        &self,
        load: impl Future<Output = Result<T, String>> + Send + 'static,
    ) -> impl Future<Output = Result<T, SharedString>> + Send + 'static {
        let reads = self.reads.clone();
        let task = self.runtime.spawn(async move {
            let _read = reads.acquire_owned().await;
            tokio::time::timeout(TIMEOUT, load).await
        });
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
        large: first(&["twitter:card"]).is_none_or(|card| card.trim() != "summary"),
        color: first(&["theme-color"]).and_then(|color| hex_color(&color)),
    }
}

/// `#rgb` or `#rrggbb` as `0xRRGGBB`.
fn hex_color(value: &str) -> Option<u32> {
    let hex = value.trim().strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => {
            let doubled: String = hex.chars().flat_map(|c| [c, c]).collect();
            u32::from_str_radix(&doubled, 16).ok()
        }
        _ => None,
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

/// Failed reads already tried again, by asset and source.
#[derive(Default)]
struct Retried(Arc<Mutex<HashSet<(&'static str, String)>>>);

impl Global for Retried {}

/// Forgets a failed asset after [`RETRY_AFTER`], so the next draw reads
/// it again; a second failure is kept for the session.
fn retry_later<A: Asset>(source: A::Source, key: String, window: &Window, cx: &mut App)
where
    A::Source: Send + 'static,
{
    let entry = (std::any::type_name::<A>(), key);
    let set = cx.default_global::<Retried>().0.clone();
    if !set.lock().expect("retried").insert(entry) {
        return;
    }
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(RETRY_AFTER).await;
            let _ = cx.update(|_, cx| cx.remove_asset::<A>(&source));
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

/// The host an embed names when the page names no site.
fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|h| h.trim_start_matches("www.").to_string())
        })
        .unwrap_or_else(|| url.to_string())
}

type Open = Rc<dyn Fn(Modifiers, &mut Window, &mut App)>;

/// A web link's preview under its message: a bar in the page's theme
/// colour beside its site, linked title, description and image. Nothing
/// shows while the page loads or when it has nothing to show.
#[derive(IntoElement)]
pub struct Embed {
    id: SharedString,
    url: String,
    open: Open,
}

impl Embed {
    /// `open` opens the link, as clicking it in the body does.
    pub fn new(
        id: SharedString,
        url: String,
        open: impl Fn(Modifiers, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            url,
            open: Rc::new(open),
        }
    }
}

impl RenderOnce for Embed {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let source = SharedString::from(self.url.clone());
        let page = match window.use_asset::<WebPage>(&source, cx) {
            Some(Ok(page)) => page,
            Some(Err(_)) => {
                retry_later::<WebPage>(source, self.url.clone(), window, cx);
                return div().into_any_element();
            }
            None => return div().into_any_element(),
        };
        let c = *cx.colors();
        let bar = page
            .color
            .map_or(c.border_prominent, |color| rgb(color).into());
        let clickable = |id: &str, open: &Open| {
            let open = open.clone();
            div()
                .id(SharedString::from(format!("{}-{id}", self.id)))
                .cursor_pointer()
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    open(event.modifiers(), window, cx)
                })
        };
        let image = page.image.clone().map(|image| {
            img(image_source(image.into()))
                .rounded(px(4.))
                .object_fit(ObjectFit::Cover)
                .with_fallback(|| div().into_any_element())
        });
        let (thumbnail, large) = match image {
            Some(image) if page.large => (None, Some(image)),
            image => (image, None),
        };
        let text = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .text_caption()
                    .text_color(c.text_subtle)
                    .truncate()
                    .child(page.site.clone().unwrap_or_else(|| host(&self.url))),
            )
            .when_some(page.title.clone(), |d, title| {
                d.child(
                    clickable("title", &self.open)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c.text_link)
                        .hover(|s| s.underline())
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(title),
                )
            })
            .when_some(page.description.clone(), |d, description| {
                d.child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_standard)
                        .line_clamp(4)
                        .text_ellipsis()
                        .child(description),
                )
            });
        div()
            .id(self.id.clone())
            .mt(px(6.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .max_w(px(MAX_WIDTH))
            .pt(px(8.))
            .pr(px(16.))
            .pb(px(16.))
            .pl(px(12.))
            .rounded(px(4.))
            .border_l_4()
            .border_color(bar)
            .bg(c.surface_inset)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(16.))
                    .child(text)
                    .when_some(thumbnail, |d, thumbnail| {
                        d.child(
                            clickable("thumbnail", &self.open)
                                .flex_none()
                                .child(thumbnail.size(px(THUMBNAIL))),
                        )
                    }),
            )
            .when_some(large, |d, image| {
                d.child(
                    clickable("image", &self.open)
                        .max_w_full()
                        .child(image.w(px(IMAGE_WIDTH)).max_w_full().max_h(px(300.))),
                )
            })
            .into_any_element()
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
                large: true,
                color: None,
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
    fn reads_the_card_kind_and_theme_colour() {
        let html = "<meta name=twitter:card content=summary>\
            <meta name=theme-color content='#1e2327'>\
            <meta property=og:title content=T>";
        let page = parse(html, &base());
        assert!(!page.large);
        assert_eq!(page.color, Some(0x1e2327));
        let page = parse(
            "<meta name=theme-color content='#fa0'><title>T</title>",
            &base(),
        );
        assert!(page.large);
        assert_eq!(page.color, Some(0xffaa00));
        assert_eq!(hex_color("red"), None);
        assert_eq!(hex_color("#12345"), None);
    }

    #[test]
    fn names_the_host_without_www() {
        assert_eq!(host("https://www.example.com/x"), "example.com");
        assert_eq!(host("https://docs.rs/"), "docs.rs");
    }
}

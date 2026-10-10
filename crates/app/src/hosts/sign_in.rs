//! Browser sign-in for a host on another machine.
//!
//! A host's sign-in page (Databricks, for Buzz Agent's model catalog)
//! redirects the browser to a loopback port, and the host listens there.
//! When the host is remote, that port is on the wrong machine. So the GUI
//! listens on the same port here, takes the browser's callback, and
//! forwards it to the host, which replays it on its own listener and sends
//! back the page to show.

use buzz_host::api::{Request, Response, SignInPage};
use buzz_host_rpc::HostClient;
use std::sync::Weak;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// buzz-agent's own sign-in timeout.
const WAIT: Duration = Duration::from_secs(300);
const MAX_HEAD: usize = 16 * 1024;

type Client = HostClient<buzz_host::api::HostProtocol>;

/// The loopback port the page redirects to.
fn redirect_port(url: &str) -> Option<u16> {
    let url = url::Url::parse(url).ok()?;
    let redirect = url
        .query_pairs()
        .find(|(name, _)| name == "redirect_uri")?
        .1;
    let redirect = url::Url::parse(&redirect).ok()?;
    matches!(redirect.host_str(), Some("localhost" | "127.0.0.1"))
        .then(|| redirect.port())
        .flatten()
}

/// Starts listening on the page's loopback port. Fails when the port is
/// taken here, so the caller does not open a page whose callback would be
/// lost.
pub async fn relay(url: &str, client: Weak<Client>) -> Result<(), String> {
    let port = redirect_port(url).ok_or("This sign-in page has no loopback redirect")?;
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|_| {
        format!("Port {port} is in use on this computer; close what uses it and retry")
    })?;
    tokio::spawn(async move {
        let _ = tokio::time::timeout(WAIT, serve(listener, port, client)).await;
    });
    Ok(())
}

/// Answers browser requests until one has been forwarded to the host.
async fn serve(listener: TcpListener, port: u16, client: Weak<Client>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        if forward(stream, port, &client).await {
            return;
        }
    }
}

/// Handles one request; true once the callback reached the host.
async fn forward(mut stream: TcpStream, port: u16, client: &Weak<Client>) -> bool {
    let Some(target) = read_target(&mut stream).await else {
        let _ = respond(&mut stream, &page(400, "Bad request")).await;
        return false;
    };
    // Browsers also ask for an icon; only the callback goes to the host.
    if target == "/favicon.ico" {
        let _ = respond(&mut stream, &page(404, "Not found")).await;
        return false;
    }
    let Some(client) = client.upgrade() else {
        let _ = respond(
            &mut stream,
            &page(502, "Buzz is no longer connected to the host."),
        )
        .await;
        return true;
    };
    let answer = client
        .request(Request::SignInCallback { port, target })
        .await;
    let shown = match answer {
        Ok(Response::SignInPage(page)) => page,
        Ok(_) => page(502, "The host sent an unexpected reply."),
        Err(error) => page(
            502,
            &format!("The host could not finish signing in: {error}"),
        ),
    };
    let _ = respond(&mut stream, &shown).await;
    true
}

/// The request target of a `GET`, from its request line.
async fn read_target(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut buffer = [0; 2048];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .ok()?
            .ok()?;
        if read == 0 || head.len() + read > MAX_HEAD {
            return None;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let line = std::str::from_utf8(&head).ok()?.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target) = (parts.next()?, parts.next()?);
    (method == "GET" && target.starts_with('/') && !target.starts_with("//"))
        .then(|| target.to_string())
}

fn page(status: u16, text: &str) -> SignInPage {
    SignInPage {
        status,
        content_type: "text/plain; charset=utf-8".into(),
        body: text.into(),
    }
}

async fn respond(stream: &mut TcpStream, page: &SignInPage) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        page.status,
        if page.status < 400 { "OK" } else { "Error" },
        page.content_type.replace(['\r', '\n'], ""),
        page.body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(page.body.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_redirects_are_relayed() {
        let page = |redirect: &str| {
            format!(
                "https://dbc.example/authorize?redirect_uri={}",
                url::form_urlencoded::byte_serialize(redirect.as_bytes()).collect::<String>()
            )
        };
        assert_eq!(redirect_port(&page("http://localhost:41234")), Some(41234));
        assert_eq!(redirect_port(&page("http://example.com:41234")), None);
    }

    #[tokio::test]
    async fn the_request_target_comes_from_a_get_line() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(b"GET /?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).await.unwrap();
            answer
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(
            read_target(&mut stream).await.as_deref(),
            Some("/?code=abc&state=xyz")
        );
        respond(&mut stream, &page(200, "Signed in")).await.unwrap();
        let answer = client.await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 200 OK"));
        assert!(answer.ends_with("Signed in"));
    }
}

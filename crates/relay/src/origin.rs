//! A community is identified by its relay origin. The same origin is reached
//! over WebSocket for live traffic and HTTPS for finite reads.

use std::fmt;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelayOrigin {
    /// `wss://host[:port]` (or `ws://` for local development).
    websocket: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct OriginError(&'static str);

impl RelayOrigin {
    /// Accepts `wss://`, `ws://`, `https://` or `http://` origins. Paths,
    /// queries, fragments and credentials are refused rather than dropped,
    /// so a typo never silently names a different relay.
    pub fn parse(input: &str) -> Result<Self, OriginError> {
        let url = Url::parse(input.trim()).map_err(|_| OriginError("Enter a relay URL"))?;
        let secure = match url.scheme() {
            "wss" | "https" => true,
            "ws" | "http" => false,
            _ => return Err(OriginError("Relay URLs start with wss:// or https://")),
        };
        if !url.username().is_empty() || url.password().is_some() {
            return Err(OriginError("Relay URLs cannot contain credentials"));
        }
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err(OriginError("Enter only the relay origin, without a path"));
        }
        let host = url.host_str().ok_or(OriginError("Relay URL has no host"))?;
        let host = host.to_ascii_lowercase();
        let scheme = if secure { "wss" } else { "ws" };
        let websocket = match url.port() {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        };
        Ok(Self { websocket })
    }

    pub fn websocket(&self) -> &str {
        &self.websocket
    }

    /// The HTTP origin, without a trailing slash.
    pub fn http(&self) -> String {
        match self.websocket.strip_prefix("wss://") {
            Some(rest) => format!("https://{rest}"),
            None => format!("http://{}", &self.websocket["ws://".len()..]),
        }
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.http())
    }
}

impl fmt::Display for RelayOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.websocket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_wss_name_the_same_origin() {
        let a = RelayOrigin::parse("https://Relay.Example.com/").unwrap();
        let b = RelayOrigin::parse("wss://relay.example.com").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.http(), "https://relay.example.com");
        assert_eq!(a.endpoint("/query"), "https://relay.example.com/query");
        let local = RelayOrigin::parse("ws://127.0.0.1:53000").unwrap();
        assert_eq!(local.http(), "http://127.0.0.1:53000");
    }

    #[test]
    fn refuses_anything_but_an_origin() {
        for bad in [
            "relay.example.com",
            "ftp://relay.example.com",
            "wss://relay.example.com/path",
            "wss://relay.example.com?x=1",
            "wss://user:pw@relay.example.com",
        ] {
            assert!(RelayOrigin::parse(bad).is_err(), "{bad}");
        }
    }
}

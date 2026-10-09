use crate::transport::BoxConnection;
use crate::{Message, Protocol, RpcError};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::io;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Bumped on any change the other side cannot ignore. Adding optional fields
/// or new enum variants that old peers never receive does not need a bump.
pub const PROTOCOL_VERSION: u32 = 1;

const MAX_FRAME: usize = 16 << 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub token: Option<String>,
    pub client: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Stable for the lifetime of the host's state directory.
    pub id: String,
    pub name: String,
    pub platform: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    pub host: HostInfo,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub(crate) enum Frame<P: Protocol> {
    Hello(Hello),
    Welcome(Welcome),
    Rejected {
        reason: String,
    },
    ClientRequest {
        id: u64,
        body: P::ClientRequest,
    },
    ClientResponse {
        id: u64,
        body: Result<P::ClientResponse, RpcError>,
    },
    HostRequest {
        id: u64,
        body: P::HostRequest,
    },
    HostResponse {
        id: u64,
        body: Result<P::HostResponse, RpcError>,
    },
    Event {
        seq: u64,
        body: P::Event,
    },
    /// The host dropped events for this client; it must fetch fresh state.
    Resync,
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
}

pub(crate) type Wire = Framed<BoxConnection, LengthDelimitedCodec>;

pub(crate) fn wire(connection: BoxConnection) -> Wire {
    Framed::new(
        connection,
        LengthDelimitedCodec::builder()
            .max_frame_length(MAX_FRAME)
            .new_codec(),
    )
}

pub(crate) fn encode<T: Message>(value: &T) -> io::Result<Bytes> {
    rmp_serde::to_vec_named(value)
        .map(Bytes::from)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) fn decode<T: Message>(bytes: &[u8]) -> io::Result<T> {
    rmp_serde::from_slice(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) async fn send<P: Protocol>(wire: &mut Wire, frame: &Frame<P>) -> io::Result<()> {
    wire.send(encode(frame)?).await
}

pub(crate) async fn receive<P: Protocol>(wire: &mut Wire) -> io::Result<Frame<P>> {
    match wire.next().await {
        Some(bytes) => decode(&bytes?),
        None => Err(io::ErrorKind::UnexpectedEof.into()),
    }
}

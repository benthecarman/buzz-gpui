//! One running app per profile. A second launch hands its `buzz://` URL
//! (or nothing, just to be focused) and its activation token to the
//! running instance over a Unix socket and exits.
//!
//! The socket is keyed by the data directory, so separate profiles (and
//! development runs with their own `XDG_DATA_HOME`) never capture each
//! other's launches. It lives in `$XDG_RUNTIME_DIR`, which only the user
//! can enter; without one it falls back to the data directory.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest message accepted: a URL as long as the link parser takes.
const MAX_MESSAGE: u64 = 32_768;
const TIMEOUT: Duration = Duration::from_secs(2);

/// What a second launch hands over: a line with the link, then a line
/// with the activation token, if any. Instances that predate the token
/// read the first line only.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Handoff {
    /// Empty to only bring the window to the front.
    pub link: String,
    pub activation_token: Option<String>,
}

pub enum Claim {
    /// This is the running instance; serve the socket.
    Primary(UnixListener),
    /// Another instance took the message.
    Forwarded,
}

/// The socket for the profile stored in `data_dir`.
pub fn socket_path(data_dir: &Path) -> PathBuf {
    let base = dirs::runtime_dir().unwrap_or_else(|| data_dir.to_path_buf());
    base.join(format!(
        "buzz-gpui-{:016x}.sock",
        fnv1a(data_dir.as_os_str().as_encoded_bytes())
    ))
}

/// A hash that stays the same across builds, so differently built copies
/// still find each other.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Becomes the running instance, or hands `handoff` to the one that is.
pub fn claim(path: &Path, handoff: &Handoff) -> io::Result<Claim> {
    let mut message = single_line(&handoff.link);
    if let Some(token) = &handoff.activation_token {
        message.push('\n');
        message.push_str(&single_line(token));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _ in 0..2 {
        match UnixListener::bind(path) {
            Ok(listener) => return Ok(Claim::Primary(listener)),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                match UnixStream::connect(path) {
                    Ok(mut stream) => {
                        stream.set_write_timeout(Some(TIMEOUT))?;
                        stream.write_all(message.as_bytes())?;
                        stream.write_all(b"\n")?;
                        return Ok(Claim::Forwarded);
                    }
                    // Left behind by an instance that exited.
                    Err(_) => std::fs::remove_file(path)?,
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("the instance socket stayed in use"))
}

/// A field may not break the line-based message.
fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], "")
}

/// Passes each handoff from later launches to `deliver`, on a thread of
/// its own, for as long as the process runs.
pub fn serve(listener: UnixListener, deliver: impl Fn(Handoff) + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("buzz-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream.and_then(read_message) {
                    Ok(message) => deliver(message),
                    Err(error) => tracing::warn!(%error, "could not read from a second launch"),
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not listen for second launches");
    }
}

fn read_message(stream: UnixStream) -> io::Result<Handoff> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut reader = BufReader::new(stream.take(MAX_MESSAGE + 1));
    let mut link = String::new();
    reader.read_line(&mut link)?;
    let mut token = String::new();
    reader.read_line(&mut token)?;
    if (link.len() + token.len()) as u64 > MAX_MESSAGE {
        return Err(io::Error::other("message too long"));
    }
    let token = token.trim_end_matches(['\r', '\n']);
    Ok(Handoff {
        link: link.trim_end_matches(['\r', '\n']).to_string(),
        activation_token: (!token.is_empty()).then(|| token.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;
    use std::sync::mpsc;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "buzz-instance-{}-{name}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("s.sock")
    }

    #[test]
    fn second_claim_forwards_to_the_first() {
        let path = scratch("forward");
        let Claim::Primary(listener) = claim(&path, &Handoff::default()).unwrap() else {
            panic!("first claim should be primary");
        };
        let (sender, received) = mpsc::channel();
        serve(listener, move |message| sender.send(message).unwrap());
        let link = Handoff {
            link: "buzz://channel/general".into(),
            activation_token: Some("token\nwith a break".into()),
        };
        assert!(matches!(claim(&path, &link).unwrap(), Claim::Forwarded));
        assert!(matches!(
            claim(&path, &Handoff::default()).unwrap(),
            Claim::Forwarded
        ));
        let wait = Duration::from_secs(5);
        assert_eq!(
            received.recv_timeout(wait).unwrap(),
            Handoff {
                link: "buzz://channel/general".into(),
                activation_token: Some("tokenwith a break".into()),
            }
        );
        assert_eq!(received.recv_timeout(wait).unwrap(), Handoff::default());
    }

    #[test]
    fn reads_handoffs_without_a_token() {
        let path = scratch("legacy");
        let Claim::Primary(listener) = claim(&path, &Handoff::default()).unwrap() else {
            panic!("first claim should be primary");
        };
        let (sender, received) = mpsc::channel();
        serve(listener, move |message| sender.send(message).unwrap());
        // What an older launch writes.
        let mut stream = UnixStream::connect(&path).unwrap();
        stream.write_all(b"buzz://channel/general\n").unwrap();
        drop(stream);
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            Handoff {
                link: "buzz://channel/general".into(),
                activation_token: None,
            }
        );
    }

    #[test]
    fn replaces_a_stale_socket() {
        let path = scratch("stale");
        // A datagram socket leaves the same file and always refuses a
        // stream connection. A dropped listener would not: a process
        // another test spawns at that moment holds a copy of it until it
        // execs, and the connection gets through.
        drop(UnixDatagram::bind(&path).unwrap());
        assert!(path.exists());
        assert!(matches!(
            claim(&path, &Handoff::default()).unwrap(),
            Claim::Primary(_)
        ));
    }

    #[test]
    fn profiles_get_their_own_socket() {
        let a = socket_path(Path::new("/home/a/.local/share/buzz-gpui"));
        let b = socket_path(Path::new("/tmp/t/data/buzz-gpui"));
        assert_ne!(a, b);
        assert_eq!(a, socket_path(Path::new("/home/a/.local/share/buzz-gpui")));
        assert!(a.to_string_lossy().ends_with(".sock"));
    }
}

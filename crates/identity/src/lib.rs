//! The human's Buzz identity.
//!
//! The key is the same OS keyring entry buzz-app uses, so both apps share
//! one identity. Nothing outside this crate sees the secret key. Callers
//! ask for a specific purpose (relay auth, an HTTP request, a chat event)
//! rather than "sign this", so a bug elsewhere cannot mint HTTP or media
//! tokens or forge kinds the app never writes.

use base64::Engine;
use buzz_credential_store::Error as StoreError;
use nostr::nips::nip19::ToBech32;
use nostr::nips::nip44;
use nostr::prelude::{
    Event, EventBuilder, FinalizeEvent, Keys, Kind, PublicKey, SecretKey, SignEvent, Tag,
    Timestamp, UnsignedEvent,
};
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::Zeroizing;

/// buzz-app's release keyring entry. Debug builds of buzz-app use a
/// `.debug` service; this app always uses the release one so it shares the
/// identity the user actually signs in with.
pub const SERVICE: &str = "dev.local.buzz.foundation.identity";
pub const ACCOUNT: &str = "human";

/// Largest event this app signs, as serialized JSON.
pub const MAX_EVENT_BYTES: usize = 64 * 1024;

/// Storage messages are buzz-app's (`src-tauri/src/identity.rs`), so both
/// apps explain the shared keyring the same way.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("Secure storage access was denied. Allow access and retry; no identity was created.")]
    Denied,
    #[error("Another Buzz app is accessing secure storage. Retry shortly.")]
    Busy,
    #[error(
        "Your identity could not be accessed in secure storage. Unlock your credential store \
         and retry without changing keys."
    )]
    Unavailable,
    #[error(
        "Saved identity is malformed; nothing was changed. Keep your key backup and contact \
         support before changing secure storage."
    )]
    Malformed,
    /// A key was found before saving, so nothing was generated or written.
    #[error("An identity is already saved; nothing was overwritten")]
    AlreadySaved,
    /// Another process saved a key between the read and the write.
    #[error("An identity is already saved; nothing was overwritten. Restart to restore it.")]
    Occupied,
    /// The key was deleted, but a fresh read still finds it.
    #[error("Your key is still saved in secure storage")]
    StillSaved,
    #[error("Enter a valid nsec private key")]
    InvalidKey,
    #[error("This app does not sign that kind of event.")]
    NotAllowed,
    #[error("The event is too large to sign.")]
    TooLarge,
    #[error("Could not sign: {0}")]
    Signing(String),
    #[error("Could not decrypt.")]
    Decrypt,
}

fn store_error(error: StoreError) -> IdentityError {
    match error {
        StoreError::Denied => IdentityError::Denied,
        StoreError::Busy => IdentityError::Busy,
        StoreError::Corrupt => IdentityError::Malformed,
        StoreError::Occupied => IdentityError::Occupied,
        StoreError::Absent | StoreError::Unavailable => IdentityError::Unavailable,
    }
}

pub struct Identity {
    keys: Keys,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("pubkey", &self.keys.public_key().to_hex())
            .finish_non_exhaustive()
    }
}

/// Reads the saved identity. `Ok(None)` means none is saved; errors never
/// lead to generating a key.
pub fn load() -> Result<Option<Identity>, IdentityError> {
    match buzz_credential_store::read(SERVICE, ACCOUNT) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|_| IdentityError::Malformed)?;
            Identity::from_nsec(text)
                .map(Some)
                .map_err(|_| IdentityError::Malformed)
        }
        Err(StoreError::Absent) => Ok(None),
        Err(error) => Err(store_error(error)),
    }
}

/// Saves a new random identity. Refuses if one is already saved.
pub fn create() -> Result<Identity, IdentityError> {
    ensure_absent()?;
    save(Identity {
        keys: Keys::generate(),
    })
}

/// Saves an imported `nsec`. Refuses if one is already saved.
pub fn import(nsec: &str) -> Result<Identity, IdentityError> {
    ensure_absent()?;
    save(Identity::from_nsec(nsec)?)
}

/// buzz-app restores before it saves: a saved key, or one that can't be
/// read, stops the save before a key is generated or written.
fn ensure_absent() -> Result<(), IdentityError> {
    match load()? {
        Some(_) => Err(IdentityError::AlreadySaved),
        None => Ok(()),
    }
}

/// Deletes the saved identity (sign out), then confirms a fresh read finds
/// nothing. buzz-app shares this entry, so it is signed out too.
pub fn forget() -> Result<(), IdentityError> {
    match buzz_credential_store::delete(SERVICE, ACCOUNT) {
        Ok(()) | Err(StoreError::Absent) => {}
        Err(error) => return Err(store_error(error)),
    }
    match buzz_credential_store::read(SERVICE, ACCOUNT) {
        Err(StoreError::Absent) => Ok(()),
        Ok(_) => Err(IdentityError::StillSaved),
        Err(error) => Err(store_error(error)),
    }
}

fn save(identity: Identity) -> Result<Identity, IdentityError> {
    let nsec = identity.nsec();
    buzz_credential_store::add(SERVICE, ACCOUNT, nsec.as_bytes()).map_err(store_error)?;
    // Commit in memory only once storage confirms.
    Ok(identity)
}

/// What a signed event is for. Each purpose fixes the kinds it may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Chat, reactions, edits, deletions, reports, profile, status, emoji,
    /// channel membership and community requests.
    Content,
    /// NIP-44 self-encrypted app data (read state, sidebar preferences).
    AppData,
    /// Presence over the live socket.
    Presence,
}

/// Kinds the app writes as ordinary content.
const CONTENT_KINDS: &[u16] = &[
    0, 5, 7, 9, 1984, 9000, 9001, 9002, 9007, 9008, 9021, 9022, 9030, 9031, 9032, 9035, 9036,
    28936, 30030, 30315, 40003, 41010, 41012, 42000,
];

impl Identity {
    /// Parses an `nsec1…` string (hex is not accepted, as in buzz-app).
    pub fn from_nsec(text: &str) -> Result<Self, IdentityError> {
        let text = text.trim();
        if text.len() != 63 || !text.to_ascii_lowercase().starts_with("nsec1") {
            return Err(IdentityError::InvalidKey);
        }
        let secret =
            SecretKey::parse(&text.to_ascii_lowercase()).map_err(|_| IdentityError::InvalidKey)?;
        Ok(Self {
            keys: Keys::new(secret),
        })
    }

    /// A throwaway identity that is never saved.
    pub fn ephemeral() -> Self {
        Self {
            keys: Keys::generate(),
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.keys.public_key()
    }

    pub fn pubkey_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }

    /// For the explicit "export key" action only.
    pub fn nsec(&self) -> Zeroizing<String> {
        Zeroizing::new(
            self.keys
                .secret_key()
                .to_bech32()
                .expect("secret keys encode as nsec"),
        )
    }

    /// Builds the unsigned event, with its final ID, that [`Self::sign`]
    /// will sign. The outbox saves this before signing.
    pub fn template(
        &self,
        purpose: Purpose,
        kind: u16,
        created_at: u64,
        tags: Vec<Vec<String>>,
        content: String,
    ) -> Result<UnsignedEvent, IdentityError> {
        check_kind(purpose, kind)?;
        let tags = tags
            .into_iter()
            .map(|tag| Tag::parse(tag).map_err(|error| IdentityError::Signing(error.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        let mut unsigned = UnsignedEvent::new(
            self.keys.public_key(),
            Timestamp::from_secs(created_at),
            Kind::from_u16(kind),
            tags,
            content,
        );
        unsigned.ensure_id();
        if unsigned.as_json().len() > MAX_EVENT_BYTES {
            return Err(IdentityError::TooLarge);
        }
        Ok(unsigned)
    }

    /// Signs an event built by [`Self::template`], keeping its ID.
    pub fn sign(&self, purpose: Purpose, unsigned: UnsignedEvent) -> Result<Event, IdentityError> {
        check_kind(purpose, unsigned.kind.as_u16())?;
        if unsigned.pubkey != self.keys.public_key() {
            return Err(IdentityError::NotAllowed);
        }
        self.keys
            .sign_event(unsigned)
            .map_err(|error| IdentityError::Signing(error.to_string()))
    }

    /// NIP-42 relay authentication: kind 22242 with exactly the relay URL
    /// (`wss://host`) and the challenge.
    pub fn relay_auth(&self, relay: &str, challenge: &str) -> Result<Event, IdentityError> {
        if challenge.is_empty() || challenge.len() > 4096 || !relay.starts_with("ws") {
            return Err(IdentityError::NotAllowed);
        }
        EventBuilder::new(Kind::Authentication, "")
            .tags([
                Tag::custom("relay", [relay]),
                Tag::custom("challenge", [challenge]),
            ])
            .finalize(&self.keys)
            .map_err(|error| IdentityError::Signing(error.to_string()))
    }

    /// NIP-98 `Authorization` header value for one request. A fresh event
    /// (with a random nonce) every time: the relay refuses reused ones.
    pub fn http_auth(
        &self,
        url: &str,
        method: &str,
        body: Option<&[u8]>,
    ) -> Result<String, IdentityError> {
        let mut tags = vec![Tag::custom("u", [url]), Tag::custom("method", [method])];
        if let Some(body) = body {
            tags.push(Tag::custom(
                "payload",
                [format!("{:x}", Sha256::digest(body))],
            ));
        }
        tags.push(Tag::custom("nonce", [uuid::Uuid::new_v4().to_string()]));
        let event = EventBuilder::new(Kind::HttpAuth, "")
            .tags(tags)
            .finalize(&self.keys)
            .map_err(|error| IdentityError::Signing(error.to_string()))?;
        Ok(header(&event))
    }

    /// Blossom (kind 24242) authorization for one media request, valid for
    /// 60 seconds. `server` is the relay's `host[:port]`.
    pub fn media_auth(
        &self,
        verb: MediaVerb,
        server: &str,
        sha256: Option<&str>,
    ) -> Result<String, IdentityError> {
        let now = Timestamp::now().as_secs();
        let (content, verb) = match verb {
            MediaVerb::Upload => ("Upload attachment", "upload"),
            MediaVerb::Get => ("Get buzz-media", "get"),
        };
        let mut tags = Vec::new();
        if let Some(hash) = sha256 {
            tags.push(Tag::custom("x", [hash]));
        }
        tags.push(Tag::custom("t", [verb]));
        tags.push(Tag::custom("server", [server]));
        tags.push(Tag::custom("expiration", [(now + 60).to_string()]));
        let event = EventBuilder::new(Kind::from_u16(24242), content)
            .tags(tags)
            .finalize(&self.keys)
            .map_err(|error| IdentityError::Signing(error.to_string()))?;
        Ok(header(&event))
    }

    /// Signs a Blossom media authorization that buzz-desktop's upload and
    /// media services built, after checking it is exactly what
    /// [`Self::media_auth`] would sign: kind 24242 with the fixed content of
    /// its verb, an `x` hash only for an upload, one `server`, and an
    /// expiry at most 60 seconds after a current `created_at`. Anything
    /// else is refused, so those services cannot mint other tokens.
    pub fn sign_media_event(
        &self,
        kind: u16,
        created_at: u64,
        tags: &[Vec<String>],
        content: &str,
    ) -> Result<Event, IdentityError> {
        let now = Timestamp::now().as_secs();
        let tag = |name: &str| {
            let mut found = tags
                .iter()
                .filter(|tag| tag.first().map(String::as_str) == Some(name));
            match (found.next(), found.next()) {
                (Some(tag), None) if tag.len() == 2 => Some(tag[1].as_str()),
                _ => None,
            }
        };
        let verb = tag("t");
        let hash = tag("x");
        let hex = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        let shape_ok = match (verb, content) {
            (Some("upload"), "Upload attachment") => hash.is_some_and(hex) && tags.len() == 4,
            (Some("get"), "Get buzz-media") => hash.is_none() && tags.len() == 3,
            _ => false,
        };
        let server_ok = tag("server").is_some_and(|server| {
            !server.is_empty()
                && server.len() <= 255
                && server
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
        });
        let expiry_ok = tag("expiration")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|expiry| expiry > created_at && expiry <= created_at + 60);
        if kind != 24242 || !shape_ok || !server_ok || !expiry_ok || created_at.abs_diff(now) > 300
        {
            return Err(IdentityError::NotAllowed);
        }
        let tags = tags
            .iter()
            .map(|tag| {
                Tag::parse(tag.clone()).map_err(|error| IdentityError::Signing(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        EventBuilder::new(Kind::from_u16(kind), content)
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(created_at))
            .finalize(&self.keys)
            .map_err(|error| IdentityError::Signing(error.to_string()))
    }

    /// NIP-OA owner attestation for an agent key, as the JSON `auth` tag
    /// the agent host stores: `["auth", owner, "", signature]`.
    pub fn attest_agent(&self, agent_pubkey: &str) -> Result<String, IdentityError> {
        if agent_pubkey.len() != 64
            || !agent_pubkey
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            || agent_pubkey == self.pubkey_hex()
        {
            return Err(IdentityError::NotAllowed);
        }
        let digest: [u8; 32] = Sha256::digest(format!("nostr:agent-auth:{agent_pubkey}:")).into();
        let signature = self.keys.sign_schnorr(digest);
        Ok(
            serde_json::to_string(&["auth", &self.pubkey_hex(), "", &signature.to_string()])
                .expect("strings serialize"),
        )
    }

    /// The owner's signed intent to set up `agent_pubkey` in the community
    /// whose relay is `relay_url` (`wss://host`): a schnorr signature over
    /// `sha256("nostr:agent-community:<agent>:<relay>")`, as buzz-app's Use
    /// here sends it to the agent host.
    pub fn resolve_agent_community(
        &self,
        agent_pubkey: &str,
        relay_url: &str,
    ) -> Result<String, IdentityError> {
        let hex = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        if !hex(agent_pubkey)
            || agent_pubkey == self.pubkey_hex()
            || !relay_url.starts_with("wss://")
            || relay_url.ends_with('/')
        {
            return Err(IdentityError::NotAllowed);
        }
        let digest: [u8; 32] =
            Sha256::digest(format!("nostr:agent-community:{agent_pubkey}:{relay_url}")).into();
        Ok(self.keys.sign_schnorr(digest).to_string())
    }

    /// NIP-44 v2 encryption to our own key, for app data only.
    pub fn encrypt_to_self(&self, plaintext: &str) -> Result<String, IdentityError> {
        nip44::encrypt(
            self.keys.secret_key(),
            &self.keys.public_key(),
            plaintext,
            nip44::Version::V2,
        )
        .map_err(|error| IdentityError::Signing(error.to_string()))
    }

    /// Decrypts app data this identity encrypted to itself. The event must
    /// be ours and already verified by the caller.
    pub fn decrypt_from_self(&self, event: &Event) -> Result<String, IdentityError> {
        if event.pubkey != self.keys.public_key() {
            return Err(IdentityError::NotAllowed);
        }
        nip44::decrypt(
            self.keys.secret_key(),
            &self.keys.public_key(),
            &event.content,
        )
        .map_err(|_| IdentityError::Decrypt)
    }

    /// Decrypts an agent's activity frame or turn metrics (kinds 24200 and
    /// 44200), which agents encrypt to their owner. Nothing else is opened
    /// here; the event must be verified by the caller.
    pub fn decrypt_agent_frame(&self, event: &Event) -> Result<String, IdentityError> {
        let ours = self.keys.public_key().to_hex();
        let addressed = event.tags.iter().any(|tag| {
            let tag = tag.as_slice();
            tag.first().map(String::as_str) == Some("p") && tag.get(1) == Some(&ours)
        });
        if !matches!(event.kind.as_u16(), 24200 | 44200) || !addressed {
            return Err(IdentityError::NotAllowed);
        }
        nip44::decrypt(self.keys.secret_key(), &event.pubkey, &event.content)
            .map_err(|_| IdentityError::Decrypt)
    }
}

/// Owner-key decoding of agent records with buzz-app's native decoders
/// (`buzz_desktop::relay::agent`). Each opens only what agents address to
/// this identity; the secret never leaves this crate.
impl Identity {
    fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.keys.secret_key().secret_bytes())
    }

    /// The newest memory per slug an agent wrote to this owner (kind 30174),
    /// from the records of a `memory_query` read: `{entries, partial}`.
    pub fn decode_agent_memories(
        &self,
        agent: &str,
        records: &[Option<serde_json::Value>],
    ) -> Result<serde_json::Value, String> {
        let viewer = self.pubkey_hex();
        buzz_desktop::relay::agent::decode_memories(
            &self.secret_bytes(),
            &viewer,
            &viewer,
            agent,
            records,
        )
    }

    /// The plaintext of an agent's observer frame (kind 24200) or saved
    /// frame (44200). `history` accepts frames up to 90 days old instead of
    /// five minutes: `{id, agent, createdAt, plaintext}`.
    pub fn decode_agent_frame(
        &self,
        event: &buzz_desktop::relay::agent::AgentEvent,
        history: bool,
    ) -> Result<serde_json::Value, String> {
        buzz_desktop::relay::agent::decode_archive(
            &self.secret_bytes(),
            &self.pubkey_hex(),
            event,
            history,
        )
    }

    /// Decrypts a page read from the local activity archive.
    pub fn decode_archive_page(
        &self,
        page: buzz_desktop::archive::Page,
    ) -> Result<serde_json::Value, String> {
        page.decode(&self.secret_bytes(), &self.pubkey_hex())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum MediaVerb {
    Upload,
    Get,
}

fn check_kind(purpose: Purpose, kind: u16) -> Result<(), IdentityError> {
    let allowed = match purpose {
        Purpose::Content => CONTENT_KINDS.contains(&kind),
        Purpose::AppData => kind == 30078,
        Purpose::Presence => kind == 20001,
    };
    if allowed {
        Ok(())
    } else {
        Err(IdentityError::NotAllowed)
    }
}

fn header(event: &Event) -> String {
    let json = serde_json::to_vec(event).expect("events serialize");
    format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD.encode(json)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An observer frame an agent encrypted to its owner opens only for
    /// that owner, through buzz-desktop's decoder.
    #[test]
    fn agent_frames_open_only_for_their_owner() {
        let owner = Identity::ephemeral();
        let agent = Keys::generate();
        let plaintext = r#"{"type":"status","message":"active"}"#;
        let content = nip44::encrypt(
            agent.secret_key(),
            &owner.public_key(),
            plaintext,
            nip44::Version::V2,
        )
        .unwrap();
        let event = EventBuilder::new(Kind::from_u16(24200), content)
            .tags([
                Tag::custom("p", [owner.pubkey_hex()]),
                Tag::custom("agent", [agent.public_key().to_hex()]),
                Tag::custom("frame", ["telemetry"]),
            ])
            .finalize(&agent)
            .unwrap();
        let event: buzz_desktop::relay::agent::AgentEvent =
            serde_json::from_str(&event.as_json()).unwrap();
        let frame = owner.decode_agent_frame(&event, false).unwrap();
        assert_eq!(frame["plaintext"], plaintext);
        assert_eq!(frame["agent"], agent.public_key().to_hex());
        assert!(
            Identity::ephemeral()
                .decode_agent_frame(&event, false)
                .is_err()
        );
        // Memories need a real agent target.
        assert!(
            owner
                .decode_agent_memories(&owner.pubkey_hex(), &[])
                .is_err()
        );
        let empty = owner
            .decode_agent_memories(&agent.public_key().to_hex(), &[])
            .unwrap();
        assert_eq!(empty["partial"], false);
    }

    #[test]
    fn nsec_round_trips_and_hex_is_refused() {
        let identity = Identity::ephemeral();
        let nsec = identity.nsec();
        let parsed = Identity::from_nsec(&nsec).unwrap();
        assert_eq!(parsed.public_key(), identity.public_key());
        assert_eq!(
            Identity::from_nsec(&identity.keys.secret_key().to_secret_hex()).unwrap_err(),
            IdentityError::InvalidKey
        );
    }

    #[test]
    fn template_id_is_kept_when_signing() {
        let identity = Identity::ephemeral();
        let unsigned = identity
            .template(
                Purpose::Content,
                9,
                1_700_000_000,
                vec![vec!["h".into(), "general".into()]],
                "hi".into(),
            )
            .unwrap();
        let id = unsigned.id.unwrap();
        let event = identity.sign(Purpose::Content, unsigned).unwrap();
        assert_eq!(event.id, id);
        assert!(event.verify().is_ok());
    }

    #[test]
    fn purposes_limit_kinds() {
        let identity = Identity::ephemeral();
        for (purpose, kind) in [
            (Purpose::Content, 27235),
            (Purpose::Content, 24242),
            (Purpose::Content, 30078),
            (Purpose::AppData, 9),
            (Purpose::Presence, 9),
        ] {
            assert_eq!(
                identity
                    .template(purpose, kind, 1, Vec::new(), String::new())
                    .unwrap_err(),
                IdentityError::NotAllowed,
                "{purpose:?} {kind}"
            );
        }
    }

    #[test]
    fn agent_attestation_matches_nip_oa() {
        let identity = Identity::ephemeral();
        let agent = Keys::generate().public_key().to_hex();
        let tag: Vec<String> =
            serde_json::from_str(&identity.attest_agent(&agent).unwrap()).unwrap();
        assert_eq!(tag[0], "auth");
        assert_eq!(tag[1], identity.pubkey_hex());
        assert_eq!(tag[2], "");
        let digest: [u8; 32] = Sha256::digest(format!("nostr:agent-auth:{agent}:")).into();
        let signature: secp256k1::schnorr::Signature = tag[3].parse().unwrap();
        let owner: secp256k1::XOnlyPublicKey = tag[1].parse().unwrap();
        secp256k1::Secp256k1::verification_only()
            .verify_schnorr(&signature, &digest, &owner)
            .unwrap();
        assert!(identity.attest_agent(&identity.pubkey_hex()).is_err());
    }

    #[test]
    fn self_encryption_round_trips() {
        let identity = Identity::ephemeral();
        let ciphertext = identity.encrypt_to_self("{\"v\":1}").unwrap();
        let unsigned = identity
            .template(
                Purpose::AppData,
                30078,
                Timestamp::now().as_secs(),
                Vec::new(),
                ciphertext,
            )
            .unwrap();
        let event = identity.sign(Purpose::AppData, unsigned).unwrap();
        assert_eq!(identity.decrypt_from_self(&event).unwrap(), "{\"v\":1}");
    }

    #[test]
    fn http_auth_is_fresh_each_time() {
        let identity = Identity::ephemeral();
        let a = identity
            .http_auth("https://relay.example.com/query", "POST", Some(b"[]"))
            .unwrap();
        let b = identity
            .http_auth("https://relay.example.com/query", "POST", Some(b"[]"))
            .unwrap();
        assert_ne!(a, b);
        let json = base64::engine::general_purpose::STANDARD
            .decode(a.strip_prefix("Nostr ").unwrap())
            .unwrap();
        let event: Event = serde_json::from_slice(&json).unwrap();
        assert_eq!(event.kind, Kind::HttpAuth);
        let tags: Vec<&str> = event.tags.iter().map(|t| t.kind()).collect();
        assert_eq!(tags, ["u", "method", "payload", "nonce"]);
    }

    #[test]
    fn signs_only_blossom_media_tokens() {
        let identity = Identity::ephemeral();
        let now = Timestamp::now().as_secs();
        let hash = "ab".repeat(32);
        let tag = |name: &str, value: &str| vec![name.to_string(), value.to_string()];
        let upload = [
            tag("x", &hash),
            tag("t", "upload"),
            tag("server", "relay.example.com"),
            tag("expiration", &(now + 60).to_string()),
        ];
        let event = identity
            .sign_media_event(24242, now, &upload, "Upload attachment")
            .unwrap();
        assert!(event.verify().is_ok());
        assert_eq!(event.created_at.as_secs(), now);
        let get = [
            tag("t", "get"),
            tag("server", "127.0.0.1:53000"),
            tag("expiration", &(now + 60).to_string()),
        ];
        assert!(
            identity
                .sign_media_event(24242, now, &get, "Get buzz-media")
                .is_ok()
        );
        let refused = |kind, created_at, tags: &[Vec<String>], content| {
            identity
                .sign_media_event(kind, created_at, tags, content)
                .unwrap_err()
        };
        assert_eq!(
            refused(9, now, &get, "Get buzz-media"),
            IdentityError::NotAllowed
        );
        assert_eq!(
            refused(24242, now, &get, "Upload attachment"),
            IdentityError::NotAllowed
        );
        // A delete, a long-lived token, a stale one, or an extra tag.
        let mut delete = upload.clone();
        delete[1] = tag("t", "delete");
        assert_eq!(
            refused(24242, now, &delete, "Upload attachment"),
            IdentityError::NotAllowed
        );
        let mut long = get.clone();
        long[2] = tag("expiration", &(now + 3600).to_string());
        assert_eq!(
            refused(24242, now, &long, "Get buzz-media"),
            IdentityError::NotAllowed
        );
        assert_eq!(
            refused(24242, now - 3600, &get, "Get buzz-media"),
            IdentityError::NotAllowed
        );
        let mut extra = get.to_vec();
        extra.push(tag("p", &hash));
        assert_eq!(
            refused(24242, now, &extra, "Get buzz-media"),
            IdentityError::NotAllowed
        );
    }
}

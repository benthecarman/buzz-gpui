//! Agents2 profile pictures, an addition of ours: buzz-app's agents have
//! none. They live in their own file beside the identities rather than in
//! them, because the identity file is buzz-app's format and refuses fields
//! it does not know; copies of the app sharing the state directory keep
//! reading it.

use buzz_agent_controller::{AppAgent, Secret};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

type Result<T> = std::result::Result<T, String>;

/// As the community's own profiles allow.
const PICTURE_LIMIT: usize = 2048;

/// One agent's picture.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Picture {
    /// The picture its profile should show; empty for none.
    pub picture: String,
    /// The picture in the profile its community last accepted from this
    /// host, by that profile's `created_at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<PublishedPicture>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct PublishedPicture {
    pub picture: String,
    pub created_at: u64,
}

impl Picture {
    /// Whether the agent's community already has its profile: the name and
    /// picture it last accepted are the saved ones. A profile this file did
    /// not record came from a copy of the app without pictures, so it has
    /// none.
    pub(crate) fn current(&self, agent: &AppAgent) -> bool {
        let Some(last) = &agent.profile else {
            return false;
        };
        let shown = self
            .published
            .as_ref()
            .filter(|published| published.created_at == last.created_at)
            .map_or("", |published| published.picture.as_str());
        last.name == agent.name && shown == self.picture
    }
}

/// The saved pictures by agent pubkey, one JSON file. Writes rename into
/// place under a lock beside it, as the identities do.
#[derive(Clone)]
pub(crate) struct Pictures {
    path: PathBuf,
}

impl Pictures {
    pub(crate) fn open(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn all(&self) -> Result<BTreeMap<String, Picture>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| "Saved agent pictures are malformed".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(_) => Err("Could not read saved agent pictures".into()),
        }
    }

    pub(crate) fn get(&self, pubkey: &str) -> Result<Picture> {
        Ok(self.all()?.remove(pubkey).unwrap_or_default())
    }

    fn update(&self, change: impl FnOnce(&mut BTreeMap<String, Picture>)) -> Result<()> {
        let dir = self.path.parent().ok_or("Invalid agent picture storage")?;
        std::fs::create_dir_all(dir).map_err(|_| "Could not create agent picture storage")?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(self.path.with_extension("lock"))
            .map_err(|_| "Could not open agent picture lock")?;
        lock.lock().map_err(|_| "Could not lock agent pictures")?;
        let mut pictures = self.all()?;
        change(&mut pictures);
        pictures.retain(|_, picture| *picture != Picture::default());
        let bytes =
            serde_json::to_vec_pretty(&pictures).map_err(|_| "Could not encode agent pictures")?;
        let temp = self.path.with_extension("tmp");
        std::fs::write(&temp, bytes).map_err(|_| "Could not save agent pictures")?;
        std::fs::rename(&temp, &self.path).map_err(|_| "Could not save agent pictures".into())
    }

    /// Saves the picture its profile should show; empty removes it.
    pub(crate) fn set(&self, pubkey: &str, picture: &str) -> Result<()> {
        let picture = valid_picture(picture)?;
        self.update(|pictures| pictures.entry(pubkey.into()).or_default().picture = picture)
    }

    /// Records the picture in a profile its community accepted, unless a
    /// newer one already is.
    pub(crate) fn published(&self, pubkey: &str, picture: String, created_at: u64) -> Result<()> {
        self.update(|pictures| {
            let saved = pictures.entry(pubkey.into()).or_default();
            if saved
                .published
                .as_ref()
                .is_none_or(|last| last.created_at < created_at)
            {
                saved.published = Some(PublishedPicture {
                    picture,
                    created_at,
                });
            }
        })
    }

    pub(crate) fn forget(&self, pubkey: &str) -> Result<()> {
        self.update(|pictures| {
            pictures.remove(pubkey);
        })
    }
}

/// An `https` URL without credentials, as the community's profiles take,
/// or empty for none.
fn valid_picture(picture: &str) -> Result<String> {
    let picture = picture.trim();
    if picture.is_empty() {
        return Ok(String::new());
    }
    let valid = picture.len() <= PICTURE_LIMIT
        && url::Url::parse(picture).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
        });
    if !valid {
        return Err("Choose a picture uploaded to the agent's community".into());
    }
    Ok(picture.into())
}

/// The agent's profile as its saved name and picture describe it, newer
/// than the last one its community accepted. Without a picture it is
/// exactly buzz-app's (`name` and `bot`); with one it adds `picture`.
pub(crate) fn profile_event(agent: &AppAgent, key: &Secret, picture: &str) -> Result<Value> {
    use secp256k1::{Keypair, Secp256k1, SecretKey};
    use sha2::{Digest, Sha256};
    if picture.is_empty() {
        return agent.profile_event(key);
    }
    if key.pubkey() != agent.pubkey {
        return Err("Agent identity changed".into());
    }
    let content = json!({ "name": agent.name, "bot": true, "picture": picture }).to_string();
    let auth: Vec<String> =
        serde_json::from_str(&agent.auth).map_err(|_| "Invalid owner authorization")?;
    let tags = vec![auth];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "System clock is unavailable")?
        .as_secs();
    // NIP-01 ties choose the lower event ID. A replacement must be newer.
    let created_at = agent
        .profile
        .as_ref()
        .map_or(now, |last| now.max(last.created_at.saturating_add(1)));
    let serialized = serde_json::to_vec(&json!([0, agent.pubkey, created_at, 0, tags, content]))
        .map_err(|_| "Could not encode agent event")?;
    let id: [u8; 32] = Sha256::digest(serialized).into();
    let mut secret: SecretKey = key.hex().parse().map_err(|_| "Invalid agent key")?;
    let mut pair = Keypair::from_secret_key(&Secp256k1::signing_only(), &secret);
    let sig = Secp256k1::signing_only()
        .sign_schnorr_no_aux_rand(&id, &pair)
        .to_string();
    secret.non_secure_erase();
    pair.non_secure_erase();
    Ok(json!({
        "id": id.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "pubkey": agent.pubkey,
        "created_at": created_at,
        "kind": 0,
        "tags": tags,
        "content": content,
        "sig": sig,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_agent_controller::Published;

    fn agent(key: &Secret, profile: Option<Published>) -> AppAgent {
        AppAgent {
            pubkey: key.pubkey().into(),
            relay: "wss://relay.example".into(),
            owner: "a".repeat(64),
            auth: r#"["auth","owner","","sig"]"#.into(),
            agent_type: "p/t".into(),
            name: "Ada".into(),
            picture: None,
            about: None,
            profile,
            deleted: false,
        }
    }

    fn published(name: &str, created_at: u64) -> Option<Published> {
        Some(Published {
            name: name.into(),
            picture: None,
            about: None,
            created_at,
        })
    }

    #[test]
    fn pictures_are_saved_validated_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let pictures = Pictures::open(dir.path().join("pictures.json"));
        let pubkey = "b".repeat(64);
        assert_eq!(pictures.get(&pubkey).unwrap(), Picture::default());
        pictures
            .set(&pubkey, " https://relay.example/media/a.png ")
            .unwrap();
        assert_eq!(
            pictures.get(&pubkey).unwrap().picture,
            "https://relay.example/media/a.png"
        );
        for invalid in [
            "http://relay.example/a.png",
            "https://user:pass@relay.example/a.png",
            "not a url",
            &format!("https://relay.example/{}", "a".repeat(PICTURE_LIMIT)),
        ] {
            assert!(pictures.set(&pubkey, invalid).is_err(), "{invalid}");
        }
        pictures
            .published(&pubkey, "https://relay.example/media/a.png".into(), 10)
            .unwrap();
        pictures.published(&pubkey, "older".into(), 9).unwrap();
        assert_eq!(
            pictures.get(&pubkey).unwrap().published.unwrap().created_at,
            10,
            "an older publication does not replace a newer one"
        );
        pictures.set(&pubkey, "").unwrap();
        assert_eq!(pictures.get(&pubkey).unwrap().picture, "");
        pictures.forget(&pubkey).unwrap();
        assert!(pictures.all().unwrap().is_empty());
    }

    #[test]
    fn a_profile_is_current_when_its_name_and_picture_were_published() {
        let key = Secret::generate().unwrap();
        let url = "https://relay.example/media/a.png";
        let none = Picture::default();
        let set = Picture {
            picture: url.into(),
            published: None,
        };
        let shown = Picture {
            picture: url.into(),
            published: Some(PublishedPicture {
                picture: url.into(),
                created_at: 10,
            }),
        };
        assert!(!none.current(&agent(&key, None)), "never published");
        assert!(none.current(&agent(&key, published("Ada", 10))));
        assert!(!none.current(&agent(&key, published("Eve", 10))), "renamed");
        assert!(
            !set.current(&agent(&key, published("Ada", 10))),
            "new picture"
        );
        assert!(shown.current(&agent(&key, published("Ada", 10))));
        assert!(
            !shown.current(&agent(&key, published("Ada", 11))),
            "a later profile from a copy without pictures dropped it"
        );
        let removed = Picture {
            picture: String::new(),
            ..shown.clone()
        };
        assert!(!removed.current(&agent(&key, published("Ada", 10))));
    }

    #[test]
    fn the_profile_event_adds_the_picture_to_buzz_apps_fields() {
        use secp256k1::{Secp256k1, XOnlyPublicKey, schnorr::Signature};
        let key = Secret::generate().unwrap();
        let agent = agent(&key, published("Ada", u64::MAX / 2));
        let url = "https://relay.example/media/a.png";
        let event = profile_event(&agent, &key, url).unwrap();
        let content: Value = serde_json::from_str(event["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            content,
            json!({ "name": "Ada", "bot": true, "picture": url })
        );
        assert_eq!(event["kind"], 0);
        assert_eq!(event["tags"], json!([["auth", "owner", "", "sig"]]));
        assert_eq!(
            event["created_at"],
            u64::MAX / 2 + 1,
            "newer than the last one accepted"
        );
        let serialized = serde_json::to_vec(&json!([
            0,
            event["pubkey"],
            event["created_at"],
            0,
            event["tags"],
            event["content"]
        ]))
        .unwrap();
        let id: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&serialized).into();
        assert_eq!(
            event["id"],
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let signature: Signature = event["sig"].as_str().unwrap().parse().unwrap();
        let pubkey: XOnlyPublicKey = key.pubkey().parse().unwrap();
        Secp256k1::verification_only()
            .verify_schnorr(&signature, &id, &pubkey)
            .unwrap();

        let plain = profile_event(&agent, &key, "").unwrap();
        let content: Value = serde_json::from_str(plain["content"].as_str().unwrap()).unwrap();
        assert_eq!(content, json!({ "name": "Ada", "bot": true }));
        let other = Secret::generate().unwrap();
        assert!(profile_event(&agent, &other, url).is_err());
    }
}

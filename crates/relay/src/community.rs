//! Joining a community (`docs/port/relay.md` §1.6–1.7): invite links in
//! both forms, the NIP-11 and join-policy check, the membership probe, and
//! the invite claim.
//!
//! The parsers are pure. [`inspect`], [`probe`] and [`claim`] run one HTTP
//! step each over a [`RelayHttp`] for the joining identity, before any
//! session exists. Ports `invite-link.ts`, `native-api.ts` and the order of
//! `CommunityDialog.tsx`.

use crate::http::{HttpError, RelayHttp};
use crate::origin::RelayOrigin;
use serde_json::{Value, json};
use std::time::Duration;
use url::Url;

/// Each join request's budget (`native-api.ts`).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_LINK: usize = 4096;
const MAX_CODE: usize = 256;

/// What the user pasted into "Add a community".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommunityInput {
    /// A bare relay address: open it as an existing member.
    Relay(RelayOrigin),
    /// An invite: claim `code` at `origin`. Only an intent; the user still
    /// confirms.
    Invite { origin: RelayOrigin, code: String },
}

impl CommunityInput {
    pub fn origin(&self) -> &RelayOrigin {
        match self {
            Self::Relay(origin) | Self::Invite { origin, .. } => origin,
        }
    }
}

/// `/^[a-zA-Z0-9._-]{1,256}$/`.
pub fn valid_code(code: &str) -> bool {
    (1..=MAX_CODE).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Parses a relay URL, `buzz://join?relay=…&code=…[&policy_receipt=…]`,
/// or the relay's share link `https://host/invite/<code>`.
pub fn parse_input(input: &str) -> Result<CommunityInput, String> {
    let input = input.trim();
    if input.len() > MAX_LINK {
        return Err("This link is too long.".into());
    }
    if input.to_ascii_lowercase().starts_with("buzz:") {
        return parse_deep_link(input).ok_or_else(|| "This invite link is not valid.".to_owned());
    }
    if let Some(invite) = parse_share_link(input) {
        return Ok(invite);
    }
    RelayOrigin::parse(input)
        .map(CommunityInput::Relay)
        .map_err(|error| error.to_string())
}

/// `invite-link.ts`. The policy receipt is validated, then discarded: the
/// join asks for a fresh one.
fn parse_deep_link(input: &str) -> Option<CommunityInput> {
    let rest = input.get("buzz://join".len()..)?;
    if !input[..11].eq_ignore_ascii_case("buzz://join")
        || !(rest.starts_with('?') || rest.starts_with("/?"))
    {
        return None;
    }
    let url = Url::parse(input).ok()?;
    if url.host_str() != Some("join")
        || !matches!(url.path(), "" | "/")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let (mut relay, mut code, mut receipts) = (Vec::new(), Vec::new(), 0);
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "relay" => relay.push(value.into_owned()),
            "code" => code.push(value.into_owned()),
            "policy_receipt" if !value.is_empty() => receipts += 1,
            "policy_receipt" => return None,
            _ => {}
        }
    }
    let ([relay], [code]) = (relay.as_slice(), code.as_slice()) else {
        return None;
    };
    if receipts > 1 || code.is_empty() || code.len() > MAX_CODE {
        return None;
    }
    Some(CommunityInput::Invite {
        origin: RelayOrigin::parse(relay).ok()?,
        code: code.clone(),
    })
}

/// `https://host/invite/<code>` → community `https://host`, that code.
fn parse_share_link(input: &str) -> Option<CommunityInput> {
    let url = Url::parse(input).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().strip_prefix("/invite/")?;
    let code = path.strip_suffix('/').unwrap_or(path);
    if !valid_code(code) {
        return None;
    }
    let mut origin = url.clone();
    origin.set_path("");
    let origin = RelayOrigin::parse(origin.as_str()).ok()?;
    Some(CommunityInput::Invite {
        origin,
        code: code.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Relay information and join policy

/// What the relay says about itself before joining.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinInfo {
    /// The NIP-11 name, unless it is the default "Buzz Relay".
    pub name: Option<String>,
    pub policy: Option<JoinPolicy>,
}

/// `GET /api/join-policy`'s `policy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinPolicy {
    pub terms: Option<String>,
    pub privacy: Option<String>,
    pub age_required: bool,
    pub version: String,
}

/// `{}` means no policy; a malformed policy is an error, never ignored.
pub fn parse_policy(body: &Value) -> Result<Option<JoinPolicy>, String> {
    let Some(policy) = body.get("policy") else {
        return Ok(None);
    };
    let text = |key: &str| {
        policy
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
    };
    match (
        policy.get("version").and_then(Value::as_str),
        policy
            .get("age_attestation_required")
            .and_then(Value::as_bool),
    ) {
        (Some(version), Some(age_required)) => Ok(Some(JoinPolicy {
            terms: text("terms_markdown"),
            privacy: text("privacy_markdown"),
            age_required,
            version: version.to_owned(),
        })),
        _ => Err("This community's join policy could not be read.".into()),
    }
}

fn nip11_name(info: &Value) -> Option<String> {
    info.get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "Buzz Relay")
        .map(str::to_owned)
}

async fn timed<T>(future: impl Future<Output = Result<T, HttpError>>) -> Result<T, HttpError> {
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .unwrap_or_else(|_| Err(HttpError::Transport("timed out".into())))
}

/// NIP-11 and the join policy, read together. A missing policy route is no
/// policy.
pub async fn inspect(http: &RelayHttp) -> Result<JoinInfo, String> {
    let (info, policy) = tokio::join!(
        timed(http.information()),
        timed(http.get_public("/api/join-policy"))
    );
    let info =
        info.map_err(|_| "This relay could not be reached. Check the address.".to_owned())?;
    if info.get("self").and_then(Value::as_str).is_none() {
        return Err("Relay did not advertise its identity".into());
    }
    let policy = match policy {
        Ok(body) => parse_policy(&body)?,
        Err(HttpError::Status { status: 404, .. }) => None,
        Err(_) => return Err("This community's join policy could not be read.".into()),
    };
    Ok(JoinInfo {
        name: nip11_name(&info),
        policy,
    })
}

/// Whether the identity is already a member: a strong read of its own
/// profile succeeds for members and is refused (401/403) otherwise.
pub async fn probe(http: &RelayHttp) -> Result<bool, String> {
    let viewer = http.identity().pubkey_hex();
    let filter = json!({
        "kinds": [0],
        "authors": [viewer],
        "limit": 5,
        "consistency": "strong",
    });
    let filter = filter.as_object().expect("object").clone();
    match timed(http.query_raw(&[filter])).await {
        Ok(_) => Ok(true),
        Err(HttpError::Status {
            status: 401 | 403, ..
        }) => Ok(false),
        Err(error) => Err(format!("Membership could not be checked: {error}")),
    }
}

/// How a claim ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claimed {
    Joined,
    AlreadyMember,
}

/// Accepts the policy (when there is one) and claims the invite.
/// `age_confirmed` answers the policy's age question.
pub async fn claim(
    http: &RelayHttp,
    code: &str,
    policy: Option<&JoinPolicy>,
    age_confirmed: bool,
) -> Result<Claimed, String> {
    if !valid_code(code) {
        return Err("This invite code is not valid.".into());
    }
    let receipt = match policy {
        Some(policy) => {
            let body = json!({
                "code": code,
                "policy_version": policy.version,
                "age_confirmed": age_confirmed,
            });
            let bytes = timed(http.post("/api/invites/accept-policy", body.to_string()))
                .await
                .map_err(claim_error)?;
            let receipt = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| value.get("receipt")?.as_str().map(str::to_owned))
                .filter(|receipt| !receipt.is_empty())
                .ok_or("The relay did not accept the join policy.")?;
            Some(receipt)
        }
        None => None,
    };
    let mut body = json!({"code": code});
    if let Some(receipt) = receipt {
        body["policy_receipt"] = json!(receipt);
    }
    let bytes = timed(http.post("/api/invites/claim", body.to_string()))
        .await
        .map_err(claim_error)?;
    parse_claim(&bytes)
}

/// `{"status":"joined"|"already_member", …}`; nothing else counts.
pub fn parse_claim(body: &[u8]) -> Result<Claimed, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| "The relay returned an invalid invite response.")?;
    match value.get("status").and_then(Value::as_str) {
        Some("joined") => Ok(Claimed::Joined),
        Some("already_member") => Ok(Claimed::AlreadyMember),
        _ => Err("The relay returned an invalid invite response.".into()),
    }
}

/// The relay's refusal codes, worded for people.
pub fn claim_error(error: HttpError) -> String {
    match &error {
        HttpError::Status { status: 429, .. } => {
            "Too many invite attempts. Wait a minute and try again.".into()
        }
        HttpError::Status { message, .. } => match message.as_str() {
            "invite_invalid" => "This invite is not valid.".into(),
            "invite_expired" => "This invite has expired. Ask for a new one.".into(),
            "invite_exhausted" => "This invite has been used up. Ask for a new one.".into(),
            "join_policy_required" | "join_policy_not_accepted" => {
                "Accept the community's terms to join.".into()
            }
            "join_policy_not_configured" => {
                "This community's join policy changed. Try again.".into()
            }
            _ => format!("The invite could not be claimed: {error}"),
        },
        _ => format!("The invite could not be claimed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite(origin: &str, code: &str) -> CommunityInput {
        CommunityInput::Invite {
            origin: RelayOrigin::parse(origin).unwrap(),
            code: code.into(),
        }
    }

    #[test]
    fn parses_both_invite_forms_and_bare_relays() {
        assert_eq!(
            parse_input("buzz://join?relay=wss%3A%2F%2Frelay.example.com&code=v2.abc_1-2").unwrap(),
            invite("wss://relay.example.com", "v2.abc_1-2")
        );
        assert_eq!(
            parse_input("buzz://join/?code=x&relay=https://relay.example.com&policy_receipt=r")
                .unwrap(),
            invite("wss://relay.example.com", "x")
        );
        assert_eq!(
            parse_input(" https://Relay.Example.com/invite/abc ").unwrap(),
            invite("wss://relay.example.com", "abc")
        );
        assert_eq!(
            parse_input("http://127.0.0.1:53000/invite/abc/").unwrap(),
            invite("ws://127.0.0.1:53000", "abc")
        );
        assert_eq!(
            parse_input("wss://relay.example.com").unwrap(),
            CommunityInput::Relay(RelayOrigin::parse("wss://relay.example.com").unwrap())
        );
    }

    #[test]
    fn refuses_malformed_invites() {
        for bad in [
            "buzz://join?relay=wss://a.example&code=",
            "buzz://join?relay=wss://a.example",
            "buzz://join?relay=wss://a.example&code=x&code=y",
            "buzz://join?relay=wss://a.example/path&code=x",
            "buzz://join?relay=wss://a.example&code=x&policy_receipt=",
            "buzz://join?relay=wss://a.example&code=x&policy_receipt=a&policy_receipt=b",
            "buzz://join/extra?relay=wss://a.example&code=x",
            "buzz://channel?relay=wss://a.example&code=x",
            "buzz://join?relay=wss://a.example&code=x#frag",
        ] {
            assert!(parse_input(bad).is_err(), "{bad}");
        }
        assert!(parse_input("https://a.example/invite/bad code").is_err());
        assert!(parse_input("https://a.example/invite/x?y=1").is_err());
        assert!(
            parse_input(&format!(
                "buzz://join?relay=wss://a&code={}",
                "x".repeat(4100)
            ))
            .is_err()
        );
    }

    #[test]
    fn reads_the_join_policy() {
        assert_eq!(parse_policy(&json!({})), Ok(None));
        let policy = parse_policy(&json!({"policy": {
            "terms_markdown": "Be kind", "privacy_markdown": null,
            "age_attestation_required": true, "version": "3"
        }}))
        .unwrap()
        .unwrap();
        assert_eq!(policy.terms.as_deref(), Some("Be kind"));
        assert!(policy.privacy.is_none() && policy.age_required);
        assert_eq!(policy.version, "3");
        assert!(parse_policy(&json!({"policy": {"version": 3}})).is_err());
    }

    #[test]
    fn accepts_only_known_claim_statuses() {
        assert_eq!(parse_claim(br#"{"status":"joined"}"#), Ok(Claimed::Joined));
        assert_eq!(
            parse_claim(br#"{"status":"already_member","role":"member"}"#),
            Ok(Claimed::AlreadyMember)
        );
        assert!(parse_claim(br#"{"status":"pending"}"#).is_err());
        let expired = HttpError::Status {
            status: 403,
            message: "invite_expired".into(),
            retry_after: None,
        };
        assert!(claim_error(expired).contains("expired"));
    }

    #[test]
    fn hides_the_default_relay_name() {
        assert_eq!(nip11_name(&json!({"name": "Buzz Relay"})), None);
        assert_eq!(nip11_name(&json!({"name": " Team "})), Some("Team".into()));
    }
}

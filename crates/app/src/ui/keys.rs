//! Public keys as people see them.

use nostr::nips::nip19::ToBech32;
use nostr::prelude::PublicKey;

/// The full `npub1…` of a hex key, or the key itself if it is not one.
pub fn npub(hex: &str) -> String {
    PublicKey::from_hex(hex)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .unwrap_or_else(|| hex.to_string())
}

/// `formatPublicKey`: `npub…` and the last three characters, for a valid
/// hex key.
pub fn format_public_key(hex: &str) -> Option<String> {
    let npub = PublicKey::from_hex(hex).ok()?.to_bech32().ok()?;
    Some(format!("npub…{}", &npub[npub.len() - 3..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn keys_read_as_npubs() {
        let key = buzz_identity::Identity::ephemeral().public_key();
        let npub = key.to_bech32().unwrap();
        assert_eq!(super::npub(&key.to_hex()), npub);
        assert_eq!(super::npub("nope"), "nope");
        assert_eq!(
            format_public_key(&key.to_hex()),
            Some(format!("npub…{}", &npub[npub.len() - 3..]))
        );
        assert_eq!(format_public_key("nope"), None);
    }
}

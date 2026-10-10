//! The card a clicked mention opens: avatar, name, key, status, about and
//! a way to message the person.

use super::media;
use buzz_relay_client::{Media, Profile};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, ButtonVariant, Popover, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nostr::nips::nip19::ToBech32;
use nostr::prelude::PublicKey;
use std::rc::Rc;

/// The card's width (`PopoverWidth::Default`), for keeping it on screen.
pub const WIDTH: Pixels = px(320.);

pub type OnMessage = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct ProfileCard<'a> {
    pub pubkey: &'a str,
    pub profile: Option<&'a Profile>,
    pub media: &'a Media,
    /// The person's status (`UserStatusDisplay`), if they have one.
    pub status: Option<AnyElement>,
    /// Opens the direct message with this person; `None` when there is
    /// none to open.
    pub on_message: Option<OnMessage>,
}

impl ProfileCard<'_> {
    pub fn render(self, cx: &App) -> AnyElement {
        Popover::new().children(self.content(cx)).into_any_element()
    }

    /// The same content filling a companion panel, at its 20px inset.
    pub fn panel(self, cx: &App) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .p(px(20.))
            .children(self.content(cx))
            .into_any_element()
    }

    fn content(self, cx: &App) -> Vec<AnyElement> {
        let c = cx.colors();
        let fallback = Profile::fallback(self.pubkey);
        let profile = self.profile.unwrap_or(&fallback);
        let mut avatar = Avatar::new(profile.name.clone()).size(AvatarSize::Px(48.));
        if profile.is_agent {
            avatar = avatar.squircle();
        }
        if let Some(picture) = &profile.picture {
            avatar = avatar.image(media::avatar(picture, self.media));
        }
        let detail = profile
            .nip05
            .clone()
            .unwrap_or_else(|| short_npub(self.pubkey));
        let message = Button::new("profile-message", "Message")
            .variant(ButtonVariant::Prominent)
            .size(ButtonSize::Sm)
            .full_width()
            .map(|button| match self.on_message {
                Some(on_message) => button.on_click(move |_, window, cx| on_message(window, cx)),
                // Starting a conversation needs the DM flow, which is not
                // ported yet.
                None => button
                    .disabled(true)
                    .tooltip("No direct message with this person yet"),
            });
        let about = profile
            .about
            .as_ref()
            .filter(|about| !about.trim().is_empty())
            .map(|about| {
                div()
                    .mt(px(12.))
                    .text_body_sm()
                    .text_color(c.text_standard)
                    .line_clamp(4)
                    .child(about.trim().to_string())
                    .into_any_element()
            });
        [div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(avatar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_label()
                            .text_color(c.text_standard)
                            .truncate()
                            .child(profile.name.clone()),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .truncate()
                            .child(if profile.is_agent {
                                format!("Agent · {detail}")
                            } else {
                                detail
                            }),
                    ),
            )
            .into_any_element()]
        .into_iter()
        .chain(
            self.status
                .map(|status| div().mt(px(12.)).child(status).into_any_element()),
        )
        .chain(about)
        .chain([div().mt(px(16.)).child(message).into_any_element()])
        .collect()
    }
}

/// `npub1abcd…wxyz`, or the hex prefix for a malformed key.
fn short_npub(pubkey: &str) -> String {
    match PublicKey::from_hex(pubkey)
        .ok()
        .and_then(|key| key.to_bech32().ok())
    {
        Some(npub) => format!("{}…{}", &npub[..10], &npub[npub.len() - 4..]),
        None => pubkey.chars().take(10).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn shortens_keys() {
        let key = buzz_identity::Identity::ephemeral().public_key();
        let short = short_npub(&key.to_hex());
        assert!(short.starts_with("npub1") && short.contains('…'));
        assert_eq!(short.chars().count(), 15);
        assert_eq!(short_npub("nothex"), "nothex");
    }
}

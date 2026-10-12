//! The identity preview of an agent choice (`IdentityRow`'s preview card)
//! with its "Managed by" block (`AgentOwnerPreview.tsx`): the owner proved
//! by the agent's signed profile attestation, never by its metadata.

use crate::community::Community;
use crate::ui::keys::npub;
use buzz_relay_client::kit::AgentChoice;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Tooltip, TypeStyled, tokens::radius, tokens::space,
};
use gpui_kit::*;

/// Who manages an agent, as its profile proves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The profile has not been read yet.
    Checking,
    Unavailable,
    Known {
        pubkey: String,
        name: String,
        picture: Option<String>,
        you: bool,
    },
}

impl Owner {
    pub fn of(community: &Community, agent: &str) -> Self {
        let profiles = &community.snapshot.profiles;
        let Some(profile) = profiles.get(agent) else {
            return Self::Checking;
        };
        let Some(owner) = profile.attested_owner.clone() else {
            return Self::Unavailable;
        };
        let owner_profile = profiles.get(&owner);
        Self::Known {
            name: owner_profile
                .map(|p| p.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format_public_key(&owner)),
            picture: owner_profile.and_then(|p| p.picture.clone()),
            you: owner == community.session.viewer(),
            pubkey: owner,
        }
    }
}

/// `formatPublicKey`.
fn format_public_key(pubkey: &str) -> String {
    let npub = npub(pubkey);
    format!("npub…{}", &npub[npub.len().saturating_sub(3)..])
}

/// The "Managed by" block under a preview.
pub fn managed_by(community: &Entity<Community>, agent: &str, cx: &mut App) -> Div {
    let c = *cx.colors();
    let owner = Owner::of(community.read(cx), agent);
    let session = community.read(cx).session.clone();
    match &owner {
        Owner::Checking => session.want_profiles(vec![agent.to_owned()]),
        Owner::Known { pubkey, .. } => session.want_profiles(vec![pubkey.clone()]),
        Owner::Unavailable => {}
    }
    let block = div()
        .mt(space::S3)
        .min_w_0()
        .border_t_1()
        .border_color(c.border_standard)
        .pt(space::S3)
        .child(
            div()
                .mb(space::S2)
                .text_caption()
                .text_color(c.text_subtle)
                .child("Managed by"),
        );
    match owner {
        Owner::Known {
            pubkey,
            name,
            picture,
            you,
        } => {
            let mut avatar = Avatar::new(name.clone()).size(AvatarSize::Small);
            if let Some(url) = picture {
                avatar = avatar.image(crate::ui::media::avatar(&url, session.media()));
            }
            block
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::S2)
                        .child(avatar)
                        .child(div().text_body_sm().child(if you {
                            format!("{name} (you)")
                        } else {
                            name
                        })),
                )
                .child(
                    div()
                        .mt(space::S1)
                        .text_mono()
                        .text_color(c.text_subtle)
                        .child(npub(&pubkey)),
                )
        }
        Owner::Checking => block.child(
            div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("Checking ownership…"),
        ),
        Owner::Unavailable => block.child(
            div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("Owner unavailable"),
        ),
    }
}

/// The preview card shown over an agent choice.
pub struct IdentityPreview {
    community: Entity<Community>,
    agent: AgentChoice,
    npub: String,
}

impl IdentityPreview {
    pub fn new(community: Entity<Community>, agent: AgentChoice, npub: String) -> Self {
        Self {
            community,
            agent,
            npub,
        }
    }
}

impl Render for IdentityPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let mut avatar = Avatar::new(self.agent.name.clone())
            .size(AvatarSize::Large)
            .squircle();
        if let Some(url) = &self.agent.avatar {
            avatar = avatar.image(crate::ui::media::avatar(
                url,
                self.community.read(cx).session.media(),
            ));
        }
        let managed = managed_by(&self.community, &self.agent.pubkey, cx);
        Tooltip::placed(
            div()
                .w(rems(20.))
                .p(space::S4)
                .rounded(radius::CONTAINER)
                .bg(c.surface_popover)
                .border_1()
                .border_color(c.border_standard)
                .text_color(c.text_standard)
                .shadow(buzz_ui::tokens::shadow::sm())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::S3)
                        .min_w_0()
                        .child(avatar)
                        .child(
                            div()
                                .min_w_0()
                                .child(div().text_label().child(self.agent.name.clone()))
                                .child(
                                    div()
                                        .text_body_sm()
                                        .text_color(c.text_subtle)
                                        .child("Agent"),
                                ),
                        ),
                )
                .child(div().mt(space::S2).text_mono().child(self.npub.clone()))
                .child(managed),
        )
    }
}

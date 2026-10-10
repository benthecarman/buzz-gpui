//! Settings cards contributed by bundled plugins (`features/settings/service.ts`,
//! `app/settings-sections.ts`), such as Templates and Membership.
//!
//! buzz-app's plugins register cards at run time; here each slice calls
//! [`register`] from its init. The settings sidebar places a card under the
//! selected community, in the community's Administration group, or in a
//! named account-level group, and lists it only while its visibility says
//! so.

// Slices that register cards use the rest; until each does, parts are idle.
#![allow(dead_code)]

use buzz_ui::IconName;
use gpui_kit::*;

/// Where the settings sidebar lists a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardPlacement {
    /// After Profile under the selected community's name (no `group` and
    /// no `section`). The icon defaults to a chat bubble.
    Community,
    /// The community's "Administration" group (`section: "administration"`),
    /// for permission-gated controls. The icon defaults to people.
    Administration,
    /// An account-level group with this label, such as "Integrations".
    Group(&'static str),
}

/// A card's verified visibility (`visibility.snapshot()`): only `Visible`
/// lists it, but an open card stays open while its check is pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardVisibility {
    Visible,
    Hidden,
    Pending,
    Error,
}

/// The view a card shows; the page keeps it while its section is open.
pub type BuildCard = fn(&mut Window, &mut App) -> AnyView;

#[derive(Clone)]
pub struct SettingsCard {
    /// `plugin/card`, such as `buzz.moderation/membership`. The text before
    /// the slash is the plugin id the Plugins switches gate.
    pub key: &'static str,
    pub title: &'static str,
    pub icon: Option<IconName>,
    pub placement: CardPlacement,
    /// `None` is always visible.
    pub visibility: Option<fn(&App) -> CardVisibility>,
    pub build: BuildCard,
}

impl SettingsCard {
    pub fn plugin(&self) -> &'static str {
        self.key.split('/').next().unwrap_or(self.key)
    }

    pub fn visibility(&self, cx: &App) -> CardVisibility {
        self.visibility
            .map_or(CardVisibility::Visible, |visibility| visibility(cx))
    }

    /// The sidebar icon: the card's own, else its placement's default.
    pub fn sidebar_icon(&self) -> Option<IconName> {
        self.icon.or(match self.placement {
            CardPlacement::Community => Some(IconName::ChatCircle),
            CardPlacement::Administration => Some(IconName::Users),
            CardPlacement::Group(_) => None,
        })
    }
}

#[derive(Default)]
struct SettingsCards(Vec<SettingsCard>);

impl Global for SettingsCards {}

/// Adds a card, replacing one with the same key.
pub fn register(card: SettingsCard, cx: &mut App) {
    let cards = &mut cx.default_global::<SettingsCards>().0;
    cards.retain(|existing| existing.key != card.key);
    cards.push(card);
}

/// Every registered card, in registration order.
pub fn all(cx: &App) -> Vec<SettingsCard> {
    cx.try_global::<SettingsCards>()
        .map(|cards| cards.0.clone())
        .unwrap_or_default()
}

pub fn get(key: &str, cx: &App) -> Option<SettingsCard> {
    all(cx).into_iter().find(|card| card.key == key)
}

/// The cards the sidebar lists now.
pub fn visible(cx: &App) -> Vec<SettingsCard> {
    all(cx)
        .into_iter()
        .filter(|card| card.visibility(cx) == CardVisibility::Visible)
        .collect()
}

/// The labels of the account-level groups, in first-registration order.
pub fn groups(cards: &[SettingsCard]) -> Vec<&'static str> {
    let mut labels = Vec::new();
    for card in cards {
        if let CardPlacement::Group(label) = card.placement
            && !labels.contains(&label)
        {
            labels.push(label);
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn card(key: &'static str, placement: CardPlacement) -> SettingsCard {
        SettingsCard {
            key,
            title: "Card",
            icon: None,
            placement,
            visibility: None,
            build: |_, _| unreachable!(),
        }
    }

    #[test]
    fn groups_keep_first_registration_order() {
        let cards = [
            card("a/one", CardPlacement::Group("Integrations")),
            card("b/two", CardPlacement::Community),
            card("c/three", CardPlacement::Group("Account")),
            card("d/four", CardPlacement::Group("Integrations")),
        ];
        assert_eq!(groups(&cards), ["Integrations", "Account"]);
    }

    #[test]
    fn placement_picks_the_default_icon_and_plugin_comes_from_the_key() {
        let admin = card("buzz.moderation/membership", CardPlacement::Administration);
        assert_eq!(admin.sidebar_icon(), Some(IconName::Users));
        assert_eq!(admin.plugin(), "buzz.moderation");
        assert_eq!(
            card("x/y", CardPlacement::Group("Account")).sidebar_icon(),
            None
        );
    }
}

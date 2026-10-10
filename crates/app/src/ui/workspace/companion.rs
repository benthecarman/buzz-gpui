//! The companion dock (`PanelFrame.tsx`, `usePanelLauncher.ts`): a panel
//! the shell opens beside any page, such as the viewer's own profile from
//! the account menu.

use super::Workspace;
use super::layout::{self, Split};
use crate::ui::navigation::Target;
use crate::ui::pages::{self, Page};
use crate::ui::panels::{self, Dock};
use crate::ui::profile_card::{self, ProfileCard};
use gpui_kit::*;
use std::rc::Rc;

/// What the companion dock shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Companion {
    /// A person's profile; the account menu opens the viewer's.
    Profile { pubkey: String },
}

impl Workspace {
    /// "View your profile": the viewer's profile beside the page.
    pub(super) fn open_own_profile(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = &self.community else {
            return;
        };
        let pubkey = community.read(cx).session.viewer().to_string();
        let next = Companion::Profile { pubkey };
        if self.companion.as_ref() != Some(&next) {
            self.companion = Some(next);
            self.companion_opening += 1;
        }
        cx.notify();
    }

    /// Closing returns focus to the account menu's trigger, which opened it.
    pub(super) fn close_companion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.companion.take().is_some() {
            window.focus(&self.main_focus, cx);
            cx.notify();
        }
    }

    /// The companion's card, if one is open.
    pub(super) fn companion_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Companion::Profile { pubkey } = self.companion.clone()?;
        let community = self.community.as_ref()?.read(cx);
        let snapshot = community.snapshot.clone();
        let media = community.session.media().clone();
        let origin = community.origin.to_string();
        let viewer = community.session.viewer().to_string();
        // Notes to self, for the viewer's own profile.
        let dm = (pubkey == viewer)
            .then(|| {
                snapshot.channels.iter().find(|channel| {
                    channel.channel_type == Some(buzz_relay_client::ChannelType::Dm)
                        && channel.participants.as_deref() == Some(&[])
                })
            })
            .flatten()
            .map(|channel| channel.id.clone());
        let weak = cx.entity().downgrade();
        let on_message: Option<profile_card::OnMessage> = dm.map(|dm| {
            let weak = weak.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let target = Target::channel(Some(origin.clone()), dm.clone());
                let _ = weak.update(cx, |this, cx| this.navigate(target, window, cx));
            }) as profile_card::OnMessage
        });
        let content = ProfileCard {
            pubkey: &pubkey,
            profile: snapshot.profiles.get(&pubkey).map(|p| &**p),
            media: &media,
            status: self.community.as_ref().and_then(|community| {
                crate::ui::status::display(snapshot.statuses.get(&pubkey), community, cx)
            }),
            on_message,
        }
        .panel(cx);
        // An Agents2 agent's sections and Delete (ProfilePanel.tsx).
        let agents2 = crate::ui::agents2::profile_sections(&pubkey, window, cx);
        let content = div()
            .flex()
            .flex_col()
            .child(content)
            .children(agents2)
            .into_any_element();
        Some(panels::card(
            "companion",
            "Profile",
            None,
            content,
            Rc::new(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.close_companion(window, cx));
            }),
            cx,
        ))
    }

    /// The page region's width: the joined panel less the sidebar beside
    /// it.
    pub(super) fn main_width(&self, window: &Window, cx: &App) -> f32 {
        let width = f32::from(window.viewport_size().width);
        let content = Self::content_width(window);
        let collapsible = self.page == Page::SETTINGS
            || pages::spec(self.page, cx).is_some_and(|spec| spec.collapsible);
        let has_sidebar = self.page == Page::SETTINGS
            || self.own_sidebars.contains_key(&self.page)
            || self.community.is_some();
        let beside = !layout::narrow(width) && has_sidebar && (self.sidebar_open || !collapsible);
        match beside {
            true => content - self.sidebar_width(window),
            false => content,
        }
    }

    /// A page other than Messages with the companion dock beside it.
    pub(super) fn frame(
        &mut self,
        page: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let companion = self.companion_panel(window, cx);
        let weak = cx.entity().downgrade();
        Dock {
            id: "companion".into(),
            split: Split::COMPANION,
            preferred: self.companion_width,
            window: f32::from(window.viewport_size().width),
            available: self.main_width(window, cx),
            opening: self.companion_opening,
            on_resize: Rc::new(move |width, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.companion_width = width;
                    cx.notify();
                });
            }),
            on_commit: Rc::new(|_, _| {}),
        }
        .render(page, companion, cx)
    }
}

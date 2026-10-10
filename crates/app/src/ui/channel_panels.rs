//! Panels contributed to a channel (`features/panels/service.ts`,
//! `useChannelPanels.tsx`): channel tools such as Terminal or Todos, and
//! details opened from the channel menu or a link, such as channel usage.
//!
//! buzz-app's plugins register panels at run time; here each slice calls
//! [`register`] from its init. A panel can show as a tab in the channel's
//! tab pane, as a drawer under the conversation from a header launcher,
//! or both.

use crate::community::Community;
use buzz_relay_client::ChannelSummary;
use buzz_ui::IconName;
use gpui_kit::*;
use std::rc::Rc;

/// Closes the panel through its host.
pub type Close = Rc<dyn Fn(&mut Window, &mut App)>;

/// What a panel is opened with.
pub struct Opening {
    pub community: Entity<Community>,
    /// The conversation it was opened from.
    pub channel: String,
    /// The link or channel id it shows.
    pub target: String,
    pub close: Close,
}

/// Where a header launcher shows its panel (`channelPlacement`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A drawer under the conversation.
    Bottom,
    /// The column beside the conversation.
    Side,
}

/// A button in the channel header that toggles the panel.
#[derive(Clone, Copy)]
pub struct Launcher {
    pub icon: IconName,
    pub label: &'static str,
    pub placement: Placement,
}

/// An entry in the channel's menu that opens the panel as a tab
/// (`channelMenu`), such as "View channel usage".
#[derive(Clone, Copy)]
pub struct MenuEntry {
    pub label: &'static str,
    /// Presentation only, not access.
    pub eligible: fn(&ChannelSummary, &App) -> bool,
}

#[derive(Clone)]
pub struct ChannelPanel {
    /// `plugin/panel`, unique.
    pub key: &'static str,
    pub title: &'static str,
    pub icon: IconName,
    /// Links this panel shows, opened as a tab beside the conversation.
    pub matches: fn(&str) -> bool,
    /// Offered under Tools in the New tab picker.
    pub tool: bool,
    pub launcher: Option<Launcher>,
    pub menu: Option<MenuEntry>,
    pub build: fn(Opening, &mut Window, &mut App) -> AnyView,
}

#[derive(Default)]
struct ChannelPanels(Vec<ChannelPanel>);

impl Global for ChannelPanels {}

/// Adds a panel, replacing one with the same key.
pub fn register(panel: ChannelPanel, cx: &mut App) {
    let panels = &mut cx.default_global::<ChannelPanels>().0;
    panels.retain(|existing| existing.key != panel.key);
    panels.push(panel);
}

/// A panel tab's title from its view, such as the person a profile
/// shows (`usePanelTabTitle`). Without one the tab shows the panel's
/// title.
pub type Title = fn(&AnyView, &App) -> Option<SharedString>;

#[derive(Default)]
struct Titles(Vec<(&'static str, Title)>);

impl Global for Titles {}

/// Gives a registered panel's tabs a title of their own.
pub fn register_title(key: &'static str, title: Title, cx: &mut App) {
    let titles = &mut cx.default_global::<Titles>().0;
    titles.retain(|(existing, _)| *existing != key);
    titles.push((key, title));
}

/// The tab title of `view`, a view of the panel `key`.
pub fn title(key: &str, view: &AnyView, cx: &App) -> Option<SharedString> {
    let (_, title) = cx
        .try_global::<Titles>()?
        .0
        .iter()
        .find(|(existing, _)| *existing == key)?;
    title(view, cx)
}

/// The registered panels whose plugin is on.
pub fn all(cx: &App) -> Vec<ChannelPanel> {
    cx.try_global::<ChannelPanels>()
        .map(|panels| panels.0.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|panel| crate::plugins::enabled_key(panel.key))
        .collect()
}

pub fn get(key: &str, cx: &App) -> Option<ChannelPanel> {
    all(cx).into_iter().find(|panel| panel.key == key)
}

/// The first panel that shows `target`, in registration order.
pub fn resolve(target: &str, cx: &App) -> Option<ChannelPanel> {
    all(cx).into_iter().find(|panel| (panel.matches)(target))
}

/// Tools offered for a channel: not for archived or read-only ones.
pub fn tools(channel: &ChannelSummary, cx: &App) -> Vec<ChannelPanel> {
    if channel.archived || channel.read_only {
        return Vec::new();
    }
    all(cx).into_iter().filter(|panel| panel.tool).collect()
}

/// The channel menu's panel entries for `channel`.
pub fn menu_entries(channel: &ChannelSummary, cx: &App) -> Vec<(ChannelPanel, &'static str)> {
    if channel.cached || channel.read_only {
        return Vec::new();
    }
    all(cx)
        .into_iter()
        .filter_map(|panel| {
            let menu = panel.menu?;
            (menu.eligible)(channel, cx).then_some((panel, menu.label))
        })
        .collect()
}

pub fn launchers(cx: &App) -> Vec<(ChannelPanel, Launcher)> {
    all(cx)
        .into_iter()
        .filter_map(|panel| Some((panel.clone(), panel.launcher?)))
        .collect()
}

/// Development only: `BUZZ_DEV_CHANNEL_PANEL=1` registers a sample panel
/// with a launcher, a Tools entry and a menu entry, so headless runs can
/// screenshot the panel host before any slice registers a real panel.
pub fn dev_sample(cx: &mut App) {
    if std::env::var_os("BUZZ_DEV_CHANNEL_PANEL").is_none() {
        return;
    }
    struct Sample(Opening);
    impl Render for Sample {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let opening = &self.0;
            let close = opening.close.clone();
            div()
                .id("sample-panel")
                .p(px(20.))
                .child(format!(
                    "Sample panel in {} for {} from {}",
                    opening.community.read(cx).name(),
                    opening.target,
                    opening.channel
                ))
                .child(
                    div()
                        .id("sample-close")
                        .child("Close")
                        .on_click(move |_, window, cx| close(window, cx)),
                )
        }
    }
    register(
        ChannelPanel {
            key: "buzz.dev/sample",
            title: "Sample",
            icon: IconName::ListChecks,
            matches: |target| target.starts_with("buzz://dev-sample"),
            tool: true,
            launcher: Some(Launcher {
                icon: IconName::ListChecks,
                label: "Sample",
                placement: Placement::Bottom,
            }),
            menu: Some(MenuEntry {
                label: "View sample",
                eligible: |_, _| true,
            }),
            build: |opening, _, cx| cx.new(|_| Sample(opening)).into(),
        },
        cx,
    );
}

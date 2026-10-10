//! The Settings page (ui.md §5b): a shell destination, not a modal. The
//! settings sidebar takes the channel sidebar's place and the detail pane
//! shows one section at a time.

mod about;
mod agents;
mod appearance;
mod archive;
mod avatar;
mod communities;
mod emoji;
mod notifications;
mod profile;
mod shortcuts;

use crate::community::{AppState, Community};
use crate::hosts::HostConnection;
use crate::ui::agents::{AddHost, DefaultsDialog};
use crate::ui::overlay;
pub use agents::{followup_draft, remember_mentioned_agents};
pub use archive::capture as capture_agent_activity;
use buzz_ui::{
    ActiveTheme, Button, ButtonShape, Icon, IconName, NavigationItem, Toast, ToastTone, TypeStyled,
    tokens::radius,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use profile::ProfileForm;
pub use profile::{default_profile, remember_profile, sign_out_dialog};
use std::rc::Rc;
use std::time::Duration;

actions!(
    buzz,
    [
        /// Mod+, : open Settings.
        OpenSettings,
        /// Open Settings at Agents, where hosts are managed.
        OpenHostSettings,
        /// The settings sidebar's Back row.
        CloseSettings,
    ]
);

const TOAST_DURATION: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Profile,
    CustomEmoji,
    Communities,
    Appearance,
    Notifications,
    Shortcuts,
    Agents,
    About,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::CustomEmoji => "Custom emoji",
            Self::Communities => "Communities",
            Self::Appearance => "Appearance",
            Self::Notifications => "Notifications",
            Self::Shortcuts => "Shortcuts",
            Self::Agents => "Agents",
            Self::About => "About",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Profile => IconName::User,
            Self::CustomEmoji => IconName::Smiley,
            Self::Communities => IconName::Globe,
            Self::Appearance => IconName::Palette,
            Self::Notifications => IconName::Bell,
            Self::Shortcuts => IconName::Keyboard,
            Self::Agents => IconName::Robot,
            Self::About => IconName::Info,
        }
    }
}

enum Modal {
    AddHost {
        dialog: Entity<AddHost>,
        _subscription: Subscription,
    },
    /// The Agents page's defaults dialog, for one host.
    Defaults {
        dialog: Entity<DefaultsDialog>,
        _subscriptions: [Subscription; 2],
    },
    RemoveHost {
        host: Entity<HostConnection>,
        error: Option<SharedString>,
    },
    Leave {
        community: Entity<Community>,
        busy: bool,
        error: Option<SharedString>,
    },
    SignOut {
        error: Option<SharedString>,
    },
    /// Shorten retention or clear saved agent activity.
    Archive(archive::Confirm),
}

pub struct SettingsPage {
    section: Section,
    /// Follows the header's sidebar toggle.
    pub show_sidebar: bool,
    profile: ProfileForm,
    modal: Option<Modal>,
    toast: Option<(SharedString, usize)>,
    toast_tone: ToastTone,
    toasts: usize,
    /// Made on first visit; its saves raise this page's toast.
    custom_emoji: Option<(Entity<emoji::CustomEmojiSettings>, Subscription)>,
    /// The notification sound select that is open.
    sound_menu: Option<buzz_relay_client::Category>,
    /// The archive's "Activity retention" select is open.
    retention_menu: bool,
    /// The last archive save or clear failed.
    archive_error: Option<SharedString>,
    _app_observer: Subscription,
    _community_observer: Option<Subscription>,
}

impl SettingsPage {
    /// Opens at `section`, or the default: Profile with a community
    /// selected, otherwise Appearance.
    pub fn new(section: Option<Section>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let has_community = AppState::current(cx).is_some();
        let mut page = Self {
            section: section.unwrap_or(if has_community {
                Section::Profile
            } else {
                Section::Appearance
            }),
            show_sidebar: true,
            profile: ProfileForm::new(window, cx),
            modal: None,
            toast: None,
            toast_tone: ToastTone::Success,
            toasts: 0,
            custom_emoji: None,
            sound_menu: None,
            retention_menu: false,
            archive_error: None,
            _app_observer: cx.observe_global_in::<AppState>(window, |this, window, cx| {
                this.follow_community(window, cx)
            }),
            _community_observer: None,
        };
        page.follow_community(window, cx);
        page
    }

    pub fn open(&mut self, section: Section, cx: &mut Context<Self>) {
        if section != self.section {
            self.quiet();
        }
        self.section = section;
        cx.notify();
    }

    /// Keeps the profile form on the selected community.
    fn follow_community(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let community = AppState::current(cx);
        self._community_observer = community.as_ref().map(|community| {
            cx.observe_in(community, window, |this, community, window, cx| {
                this.profile.sync(Some(&community), window, cx);
                cx.notify();
            })
        });
        self.profile.sync(community.as_ref(), window, cx);
        cx.notify();
    }

    fn show_toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_toast_tone(text, ToastTone::Success, cx);
    }

    fn show_toast_tone(
        &mut self,
        text: impl Into<SharedString>,
        tone: ToastTone,
        cx: &mut Context<Self>,
    ) {
        self.toast_tone = tone;
        self.toasts += 1;
        let generation = self.toasts;
        self.toast = Some((text.into(), generation));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|(_, g)| *g == generation) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        cx.notify();
    }

    /// Closes the sound menu, stops a preview and hides the private key,
    /// on leaving a section.
    fn quiet(&mut self) {
        self.sound_menu = None;
        self.retention_menu = false;
        crate::sounds::stop_preview();
        // Leaving Profile hides the private key, as unmounting it does.
        self.profile.nsec_revealed = false;
        self.profile.nsec_message = None;
    }

    /// The settings sidebar (§5b.2).
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let community = AppState::current(cx).map(|community| community.read(cx).name());
        let item = |section: Section, cx: &mut Context<Self>| {
            NavigationItem::new(
                SharedString::from(format!("settings-{}", section.label())),
                section.label(),
            )
            .sidebar()
            .leading(Icon::new(section.icon()).size(18.))
            .selected(self.section == section)
            .on_click(cx.listener(move |this, _, _, cx| this.open(section, cx)))
        };
        let group = |title: SharedString, items: Vec<NavigationItem>| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .mb(rems(0.25))
                        .px(rems(0.5))
                        .text_caption()
                        .text_color(c.text_metadata)
                        .child(title),
                )
                .child(div().flex().flex_col().gap(rems(0.125)).children(items))
        };
        let mut groups = Vec::new();
        let mut app = Vec::new();
        match community {
            Some(name) => groups.push(group(
                name.into(),
                vec![item(Section::Profile, cx), item(Section::CustomEmoji, cx)],
            )),
            None => app.push(item(Section::Profile, cx)),
        }
        groups.push(group(
            "Communities".into(),
            vec![item(Section::Communities, cx)],
        ));
        for section in [
            Section::Appearance,
            Section::Notifications,
            Section::Shortcuts,
            Section::Agents,
            Section::About,
        ] {
            app.push(item(section, cx));
        }
        groups.push(group("App".into(), app));
        div()
            .id("settings-sidebar")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(260.))
            .h_full()
            .pt(rems(0.5))
            .px(rems(0.5))
            .overflow_y_scroll()
            .border_r_1()
            .border_color(c.border_standard)
            .text_body_sm()
            .child(
                NavigationItem::new("settings-back", "Back")
                    .sidebar()
                    .leading(Icon::new(IconName::ArrowLeft).size(18.))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(CloseSettings), cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .pt(rems(0.75))
                    .pb(rems(0.5))
                    .children(groups),
            )
    }

    fn detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.section {
            Section::Profile => self.profile_section(window, cx),
            Section::CustomEmoji => {
                let (settings, _) = self.custom_emoji.get_or_insert_with(|| {
                    let settings = cx.new(|cx| emoji::CustomEmojiSettings::new(window, cx));
                    let added = cx.subscribe(&settings, |this, _, emoji::EmojiAdded(code), cx| {
                        this.show_toast(format!("Added :{code}:"), cx)
                    });
                    (settings, added)
                });
                settings.clone().into_any_element()
            }
            Section::Communities => self.communities_section(cx),
            Section::Appearance => appearance::render(cx),
            Section::Notifications => self.notifications_section(window, cx),
            Section::Shortcuts => shortcuts::render(cx),
            Section::Agents => self.agents_section(window, cx),
            Section::About => about::render(cx),
        }
    }

    fn modal(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let page = cx.entity().downgrade();
        let close: overlay::Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.close_modal(cx));
        });
        let (content, dismiss): (AnyElement, Option<overlay::Dismiss>) =
            match self.modal.as_ref()? {
                Modal::AddHost { dialog, .. } => (dialog.clone().into_any_element(), None),
                Modal::Defaults { dialog, .. } => (dialog.clone().into_any_element(), None),
                Modal::RemoveHost { host, error } => (
                    self.remove_host_dialog(host, error.clone(), cx),
                    Some(close),
                ),
                Modal::Leave {
                    community,
                    busy,
                    error,
                } => (
                    self.leave_dialog(community, *busy, error.clone(), cx),
                    (!*busy).then_some(close),
                ),
                Modal::SignOut { error } => (self.sign_out_dialog(error.clone(), cx), Some(close)),
                Modal::Archive(confirm) => (self.archive_dialog(*confirm, cx), Some(close)),
            };
        Some(overlay::modal(window, content, dismiss).into_any_element())
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let detail = self.detail(window, cx);
        let modal = self.modal(window, cx);
        let toast = self.toast.as_ref().map(|(text, generation)| {
            let right = window.viewport_size().width - px(16.) - px(384.);
            deferred(
                anchored()
                    .position_mode(AnchoredPositionMode::Window)
                    .position(point(right, px(128.)))
                    .child(
                        Toast::new(("settings-toast", *generation), text.clone())
                            .tone(self.toast_tone)
                            .on_dismiss(cx.listener(|this, _, _, cx| {
                                this.toast = None;
                                cx.notify();
                            })),
                    ),
            )
            .with_priority(3)
        });
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .text_color(c.text_standard)
            .when(self.show_sidebar, |this| this.child(self.sidebar(cx)))
            .child(
                // `.detail`: 32 / 24 / 24 padding; every child is capped at
                // 768px and centered.
                div()
                    .id("settings-detail")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .pt(rems(2.))
                    .px(rems(1.5))
                    .pb(rems(1.5))
                    .child(div().w_full().max_w(rems(48.)).mx_auto().child(detail)),
            )
            .children(modal)
            .children(toast)
    }
}

// ----- shared settings components (§5b.4) -----

/// The inset that lines titles up with group content (16 + 1px border).
fn inset() -> Rems {
    rems(1.0625)
}

/// The section `Header`: `text-heading` title, caption subtitle, 32px
/// below.
fn header(title: &'static str, subtitle: Option<SharedString>, cx: &App) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(rems(0.5))
        .px(inset())
        .child(div().text_heading().child(title))
        .children(subtitle.map(|text| div().text_caption().text_color(c.text_subtle).child(text)))
}

/// A subsection `InlineHeader`: `text-label-sm` title, caption subtitle,
/// actions on the right, 12px below.
fn inline_header(
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    actions: Vec<AnyElement>,
    cx: &App,
) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .items_start()
        .gap(rems(1.))
        .mb(rems(0.75))
        .px(inset())
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(rems(0.5))
                .child(div().text_label_sm().child(title.into()))
                .children(
                    subtitle.map(|text| div().text_caption().text_color(c.text_subtle).child(text)),
                ),
        )
        .child(div().flex().items_center().gap(rems(0.5)).children(actions))
}

/// `SettingsGroup layout="rows"`: an outlined card with a divider between
/// rows.
fn rows_group(rows: Vec<AnyElement>, cx: &App) -> Div {
    let c = cx.colors();
    let count = rows.len();
    div()
        .flex()
        .flex_col()
        .px(rems(1.))
        .border_1()
        .border_color(c.border_standard)
        .rounded(radius::CONTAINER)
        .children(rows.into_iter().enumerate().map(move |(index, row)| {
            div()
                .when(index + 1 < count, |this| {
                    this.border_b_1().border_color(c.border_standard)
                })
                .child(row)
        }))
}

/// `SettingsGroup layout="form"`: an outlined card with 16px gaps.
fn form_group(children: Vec<AnyElement>, cx: &App) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(rems(1.))
        .p(rems(1.))
        .border_1()
        .border_color(c.border_standard)
        .rounded(radius::CONTAINER)
        .children(children)
}

/// `PreferenceRow`: optional 20px icon, label and description, trailing
/// controls on the right.
fn preference_row(
    icon: Option<AnyElement>,
    title: impl Into<SharedString>,
    description: Option<AnyElement>,
    trailing: Vec<AnyElement>,
    cx: &App,
) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .items_center()
        .gap(rems(1.))
        .py(rems(0.75))
        .children(icon.map(|icon| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(rems(1.25))
                .text_color(c.text_subtle)
                .child(icon)
        }))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(rems(0.125))
                .child(div().text_label_sm().child(title.into()))
                .children(
                    description
                        .map(|text| div().text_caption().text_color(c.text_subtle).child(text)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap(rems(0.5))
                .children(trailing),
        )
}

/// A `Field`: label, control, then the description.
fn field(
    label: &'static str,
    control: impl IntoElement,
    description: Option<SharedString>,
    error: Option<SharedString>,
    cx: &App,
) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(rems(0.5))
        .min_w_0()
        .child(div().text_label_sm().child(label))
        .child(control)
        .children(
            description.map(|text| div().text_caption().text_color(c.text_subtle).child(text)),
        )
        .children(error.map(|text| div().text_caption().text_color(c.text_danger).child(text)))
}

/// Settings buttons use 12px corners instead of pills.
fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id, label).shape(ButtonShape::Control)
}

/// A plain status line in place of a section's content.
fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    let c = cx.colors();
    div()
        .px(inset())
        .text_body()
        .text_color(c.text_subtle)
        .child(text.into())
}

fn section(children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(rems(2.))
        .children(children)
        .into_any_element()
}

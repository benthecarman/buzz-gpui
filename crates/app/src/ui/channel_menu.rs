//! The channel header's actions (ui.md §4.2): "Channel members", the
//! "Channel actions" menu with "View channel details" and the lifecycle
//! actions (`ChannelHeaderMenu.tsx`, `ChannelLifecycleMenu.tsx`), and the
//! read-only preview's join notice (§4.12, `ChannelJoinNotice.tsx`).
//!
//! "View channel details" opens "Channel settings" through
//! [`OpenSettings`]. Without one set, the settings panel shows in a modal
//! here, until the channel's tab pane hosts it.

use super::canvas::CanvasDialog;
use super::channel_settings::{
    Callback, ChannelRemoved, ChannelSettingsPanel, LifecycleDialog, LifecycleOutcome,
    lifecycle_modal,
};
use super::members::MembersDialog;
use super::modal::Dismissed;
use super::overlay::{self, Align, Dismiss};
use crate::community::Community;
use buzz_relay_client::lifecycle::{Action, ActionError, LifecyclePermissions};
use buzz_relay_client::{ChannelSummary, ChannelType};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, Icon, IconButton, IconButtonSize, IconName, Menu, MenuItem,
    MenuLabel, MenuSeparator, TypeStyled,
    tokens::{radius, shadow, size, space},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// Called after the viewer left, hid or deleted the channel, so the page
/// closes.
pub type OnRemoved = Rc<dyn Fn(&mut Window, &mut App)>;

/// A channel panel the menu opens as a tab, such as "View channel usage".
pub type MenuPanel = (SharedString, Rc<dyn Fn(&mut Window, &mut App)>);

/// Opens "Channel settings" for a channel id. The channel's tab pane sets
/// this to open its settings tab; until then the header shows the panel
/// in a modal.
pub type OpenSettings = Rc<dyn Fn(&str, &mut Window, &mut App)>;

enum Modal {
    Members {
        dialog: Entity<MembersDialog>,
        _subscription: Subscription,
    },
    Settings {
        panel: Entity<ChannelSettingsPanel>,
        _subscriptions: [Subscription; 1],
    },
    Confirm {
        dialog: Entity<LifecycleDialog>,
        _subscription: Subscription,
    },
    Canvas {
        dialog: Entity<CanvasDialog>,
        _subscription: Subscription,
    },
}

/// The header's "Channel members" and "Channel actions" buttons, with the
/// menu and dialogs they open.
pub struct ChannelActions {
    community: Entity<Community>,
    channel: ChannelSummary,
    menu_open: bool,
    /// Lifecycle permissions, read when the menu opens.
    permissions: Option<Result<LifecyclePermissions, String>>,
    permissions_generation: u64,
    modal: Option<Modal>,
    on_removed: OnRemoved,
    /// Registered channel panels' entries, set by the page each frame.
    pub menu_panels: Vec<MenuPanel>,
    open_settings: Option<OpenSettings>,
}

impl ChannelActions {
    pub fn new(
        community: Entity<Community>,
        channel: ChannelSummary,
        on_removed: OnRemoved,
    ) -> Self {
        Self {
            community,
            channel,
            menu_open: false,
            permissions: None,
            permissions_generation: 0,
            modal: None,
            on_removed,
            menu_panels: Vec::new(),
            open_settings: None,
        }
    }

    /// Routes "View channel details" to the host's settings tab.
    #[allow(dead_code, reason = "the channel tab pane routes settings here")]
    pub fn set_open_settings(&mut self, open: OpenSettings) {
        self.open_settings = Some(open);
    }

    /// Opens the members dialog, as the header button does.
    pub fn open_members(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = cx
            .new(|cx| MembersDialog::new(self.community.clone(), self.channel.clone(), window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close(cx));
        self.modal = Some(Modal::Members {
            dialog,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn open_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        if let Some(open) = self.open_settings.clone() {
            open(&self.channel.id, window, cx);
            cx.notify();
            return;
        }
        let page = cx.entity().downgrade();
        let on_close: Callback = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.close(cx));
        });
        let page = cx.entity().downgrade();
        let open_members: Callback = Rc::new(move |window, cx| {
            let _ = page.update(cx, |this, cx| this.open_members(window, cx));
        });
        let (community, channel) = (self.community.clone(), self.channel.clone());
        let panel = cx.new(|cx| {
            ChannelSettingsPanel::new(
                community,
                channel.id.clone(),
                on_close,
                open_members,
                window,
                cx,
            )
            .with_channel(channel, window, cx)
        });
        panel.read(cx).focus_handle(cx).focus(window, cx);
        let removed = cx.subscribe_in(&panel, window, |this, _, _: &ChannelRemoved, window, cx| {
            this.close(cx);
            (this.on_removed)(window, cx);
        });
        self.modal = Some(Modal::Settings {
            panel,
            _subscriptions: [removed],
        });
        cx.notify();
    }

    /// "View canvas": the channel's shared Markdown.
    fn open_canvas(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        let (community, channel) = (self.community.clone(), self.channel.id.clone());
        let dialog = cx.new(|cx| CanvasDialog::new(community, channel, window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close(cx));
        self.modal = Some(Modal::Canvas {
            dialog,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        self.community
            .update(cx, |community, _| community.set_presence_extra(Vec::new()));
        cx.notify();
    }

    /// The menu offers lifecycle actions only for joined, live channels.
    fn lifecycle_shown(&self) -> bool {
        !self.channel.read_only && !self.channel.cached && !self.channel.archived
    }

    fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = true;
        if self.lifecycle_shown() {
            self.load_permissions(window, cx);
        }
        cx.notify();
    }

    fn load_permissions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.permissions = None;
        self.permissions_generation += 1;
        let generation = self.permissions_generation;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let permissions = session.channel_permissions(&channel).await;
            let _ = this.update(cx, |this, cx| {
                if this.permissions_generation == generation {
                    this.permissions = Some(permissions);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn confirm(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        let dialog = cx.new(|cx| {
            LifecycleDialog::new(self.community.clone(), &self.channel, action, window, cx)
        });
        let subscription = cx.subscribe_in(
            &dialog,
            window,
            |this, _, outcome: &LifecycleOutcome, window, cx| {
                this.close(cx);
                if let LifecycleOutcome::Completed(Action::Leave | Action::Hide | Action::Delete) =
                    outcome
                {
                    (this.on_removed)(window, cx);
                }
            },
        );
        self.modal = Some(Modal::Confirm {
            dialog,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn dismiss_menu(cx: &mut Context<Self>) -> Dismiss {
        let weak = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.menu_open = false;
                cx.notify();
            });
        })
    }

    fn menu(&self, cx: &mut Context<Self>) -> Menu {
        let item = |id: &'static str, label: &'static str, icon: IconName| {
            MenuItem::new(id, label).icon(icon).icon_size(14.)
        };
        let panels = self
            .menu_panels
            .iter()
            .enumerate()
            .map(|(index, (label, open))| {
                let open = open.clone();
                MenuItem::new(("menu-panel", index), label.clone())
                    .icon(IconName::Info)
                    .icon_size(14.)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu_open = false;
                        open(window, cx);
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();
        // The sidebar's row actions: move, mute, read, copy link.
        let shared = super::sidebar::header_actions(
            &self.community,
            &self.channel,
            Self::dismiss_menu(cx),
            cx,
        );
        let mut menu = Menu::new()
            .child(
                MenuItem::new("view-details", "View channel details")
                    .icon(IconName::Info)
                    .icon_size(16.)
                    .on_click(cx.listener(|this, _, window, cx| this.open_details(window, cx))),
            )
            .when(!self.channel.read_only, |menu| {
                menu.child(
                    MenuItem::new("view-canvas", "View canvas")
                        .icon(IconName::FileText)
                        .icon_size(16.)
                        .on_click(cx.listener(|this, _, window, cx| this.open_canvas(window, cx))),
                )
            })
            .children(panels)
            .when(!shared.is_empty(), |menu| {
                menu.child(MenuSeparator::new()).children(shared)
            });
        if !self.lifecycle_shown() {
            return menu;
        }
        let confirm = |action: Action| {
            cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.confirm(action, window, cx)
            })
        };
        let retry = || {
            cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
                this.load_permissions(window, cx)
            })
        };
        match &self.permissions {
            // `ChannelLifecycleMenu`: nothing until the permissions arrive.
            None => {}
            Some(Err(_)) => {
                menu = menu
                    .child(MenuSeparator::new())
                    .child(MenuLabel::new("Channel actions unavailable"))
                    .child(
                        item(
                            "retry-permissions",
                            "Retry channel permissions",
                            IconName::ArrowClockwise,
                        )
                        .on_click(retry()),
                    );
            }
            Some(Ok(permissions)) => {
                let any = permissions.can_hide
                    || permissions.can_leave
                    || permissions.can_archive
                    || permissions.can_unarchive
                    || permissions.can_delete
                    || permissions.delete_unavailable;
                if !any {
                    return menu;
                }
                menu = menu.child(MenuSeparator::new());
                if permissions.can_hide {
                    return menu.child(
                        item("hide", "Hide conversation", IconName::EyeSlash)
                            .on_click(confirm(Action::Hide)),
                    );
                }
                if permissions.can_leave {
                    menu = menu.child(
                        item("leave", "Leave channel", IconName::SignOut)
                            .on_click(confirm(Action::Leave)),
                    );
                }
                if permissions.can_archive || permissions.can_unarchive {
                    let (label, action) = if permissions.can_unarchive {
                        ("Unarchive channel", Action::Unarchive)
                    } else {
                        ("Archive channel", Action::Archive)
                    };
                    menu = menu
                        .child(item("archive", label, IconName::Archive).on_click(confirm(action)));
                }
                // An owner agent's check could not be read.
                if permissions.delete_unavailable {
                    menu = menu
                        .child(
                            item(
                                "delete-unavailable",
                                "Delete check unavailable",
                                IconName::WarningCircle,
                            )
                            .disabled(true),
                        )
                        .child(
                            item(
                                "retry-delete",
                                "Retry Delete check",
                                IconName::ArrowClockwise,
                            )
                            .on_click(retry()),
                        );
                }
                if permissions.can_delete {
                    menu = menu.child(
                        item("delete", "Delete channel", IconName::Trash)
                            .danger()
                            .on_click(confirm(Action::Delete)),
                    );
                }
            }
        }
        menu
    }

    /// The settings panel in a modal, with the tab's header.
    fn settings_modal(
        &self,
        panel: &Entity<ChannelSettingsPanel>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let height = (window.viewport_size().height - px(48.)).min(px(760.));
        div()
            .flex()
            .flex_col()
            .w(rems(26.))
            .h(height)
            .bg(c.surface_panel)
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::PANEL)
            .shadow(shadow::sm())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(size::PANEL_HEADER)
                    .px(space::S2)
                    .mt(space::S2)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::S2)
                            .px(space::S3)
                            .h(size::CONTROL_SM)
                            .rounded(radius::ICON_BUTTON)
                            .bg(c.affordance_selected)
                            .text_label_sm()
                            .text_color(c.text_standard)
                            .child(Icon::new(IconName::Gear).size(16.))
                            .child("Channel settings"),
                    )
                    .child(
                        IconButton::new("close-settings", IconName::X)
                            .size(IconButtonSize::Sm)
                            .tooltip("Close channel settings")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(panel.clone()))
            .into_any_element()
    }
}

impl Render for ChannelActions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(current) = self
            .community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .find(|c| c.id == self.channel.id)
        {
            self.channel = current.clone();
        }
        let menu = self
            .menu_open
            .then(|| overlay::dropdown(window, Align::End, self.menu(cx), Self::dismiss_menu(cx)));
        let page = cx.entity().downgrade();
        let close: Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.close(cx));
        });
        let modal = match &self.modal {
            Some(Modal::Members { dialog, .. }) => {
                Some(overlay::modal(window, dialog.clone(), Some(close)).into_any_element())
            }
            Some(Modal::Settings { panel, .. }) => {
                let content = self.settings_modal(panel, window, cx);
                Some(overlay::modal(window, content, Some(close)).into_any_element())
            }
            Some(Modal::Confirm { dialog, .. }) => Some(lifecycle_modal(dialog, window)),
            Some(Modal::Canvas { dialog, .. }) => {
                let weak = dialog.downgrade();
                let dismiss: Dismiss = Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |dialog, cx| dialog.request_close(cx));
                });
                Some(overlay::modal(window, dialog.clone(), Some(dismiss)).into_any_element())
            }
            None => None,
        };
        div()
            .flex()
            .items_center()
            .gap(rems(0.25))
            .child(
                IconButton::new("members", IconName::Users)
                    .size(IconButtonSize::Sm)
                    .tooltip("Channel members")
                    .selected(matches!(self.modal, Some(Modal::Members { .. })))
                    .on_click(cx.listener(|this, _, window, cx| this.open_members(window, cx))),
            )
            .child(
                div()
                    .relative()
                    .child(
                        IconButton::new("more", IconName::DotsThree)
                            .size(IconButtonSize::Sm)
                            .when(!self.menu_open, |b| b.tooltip("Channel actions"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_menu(window, cx)),
                            ),
                    )
                    .children(menu),
            )
            .children(modal)
    }
}

// ---------------------------------------------------------------------------
// Join notice

/// "Read-only preview · You haven’t joined this conversation.", with Join
/// for streams and forums (`ChannelJoinNotice`).
pub struct JoinNotice {
    community: Entity<Community>,
    channel: ChannelSummary,
    pending: bool,
    error: Option<SharedString>,
}

/// A join started; the page focuses its composer once the channel is
/// writable.
pub struct Joining;

impl EventEmitter<Joining> for JoinNotice {}

impl JoinNotice {
    pub fn new(community: Entity<Community>, channel: ChannelSummary) -> Self {
        Self {
            community,
            channel,
            pending: false,
            error: None,
        }
    }

    fn join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.pending = true;
        self.error = None;
        cx.emit(Joining);
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.channel_action(Action::Join, &channel).await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                if let Err(error) = result {
                    this.error = Some(join_error(error).into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for JoinNotice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let joinable = matches!(
            self.channel.channel_type,
            Some(ChannelType::Stream | ChannelType::Forum)
        ) && !self.channel.archived;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(8.))
            .mb(px(8.))
            .text_body_sm()
            .text_color(c.text_subtle)
            .child("Read-only preview · You haven’t joined this conversation.")
            .when(joinable, |this| {
                this.child(
                    Button::new("join-channel", "Join channel")
                        .size(ButtonSize::Sm)
                        .loading(self.pending)
                        .on_click(cx.listener(|this, _, window, cx| this.join(window, cx))),
                )
                .children(self.error.clone())
            })
    }
}

/// The join's failure as `ChannelJoinNotice` words it.
fn join_error(error: ActionError) -> String {
    match error {
        ActionError::Unconfirmed(_) => {
            "Join may have taken effect, but it is not confirmed yet. Try again to check.".into()
        }
        ActionError::Failed(error) if error.is_empty() => "Couldn’t join this channel.".into(),
        ActionError::Failed(error) => error,
    }
}

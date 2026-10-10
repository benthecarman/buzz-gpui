//! The channel header's actions (ui.md §4.2): "Channel members", the
//! "Channel actions" menu with channel details and the lifecycle actions
//! (`ChannelHeaderMenu.tsx`, `ChannelLifecycleMenu.tsx`,
//! `ChannelLifecycleDialog.tsx`, `ChannelDetailsEditor.tsx`), and the
//! read-only preview's join notice (§4.12, `ChannelJoinNotice.tsx`).

use super::members::MembersDialog;
use super::modal::{Dismissed, error};
use super::overlay::{self, Align, Dismiss};
use crate::community::Community;
use buzz_relay_client::lifecycle::{Action, ChannelDetails, DetailsDraft, LifecyclePermissions};
use buzz_relay_client::{ChannelSummary, ChannelType, Visibility};
use buzz_ui::{
    ActiveTheme, AlertDialog, Button, ButtonSize, Dialog, IconButton, IconButtonSize, IconName,
    Menu, MenuItem, MenuSeparator, Spinner, Surface, Switch, TextArea, TextField, TypeStyled,
};
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// Called after the viewer left, hid or deleted the channel, so the page
/// closes.
pub type OnRemoved = Rc<dyn Fn(&mut Window, &mut App)>;

/// A channel panel the menu opens as a tab, such as "View channel usage".
pub type MenuPanel = (SharedString, Rc<dyn Fn(&mut Window, &mut App)>);

enum Modal {
    Members {
        dialog: Entity<MembersDialog>,
        _subscription: Subscription,
    },
    Details {
        dialog: Entity<DetailsDialog>,
        _subscription: Subscription,
    },
    Confirm(Action),
}

/// The header's "Channel members" and "Channel actions" buttons, with the
/// menu and dialogs they open.
pub struct ChannelActions {
    community: Entity<Community>,
    channel: ChannelSummary,
    menu_open: bool,
    /// Lifecycle permissions, read when the menu opens.
    permissions: Option<Result<LifecyclePermissions, String>>,
    modal: Option<Modal>,
    busy: bool,
    error: Option<SharedString>,
    on_removed: OnRemoved,
    /// Registered channel panels' entries, set by the page each frame.
    pub menu_panels: Vec<MenuPanel>,
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
            modal: None,
            busy: false,
            error: None,
            on_removed,
            menu_panels: Vec::new(),
        }
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
        let dialog = cx
            .new(|cx| DetailsDialog::new(self.community.clone(), self.channel.clone(), window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close(cx));
        self.modal = Some(Modal::Details {
            dialog,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        self.error = None;
        self.community
            .update(cx, |community, _| community.set_presence_extra(Vec::new()));
        cx.notify();
    }

    fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = true;
        self.permissions = None;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let permissions = session.channel_permissions(&channel).await;
            let _ = this.update(cx, |this, cx| {
                this.permissions = Some(permissions);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.channel_action(action, &channel).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.modal = None;
                        if matches!(action, Action::Leave | Action::Hide | Action::Delete) {
                            (this.on_removed)(window, cx);
                        }
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
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
        let link = format!("buzz://channel/{}", self.channel.id);
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
            });
        let mut menu = Menu::new()
            .child(
                item("view-details", "View channel details", IconName::Info)
                    .on_click(cx.listener(|this, _, window, cx| this.open_details(window, cx))),
            )
            .children(panels)
            .child(
                item("copy-link", "Copy link", IconName::Link).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                        this.menu_open = false;
                        cx.notify();
                    },
                )),
            );
        let confirm = |action: Action| {
            cx.listener(move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx| {
                this.menu_open = false;
                this.modal = Some(Modal::Confirm(action));
                cx.notify();
            })
        };
        match &self.permissions {
            None => {
                menu = menu.child(MenuSeparator::new()).child(
                    item(
                        "checking",
                        "Checking channel actions…",
                        IconName::CircleNotch,
                    )
                    .disabled(true),
                );
            }
            Some(Err(_)) if self.channel.read_only => {}
            Some(Err(_)) => {
                menu = menu.child(MenuSeparator::new()).child(
                    item(
                        "unavailable",
                        "Channel actions unavailable",
                        IconName::WarningCircle,
                    )
                    .disabled(true),
                );
            }
            Some(Ok(permissions)) => {
                let any = permissions.can_hide
                    || permissions.can_leave
                    || permissions.can_archive
                    || permissions.can_unarchive
                    || permissions.can_delete
                    || permissions.delete_unavailable;
                if any {
                    menu = menu.child(MenuSeparator::new());
                }
                if permissions.can_hide {
                    menu = menu.child(
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
                if permissions.can_archive {
                    menu = menu.child(
                        item("archive", "Archive channel", IconName::Archive)
                            .on_click(confirm(Action::Archive)),
                    );
                }
                if permissions.can_unarchive {
                    menu = menu.child(
                        item("unarchive", "Unarchive channel", IconName::Archive)
                            .on_click(confirm(Action::Unarchive)),
                    );
                }
                // An owner agent's check could not be read; reopening the
                // menu reads it again.
                if permissions.delete_unavailable {
                    menu = menu.child(
                        item(
                            "delete-unavailable",
                            "Delete check unavailable",
                            IconName::WarningCircle,
                        )
                        .disabled(true),
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

    fn confirmation(&self, action: Action, cx: &mut Context<Self>) -> AnyElement {
        let (label, description) = match action {
            Action::Archive => (
                "Archive channel",
                "Archive this channel for everyone and remove it from the sidebar. Messages are \
                 kept. Find it in search and open Settings to unarchive it later.",
            ),
            Action::Unarchive => (
                "Unarchive channel",
                "Unarchive this channel for everyone and return it to the sidebar. Members can \
                 send messages again.",
            ),
            Action::Leave => (
                "Leave channel",
                "Leave this channel and remove it from your sidebar. You may need an invitation \
                 to rejoin a private channel.",
            ),
            Action::Hide => (
                "Hide conversation",
                "Hide this conversation from your sidebar only. Messages and membership are \
                 kept; other participants are not removed.",
            ),
            Action::Delete => (
                "Delete channel",
                "Delete this channel for everyone. You cannot undo this action from Buzz.",
            ),
            Action::Join => ("Join channel", ""),
        };
        // `ChannelLifecycleDialog.tsx`: only Delete is destructive.
        let destructive = action == Action::Delete;
        let profiles = &self.community.read(cx).snapshot.profiles;
        let name = super::sidebar::channel_label(&self.channel, profiles);
        AlertDialog::new(format!("{label}: {name}"))
            .description(description)
            .children(self.error.clone().map(|message| error(message, cx)))
            .action(
                Button::new("lifecycle-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .action(
                Button::new("lifecycle-confirm", label)
                    .map(|b| {
                        if destructive {
                            b.destructive()
                        } else {
                            b.prominent()
                        }
                    })
                    .loading(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| this.run(action, window, cx))),
            )
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
        let modal = self.modal.as_ref().map(|modal| {
            let page = cx.entity().downgrade();
            let close: Dismiss = Rc::new(move |_, cx| {
                let _ = page.update(cx, |this, cx| this.close(cx));
            });
            let (content, dismiss) = match modal {
                Modal::Members { dialog, .. } => (dialog.clone().into_any_element(), Some(close)),
                Modal::Details { dialog, .. } => (dialog.clone().into_any_element(), Some(close)),
                Modal::Confirm(action) => (
                    self.confirmation(*action, cx),
                    (!self.busy).then_some(close),
                ),
            };
            overlay::modal(window, content, dismiss)
        });
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
// Channel details

/// "View channel details": name, description and privacy, editable by the
/// channel's owners and admins.
pub struct DetailsDialog {
    community: Entity<Community>,
    channel: ChannelSummary,
    details: Option<Result<ChannelDetails, String>>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    private: bool,
    saving: bool,
    error: Option<SharedString>,
}

impl EventEmitter<Dismissed> for DetailsDialog {}

impl DetailsDialog {
    pub fn new(
        community: Entity<Community>,
        channel: ChannelSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("release-notes"));
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("What this channel is for")
                .auto_grow(3, 8)
        });
        let session = community.read(cx).session.clone();
        let id = channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let details = session.channel_details(&id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Ok(details) = &details {
                    let draft = &details.draft;
                    this.name
                        .update(cx, |s, cx| s.set_value(draft.name.clone(), window, cx));
                    this.description.update(cx, |s, cx| {
                        s.set_value(draft.description.clone(), window, cx)
                    });
                    this.private = draft.private;
                }
                this.details = Some(details);
                cx.notify();
            });
        })
        .detach();
        Self {
            private: channel.visibility == Some(Visibility::Private),
            community,
            channel,
            details: None,
            name,
            description,
            saving: false,
            error: None,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(details)) = &self.details else {
            return;
        };
        let draft = DetailsDraft {
            name: buzz_relay_client::lifecycle::canonical_name(&self.name.read(cx).value()),
            description: self.description.read(cx).value().trim().to_string(),
            private: self.private,
            ttl: details.draft.ttl,
        };
        if draft == details.draft {
            cx.emit(Dismissed);
            return;
        }
        let (version, channel) = (details.version, details.channel.clone());
        let session = self.community.read(cx).session.clone();
        self.saving = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = session.save_channel_details(&channel, version, draft).await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(()) => cx.emit(Dismissed),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for DetailsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let weak = cx.entity().downgrade();
        let editable = matches!(&self.details, Some(Ok(details)) if details.can_edit);
        let dm = self.channel.channel_type == Some(ChannelType::Dm);
        let label = |text: &'static str| div().text_label_sm().child(text);
        let body: AnyElement = match &self.details {
            None => div()
                .flex()
                .justify_center()
                .py(rems(2.))
                .child(Spinner::new().size(24.))
                .into_any_element(),
            Some(Err(message)) => error(message.clone(), cx).into_any_element(),
            Some(Ok(details)) => {
                let name_len = self.name.read(cx).value().chars().count();
                let about_len = self.description.read(cx).value().chars().count();
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .child(
                                div().flex().justify_between().child(label("Name")).child(
                                    div()
                                        .text_body_sm()
                                        .text_color(c.text_subtle)
                                        .child(format!("{name_len}/120")),
                                ),
                            )
                            .child(
                                TextField::new(&self.name)
                                    .surface(Surface::Floating)
                                    .disabled(!editable),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(label("Description"))
                                    .child(
                                        div()
                                            .text_body_sm()
                                            .text_color(c.text_subtle)
                                            .child(format!("{about_len}/1000")),
                                    ),
                            )
                            .child(
                                TextArea::new(&self.description)
                                    .surface(Surface::Floating)
                                    .disabled(!editable),
                            ),
                    )
                    .when(!dm, |this| {
                        this.child(
                            Switch::new("details-private", self.private)
                                .label("Private")
                                .disabled(!editable)
                                .on_change(move |checked, _, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.private = checked;
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                    .when(!editable, |this| {
                        this.child(div().text_body_sm().text_color(c.text_subtle).child(if dm {
                            "Direct message details can’t be changed."
                        } else if details.draft.ttl.is_some() || self.channel.archived {
                            "This channel’s details can’t be changed now."
                        } else {
                            "Only channel owners and admins can change these details."
                        }))
                    })
                    .children(self.error.clone().map(|message| error(message, cx)))
                    .into_any_element()
            }
        };
        let mut dialog = Dialog::new("channel-details", "Channel details")
            .description(self.channel.name.clone())
            .compact_header_gap()
            .on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
            .child(body);
        if editable {
            dialog = dialog
                .footer(
                    Button::new("details-cancel", "Cancel")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
                )
                .footer(
                    Button::new("details-save", "Save")
                        .prominent()
                        .loading(self.saving)
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                );
        }
        dialog
    }
}

// ---------------------------------------------------------------------------
// Join notice

/// "Read-only preview · You haven’t joined this conversation.", with Join
/// for public streams and forums.
pub struct JoinNotice {
    community: Entity<Community>,
    channel: ChannelSummary,
    pending: bool,
    error: Option<SharedString>,
}

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
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.channel_action(Action::Join, &channel).await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                if let Err(error) = result {
                    this.error = Some(error.into());
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
        ) && self.channel.visibility == Some(Visibility::Public)
            && !self.channel.archived;
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

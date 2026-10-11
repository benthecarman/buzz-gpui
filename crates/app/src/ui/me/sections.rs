//! Me's sidebar (`me/MeSidebar.tsx`) and its grouped history
//! (`sessions/SessionSections.tsx`): "New conversation", the viewer's Me
//! groups, then "Conversations", each row with its actions menu. Rows share
//! the Messages sidebar's working indicator and activity popover.
//!
//! Groups and moves go through the Me groups store, which shows a move at
//! once and keeps a failure until it is retried or dismissed.

use super::{MeModel, Selection, dialogs};
use crate::community::{AppState, Community};
use crate::ui::overlay::{self, Align, Dismiss};
use crate::ui::sidebar::menus::channel_link;
use crate::ui::toasts::{self, Notice};
use buzz_relay_client::sidebar::NewSection;
use buzz_relay_client::sidebar_store::PreferencesStatus;
use buzz_relay_client::{ChannelSummary, RosterStatus, SessionStatus};
use buzz_ui::{
    ActiveTheme, Button, Icon, IconButton, IconButtonSize, IconName, Menu, MenuItem, Surface,
    ToastTone, TypeStyled, tokens::radius,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::time::Duration;

/// Collapsed groups, saved per community (`me:collapsed-sections`).
const COLLAPSED: &str = "me:collapsed-sections";
/// Menus paint above dialogs (`overlay.rs`).
const MENU_LAYER: usize = 3;

/// A row's open actions menu: from its button, or at the pointer.
struct RowMenu {
    id: String,
    title: String,
    /// The group the row is in; empty for Conversations.
    section: String,
    at: Option<Point<Pixels>>,
    sub: Sub,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sub {
    None,
    Move,
    Copy,
}

/// One group of rows: a Me group, or Conversations (empty id).
struct Group {
    id: String,
    name: String,
    rows: Vec<ChannelSummary>,
}

pub struct MeSidebar {
    model: Entity<MeModel>,
    collapsed: Vec<String>,
    /// The community `collapsed` was read for.
    collapsed_for: Option<EntityId>,
    menu: Option<RowMenu>,
    /// The group whose header menu is open.
    section_menu: Option<String>,
    dialog: Option<dialogs::Dialog>,
    deleting: Option<String>,
    delete_error: Option<SharedString>,
    _model: Subscription,
}

impl MeSidebar {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = MeModel::global(cx);
        let subscription = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            collapsed: Vec::new(),
            collapsed_for: None,
            menu: None,
            section_menu: None,
            dialog: None,
            deleting: None,
            delete_error: None,
            _model: subscription,
        }
    }

    fn community(&self, cx: &App) -> Option<Entity<Community>> {
        self.model.read(cx).community.clone()
    }

    /// Reads the collapsed groups when the community changes.
    fn load_collapsed(&mut self, community: &Entity<Community>, cx: &App) {
        if self.collapsed_for == Some(community.entity_id()) {
            return;
        }
        self.collapsed_for = Some(community.entity_id());
        self.collapsed = cx
            .global::<AppState>()
            .store
            .view_state(&community.read(cx).partition(), COLLAPSED)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
    }

    fn toggle(&mut self, id: &str, open: bool, cx: &mut Context<Self>) {
        self.collapsed.retain(|key| key != id);
        if !open {
            self.collapsed.push(id.to_owned());
        }
        if let Some(community) = self.community(cx) {
            let json = serde_json::to_string(&self.collapsed).expect("ids serialize");
            let _ = cx.global::<AppState>().store.set_view_state(
                &community.read(cx).partition(),
                COLLAPSED,
                Some(&json),
            );
        }
        cx.notify();
    }

    fn select(&self, selection: Selection, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, cx| model.select(selection, cx));
    }

    fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        self.section_menu = None;
        cx.notify();
    }

    /// Moves a conversation to a group (`None`: Conversations).
    fn move_to(&mut self, id: &str, section: Option<String>, cx: &mut Context<Self>) {
        self.close_menus(cx);
        let Some(community) = self.community(cx) else {
            return;
        };
        self.toggle(section.as_deref().unwrap_or(""), true, cx);
        let kit = community.read(cx).session.me_kit().clone();
        let id = id.to_owned();
        cx.background_spawn(async move {
            let _ = kit.me_assign(&id, section).await;
        })
        .detach();
    }

    fn copy(&mut self, value: String, label: &str, cx: &mut Context<Self>) {
        self.close_menus(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(value));
        // GPUI's clipboard write cannot fail, so there is no error notice.
        toasts::show(
            Notice {
                key: "me-copy".into(),
                title: format!("{label} copied").into(),
                description: None,
                tone: ToastTone::Success,
                timeout: Some(Duration::from_secs(4)),
                dismissible: true,
                actions: Vec::new(),
            },
            cx,
        );
    }

    fn delete_section(&mut self, id: String, cx: &mut Context<Self>) {
        self.close_menus(cx);
        let Some(community) = self.community(cx) else {
            return;
        };
        self.deleting = Some(id.clone());
        self.delete_error = None;
        let kit = community.read(cx).session.me_kit().clone();
        cx.spawn(async move |this, cx| {
            let result = kit.me_remove_section(&id).await;
            let _ = this.update(cx, |this, cx| {
                this.deleting = None;
                if let Err(error) = result {
                    this.delete_error = Some(if error.is_empty() {
                        "Section could not be deleted. Try again.".into()
                    } else {
                        error.into()
                    });
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_dialog(&mut self, dialog: dialogs::Dialog, cx: &mut Context<Self>) {
        self.close_menus(cx);
        self.dialog = Some(dialog);
        cx.notify();
    }

    /// The groups and their rows (`sections` in `SessionSections`).
    fn groups(&self, conversations: &[ChannelSummary], cx: &App) -> Vec<Group> {
        let me = &self.model.read(cx).me;
        let data = me.data.as_ref();
        let sections = data.map(|d| d.sections.clone()).unwrap_or_default();
        let assigned = |id: &str| data.and_then(|d| d.assignments.get(id).cloned());
        let mut groups: Vec<Group> = sections
            .iter()
            .map(|section| Group {
                id: section.id.clone(),
                name: section.name.clone(),
                rows: conversations
                    .iter()
                    .filter(|c| assigned(&c.id).as_deref() == Some(section.id.as_str()))
                    .cloned()
                    .collect(),
            })
            .collect();
        let ids: Vec<&String> = sections.iter().map(|s| &s.id).collect();
        groups.push(Group {
            id: String::new(),
            name: "Conversations".into(),
            rows: conversations
                .iter()
                .filter(|c| assigned(&c.id).is_none_or(|section| !ids.contains(&&section)))
                .cloned()
                .collect(),
        });
        // Me hides an empty Conversations group.
        groups.retain(|group| !group.id.is_empty() || !group.rows.is_empty());
        groups
    }

    fn heading_icon(icon: IconName) -> Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(rems(1.25))
            .h(rems(1.5))
            .child(Icon::new(icon).size(15.).stroke_width(2.5))
    }

    fn new_conversation(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let ix = c.interaction(Surface::Navigation);
        div()
            .id("me-new-conversation")
            .my(rems(0.5))
            .flex()
            .items_center()
            .gap(rems(0.5))
            .min_h(rems(2.))
            .px(rems(0.5))
            .py(px(2.))
            .rounded(radius::ROW)
            .cursor_pointer()
            .text_body_sm()
            .text_color(c.text_standard)
            .hover(move |s| s.bg(ix.row_hover))
            .child(Self::heading_icon(IconName::Plus))
            .child("New conversation")
            .on_click(cx.listener(|this, _, _, cx| this.select(Selection::New, cx)))
            .into_any_element()
    }

    fn group(
        &mut self,
        group: &Group,
        selected: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let ix = c.interaction(Surface::Navigation);
        let open = !self.collapsed.contains(&group.id);
        let key = group.id.clone();
        let hover: SharedString = format!("me-section-{}", group.id).into();
        let menu_open = self.section_menu.as_deref() == Some(group.id.as_str());
        let caret = Icon::new(if open {
            IconName::CaretDown
        } else {
            IconName::CaretRight
        })
        .size(15.)
        .stroke_width(2.5);
        let mut heading = div()
            .id(SharedString::from(format!("me-heading-{}", group.id)))
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(rems(0.5))
            .min_h(rems(2.))
            .px(rems(0.5))
            .py(px(2.))
            .cursor_pointer()
            .text_body_sm()
            .text_color(c.text_unavailable)
            .on_click(cx.listener(move |this, _, _, cx| {
                let open = !this.collapsed.contains(&key);
                this.toggle(&key, !open, cx);
            }));
        if group.id.is_empty() {
            heading = heading
                .child(div().min_w_0().truncate().child(group.name.clone()))
                .child(caret);
        } else {
            // The folder turns into the caret while the header is hovered.
            heading = heading
                .child(
                    div()
                        .relative()
                        .flex_none()
                        .w(rems(1.25))
                        .h(rems(1.5))
                        .child(
                            Self::heading_icon(IconName::FolderSimple)
                                .absolute()
                                .inset_0()
                                .group_hover(hover.clone(), |s| s.opacity(0.)),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.)
                                .group_hover(hover.clone(), |s| s.opacity(1.))
                                .child(caret),
                        ),
                )
                .child(div().min_w_0().truncate().child(group.name.clone()));
        }
        let actions = (!group.id.is_empty()).then(|| {
            let (new_id, menu_id) = (group.id.clone(), group.id.clone());
            let name = group.name.clone();
            let menu = menu_open.then(|| {
                let weak = cx.entity().downgrade();
                let dismiss: Dismiss = Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.close_menus(cx));
                });
                let content = self.section_menu_content(group, cx);
                overlay::dropdown(window, Align::End, content, dismiss)
            });
            div()
                .flex()
                .flex_none()
                .items_center()
                .when(!menu_open, |this| {
                    this.opacity(0.)
                        .group_hover(hover.clone(), |s| s.opacity(1.))
                })
                .child(
                    IconButton::new(
                        SharedString::from(format!("me-new-in-{}", group.id)),
                        IconName::Plus,
                    )
                    .size(IconButtonSize::Sm)
                    .surface(Surface::Navigation)
                    .icon_size(15.)
                    .stroke_width(2.5)
                    .icon_color(c.text_unavailable)
                    .tooltip(format!("New session in {name}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(Selection::NewIn(new_id.clone()), cx)
                    })),
                )
                .child(
                    div()
                        .relative()
                        .child(
                            IconButton::new(
                                SharedString::from(format!("me-section-actions-{}", group.id)),
                                IconName::DotsThree,
                            )
                            .size(IconButtonSize::Sm)
                            .surface(Surface::Navigation)
                            .icon_size(15.)
                            .stroke_width(2.5)
                            .icon_color(c.text_unavailable)
                            .when(!menu_open, |b| {
                                b.tooltip(format!("Actions for {}", group.name))
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.menu = None;
                                    this.section_menu = Some(menu_id.clone());
                                    cx.notify();
                                },
                            )),
                        )
                        .children(menu),
                )
        });
        let header = div()
            .id(hover.clone())
            .group(hover)
            .flex()
            .items_center()
            .gap(rems(0.25))
            .rounded(radius::ROW)
            .when(!group.id.is_empty(), |this| {
                this.hover(move |s| s.bg(ix.row_hover))
                    .when(menu_open, |this| this.bg(ix.row_hover))
            })
            .child(heading)
            .children(actions);
        let rows: Vec<AnyElement> = if open {
            let mut rows: Vec<AnyElement> = group
                .rows
                .iter()
                .map(|row| self.row(row, &group.id, selected, window, cx))
                .collect();
            if rows.is_empty() && !group.id.is_empty() {
                rows.push(list_message("No sessions in this section.", cx).into_any_element());
            }
            rows
        } else {
            Vec::new()
        };
        div()
            .flex()
            .flex_col()
            .my(rems(0.5))
            .child(header)
            .children(rows)
            .into_any_element()
    }

    fn section_menu_content(&self, group: &Group, cx: &mut Context<Self>) -> AnyElement {
        let writable = self.model.read(cx).me.remove_section_writable();
        let (settings_id, settings_name, delete_id) =
            (group.id.clone(), group.name.clone(), group.id.clone());
        Menu::new()
            .child(
                MenuItem::new("me-section-settings", "Session settings…").on_click(cx.listener(
                    move |this, _, window, cx| {
                        let Some(community) = this.community(cx) else {
                            return;
                        };
                        let dialog = dialogs::Dialog::section_settings(
                            community,
                            settings_id.clone(),
                            settings_name.clone(),
                            window,
                            cx,
                        );
                        this.open_dialog(dialog, cx);
                    },
                )),
            )
            .child(
                MenuItem::new("me-delete-section", "Delete section")
                    .danger()
                    .disabled(!writable || self.deleting.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_section(delete_id.clone(), cx)
                    })),
            )
            .into_any_element()
    }

    fn row(
        &mut self,
        channel: &ChannelSummary,
        section: &str,
        selected: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let ix = c.interaction(Surface::Navigation);
        let id = channel.id.clone();
        let is_selected = selected == Some(id.as_str());
        let menu_open = self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.id == id && menu.at.is_none());
        let group: SharedString = format!("me-row-{id}").into();
        let unread = self
            .community(cx)
            .map(|community| community.read(cx).snapshot.unread.channel(&id))
            .unwrap_or_default();
        let bold = !is_selected && unread.bold;
        let community = self.community(cx);
        let working = community.as_ref().map(|community| {
            let snapshot = community.read(cx).snapshot.clone();
            let own = crate::ui::sidebar::own_agents(cx);
            let agents = crate::ui::agent_activity::with_observed(
                crate::ui::sidebar::working_agents(&snapshot, &id, &own),
                community,
                &id,
                cx,
            );
            let badge = crate::ui::sidebar::working_badge(
                &id,
                &agents,
                &snapshot.profiles,
                &channel.name,
                false,
                cx,
            );
            (snapshot, own, agents, badge)
        });
        let open = id.clone();
        let (menu_id, menu_title, menu_section) =
            (id.clone(), channel.name.clone(), section.to_owned());
        let (ctx_id, ctx_title, ctx_section) =
            (id.clone(), channel.name.clone(), section.to_owned());
        let dropdown = menu_open.then(|| {
            let weak = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| this.close_menus(cx));
            });
            let content = self.row_menu_content(cx);
            overlay::dropdown(window, Align::End, content, dismiss)
        });
        let row = div()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .min_w_0()
            .mb(px(2.))
            .rounded(radius::ROW)
            .when(is_selected, |this| this.bg(ix.selected))
            .when(!is_selected, |this| {
                this.hover(move |s| s.bg(ix.row_hover))
                    .when(menu_open, |this| this.bg(ix.row_hover))
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.section_menu = None;
                    this.menu = Some(RowMenu {
                        id: ctx_id.clone(),
                        title: ctx_title.clone(),
                        section: ctx_section.clone(),
                        at: Some(event.position),
                        sub: Sub::None,
                    });
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("me-open-{id}")))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .min_h(rems(2.5))
                    .pl(px(10.5))
                    .pr(rems(2.25))
                    .cursor_pointer()
                    .text_body()
                    .text_color(if bold {
                        c.affordance_prominent
                    } else {
                        c.text_standard
                    })
                    .when(bold, |this| this.font_weight(FontWeight::MEDIUM))
                    .child(div().min_w_0().truncate().child(channel.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(Selection::Session(open.clone()), cx)
                    })),
            )
            .child(
                div()
                    .absolute()
                    .right(px(4.))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .when(!menu_open, |this| {
                        this.opacity(0.)
                            .group_hover(group.clone(), |s| s.opacity(1.))
                    })
                    .child(
                        div()
                            .relative()
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("me-actions-{id}")),
                                    IconName::DotsThree,
                                )
                                .size(IconButtonSize::Sm)
                                .surface(Surface::Navigation)
                                .icon_size(15.)
                                .stroke_width(2.5)
                                .icon_color(c.text_unavailable)
                                .selected(menu_open)
                                .when(!menu_open, |b| {
                                    b.tooltip(format!("Actions for {}", channel.name))
                                })
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.section_menu = None;
                                        this.menu = Some(RowMenu {
                                            id: menu_id.clone(),
                                            title: menu_title.clone(),
                                            section: menu_section.clone(),
                                            at: None,
                                            sub: Sub::None,
                                        });
                                        cx.notify();
                                    },
                                )),
                            )
                            .children(dropdown),
                    ),
            );
        let Some((snapshot, own, agents, badge)) = working else {
            return row.into_any_element();
        };
        // Working takes the actions button's slot, as in Messages, and
        // gives way to the button on hover.
        let row = match badge {
            Some(badge) => row.child(
                div()
                    .absolute()
                    .right(px(4.))
                    .top_0()
                    .bottom_0()
                    .w(rems(2.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(menu_open, |this| this.opacity(0.))
                    .group_hover(group, |s| s.opacity(0.))
                    .child(badge),
            ),
            None => row,
        };
        let community = community.expect("working needs a community");
        let media = community.read(cx).session.media().clone();
        crate::ui::channel_activity::ActivityRow::new(&id, row)
            .activity(
                snapshot.unread.activity.get(&id).cloned(),
                agents,
                std::sync::Arc::new(own),
            )
            .directory(snapshot.profiles.clone(), Some(media), Some(community))
            .opens(self.opens())
            .into_any_element()
    }

    /// The activity popover's entries: messages and threads open in the
    /// channel reader, as other Me links do; a working agent without a
    /// message, and its activity, open in Me's conversation and its panels.
    fn opens(&self) -> crate::ui::channel_activity::Opens {
        let message = |channel: &str, id: &str, window: &mut Window, cx: &mut App| {
            if let Ok(id) = buzz_relay_client::EventId::from_hex(id) {
                let url = buzz_relay_client::message_link(channel, &id);
                crate::ui::navigation::open_url(&url, window, cx);
            }
        };
        let (agent_model, activity_model) = (self.model.downgrade(), self.model.downgrade());
        crate::ui::channel_activity::Opens {
            thread: Rc::new(message),
            agent: Rc::new(move |channel, _, id, window, cx| match id {
                Some(id) => message(channel, id, window, cx),
                None => {
                    let _ = agent_model.update(cx, |model, cx| {
                        model.select(Selection::Session(channel.to_owned()), cx)
                    });
                }
            }),
            activity: Rc::new(move |channel, agent, _, cx| {
                let _ = activity_model.update(cx, |model, cx| {
                    model.update_tab_state(channel, cx, |state| {
                        state.open_panel(crate::ui::channel_tabs::PanelTab {
                            panel: crate::ui::agent_activity::PANEL,
                            channel: channel.to_owned(),
                            target: crate::agent_activity::target(agent, Some(channel), None),
                        })
                    });
                    model.select(Selection::Session(channel.to_owned()), cx)
                });
            }),
        }
    }

    /// The row actions (`SessionActionsMenu` content), with its submenu.
    fn row_menu_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(menu) = &self.menu else {
            return div().into_any_element();
        };
        let model = self.model.read(cx);
        let writable = model.me.writable;
        let community = model.community.clone();
        let channel = community.as_ref().and_then(|community| {
            community
                .read(cx)
                .snapshot
                .channels
                .iter()
                .find(|c| c.id == menu.id)
                .cloned()
        });
        let read_only = channel.as_ref().is_some_and(|c| c.read_only);
        let (id, title) = (menu.id.clone(), menu.title.clone());
        let sub = menu.sub;
        let share = {
            let (community, channel) = (community.clone(), channel.clone());
            cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                let (Some(community), Some(channel)) = (community.clone(), channel.clone()) else {
                    return;
                };
                let dialog = dialogs::Dialog::share(community, channel, window, cx);
                this.open_dialog(dialog, cx);
            })
        };
        let rename = {
            let (community, id, title) = (community.clone(), id.clone(), title.clone());
            cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                let Some(community) = community.clone() else {
                    return;
                };
                let dialog =
                    dialogs::Dialog::rename(community, id.clone(), title.clone(), window, cx);
                this.open_dialog(dialog, cx);
            })
        };
        let settings = {
            let (community, id, title) = (community.clone(), id.clone(), title.clone());
            cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                let Some(community) = community.clone() else {
                    return;
                };
                let dialog = dialogs::Dialog::session_settings(
                    community,
                    id.clone(),
                    title.clone(),
                    window,
                    cx,
                );
                this.open_dialog(dialog, cx);
            })
        };
        let main = Menu::new()
            .child(MenuItem::new("me-share", "Share…").on_click(share))
            .child(
                MenuItem::new("me-rename", "Rename")
                    .icon(IconName::PencilSimple)
                    .icon_size(16.)
                    .disabled(read_only)
                    .on_click(rename),
            )
            .child(
                div()
                    .id("me-move-trigger")
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        this.hover_sub(*hovered, Sub::Move, cx)
                    }))
                    .child(
                        MenuItem::new("me-move", "Move to")
                            .icon(IconName::FolderSimple)
                            .icon_size(16.)
                            .submenu()
                            .highlighted(sub == Sub::Move)
                            .disabled(!writable)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sub(Sub::Move, cx))),
                    ),
            )
            .child(
                div()
                    .id("me-copy-trigger")
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        this.hover_sub(*hovered, Sub::Copy, cx)
                    }))
                    .child(
                        MenuItem::new("me-copy", "Copy")
                            .icon(IconName::Copy)
                            .icon_size(16.)
                            .submenu()
                            .highlighted(sub == Sub::Copy)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sub(Sub::Copy, cx))),
                    ),
            )
            .child(
                div()
                    .id("me-settings-trigger")
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        this.hover_sub(*hovered, Sub::None, cx)
                    }))
                    .child(
                        MenuItem::new("me-session-settings", "Session settings…")
                            .icon(IconName::Gear)
                            .icon_size(16.)
                            .disabled(read_only)
                            .on_click(settings),
                    ),
            );
        let submenu = match sub {
            Sub::None => None,
            Sub::Move => Some(self.move_menu(menu, cx)),
            Sub::Copy => Some(self.copy_menu(menu, cx)),
        };
        // The submenu opens beside its trigger row, outside the menu's
        // own box so the menu keeps its place.
        let offset = match sub {
            Sub::Copy => px(112.),
            _ => px(76.),
        };
        div()
            .relative()
            .child(main)
            .children(submenu.map(|submenu| {
                div()
                    .absolute()
                    .left_full()
                    .top(offset)
                    .pl(px(4.))
                    .child(submenu)
            }))
            .into_any_element()
    }

    fn hover_sub(&mut self, hovered: bool, which: Sub, cx: &mut Context<Self>) {
        if hovered && let Some(menu) = &mut self.menu {
            menu.sub = which;
            cx.notify();
        }
    }

    fn toggle_sub(&mut self, which: Sub, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.menu {
            menu.sub = if menu.sub == which { Sub::None } else { which };
        }
        cx.notify();
    }

    fn move_menu(&self, menu: &RowMenu, cx: &mut Context<Self>) -> AnyElement {
        let groups = self
            .model
            .read(cx)
            .me
            .data
            .as_ref()
            .map(|data| data.sections.clone())
            .unwrap_or_default();
        let mut list = Menu::new();
        if !menu.section.is_empty() {
            let id = menu.id.clone();
            list = list.child(
                MenuItem::new("me-move-conversations", "Conversations")
                    .on_click(cx.listener(move |this, _, _, cx| this.move_to(&id, None, cx))),
            );
        }
        for group in groups {
            let id = menu.id.clone();
            let target = group.id.clone();
            list = list.child(
                MenuItem::new(
                    SharedString::from(format!("me-move-{}", group.id)),
                    group.name.clone(),
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.move_to(&id, Some(target.clone()), cx)),
                ),
            );
        }
        let (id, title) = (menu.id.clone(), menu.title.clone());
        list.child(
            MenuItem::new("me-move-new", "New group…").on_click(cx.listener(
                move |this, _, window, cx| {
                    let dialog =
                        dialogs::Dialog::new_section(id.clone(), title.clone(), window, cx);
                    this.open_dialog(dialog, cx);
                },
            )),
        )
        .into_any_element()
    }

    fn copy_menu(&self, menu: &RowMenu, cx: &mut Context<Self>) -> AnyElement {
        let (name, id, link) = (menu.title.clone(), menu.id.clone(), channel_link(&menu.id));
        Menu::new()
            .child(MenuItem::new("me-copy-name", "Copy session name").on_click(
                cx.listener(move |this, _, _, cx| this.copy(name.clone(), "Session name", cx)),
            ))
            .child(MenuItem::new("me-copy-id", "Copy session ID").on_click(
                cx.listener(move |this, _, _, cx| this.copy(id.clone(), "Session ID", cx)),
            ))
            .child(
                MenuItem::new("me-copy-link", "Copy link to session").on_click(
                    cx.listener(move |this, _, _, cx| this.copy(link.clone(), "Session link", cx)),
                ),
            )
            .into_any_element()
    }

    /// The row menu opened at the pointer (right-click).
    fn context_menu(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self.menu.as_ref()?.at?;
        let content = self.row_menu_content(cx);
        let size = window.viewport_size();
        let backdrop = deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("me-menu-backdrop")
                    .occlude()
                    .w(size.width)
                    .h(size.height)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close_menus(cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, _, cx| this.close_menus(cx)),
                    ),
            ),
        )
        .with_priority(MENU_LAYER);
        let popup = deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(at)
                .snap_to_window_with_margin(px(8.))
                .child(div().id("me-context-menu").occlude().child(content)),
        )
        .with_priority(MENU_LAYER + 1);
        Some(div().child(backdrop).child(popup).into_any_element())
    }

    /// Saving and failed moves (`moves` in `SessionSections`).
    fn moves(&self, conversations: &[ChannelSummary], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let me = self.model.read(cx).me.clone();
        let moves: Vec<_> = me
            .moves
            .iter()
            .filter(|m| conversations.iter().any(|c| c.id == m.intent.channel_id))
            .cloned()
            .collect();
        let mut out = Vec::new();
        if moves.iter().any(|m| m.pending) {
            out.push(list_message("Saving section…", cx).into_any_element());
        }
        for failed in moves.iter().filter(|m| m.error.is_some()) {
            let (retry, dismiss) = (
                failed.intent.channel_id.clone(),
                failed.intent.channel_id.clone(),
            );
            let community = self.community(cx);
            let kit = community.map(|c| c.read(cx).session.me_kit().clone());
            let dismiss_kit = kit.clone();
            out.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .mx(rems(0.75))
                    .my(rems(0.25))
                    .child(
                        div()
                            .text_caption()
                            .text_color(cx.colors().text_danger)
                            .child(failed.error.clone().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(rems(0.5))
                            .child(
                                Button::new(
                                    SharedString::from(format!("me-retry-move-{retry}")),
                                    "Retry move",
                                )
                                .disabled(!me.writable)
                                .on_click(move |_, _, cx| {
                                    if let Some(kit) = kit.clone() {
                                        let id = retry.clone();
                                        cx.background_spawn(async move {
                                            let _ = kit.me_retry_move(&id).await;
                                        })
                                        .detach();
                                    }
                                }),
                            )
                            .child(
                                Button::new(
                                    SharedString::from(format!("me-dismiss-move-{dismiss}")),
                                    "Dismiss",
                                )
                                .on_click(move |_, _, _| {
                                    if let Some(kit) = &dismiss_kit {
                                        kit.me_dismiss_move(&dismiss);
                                    }
                                }),
                            ),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    fn dialog_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        Some(self.dialog.as_ref()?.overlay(window, cx))
    }
}

impl dialogs::DialogHost for MeSidebar {
    fn dialog(&mut self) -> &mut Option<dialogs::Dialog> {
        &mut self.dialog
    }

    /// `createAndAssign` from "Create new section".
    fn create_section(&mut self, channel: String, name: String, cx: &mut Context<Self>) -> bool {
        let Some(community) = self.community(cx) else {
            return false;
        };
        if !self.model.read(cx).me.writable {
            return false;
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.toggle(&id, true, cx);
        let kit = community.read(cx).session.me_kit().clone();
        cx.background_spawn(async move {
            let _ = kit
                .me_create_and_assign(&channel, NewSection { id, name })
                .await;
        })
        .detach();
        true
    }
}

/// A quiet status line in the history (`.listMessage`).
fn list_message(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .mx(rems(0.75))
        .my(rems(0.25))
        .text_caption()
        .text_color(cx.colors().text_subtle)
        .child(text.into())
}

impl Render for MeSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let model = self.model.read(cx);
        let community = model.community.clone();
        let ready = community
            .as_ref()
            .is_some_and(|c| c.read(cx).snapshot.status == SessionStatus::Ready);
        let mut content = div()
            .id("me-conversations")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .px(rems(0.5))
            .text_color(c.text_standard);
        let Some(community) = community.filter(|_| ready) else {
            return div()
                .id("me-sidebar")
                .size_full()
                .child(content.child(list_message(
                    "Connect to a community to see your conversations.",
                    cx,
                )))
                .into_any_element();
        };
        self.load_collapsed(&community, cx);
        let model = self.model.read(cx);
        let selection = model.selection(cx);
        let selected = selection.session().map(str::to_owned);
        let conversations = model.conversations(cx);
        let kit = model.kit.clone();
        let me = model.me.clone();
        let snapshot = community.read(cx).snapshot.clone();
        let session = community.read(cx).session.clone();

        content = content.child(self.new_conversation(cx));
        match &me.status {
            PreferencesStatus::Idle | PreferencesStatus::Loading => {
                content = content.child(list_message("Loading sections…", cx));
            }
            PreferencesStatus::Error(error) => {
                let retry = session.me_kit().clone();
                content = content.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.5))
                        .mx(rems(0.75))
                        .child(div().text_caption().text_color(c.text_danger).child(
                            if error.is_empty() {
                                "Sections couldn’t load.".to_owned()
                            } else {
                                error.clone()
                            },
                        ))
                        .child(
                            Button::new("me-retry-sections", "Retry sections")
                                .on_click(move |_, _, _| retry.me_refresh()),
                        ),
                );
            }
            PreferencesStatus::Ready => {}
        }
        if let Some(error) = &self.delete_error {
            content = content.child(list_message(error.clone(), cx).text_color(c.text_danger));
        }
        let groups = self.groups(&conversations, cx);
        for group in &groups {
            content = content.child(self.group(group, selected.as_deref(), window, cx));
        }
        content = content.children(self.moves(&conversations, cx));
        match &snapshot.roster {
            RosterStatus::Loading => {
                content = content.child(list_message("Loading conversations…", cx))
            }
            RosterStatus::Error(error) => {
                let retry = session.clone();
                content = content.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.5))
                        .mx(rems(0.75))
                        .child(
                            div()
                                .text_caption()
                                .text_color(c.text_danger)
                                .child(error.clone()),
                        )
                        .child(
                            Button::new("me-retry-list", "Retry")
                                .on_click(move |_, _, _| retry.refresh_channels()),
                        ),
                );
            }
            RosterStatus::Ready => {}
        }
        if !kit.ready() {
            let retry = session.me_kit().clone();
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(rems(0.5))
                    .mx(rems(0.5))
                    .my(rems(0.5))
                    .text_body()
                    .child(
                        kit.error()
                            .map(str::to_owned)
                            .unwrap_or_else(|| "Loading Me placement…".into()),
                    )
                    .child(
                        Button::new("me-retry-me", "Retry Me").on_click(move |_, _, _| {
                            drop(retry.refresh());
                        }),
                    ),
            );
        }
        if conversations.is_empty() && snapshot.roster == RosterStatus::Ready && kit.ready() {
            content = content.child(list_message("Start a conversation with your agents.", cx));
        }
        let dialog = self.dialog_overlay(window, cx);
        let context = self.context_menu(window, cx);
        div()
            .id("me-sidebar")
            .size_full()
            .child(content)
            .children(context)
            .children(dialog)
            .into_any_element()
    }
}

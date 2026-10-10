//! Settings → Personal groups (`channels/ChannelSetupSettings.tsx`) and
//! its "Personal groups" dialog (`ChannelGroupsDialog.tsx`).
//!
//! Personal groups are the viewer's private Messages sections, saved in
//! the recipe catalog. Once that record exists it replaces the legacy
//! sidebar sections; nothing is migrated unless the viewer copies the
//! visible ones into the draft.

use super::cards::{self, CardPlacement, SettingsCard};
use super::{button, header, section};
use crate::community::{AppState, Community};
use crate::ui::overlay::{self, Dismiss};
use buzz_relay_client::SessionStatus;
use buzz_relay_client::me_kit::{self as kit, GroupsId};
use buzz_relay_client::session::{MeKitSnapshot, MeKitStatus};
use buzz_relay_client::sidebar::{PersonalGroup, SidebarPreferences};
use buzz_relay_client::sidebar_store::PreferencesStatus;
use buzz_ui::{
    ActiveTheme, Button, Dialog, Icon, IconName, Surface, TextField, TypeStyled, tokens::radius,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

/// Registers the "Personal groups" card under the community
/// (`channels/index.tsx`).
pub fn init(cx: &mut App) {
    cards::register(
        SettingsCard {
            key: "buzz.channels/groups",
            title: "Personal groups",
            icon: Some(IconName::FolderSimple),
            placement: CardPlacement::Community,
            visibility: None,
            build: |window, cx| cx.new(|cx| PersonalGroupsSettings::new(window, cx)).into(),
        },
        cx,
    );
}

/// `maxLength` of a group name.
const NAME_LIMIT: usize = 120;

pub struct PersonalGroupsSettings {
    community: Option<Entity<Community>>,
    kit: Arc<MeKitSnapshot>,
    dialog: Option<Entity<GroupsDialog>>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl PersonalGroupsSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            community: None,
            kit: Arc::default(),
            dialog: None,
            _tasks: Vec::new(),
            _subscriptions: vec![
                cx.observe_global_in::<AppState>(window, |this, _, cx| this.follow(cx)),
            ],
        };
        this.follow(cx);
        this
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        let current = AppState::current(cx);
        if current.as_ref().map(Entity::entity_id) == self.community.as_ref().map(Entity::entity_id)
        {
            return;
        }
        self.community = current.clone();
        self.dialog = None;
        self._tasks.clear();
        if let Some(community) = current {
            let kit = community.read(cx).session.me_kit().clone();
            kit.ensure();
            self._tasks.push(crate::mirror::mirror(
                kit.watch(),
                cx,
                |this: &mut Self, value| this.kit = value,
            ));
            self._subscriptions.truncate(1);
            self._subscriptions
                .push(cx.observe(&community, |_, _, cx| cx.notify()));
        }
        cx.notify();
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = self.community.clone() else {
            return;
        };
        let legacy = community.read(cx).snapshot.sidebar.data.clone();
        let dialog = cx.new(|cx| GroupsDialog::new(community, legacy, window, cx));
        self._subscriptions
            .push(cx.subscribe(&dialog, |this, _, _: &Closed, cx| {
                this.dialog = None;
                cx.notify();
            }));
        self.dialog = Some(dialog);
        cx.notify();
    }
}

impl Render for PersonalGroupsSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let connected = self
            .community
            .as_ref()
            .filter(|community| community.read(cx).snapshot.status == SessionStatus::Ready);
        let Some(community) = connected.cloned() else {
            return section(vec![
                header("Personal groups", None, cx).into_any_element(),
                div()
                    .px(rems(1.0625))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("Choose a connected community to manage personal groups.")
                    .into_any_element(),
            ]);
        };
        let snapshot = community.read(cx).snapshot.clone();
        let ready = self.kit.status == MeKitStatus::Ready;
        let groups = self.kit.personal();
        let empty = ready
            && match &groups {
                Some(groups) => groups.groups.is_empty(),
                None => {
                    snapshot.sidebar.status == PreferencesStatus::Ready
                        && snapshot
                            .sidebar
                            .data
                            .as_ref()
                            .is_some_and(|data| data.sections.is_empty())
                }
            };
        let card = div()
            .w_full()
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::CONTAINER)
            .flex()
            .flex_col()
            .items_center()
            .gap(rems(1.))
            .py(rems(2.))
            .px(rems(1.5))
            .text_center()
            .child(
                div()
                    .flex()
                    .text_color(c.text_subtle)
                    .child(Icon::new(IconName::FolderSimple).size(32.)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .max_w(rems(24.))
                    .child(div().text_label_sm().child(if empty {
                        "No personal groups yet"
                    } else {
                        "Organize your channels"
                    }))
                    .child(div().text_body_sm().text_color(c.text_subtle).child(
                        "Group channels privately in your sidebar. Members and Canvas \
                                 stay unchanged.",
                    )),
            )
            .child(
                div().mt(rems(0.5)).child(
                    button("manage-personal-groups", "Manage personal groups")
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, window, cx| this.open(window, cx))),
                ),
            );
        let status = (!ready).then(|| {
            let kit = community.read(cx).session.me_kit().clone();
            div()
                .flex()
                .items_center()
                .gap(rems(0.75))
                .px(rems(1.0625))
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(
                    self.kit
                        .error()
                        .map(str::to_owned)
                        .unwrap_or_else(|| "Saved groups are not ready.".into()),
                )
                .child(
                    button("reload-groups", "Reload groups").on_click(move |_, _, _| {
                        drop(kit.refresh());
                    }),
                )
                .into_any_element()
        });
        let mut children = vec![
            header("Personal groups", None, cx).into_any_element(),
            card.into_any_element(),
        ];
        children.extend(status);
        if let Some(dialog) = &self.dialog {
            let weak = dialog.downgrade();
            let close: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |dialog, cx| dialog.close(cx));
            });
            children.push(overlay::modal(window, dialog.clone(), Some(close)).into_any_element());
        }
        section(children)
    }
}

/// The dialog closed.
pub struct Closed;

/// One group as edited.
struct DraftGroup {
    id: String,
    input: Entity<InputState>,
    default_template_id: String,
    _subscription: Subscription,
}

pub struct GroupsDialog {
    community: Entity<Community>,
    legacy: Option<SidebarPreferences>,
    groups: Vec<DraftGroup>,
    /// Assignments kept from the record (or copied from legacy groups).
    assignments: std::collections::BTreeMap<String, String>,
    /// The record's head this draft started from.
    base: Option<String>,
    /// A personal-groups record exists.
    existing: bool,
    busy: bool,
    error: Option<SharedString>,
    scroll: ScrollHandle,
}

impl EventEmitter<Closed> for GroupsDialog {}

impl GroupsDialog {
    fn new(
        community: Entity<Community>,
        legacy: Option<SidebarPreferences>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot = community.read(cx).session.me_kit().snapshot();
        let entry = kit::groups_entry(&snapshot.entries, GroupsId::Personal)
            .filter(|entry| !entry.deleted());
        let groups = entry.and_then(|entry| entry.groups()).unwrap_or_default();
        let mut this = Self {
            community,
            legacy,
            groups: Vec::new(),
            assignments: groups.assignments.clone(),
            base: entry.map(|entry| entry.event_id.clone()),
            existing: entry.is_some(),
            busy: false,
            error: None,
            scroll: ScrollHandle::new(),
        };
        for group in groups.groups {
            this.push(group, window, cx);
        }
        this
    }

    fn push(&mut self, group: PersonalGroup, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(group.name.clone(), window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self.groups.push(DraftGroup {
            id: group.id,
            input,
            default_template_id: group.default_template_id,
            _subscription: subscription,
        });
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(Closed);
        }
    }

    fn name(group: &DraftGroup, cx: &App) -> String {
        group
            .input
            .read(cx)
            .value()
            .chars()
            .take(NAME_LIMIT)
            .collect()
    }

    /// "Copy visible OG groups into this draft".
    fn copy_legacy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(legacy) = self.legacy.clone() else {
            return;
        };
        self.groups.clear();
        let mut ids = std::collections::HashMap::new();
        for section in &legacy.sections {
            let id = uuid::Uuid::new_v4().to_string();
            ids.insert(section.id.clone(), id.clone());
            self.push(
                PersonalGroup {
                    id,
                    name: section.name.clone(),
                    default_template_id: String::new(),
                },
                window,
                cx,
            );
        }
        self.assignments = legacy
            .assignments
            .iter()
            .filter_map(|(channel, group)| Some((channel.clone(), ids.get(group)?.clone())))
            .collect();
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let groups: Vec<PersonalGroup> = self
            .groups
            .iter()
            .map(|group| PersonalGroup {
                id: group.id.clone(),
                name: Self::name(group, cx),
                default_template_id: group.default_template_id.clone(),
            })
            .collect();
        let mut value = buzz_relay_client::sidebar::PersonalGroups {
            groups,
            assignments: self.assignments.clone(),
            channels: Vec::new(),
        };
        let ids: Vec<String> = value.groups.iter().map(|g| g.id.clone()).collect();
        value.assignments.retain(|_, group| ids.contains(group));
        let kit = self.community.read(cx).session.me_kit().clone();
        let base = self.base.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = kit
                .save(kit::groups_value(GroupsId::Personal, &value), base, false)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => cx.emit(Closed),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for GroupsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let busy = self.busy;
        let ready = self.community.read(cx).session.me_kit().snapshot().ready();
        let incomplete = self
            .groups
            .iter()
            .any(|g| Self::name(g, cx).trim().is_empty());
        let mut body = div()
            .id("personal-groups-body")
            .flex()
            .flex_col()
            .gap(rems(1.))
            .max_h(rems(28.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(div().text_body_sm().text_color(c.text_subtle).child(
                "Personal organization only. Defaults apply to new channels; moving \
                         channels never changes their members or Canvas.",
            ));
        if !self.existing {
            body = body.child(div().text_body_sm().child(
                "This activates separate new-Buzz groups. OG groups remain untouched and will \
                 no longer be displayed here; later OG edits will not sync into this list.",
            ));
            if self.legacy.as_ref().is_some_and(|l| !l.sections.is_empty()) {
                body = body.child(
                    div().child(
                        Button::new("copy-og-groups", "Copy visible OG groups into this draft")
                            .surface(Surface::Floating)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.copy_legacy(window, cx)),
                            ),
                    ),
                );
            }
        }
        for (index, group) in self.groups.iter().enumerate() {
            let (up, remove) = (group.id.clone(), group.id.clone());
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(div().text_label().child("Group name"))
                    .child(
                        TextField::new(&group.input)
                            .surface(Surface::Floating)
                            .disabled(busy),
                    )
                    .when(!group.default_template_id.is_empty(), |this| {
                        this.child(div().text_body_sm().text_color(c.text_subtle).child(
                            "Saved template default is preserved but inactive. Enable \
                                 Templates & teams to edit or apply it.",
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .gap(rems(0.5))
                            .child(
                                Button::new(
                                    SharedString::from(format!("up-{}", group.id)),
                                    "Move up",
                                )
                                .surface(Surface::Floating)
                                .disabled(index == 0 || busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(i) = this.groups.iter().position(|g| g.id == up)
                                            && i > 0
                                        {
                                            this.groups.swap(i - 1, i);
                                            cx.notify();
                                        }
                                    },
                                )),
                            )
                            .child(
                                Button::new(
                                    SharedString::from(format!("remove-{}", group.id)),
                                    "Remove group (keep channels)",
                                )
                                .surface(Surface::Floating)
                                .disabled(busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.groups.retain(|g| g.id != remove);
                                        this.assignments.retain(|_, g| *g != remove);
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            );
        }
        body = body.child(
            div().child(
                Button::new("new-personal-group", "New group")
                    .surface(Surface::Floating)
                    .ghost()
                    .full_width()
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.push(
                            PersonalGroup {
                                id: uuid::Uuid::new_v4().to_string(),
                                name: String::new(),
                                default_template_id: String::new(),
                            },
                            window,
                            cx,
                        );
                        cx.notify();
                    })),
            ),
        );
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .text_body_sm()
                    .text_color(c.text_danger)
                    .child(error.clone()),
            );
        }
        Dialog::new("personal-groups", "Personal groups")
            .on_close(cx.listener(|this, _, _, cx| this.close(cx)))
            .child(body)
            .footer(
                Button::new("save-personal-groups", "Save groups")
                    .prominent()
                    .loading(busy)
                    .disabled(!ready || incomplete)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
    }
}

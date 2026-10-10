//! The template and team library (`TemplateLibrary.tsx`,
//! `TemplateSettings.tsx`): Settings → Templates lists channel templates;
//! the Agents page lists saved teams. Each item opens the editor, and a
//! menu edits, duplicates or deletes it.

use super::dialog::{Closed, TemplateDialog};
use super::{
    Catalog, current, empty_state, header, inline_header, line, public_key_labels, watch_kit,
};
use crate::community::{AppState, Community};
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_relay_client::EventId;
use buzz_relay_client::kit::{AgentChoice, KitEntry, KitStatus, KitValue, Team, Template};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, ButtonVariant, Dialog, Icon, IconButton,
    IconButtonSize, IconName, Menu, MenuItem, MenuSize, Surface, Tooltip, TypeStyled,
    tokens::radius, tokens::space,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// Settings → Templates (`TemplateSettings`): the template library of the
/// selected community.
pub struct TemplatesCard {
    library: Entity<TemplateLibrary>,
}

impl TemplatesCard {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            library: cx.new(|cx| TemplateLibrary::new(Section::Template, cx)),
        }
    }
}

impl Render for TemplatesCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        if current(cx).is_none() {
            return div()
                .flex()
                .flex_col()
                .child(header("Templates", None, Vec::new(), cx))
                .child(
                    div()
                        .px(rems(1.0625))
                        .text_body()
                        .text_color(c.text_subtle)
                        .child("Choose a connected community to manage templates."),
                );
        }
        div().flex().flex_col().child(self.library.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Template,
    Team,
}

/// A recipe being deleted, with the head it was read at.
struct Deleting {
    value: KitValue,
    event: EventId,
    busy: bool,
    error: Option<SharedString>,
}

pub struct TemplateLibrary {
    section: Section,
    community: Option<Entity<Community>>,
    editing: Option<(Entity<TemplateDialog>, Subscription)>,
    deleting: Option<Deleting>,
    /// The item whose actions menu is open.
    menu: Option<String>,
    _kit: Option<Task<()>>,
    _app: Subscription,
    _hosts: Option<Subscription>,
}

impl TemplateLibrary {
    pub fn new(section: Section, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            section,
            community: None,
            editing: None,
            deleting: None,
            menu: None,
            _kit: None,
            _app: cx.observe_global::<AppState>(|this, cx| this.follow(cx)),
            _hosts: None,
        };
        this.follow(cx);
        this
    }

    /// Follows the selected community, its kit and its agents.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let community = current(cx);
        if community.as_ref().map(Entity::entity_id)
            != self.community.as_ref().map(Entity::entity_id)
        {
            self.editing = None;
            self.deleting = None;
            self.menu = None;
            self._kit = community.as_ref().map(|community| watch_kit(community, cx));
            self._hosts = community
                .as_ref()
                .map(|community| cx.observe(community, |_, _, cx| cx.notify()));
            self.community = community;
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(community) = &self.community else {
            return;
        };
        let session = community.read(cx).session.clone();
        cx.background_spawn(async move {
            let _ = session.kit_refresh().await;
        })
        .detach();
        cx.notify();
    }

    fn open(
        &mut self,
        value: KitValue,
        expected: Option<EventId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(community) = self.community.clone() else {
            return;
        };
        self.menu = None;
        let dialog = cx.new(|cx| TemplateDialog::new(community, value, expected, None, window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, _: &Closed, cx| {
            this.editing = None;
            cx.notify();
        });
        self.editing = Some((dialog, subscription));
        cx.notify();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = match self.section {
            Section::Template => KitValue::Template(Template::blank()),
            Section::Team => KitValue::Team(Team {
                id: uuid::Uuid::new_v4().to_string(),
                name: String::new(),
                agents: Vec::new(),
                portable: None,
            }),
        };
        self.open(value, None, window, cx);
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let (Some(community), Some(deleting)) = (&self.community, &mut self.deleting) else {
            return;
        };
        if deleting.busy {
            return;
        }
        deleting.busy = true;
        deleting.error = None;
        let session = community.read(cx).session.clone();
        let (value, event) = (deleting.value.clone(), deleting.event);
        cx.spawn(async move |this, cx| {
            let result = session.kit_save(value, Some(event), true).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.deleting = None,
                    Err(error) => {
                        if let Some(deleting) = &mut this.deleting {
                            deleting.busy = false;
                            deleting.error = Some(error.into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn delete_dialog(&self, deleting: &Deleting, cx: &mut Context<Self>) -> Dialog {
        let team = matches!(deleting.value, KitValue::Team(_));
        let weak = cx.entity().downgrade();
        let cancel = move |_: &mut Context<Self>| {
            let weak = weak.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    if !this.deleting.as_ref().is_some_and(|d| d.busy) {
                        this.deleting = None;
                        cx.notify();
                    }
                });
            }
        };
        Dialog::new(
            "delete-recipe",
            format!("Delete “{}”?", deleting.value.name()),
        )
        .description(if team {
            "Only this saved team is deleted. Its agents and their channel memberships stay unchanged. Templates using this team will need a replacement."
        } else {
            "Existing channels stay unchanged. Group defaults using this template will need a replacement."
        })
        .on_close(cancel(cx))
        .children(deleting.error.clone().map(|error| line(error, true, cx)))
        .footer(
            Button::new("delete-cancel", "Cancel")
                .surface(Surface::Floating)
                .disabled(deleting.busy)
                .on_click(cancel(cx)),
        )
        .footer(
            Button::new("delete-confirm", "Delete")
                .destructive()
                .loading(deleting.busy)
                .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
        )
    }

    /// The actions menu of one item.
    fn menu(
        &self,
        value: &KitValue,
        entry: &KitEntry,
        disabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = value.id().to_owned();
        let open = self.menu.as_deref() == Some(id.as_str());
        let noun = match value {
            KitValue::Team(_) => "team",
            _ => "template",
        };
        let toggle_id = id.clone();
        let mut root = div().relative().child(
            IconButton::new(
                SharedString::from(format!("actions-{id}")),
                IconName::DotsThree,
            )
            .size(IconButtonSize::Sm)
            .icon_size(18.)
            .disabled(disabled)
            .selected(open)
            .tooltip(format!("Actions for {}", value.name()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu =
                    (this.menu.as_deref() != Some(toggle_id.as_str())).then(|| toggle_id.clone());
                cx.notify();
            })),
        );
        if open {
            let (edit_value, event) = (value.clone(), entry.event_id);
            let mut items = vec![
                MenuItem::new("menu-edit", format!("Edit {noun}"))
                    .size(MenuSize::Compact)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open(edit_value.clone(), Some(event), window, cx)
                    })),
            ];
            if let KitValue::Template(template) = value {
                let template = template.clone();
                items.push(
                    MenuItem::new("menu-duplicate", "Duplicate template")
                        .size(MenuSize::Compact)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let mut copy = template.clone();
                            copy.id = uuid::Uuid::new_v4().to_string();
                            let base: String = copy.name.chars().take(113).collect();
                            copy.name = format!("{} (copy)", base.trim_end());
                            this.open(KitValue::Template(copy), None, window, cx)
                        })),
                );
            }
            let delete_value = value.clone();
            items.push(
                MenuItem::new("menu-delete", format!("Delete {noun}…"))
                    .size(MenuSize::Compact)
                    .danger()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.deleting = Some(Deleting {
                            value: delete_value.clone(),
                            event,
                            busy: false,
                            error: None,
                        });
                        cx.notify();
                    })),
            );
            let weak = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            });
            root = root.child(overlay::dropdown(
                window,
                Align::End,
                Menu::new().size(MenuSize::Compact).children(items),
                dismiss,
            ));
        }
        root
    }

    /// Everyone a template or team adds, team members included.
    fn members(value: &KitValue, entries: &[KitEntry], agents: &[AgentChoice]) -> Vec<AgentChoice> {
        let mut keys: Vec<String> = Vec::new();
        let mut add = |key: &String| {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        };
        match value {
            KitValue::Team(team) => team.agents.iter().for_each(&mut add),
            KitValue::Template(template) => {
                template.agents.iter().for_each(&mut add);
                for team in buzz_relay_client::kit::teams(entries) {
                    if template.team_ids.contains(&team.id) {
                        team.agents.iter().for_each(&mut add);
                    }
                }
            }
            KitValue::Groups(_) => {}
        }
        let labels = public_key_labels(keys.iter());
        keys.iter()
            .map(|key| {
                agents
                    .iter()
                    .find(|a| a.pubkey == *key)
                    .cloned()
                    .unwrap_or_else(|| AgentChoice {
                        pubkey: key.clone(),
                        name: labels
                            .get(key)
                            .cloned()
                            .unwrap_or_else(|| "Unavailable agent".into()),
                        avatar: None,
                    })
            })
            .collect()
    }

    fn avatar(&self, agent: &AgentChoice, size: AvatarSize, cx: &App) -> Avatar {
        let mut avatar = Avatar::new(agent.name.clone()).size(size).squircle();
        if let (Some(url), Some(community)) = (&agent.avatar, &self.community) {
            avatar = avatar.image(crate::ui::media::avatar(
                url,
                community.read(cx).session.media(),
            ));
        }
        avatar
    }

    fn template_item(
        &self,
        entry: &KitEntry,
        template: &Template,
        catalog: &Catalog,
        disabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let value = KitValue::Template(template.clone());
        let members = Self::members(&value, &catalog.kit.entries, &catalog.agents);
        let teams = buzz_relay_client::kit::teams(&catalog.kit.entries);
        let missing = template
            .team_ids
            .iter()
            .any(|id| !teams.iter().any(|t| t.id == *id));
        let avatars: Vec<AnyElement> = members
            .iter()
            .map(|agent| {
                let tip = (agent.name.clone(), super::npub(&agent.pubkey));
                div()
                    .id(SharedString::from(format!(
                        "avatar-{}-{}",
                        template.id, agent.pubkey
                    )))
                    .flex_shrink_0()
                    .child(self.avatar(agent, AvatarSize::Small, cx))
                    .tooltip(move |_, cx| {
                        let tip = tip.clone();
                        cx.new(|_| AgentTip(tip.0.into(), tip.1.into())).into()
                    })
                    .into_any_element()
            })
            .collect();
        div()
            .id(SharedString::from(format!("template-{}", template.id)))
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space::S4)
            .p(space::S4)
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::CONTAINER)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S1)
                    .min_w(rems(16.))
                    .flex_1()
                    .child(div().text_label_sm().child(template.name.clone()))
                    .when(!template.description.trim().is_empty(), |this| {
                        this.child(
                            div()
                                .text_body_sm()
                                .text_color(c.text_subtle)
                                .line_clamp(2)
                                .child(template.description.clone()),
                        )
                    })
                    .when(!template.team_ids.is_empty(), |this| {
                        let n = template.team_ids.len();
                        this.child(
                            div()
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child(format!("{n} {}", if n == 1 { "team" } else { "teams" })),
                        )
                    })
                    .when(missing, |this| {
                        this.child(
                            div()
                                .text_caption()
                                .text_color(c.text_danger)
                                .child("A saved team is unavailable. Edit to replace it."),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::S3)
                    .min_w(rems(16.))
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(space::S1)
                            .min_w_0()
                            .children(avatars)
                            .when(members.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_caption()
                                        .text_color(c.text_subtle)
                                        .child("No agents"),
                                )
                            }),
                    )
                    .child(
                        self.menu(&value, entry, disabled, window, cx)
                            .flex_shrink_0(),
                    ),
            )
            .into_any_element()
    }

    fn team_card(
        &self,
        entry: &KitEntry,
        team: &Team,
        catalog: &Catalog,
        disabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let value = KitValue::Team(team.clone());
        let members = Self::members(&value, &catalog.kit.entries, &catalog.agents);
        let n = members.len();
        let stack: Vec<AnyElement> = members
            .iter()
            .take(4)
            .enumerate()
            .map(|(index, agent)| {
                div()
                    .when(index > 0, |this| this.ml(rems(-1.5)))
                    .when(index == 0 || index == 3, |this| this.mt(rems(0.4)))
                    .border_2()
                    .border_color(c.surface_panel)
                    .rounded(radius::CONTAINER)
                    .child(self.avatar(agent, AvatarSize::Px(44.), cx))
                    .into_any_element()
            })
            .collect();
        let (edit_value, event) = (value.clone(), entry.event_id);
        div()
            .relative()
            .min_w_0()
            .w(rems(12.))
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::CONTAINER)
            .child(
                div()
                    .id(SharedString::from(format!("team-{}", team.id)))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .w_full()
                    .min_h(rems(14.))
                    .py(space::S6)
                    .px(space::S3)
                    .rounded(radius::CONTAINER)
                    .when(disabled, |this| this.opacity(0.6))
                    .when(!disabled, |this| {
                        this.cursor_pointer()
                            .hover(move |s| s.bg(c.surface_base))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open(edit_value.clone(), Some(event), window, cx)
                            }))
                    })
                    .child(
                        div()
                            .relative()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(rems(5.))
                            .px(space::S2)
                            .children(stack)
                            .when(n == 0, |this| {
                                this.child(
                                    Icon::new(IconName::Users).size(48.).color(c.text_subtle),
                                )
                            })
                            .when(n > 4, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .right_0()
                                        .bottom(space::S2)
                                        .min_w(rems(1.25))
                                        .px(space::S1)
                                        .rounded_full()
                                        .bg(c.surface_panel)
                                        .text_caption()
                                        .text_center()
                                        .child(format!("+{}", n - 4)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(space::S4)
                            .text_label_sm()
                            .text_center()
                            .child(team.name.clone()),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .child(format!("{n} {}", if n == 1 { "member" } else { "members" })),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(space::S2)
                    .right(space::S2)
                    .child(self.menu(&value, entry, disabled, window, cx)),
            )
            .into_any_element()
    }
}

/// An avatar's tooltip: the agent's name and npub.
struct AgentTip(SharedString, SharedString);

impl Render for AgentTip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Tooltip::placed(
            Tooltip::surface(cx)
                .child(div().text_label_sm().child(self.0.clone()))
                .child(div().text_mono().child(self.1.clone())),
        )
    }
}

impl Render for TemplateLibrary {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(community) = self.community.clone() else {
            return div();
        };
        let c = *cx.colors();
        let catalog = Catalog::read(&community, cx);
        let section = self.section;
        let disabled = catalog.kit.status != KitStatus::Ready || !catalog.agents_ready;
        let loading = catalog.kit.status == KitStatus::Loading;
        let refresh = |id: &'static str, label: &'static str, cx: &mut Context<Self>| {
            Button::new(id, label)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Sm)
                .icon(IconName::ArrowsClockwise)
                .loading(loading)
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
                .into_any_element()
        };
        let entries: Vec<&KitEntry> = catalog
            .kit
            .entries
            .iter()
            .filter(|e| !e.record.deleted)
            .filter(|e| {
                matches!(
                    (&e.record.value, section),
                    (KitValue::Template(_), Section::Template) | (KitValue::Team(_), Section::Team)
                )
            })
            .collect();
        let count = entries.len();
        let title = match section {
            Section::Template => div()
                .flex()
                .items_baseline()
                .child("Channel templates")
                .child(
                    div()
                        .ml(space::S2)
                        .text_color(c.text_subtle)
                        .child(count.to_string()),
                )
                .into_any_element(),
            Section::Team => "Agent teams".into_any_element(),
        };
        let mut actions = Vec::new();
        if section == Section::Team {
            actions.push(refresh("refresh-teams", "Refresh teams", cx));
        }
        actions.push(
            Button::new(
                "library-create",
                match section {
                    Section::Template => "New template",
                    Section::Team => "Create team",
                },
            )
            .size(ButtonSize::Sm)
            .icon(IconName::Plus)
            .disabled(disabled)
            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx)))
            .into_any_element(),
        );
        let empty = entries.is_empty();
        let items: Vec<AnyElement> = entries
            .iter()
            .map(|entry| match &entry.record.value {
                KitValue::Template(template) => {
                    self.template_item(entry, template, &catalog, disabled, window, cx)
                }
                KitValue::Team(team) => self.team_card(entry, team, &catalog, disabled, window, cx),
                KitValue::Groups(_) => div().into_any_element(),
            })
            .collect();
        // Settings titles line up with the content inside group borders.
        let inset = if section == Section::Template {
            rems(1.0625)
        } else {
            rems(0.)
        };
        let status = catalog.status(match section {
            Section::Template => "Loading your templates…",
            Section::Team => "Loading your teams…",
        });
        let body = div()
            .flex()
            .flex_col()
            .gap(space::S8)
            .children(status.map(|text| line(text, false, cx).px(inset)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S4)
                    .min_w_0()
                    .child(
                        inline_header(
                            title,
                            (section == Section::Template)
                                .then_some("Agent and canvas presets for new channels."),
                            actions,
                            cx,
                        )
                        .px(inset),
                    )
                    .when(!empty, |this| match section {
                        Section::Template => {
                            this.child(div().flex().flex_col().gap(space::S3).children(items))
                        }
                        Section::Team => {
                            this.child(div().flex().flex_wrap().gap(space::S4).children(items))
                        }
                    })
                    .when(empty && catalog.ready(), |this| {
                        this.child(match section {
                            Section::Template => empty_state(
                                IconName::FileText,
                                "Your next channel starts here",
                                "Create a template, or save an existing channel as one from its menu.",
                                cx,
                            ),
                            Section::Team => empty_state(
                                IconName::Users,
                                "Bring your agents together",
                                "Create a team to reuse your favorite combination of agents.",
                                cx,
                            ),
                        })
                    }),
            );
        let modal = if let Some((dialog, _)) = &self.editing {
            Some(dialog.clone().into_any_element())
        } else {
            self.deleting.as_ref().map(|deleting| {
                let weak = cx.entity().downgrade();
                let dismiss: Dismiss = Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if !this.deleting.as_ref().is_some_and(|d| d.busy) {
                            this.deleting = None;
                            cx.notify();
                        }
                    });
                });
                overlay::modal(window, self.delete_dialog(deleting, cx), Some(dismiss))
                    .into_any_element()
            })
        };
        div()
            .flex()
            .flex_col()
            .when(section == Section::Template, |this| {
                this.child(header(
                    "Templates",
                    Some("Private to you in this community"),
                    vec![refresh("refresh-templates", "Refresh", cx)],
                    cx,
                ))
            })
            .child(body)
            .children(modal)
    }
}

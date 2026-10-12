//! The lineup fields (`TemplateFields.tsx`): saved teams, individual
//! agents found by name or npub, the member preview, and the starting
//! Canvas, in two form disclosures.

use super::dialog::TemplateDialog;
use super::owner::IdentityPreview;
use super::{Catalog, field, line, public_key_labels};
use crate::ui::keys::npub;
use buzz_relay_client::kit::{self, AgentChoice, KitValue};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Checkbox, Icon, IconName, Surface, TextArea, TextField,
    TypeStyled, tokens::radius, tokens::space,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

/// `choices`: the community's agents, plus selected ones no longer in it.
fn choices(catalog: &Catalog, selected: &[String]) -> Vec<AgentChoice> {
    let mut choices = catalog.agents.clone();
    for key in selected {
        if !choices.iter().any(|a| a.pubkey == *key) {
            choices.push(AgentChoice {
                pubkey: key.clone(),
                name: "Unavailable agent".into(),
                avatar: None,
            });
        }
    }
    choices
}

/// Agents whose name, npub or key contain `search`, ignoring case.
pub(super) fn matches(choices: &[AgentChoice], search: &str) -> Vec<AgentChoice> {
    let needle = search.to_lowercase();
    choices
        .iter()
        .filter(|agent| {
            format!("{} {} {}", agent.name, npub(&agent.pubkey), agent.pubkey)
                .to_lowercase()
                .contains(&needle)
        })
        .cloned()
        .collect()
}

/// A form disclosure trigger (`Accordion variant="form"`).
fn disclosure(id: &'static str, title: String, open: bool, cx: &App) -> Stateful<Div> {
    let c = *cx.colors();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap(space::S3)
        .p(space::S2)
        .rounded(radius::CONTROL)
        .cursor_pointer()
        .hover(move |s| s.bg(c.affordance_selected))
        .child(div().text_label_sm().child(title))
        .child(
            Icon::new(if open {
                IconName::CaretUp
            } else {
                IconName::CaretDown
            })
            .size(16.)
            .color(c.text_standard),
        )
}

impl TemplateDialog {
    fn selected_agents(&self) -> Vec<String> {
        match &self.draft {
            KitValue::Template(template) => template.agents.clone(),
            KitValue::Team(team) => team.agents.clone(),
            KitValue::Groups(_) => Vec::new(),
        }
    }

    /// `AgentSelection`: search and a checkbox per agent.
    pub(super) fn agent_selection(
        &self,
        catalog: &Catalog,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let selected = self.selected_agents();
        let choices = choices(catalog, &selected);
        let labels = public_key_labels(choices.iter().map(|a| &a.pubkey));
        let search = self.search.read(cx).value().to_string();
        let found = matches(&choices, &search);
        let media = self.community.read(cx).session.media().clone();
        let rows: Vec<AnyElement> = found
            .iter()
            .map(|agent| {
                let checked = selected.contains(&agent.pubkey);
                let key = agent.pubkey.clone();
                let label = labels.get(&key).cloned().unwrap_or_default();
                let mut avatar = Avatar::new(agent.name.clone())
                    .size(AvatarSize::Small)
                    .squircle();
                if let Some(url) = &agent.avatar {
                    avatar = avatar.image(crate::ui::media::avatar(url, &media));
                }
                let toggle = cx.listener({
                    let key = key.clone();
                    move |this: &mut Self, _: &ClickEvent, _, cx| {
                        let mut agents = this.selected_agents();
                        if agents.contains(&key) {
                            agents.retain(|k| *k != key);
                        } else {
                            agents.push(key.clone());
                        }
                        this.set_agents(agents, cx);
                    }
                });
                let preview = (self.community.clone(), agent.clone(), npub(&agent.pubkey));
                div()
                    .id(SharedString::from(format!("agent-{key}")))
                    .flex()
                    .items_center()
                    .gap(space::S3)
                    .min_w_0()
                    .py(space::S1)
                    .cursor_pointer()
                    .on_click(toggle)
                    .tooltip(move |_, cx| {
                        let (community, agent, npub) = preview.clone();
                        cx.new(|_| IdentityPreview::new(community, agent, npub))
                            .into()
                    })
                    .child(Checkbox::new(
                        SharedString::from(format!("check-{key}")),
                        checked,
                    ))
                    .child(avatar)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().text_body_sm().truncate().child(agent.name.clone()))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .truncate()
                                    .child(format!("Agent · {label}")),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .when(!choices.is_empty(), |this| {
                this.child(field(
                    "Find individual agents",
                    None,
                    TextField::new(&self.search).surface(Surface::Floating),
                    cx,
                ))
            })
            .child(
                div()
                    .id("agent-choices")
                    .flex()
                    .flex_col()
                    .gap(space::S2)
                    .max_h(rems(12.))
                    .overflow_y_scroll()
                    .children(rows)
                    .when(!choices.is_empty() && found.is_empty(), |this| {
                        this.child(line("No agents match your search.", false, cx))
                    })
                    .when(choices.is_empty(), |this| {
                        this.child(div().text_body().text_color(c.text_subtle).child(
                            "No agents from the Agents page are available in this community.",
                        ))
                    }),
            )
    }

    /// `TemplateFields`: "Teams & agents" and "Starting Canvas".
    pub(super) fn template_fields(
        &self,
        catalog: &Catalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let lineup = self.lineup(cx);
        let teams = kit::teams(&catalog.kit.entries);
        let missing: Vec<&String> = lineup
            .team_ids
            .iter()
            .filter(|id| !teams.iter().any(|t| t.id == **id))
            .collect();
        let (preview, error) =
            match kit::resolve_lineup(&lineup, &catalog.kit.entries, &catalog.agents) {
                Ok(agents) => {
                    let labels = public_key_labels(agents.iter().map(|a| &a.pubkey));
                    let text = agents
                        .iter()
                        .map(|a| {
                            format!(
                                "{} · {}",
                                a.name,
                                labels.get(&a.pubkey).cloned().unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    (
                        if text.is_empty() {
                            "Only you".to_owned()
                        } else {
                            text
                        },
                        None,
                    )
                }
                Err(error) => (String::new(), Some(error)),
            };
        let count = lineup.team_ids.len() + lineup.agents.len();
        let mut team_rows: Vec<AnyElement> = teams
            .iter()
            .map(|team| {
                (
                    team.id.clone(),
                    format!("{} ({})", team.name, team.agents.len()),
                )
            })
            .chain(
                missing
                    .iter()
                    .map(|id| ((*id).clone(), "Unavailable team (0)".to_owned())),
            )
            .map(|(id, label)| {
                let checked = lineup.team_ids.contains(&id);
                let ids = lineup.team_ids.clone();
                let weak = cx.entity().downgrade();
                Checkbox::new(SharedString::from(format!("team-{id}")), checked)
                    .label(label)
                    .surface(Surface::Floating)
                    .on_change(move |checked, _, cx| {
                        let mut ids = ids.clone();
                        if checked {
                            ids.push(id.clone());
                        } else {
                            ids.retain(|k| *k != id);
                        }
                        let _ = weak.update(cx, |this, cx| this.set_teams(ids, cx));
                    })
                    .into_any_element()
            })
            .collect();
        if team_rows.is_empty() {
            team_rows.push(
                div()
                    .text_body()
                    .text_color(c.text_subtle)
                    .child("Create reusable teams on the Agents page.")
                    .into_any_element(),
            );
        }
        let members_open = self.members_open;
        let canvas_open = self.canvas_open;
        let members = div()
            .flex()
            .flex_col()
            .child(
                disclosure(
                    "disclose-members",
                    if count > 0 {
                        format!("Teams & agents ({count})")
                    } else {
                        "Teams & agents".into()
                    },
                    members_open,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.members_open = !this.members_open;
                    cx.notify();
                })),
            )
            .when(members_open, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space::S4)
                        .p(space::S2)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(space::S2)
                                .child(div().mb(space::S2).text_label_sm().child("Saved teams"))
                                .children(team_rows),
                        )
                        .child(self.agent_selection(catalog, window, cx))
                        .child(
                            div()
                                .text_body()
                                .text_color(c.text_subtle)
                                .child(if error.is_none() {
                                    format!(
                                        "Channel members: {preview}. Adding agents does not start them or send a message."
                                    )
                                } else {
                                    "Adding agents does not start them or send a message.".into()
                                }),
                        ),
                )
            });
        let canvas =
            div()
                .flex()
                .flex_col()
                .child(
                    disclosure(
                        "disclose-canvas",
                        if lineup.canvas.is_empty() {
                            "Starting Canvas".into()
                        } else {
                            "Starting Canvas · Added".into()
                        },
                        canvas_open,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.canvas_open = !this.canvas_open;
                        cx.notify();
                    })),
                )
                .when(canvas_open, |this| {
                    this.child(div().p(space::S2).child(field(
                    "Starting Canvas (Markdown)",
                    Some("Optional. Copied into each new channel when this template is applied."),
                    TextArea::new(&self.canvas).rows(8).surface(Surface::Floating),
                    cx,
                )))
                });
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .children(error.map(|text| line(text, true, cx)))
            .child(
                div()
                    .mx(rems(-0.5))
                    .flex()
                    .flex_col()
                    .child(members)
                    .child(canvas),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn agents_match_by_name_or_key() {
        let agent = AgentChoice {
            pubkey: "ab".repeat(32),
            name: "Reviewer".into(),
            avatar: None,
        };
        let list = [agent];
        assert_eq!(matches(&list, "review").len(), 1);
        assert_eq!(matches(&list, "abab").len(), 1);
        assert_eq!(matches(&list, "npub1").len(), 1);
        assert!(matches(&list, "writer").is_empty());
    }
}

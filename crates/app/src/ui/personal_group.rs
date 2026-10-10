//! The "Personal group" select in Channel settings (`ChannelsPage.tsx`
//! `setupTools`): which of the viewer's personal groups the channel sits
//! in. Shown once the personal-groups record exists.

use super::overlay::{self, Align, Dismiss};
use crate::community::Community;
use crate::mirror::mirror;
use buzz_relay_client::me_kit::{self as kit, GroupsId};
use buzz_relay_client::session::MeKitSnapshot;
use buzz_ui::{ActiveTheme, Menu, MenuItem, Select, Surface, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

pub struct PersonalGroupSelect {
    community: Entity<Community>,
    channel: String,
    kit: Arc<MeKitSnapshot>,
    open: bool,
    error: Option<SharedString>,
    _watch: Task<()>,
}

impl PersonalGroupSelect {
    pub fn new(community: Entity<Community>, channel: String, cx: &mut Context<Self>) -> Self {
        let kit = community.read(cx).session.me_kit().clone();
        kit.ensure();
        let watch = mirror(kit.watch(), cx, |this: &mut Self, value| this.kit = value);
        Self {
            community,
            channel,
            kit: kit.snapshot(),
            open: false,
            error: None,
            _watch: watch,
        }
    }

    fn choose(&mut self, group: Option<String>, cx: &mut Context<Self>) {
        self.open = false;
        let Some(entry) = kit::groups_entry(&self.kit.entries, GroupsId::Personal).cloned() else {
            return;
        };
        let Some(mut groups) = entry.groups() else {
            return;
        };
        match &group {
            Some(group) => groups
                .assignments
                .insert(self.channel.clone(), group.clone()),
            None => groups.assignments.remove(&self.channel),
        };
        self.error = None;
        let kit = self.community.read(cx).session.me_kit().clone();
        cx.spawn(async move |this, cx| {
            let result = kit
                .save(
                    kit::groups_value(GroupsId::Personal, &groups),
                    Some(entry.event_id),
                    false,
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some(format!("Error: {error}").into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for PersonalGroupSelect {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(groups) = self.kit.personal() else {
            return div().into_any_element();
        };
        let c = *cx.colors();
        let current = groups.assignments.get(&self.channel).cloned();
        let label = current
            .as_ref()
            .and_then(|id| groups.groups.iter().find(|g| g.id == *id))
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "No group".into());
        let open = self.open;
        let mut field = div().relative().w_full().child(
            Select::new("personal-group")
                .value(label)
                .open(open)
                .surface(Surface::Panel)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open = !this.open;
                    cx.notify();
                })),
        );
        if open {
            let mut menu = Menu::new().child(
                MenuItem::new("personal-group-none", "No group")
                    .checked(current.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.choose(None, cx))),
            );
            for group in &groups.groups {
                let id = group.id.clone();
                menu = menu.child(
                    MenuItem::new(
                        SharedString::from(format!("personal-group-{}", group.id)),
                        group.name.clone(),
                    )
                    .checked(current.as_deref() == Some(group.id.as_str()))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(Some(id.clone()), cx))),
                );
            }
            let weak = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.open = false;
                    cx.notify();
                });
            });
            field = field.child(overlay::dropdown(
                window,
                Align::Start,
                div().min_w(rems(16.)).child(menu),
                dismiss,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .child(div().text_label().child("Personal group"))
            .child(field)
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_body_sm().text_color(c.text_danger).child(error))
            })
            .into_any_element()
    }
}

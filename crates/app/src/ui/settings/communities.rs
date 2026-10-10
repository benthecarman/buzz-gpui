//! Settings → Communities: the saved communities, and leaving one
//! (relay.md §1.8).

use super::{Modal, SettingsPage, button, header, note, preference_row, rows_group, section};
use crate::community::{AppState, Community};
use buzz_ui::{ActiveTheme, AlertDialog, Avatar, AvatarSize, Badge, Button, Surface, TypeStyled};
use gpui_kit::*;

impl SettingsPage {
    pub(super) fn communities_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = cx.global::<AppState>();
        let selected = state.selected;
        let communities = state.communities.clone();
        let mut parts = vec![
            header(
                "Communities",
                Some(
                    "Communities on this device. Leaving one ends your membership and removes \
                     its data from this device."
                        .into(),
                ),
                cx,
            )
            .into_any_element(),
        ];
        if communities.is_empty() {
            parts.push(note("You haven’t added a community yet.", cx).into_any_element());
            return section(parts);
        }
        let rows = communities
            .iter()
            .enumerate()
            .map(|(index, community)| {
                let (name, origin) = {
                    let community = community.read(cx);
                    (community.name(), community.origin.to_string())
                };
                let target = community.clone();
                let mut trailing = Vec::new();
                if selected == Some(index) {
                    trailing.push(Badge::new("Selected").into_any_element());
                }
                trailing.push(
                    button(SharedString::from(format!("leave-{index}")), "Leave")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.modal = Some(Modal::Leave {
                                community: target.clone(),
                                busy: false,
                                error: None,
                            });
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                preference_row(
                    Some(
                        Avatar::new(name.clone())
                            .squircle()
                            .size(AvatarSize::Px(20.))
                            .into_any_element(),
                    ),
                    name,
                    Some(SharedString::from(origin).into_any_element()),
                    trailing,
                    cx,
                )
                .into_any_element()
            })
            .collect();
        parts.push(rows_group(rows, cx).into_any_element());
        section(parts)
    }

    fn leave(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Leave {
            community, busy, ..
        }) = &mut self.modal
        else {
            return;
        };
        *busy = true;
        let task = AppState::leave(community.clone(), cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.modal = None,
                    Err(message) => {
                        if let Some(Modal::Leave { busy, error, .. }) = &mut this.modal {
                            *busy = false;
                            *error = Some(message.into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn leave_dialog(
        &self,
        community: &Entity<Community>,
        busy: bool,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let name = community.read(cx).name();
        AlertDialog::new(format!("Leave {name}?"))
            .description(format!(
                "You’ll stop being a member of {name} and lose access to its channels. Its \
                 messages and settings on this device are removed. To come back, someone has \
                 to add you again."
            ))
            .children(
                error.map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .action(
                Button::new("leave-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
            )
            .action(
                Button::new("leave-confirm", "Leave community")
                    .destructive()
                    .loading(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.leave(cx))),
            )
            .into_any_element()
    }
}

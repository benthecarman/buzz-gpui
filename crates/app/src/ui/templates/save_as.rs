//! "Save as template…" in channel settings (`SaveAsTemplate`): copies the
//! channel's name, its known agents and its Canvas into a new template
//! the editor opens with.

use super::dialog::{Closed, TemplateDialog};
use super::{Catalog, line, watch_kit};
use crate::community::Community;
use buzz_relay_client::kit::{KitStatus, KitValue, Template};
use buzz_ui::{Button, tokens::space};
use gpui_kit::*;

const INCOMPLETE: &str = "Incomplete agent inventory: this copy includes only currently known agents and may omit others in the channel. Review the lineup before saving; reload agents and copy again for a complete snapshot.";

pub struct SaveAsTemplate {
    community: Entity<Community>,
    channel: String,
    /// Offered as a channel menu item: only the editor renders here.
    in_menu: bool,
    busy: bool,
    error: Option<SharedString>,
    dialog: Option<(Entity<TemplateDialog>, Subscription)>,
    _kit: Task<()>,
}

impl SaveAsTemplate {
    pub fn new(community: Entity<Community>, channel: String, cx: &mut Context<Self>) -> Self {
        Self {
            _kit: watch_kit(&community, cx),
            community,
            channel,
            in_menu: false,
            busy: false,
            error: None,
            dialog: None,
        }
    }

    /// The channel menu's "Save as template…" (`menu` prop).
    pub fn menu(community: Entity<Community>, channel: String, cx: &mut Context<Self>) -> Self {
        Self {
            in_menu: true,
            ..Self::new(community, channel, cx)
        }
    }

    pub fn disabled(&self, cx: &App) -> bool {
        let catalog = Catalog::read(&self.community, cx);
        self.busy || catalog.kit.status == KitStatus::Unavailable || !catalog.agents_ready
    }

    /// Reads the channel's Canvas and opens the editor with the copy.
    pub fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled(cx) {
            return;
        }
        let community = self.community.read(cx);
        let Some(channel) = community
            .snapshot
            .channels
            .iter()
            .find(|c| c.id == self.channel)
            .cloned()
        else {
            return;
        };
        let session = community.session.clone();
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let canvas = session.kit_canvas(&channel.id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match canvas {
                    Ok(canvas) => {
                        let catalog = Catalog::read(&this.community, cx);
                        let template = Template {
                            name: channel.name.clone(),
                            agents: catalog
                                .agents
                                .iter()
                                .filter(|a| channel.members.contains(&a.pubkey))
                                .map(|a| a.pubkey.clone())
                                .collect(),
                            canvas: canvas.unwrap_or_default(),
                            ..Template::blank()
                        };
                        let notice = (!catalog.agents_complete).then(|| INCOMPLETE.to_owned());
                        let community = this.community.clone();
                        let dialog = cx.new(|cx| {
                            TemplateDialog::new(
                                community,
                                KitValue::Template(template),
                                None,
                                notice,
                                window,
                                cx,
                            )
                        });
                        let subscription = cx.subscribe(&dialog, |this, _, _: &Closed, cx| {
                            this.dialog = None;
                            cx.notify();
                        });
                        this.dialog = Some((dialog, subscription));
                    }
                    Err(error) if this.in_menu => crate::ui::notice::error(error, cx),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for SaveAsTemplate {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.in_menu {
            return div().children(self.dialog.as_ref().map(|(dialog, _)| dialog.clone()));
        }
        let disabled = self.disabled(cx);
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(space::S2)
            .child(
                Button::new("save-as-template", "Save as template…")
                    .full_width()
                    .loading(self.busy)
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, window, cx| this.copy(window, cx))),
            )
            .children(self.error.clone().map(|error| line(error, true, cx)))
            .children(self.dialog.as_ref().map(|(dialog, _)| dialog.clone()))
    }
}

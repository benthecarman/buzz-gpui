//! Conversation activity above the composer (ui.md §4.10,
//! `TypingIndicator.tsx`): who is typing on the left, and the viewer's
//! own working agents on the right, as an avatar stack that opens a
//! "Working now" list.

use super::media;
use super::message::display_name;
use crate::hosts::Hosts;
use buzz_relay_client::{EventId, Media, Profiles};
use buzz_ui::tokens::shadow;
use buzz_ui::{ActiveTheme, Avatar, AvatarSize, Popover, PopoverPadding, PopoverWidth, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

/// The chooser opens after the pointer rests this long, and closes this
/// long after it leaves (`delay={250} closeDelay={150}`).
const OPEN_DELAY: Duration = Duration::from_millis(250);
const CLOSE_DELAY: Duration = Duration::from_millis(150);
/// Avatars shown in the stack before "+N".
const STACK: usize = 4;
/// Windows this narrow inset the indicator 12px instead of 16px.
pub const NARROW: Pixels = px(650.);

/// "Alice is typing", "Alice, Bob are typing", "Alice, Bob and 2 others
/// are typing".
pub fn label(names: &[SharedString]) -> String {
    let shown = names
        .iter()
        .take(2)
        .map(SharedString::as_ref)
        .collect::<Vec<&str>>()
        .join(", ");
    let others = match names.len() {
        0..=2 => String::new(),
        n => format!(" and {} others", n - 2),
    };
    let verb = if names.len() == 1 {
        " is typing"
    } else {
        " are typing"
    };
    format!("{shown}{others}{verb}")
}

/// The viewer's own agents, on every host (`agentChoices`).
pub fn owned_agents(cx: &App) -> HashSet<String> {
    let Some(hosts) = cx.try_global::<Hosts>() else {
        return HashSet::new();
    };
    hosts
        .hosts
        .iter()
        .flat_map(|host| host.read(cx).agents.agents.iter())
        .map(|agent| agent.pubkey.clone())
        .collect()
}

/// Who is active in a conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    /// The viewer's working agents by key, each with where it works:
    /// `None` is the channel conversation, `Some` a thread root.
    pub agents: BTreeMap<String, Vec<Option<String>>>,
    /// Everyone else typing here, in the order they started.
    pub humans: Vec<String>,
}

impl Activity {
    /// A channel: its own typists, plus the viewer's agents anywhere in it,
    /// threads included. `in_channel` says whether a thread root is one of
    /// this channel's loaded messages.
    pub fn channel(
        typing: &[String],
        threads: &HashMap<String, Vec<String>>,
        in_channel: impl Fn(&str) -> bool,
        owned: &HashSet<String>,
    ) -> Self {
        let mut activity = Self::default();
        for person in typing {
            if owned.contains(person) {
                activity.add(person, None);
            }
        }
        let mut roots: Vec<&String> = threads.keys().filter(|root| in_channel(root)).collect();
        roots.sort();
        for root in roots {
            for person in &threads[root] {
                if owned.contains(person) {
                    activity.add(person, Some(root.clone()));
                }
            }
        }
        activity.humans = typing
            .iter()
            .filter(|person| !activity.agents.contains_key(*person))
            .cloned()
            .collect();
        activity
    }

    /// A thread: only those typing in it.
    pub fn thread(typing: &[String], root: &str, owned: &HashSet<String>) -> Self {
        let mut activity = Self::default();
        for person in typing {
            if owned.contains(person) {
                activity.add(person, Some(root.to_string()));
            } else {
                activity.humans.push(person.clone());
            }
        }
        activity
    }

    fn add(&mut self, agent: &str, root: Option<String>) {
        let roots = self.agents.entry(agent.to_string()).or_default();
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
}

/// Where an agent works, for the chooser's rows in a channel:
/// "Channel conversation", "Thread · {excerpt}", at most two, then
/// "+N more".
pub fn scope_label(roots: &[Option<String>], excerpt: impl Fn(&str) -> Option<String>) -> String {
    let mut labels: Vec<String> = roots
        .iter()
        .take(2)
        .map(|root| match root {
            None => "Channel conversation".to_string(),
            Some(root) => {
                let text = excerpt(root)
                    .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                    .map(|text| text.chars().take(60).collect::<String>())
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| root.chars().take(8).collect());
                format!("Thread · {text}")
            }
        })
        .collect();
    if roots.len() > 2 {
        labels.push(format!("+{} more", roots.len() - 2));
    }
    labels.join(" / ")
}

/// Three dots fading to 35% and back over 1.4 s, 0.2 s apart.
fn dots(color: Hsla, cx: &App) -> AnyElement {
    const PERIOD: f32 = 1.4;
    let row = div().flex().flex_none().items_center().gap(px(2.));
    let dot = move |opacity: f32| div().size(px(4.)).rounded_full().bg(color).opacity(opacity);
    if cx.reduce_motion() {
        return row.children((0..3).map(|_| dot(1.))).into_any_element();
    }
    row.with_animation(
        "typing-dots",
        Animation::new(Duration::from_millis(1400)).repeat(),
        move |row, delta| {
            row.children((0..3).map(|n| {
                let phase = (delta - n as f32 * 0.2 / PERIOD).rem_euclid(1.);
                // Down to the 50% keyframe and back, eased both ways.
                let fade = ease_in_out(1. - (2. * phase - 1.).abs());
                dot(1. - 0.65 * fade)
            }))
        },
    )
    .into_any_element()
}

/// Opens the conversation an agent works in: its thread, or the channel.
pub type OnOpen = Rc<dyn Fn(Option<EventId>, &mut Window, &mut App)>;

/// One agent in the chooser.
#[derive(Clone)]
struct Row {
    pubkey: String,
    name: SharedString,
    picture: Option<String>,
    /// Where it works, in a channel view.
    scope: Option<String>,
    /// The thread its conversation opens, when it works in exactly one.
    thread: Option<EventId>,
}

/// The viewer's working agents: an avatar stack that opens "Working now"
/// on hover or click (`AgentChooser`).
pub struct AgentChooser {
    rows: Vec<Row>,
    media: Option<Media>,
    on_open: Option<OnOpen>,
    open: bool,
    /// The trigger or the list is under the pointer.
    hovered: (bool, bool),
    _pending: Option<Task<()>>,
}

impl AgentChooser {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            media: None,
            on_open: None,
            open: false,
            hovered: (false, false),
            _pending: None,
        }
    }

    /// Follows the activity. `thread` is the panel's root, or `None` in a
    /// channel, where `excerpt` names threads from the loaded rows. Names
    /// and pictures come from `people`.
    pub fn set(
        &mut self,
        activity: &Activity,
        thread: Option<&str>,
        (profiles, media): (&Profiles, &Media),
        excerpt: impl Fn(&str) -> Option<String>,
        on_open: OnOpen,
        cx: &mut Context<Self>,
    ) {
        let rows: Vec<Row> = activity
            .agents
            .iter()
            .map(|(pubkey, roots)| {
                let known: Vec<&String> = roots.iter().flatten().collect();
                let conversation = thread
                    .map(str::to_string)
                    .or_else(|| (known.len() == 1).then(|| known[0].clone()));
                Row {
                    pubkey: pubkey.clone(),
                    name: display_name(profiles, pubkey),
                    picture: profiles.get(pubkey).and_then(|p| p.picture.clone()),
                    scope: thread.is_none().then(|| scope_label(roots, &excerpt)),
                    thread: conversation.and_then(|root| EventId::from_hex(&root).ok()),
                }
            })
            .collect();
        if rows.is_empty() && self.open {
            self.open = false;
        }
        self.rows = rows;
        self.media = Some(media.clone());
        self.on_open = Some(on_open);
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self._pending = None;
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    fn hover(&mut self, trigger: bool, hovered: bool, cx: &mut Context<Self>) {
        if trigger {
            self.hovered.0 = hovered;
        } else {
            self.hovered.1 = hovered;
        }
        let inside = self.hovered.0 || self.hovered.1;
        if inside == self.open {
            self._pending = None;
            return;
        }
        let delay = if inside { OPEN_DELAY } else { CLOSE_DELAY };
        self._pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                let inside = this.hovered.0 || this.hovered.1;
                if this.open != inside && !this.rows.is_empty() {
                    this.open = inside;
                    cx.notify();
                }
            });
        }));
    }

    fn avatar(&self, row: &Row, size: f32) -> Avatar {
        let mut avatar = Avatar::new(row.name.clone())
            .size(AvatarSize::Px(size))
            .squircle();
        if let (Some(picture), Some(media)) = (&row.picture, &self.media) {
            avatar = avatar.image(media::avatar(picture, media));
        }
        avatar
    }

    fn list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let rows = self.rows.iter().map(|row| {
            let (target, on_open) = (row.thread, self.on_open.clone());
            div()
                .id(SharedString::from(format!("working-agent-{}", row.pubkey)))
                .flex()
                .items_center()
                .gap(px(12.))
                .p(px(8.))
                .my(px(2.))
                .rounded(buzz_ui::tokens::radius::FLOATING_INNER)
                .cursor_pointer()
                .hover(|s| s.bg(c.affordance_floating_hover))
                .child(div().flex_none().child(self.avatar(row, 30.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_body_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(c.text_standard)
                                .truncate()
                                .child(row.name.clone()),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child("Working"),
                        )
                        .children(row.scope.clone().map(|scope| {
                            div()
                                .text_caption()
                                .text_color(c.text_subtle)
                                .truncate()
                                .child(scope)
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close(cx);
                    if let Some(on_open) = &on_open {
                        on_open(target, window, cx);
                    }
                }))
        });
        Popover::new()
            .width(PopoverWidth::Wide)
            .padding(PopoverPadding::List)
            .child(
                div()
                    .id("working-now")
                    .flex()
                    .flex_col()
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .min_h(px(28.))
                            .px(px(8.))
                            .py(px(2.))
                            .text_body_sm()
                            .text_color(c.text_metadata)
                            .child("Working now"),
                    )
                    .children(rows),
            )
    }
}

impl Default for AgentChooser {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for AgentChooser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            return div().into_any_element();
        }
        let c = *cx.colors();
        let overflow = self.rows.len().saturating_sub(STACK);
        let stack = div()
            .flex()
            .items_center()
            .pr(px(4.))
            .children(self.rows.iter().take(STACK).map(|row| {
                div()
                    .flex_none()
                    .size(px(24.))
                    .p(px(2.))
                    .mr(px(-4.))
                    .rounded(buzz_ui::tokens::radius::CONTROL)
                    .bg(c.surface_panel)
                    .shadow(shadow::sm())
                    .child(self.avatar(row, 20.))
            }))
            .when(overflow > 0, |d| {
                d.child(
                    div()
                        .pl(px(8.))
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child(format!("+{overflow}")),
                )
            });
        let trigger = div()
            .id("agent-activity")
            .flex()
            .items_center()
            .min_h(px(32.))
            .p(px(4.))
            .rounded(buzz_ui::tokens::radius::CONTROL)
            .cursor_pointer()
            .when(self.open, |d| d.bg(c.affordance_subtle_hover))
            .hover(|s| s.bg(c.affordance_subtle_hover))
            .child(stack)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.hover(true, *hovered, cx)))
            .on_click(cx.listener(|this, _, _, cx| {
                this._pending = None;
                this.open = !this.open;
                cx.notify();
            }));
        div()
            .relative()
            .child(trigger)
            .when(self.open, |d| {
                d.child(
                    div().absolute().top_0().right_0().child(
                        deferred(
                            anchored()
                                .anchor(Anchor::BottomRight)
                                .offset(point(px(0.), px(-8.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(
                                    div()
                                        .id("agent-activity-list")
                                        .occlude()
                                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                            this.hover(false, *hovered, cx)
                                        }))
                                        .on_mouse_down_out(
                                            cx.listener(|this, _, _, cx| this.close(cx)),
                                        )
                                        .child(self.list(cx)),
                                ),
                        )
                        .with_priority(2),
                    ),
                )
            })
            .into_any_element()
    }
}

/// A zero-height anchor for the top edge of the composer: the activity
/// sits 8px above it, inset 16px (12px in narrow windows), over the
/// bottom of the timeline. `None` when nobody is active.
pub fn overlay(
    humans: &[String],
    agents: Option<Entity<AgentChooser>>,
    profiles: &Profiles,
    narrow: bool,
    cx: &App,
) -> Option<AnyElement> {
    let agents = agents.filter(|chooser| !chooser.read(cx).rows.is_empty());
    if humans.is_empty() && agents.is_none() {
        return None;
    }
    let c = cx.colors();
    let names: Vec<SharedString> = humans
        .iter()
        .map(|pubkey| display_name(profiles, pubkey))
        .collect();
    let pill = (!humans.is_empty()).then(|| {
        div()
            .mr_auto()
            .max_w(relative(0.5))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(12.))
            .bg(c.surface_panel)
            .text_caption()
            .text_color(c.text_subtle)
            .child(dots(c.text_subtle, cx))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label(&names)),
            )
    });
    let inset = if narrow { px(12.) } else { px(16.) };
    Some(
        div()
            .absolute()
            .top_0()
            .left(inset)
            .right(inset)
            .h_0()
            .child(
                div()
                    .absolute()
                    .bottom(px(8.))
                    .left_0()
                    .right_0()
                    .flex()
                    .flex_row_reverse()
                    .justify_between()
                    .items_center()
                    .gap(px(6.))
                    .children(agents)
                    .children(pill),
            )
            .into_any_element(),
    )
}

/// Whether the composer shows an agent chooser, which moves the jump pill
/// into its own lane.
pub fn has_agents(chooser: &Entity<AgentChooser>, cx: &App) -> bool {
    !chooser.read(cx).rows.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use gpui_kit::SharedString;

    #[test]
    fn labels_follow_the_reference() {
        let names: Vec<SharedString> = ["Alice", "Bob", "Carol", "Dan"]
            .into_iter()
            .map(SharedString::from)
            .collect();
        assert_eq!(label(&names[..1]), "Alice is typing");
        assert_eq!(label(&names[..2]), "Alice, Bob are typing");
        assert_eq!(label(&names), "Alice, Bob and 2 others are typing");
    }

    #[test]
    fn splits_owned_agents_from_people() {
        let owned: HashSet<String> = ["agent".to_string()].into();
        let typing = vec!["alice".to_string(), "agent".to_string()];
        let threads = HashMap::from([
            (
                "root".to_string(),
                vec!["agent".to_string(), "bob".to_string()],
            ),
            ("elsewhere".to_string(), vec!["agent".to_string()]),
        ]);
        let activity = Activity::channel(&typing, &threads, |root| root == "root", &owned);
        assert_eq!(activity.humans, vec!["alice".to_string()]);
        assert_eq!(
            activity.agents["agent"],
            vec![None, Some("root".to_string())]
        );
        let thread = Activity::thread(&threads["root"], "root", &owned);
        assert_eq!(thread.humans, vec!["bob".to_string()]);
        assert_eq!(thread.agents["agent"], vec![Some("root".to_string())]);
    }

    #[test]
    fn scopes_name_the_channel_or_threads() {
        let excerpt = |root: &str| (root == "a").then(|| "Fix   the\nbuild".to_string());
        assert_eq!(scope_label(&[None], excerpt), "Channel conversation");
        assert_eq!(
            scope_label(&[Some("a".into()), Some("bcdefghijk".into())], excerpt),
            "Thread · Fix the build / Thread · bcdefghi"
        );
        assert_eq!(
            scope_label(&[None, Some("a".into()), Some("c".into())], excerpt),
            "Channel conversation / Thread · Fix the build / +1 more"
        );
    }
}

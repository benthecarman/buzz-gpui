//! The GitHub panel (`GitHubPanel` and `ObjectPanel` in `index.tsx`): the
//! object's identity, state, author and facts, its body, and for pull
//! requests the Discussion and Checks tabs.

use super::body::{self, Body};
use super::body_view;
use super::checks_view::ChecksView;
use super::client::GitHub;
use super::conversation_view::ConversationView;
use super::data::{self, Details, FactValue};
use super::references::{self, Kind, Reference};
use super::{inline_link, local_time, relative_time};
use crate::ui::channel_panels::Opening;
use buzz_relay_client::Media;
use buzz_ui::{
    ActiveTheme, Button, ButtonVariant, Icon, IconName, Tab, TabVariant, Tabs, Tooltip, TypeStyled,
    tokens::font,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PullTab {
    Discussion,
    Checks,
}

pub struct GitHubPanel {
    target: String,
    reference: Option<Reference>,
    media: Media,
    /// `None` while loading.
    result: Option<Result<Rc<Details>, String>>,
    /// Bumped by Retry; tells this load's views apart from the last's.
    attempt: usize,
    _load: Option<Task<()>>,
    body: Option<Rc<Body>>,
    tab: PullTab,
    conversation: Option<Entity<ConversationView>>,
    checks: Option<Entity<ChecksView>>,
    scroll: ScrollHandle,
}

impl GitHubPanel {
    pub fn new(opening: Opening, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let media = opening.community.read(cx).session.media().clone();
        let reference = references::parse(&opening.target);
        let mut panel = Self {
            target: opening.target,
            reference,
            media,
            result: None,
            attempt: 0,
            _load: None,
            body: None,
            tab: PullTab::Discussion,
            conversation: None,
            checks: None,
            scroll: ScrollHandle::new(),
        };
        panel.load(cx);
        panel
    }

    /// Loads the details afresh, as a new opening of the object.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.result = None;
        self.body = None;
        self.tab = PullTab::Discussion;
        self.conversation = None;
        self.checks = None;
        let Some(reference) = self.reference.clone() else {
            return;
        };
        let attempt = self.attempt;
        let load = GitHub::global(cx).run(move |fetch| Box::pin(data::load(fetch, reference)));
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                if this.attempt == attempt {
                    this.loaded(result, cx);
                }
            });
        }));
        cx.notify();
    }

    fn loaded(&mut self, result: Result<Details, String>, cx: &mut Context<Self>) {
        let Some(reference) = self.reference.clone() else {
            return;
        };
        let result = result.map(Rc::new);
        if let Ok(details) = &result {
            if reference.kind == Kind::Pull {
                let (details, media) = (details.clone(), self.media.clone());
                let url = reference.url.clone();
                let id = format!("{}-{}", reference.url, self.attempt);
                self.conversation =
                    Some(cx.new(|cx| ConversationView::new(details, url, id, media, cx)));
            } else if !details.body.is_empty() {
                self.body = Some(Rc::new(body::prepare(
                    &details.body,
                    details.body_html.as_deref(),
                    &reference.url,
                )));
            }
        }
        self.result = Some(result);
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.attempt += 1;
        self.load(cx);
    }

    fn select_tab(&mut self, tab: PullTab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == PullTab::Checks
            && self.checks.is_none()
            && let (Some(reference), Some(Ok(details))) = (&self.reference, &self.result)
        {
            let repository = reference.repository.clone();
            let sha = details.head_sha.clone();
            self.checks = Some(cx.new(|cx| ChecksView::new(repository, sha, cx)));
        }
        cx.notify();
    }
}

/// A child of the panel's block flow and its CSS margins.
struct Flow {
    children: Vec<(AnyElement, f32, f32)>,
}

impl Flow {
    fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    fn push(&mut self, element: impl IntoElement, top: f32, bottom: f32) {
        self.children
            .push((element.into_any_element(), top, bottom));
    }

    /// Stacks the children with adjacent margins collapsed. The panel's
    /// padding keeps the first and last margins inside it.
    fn render(self) -> Div {
        let mut column = div().flex().flex_col().min_w_0();
        let mut previous: Option<f32> = None;
        for (element, top, bottom) in self.children {
            let gap = previous.map_or(top, |previous| previous.max(top));
            column = column.child(div().min_w_0().pt(px(gap)).child(element));
            previous = Some(bottom);
        }
        column.when_some(previous, |column, bottom| column.pb(px(bottom)))
    }
}

fn kind_icon(kind: Kind) -> IconName {
    match kind {
        Kind::Repository => IconName::FolderSimple,
        Kind::Pull => IconName::GitPullRequest,
        Kind::Issue => IconName::GitHubIssue,
        Kind::Commit => IconName::GitCommit,
    }
}

/// `text-transform: capitalize`.
fn capitalize(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl GitHubPanel {
    fn identity(&self, reference: &Reference, cx: &App) -> AnyElement {
        let c = cx.colors();
        let (owner, name) = reference
            .repository
            .split_once('/')
            .unwrap_or((&reference.repository, ""));
        let text = if reference.kind == Kind::Pull {
            div()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .text_label()
                        .child(inline_link(
                            "owner",
                            owner.to_string(),
                            format!("https://github.com/{owner}"),
                            cx,
                        ))
                        .child(div().text_color(c.text_subtle).child("\u{a0}/\u{a0}"))
                        .child(inline_link(
                            "repository",
                            name.to_string(),
                            format!("https://github.com/{}", reference.repository),
                            cx,
                        )),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child(Kind::Pull.label()),
                )
        } else {
            div()
                .min_w_0()
                .child(
                    div()
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child(reference.repository.clone()),
                )
                .child(div().font_weight(FontWeight::BOLD).child(format!(
                    "{} {}",
                    reference.kind.label(),
                    reference.label
                )))
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(rems(2.5))
                    .rounded(px(12.))
                    .bg(c.affordance_selected)
                    .text_color(c.text_standard)
                    .child(Icon::new(kind_icon(reference.kind)).size(22.)),
            )
            .child(text)
            .into_any_element()
    }

    /// "Open on GitHub", for everything but pull requests, whose title
    /// links there.
    fn external(&self, reference: &Reference, cx: &App) -> AnyElement {
        let c = cx.colors();
        let url = reference.url.clone();
        div()
            .id("open-on-github")
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.))
            .py(px(8.))
            .rounded(px(8.))
            .bg(c.affordance_selected)
            .text_color(c.text_standard)
            .cursor_pointer()
            .hover(|s| s.underline())
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child("Open on GitHub")
            .child(Icon::new(IconName::ArrowSquareOut).size(14.))
            .into_any_element()
    }

    /// The `h2` title. A pull request's links to it, with its number.
    fn title(&self, reference: &Reference, cx: &App) -> AnyElement {
        let c = cx.colors();
        let ready = match &self.result {
            Some(Ok(details)) => Some(details.title.clone()),
            _ => None,
        };
        let base = div().text_body_lg().font_weight(FontWeight::NORMAL);
        if reference.kind != Kind::Pull {
            return base.child(ready.unwrap_or_default()).into_any_element();
        }
        let title = ready.unwrap_or_else(|| Kind::Pull.label().to_string());
        let text = format!("{title} {}", reference.label);
        let number = title.len() + 1..text.len();
        let url = reference.url.clone();
        base.child(
            div()
                .id("title")
                .cursor_pointer()
                .text_color(c.text_standard)
                .hover(|s| s.underline())
                .on_click(move |_, _, cx| cx.open_url(&url))
                .child(StyledText::new(text).with_highlights([(
                    number,
                    HighlightStyle {
                        color: Some(c.text_subtle),
                        ..Default::default()
                    },
                )])),
        )
        .into_any_element()
    }

    fn byline(&self, reference: &Reference, details: &Details, cx: &App) -> AnyElement {
        let c = cx.colors();
        let pull = reference.kind == Kind::Pull;
        let state = details.state.to_lowercase();
        let (icon, text, fill) = match state.as_str() {
            "open" if pull => (
                Some(IconName::GitPullRequest),
                c.text_success,
                c.affordance_success,
            ),
            "draft" if pull => (
                Some(IconName::CircleDashed),
                c.text_subtle,
                c.affordance_selected,
            ),
            "closed" if pull => (
                Some(IconName::XCircle),
                c.text_subtle,
                c.affordance_selected,
            ),
            "merged" if pull => (
                Some(IconName::GitMerge),
                c.text_categorical_purple,
                c.surface_categorical_purple,
            ),
            _ => (None, c.text_subtle, c.affordance_selected),
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .text_caption()
            .text_color(c.text_subtle)
            .when(!details.state.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .rounded(px(4.))
                        .px(px(6.))
                        .py(px(2.))
                        .bg(fill)
                        .text_color(text)
                        .when_some(icon, |this, icon| this.child(Icon::new(icon).size(12.)))
                        .child(capitalize(&details.state)),
                )
            })
            .when(!details.author.is_empty(), |this| {
                this.child(
                    div().flex().items_center().child("by\u{a0}").child(
                        match &details.author_url {
                            Some(url) => {
                                inline_link("author", details.author.clone(), url.clone(), cx)
                                    .into_any_element()
                            }
                            None => div().child(details.author.clone()).into_any_element(),
                        },
                    ),
                )
            })
            .into_any_element()
    }

    fn facts(&self, reference: &Reference, details: &Details, cx: &App) -> AnyElement {
        let c = cx.colors();
        let row = |label: &'static str, value: AnyElement| {
            div()
                .flex()
                .justify_between()
                .gap(px(12.))
                .py(px(4.))
                .child(div().flex_none().text_color(c.text_subtle).child(label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .child(value),
                )
        };
        let branch = |id: &'static str, branch: &data::Branch| -> AnyElement {
            let label = div()
                .font_family(font::MONO)
                .text_size(rems(0.75))
                .line_height(rems(1.));
            match &branch.url {
                Some(url) => {
                    let url = url.clone();
                    label
                        .id(id)
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .bg(c.affordance_link_hover)
                        .text_color(c.text_link)
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .child(branch.label.clone())
                        .into_any_element()
                }
                None => label.child(branch.label.clone()).into_any_element(),
            }
        };
        let mut rows: Vec<AnyElement> = details
            .facts
            .iter()
            .map(|(label, value)| {
                let value = match value {
                    FactValue::Text(text) => div().child(text.clone()).into_any_element(),
                    FactValue::Branches { head, base } => div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .child(branch("head", head))
                        .child("\u{a0}→\u{a0}")
                        .child(branch("base", base))
                        .into_any_element(),
                    FactValue::Changes {
                        additions,
                        deletions,
                    } => div()
                        .flex()
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .text_color(c.text_success)
                                .child(format!("+{additions}")),
                        )
                        .child("\u{a0}/\u{a0}")
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .text_color(c.text_danger)
                                .child(format!("−{deletions}")),
                        )
                        .into_any_element(),
                };
                row(label, value).into_any_element()
            })
            .collect();
        if reference.kind == Kind::Pull {
            let updated = match details.updated_at.as_deref() {
                Some(at) => {
                    let tip = local_time(at).unwrap_or_default();
                    div()
                        .id("updated")
                        .child(relative_time(at).unwrap_or_default())
                        .tooltip(Tooltip::text(tip))
                        .into_any_element()
                }
                None => div().child("Unavailable").into_any_element(),
            };
            rows.push(row("Last updated", updated).into_any_element());
        }
        div()
            .text_caption()
            .p(px(12.))
            .border_1()
            .border_color(c.border_standard)
            .rounded(px(12.))
            .bg(c.surface_panel)
            .children(rows)
            .into_any_element()
    }

    fn pull_content(&mut self, cx: &mut Context<Self>) -> (AnyElement, AnyElement) {
        let tabs = div()
            .py(px(20.))
            .child(
                Tabs::new(TabVariant::Panel)
                    .tab(
                        Tab::new("discussion", "Discussion")
                            .selected(self.tab == PullTab::Discussion)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.select_tab(PullTab::Discussion, cx)
                            })),
                    )
                    .tab(
                        Tab::new("checks", "Checks")
                            .selected(self.tab == PullTab::Checks)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.select_tab(PullTab::Checks, cx)),
                            ),
                    ),
            )
            .into_any_element();
        let content = match self.tab {
            PullTab::Discussion => self
                .conversation
                .clone()
                .map(|view| div().mt(px(20.)).child(view).into_any_element()),
            PullTab::Checks => self.checks.clone().map(|view| view.into_any_element()),
        };
        (tabs, content.unwrap_or_else(|| div().into_any_element()))
    }

    fn error(&self, message: &str, reference: &Reference, cx: &mut Context<Self>) -> AnyElement {
        let url = reference.url.clone();
        div()
            .child(div().child(message.to_string()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Button::new("open-github", "Open on Github")
                            .variant(ButtonVariant::Prominent)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Button::new("retry", "Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for GitHubPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let root = div()
            .id(SharedString::from(format!("github-{}", self.target)))
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(20.))
            .text_body_sm()
            .text_color(c.text_standard);
        let Some(reference) = self.reference.clone() else {
            return root
                .child(
                    div()
                        .text_color(c.text_subtle)
                        .child("Unsupported GitHub link."),
                )
                .into_any_element();
        };
        let pull = reference.kind == Kind::Pull;
        let mut flow = Flow::new();
        flow.push(self.identity(&reference, cx), 0., 0.);
        if !pull {
            flow.push(self.external(&reference, cx), 16., 16.);
        }
        let ready = matches!(self.result, Some(Ok(_)));
        if pull && !ready {
            flow.push(self.title(&reference, cx), 16., 8.);
        }
        match self.result.clone() {
            None => flow.push(div().child("Loading from GitHub…"), 0., 0.),
            Some(Err(message)) => flow.push(self.error(&message, &reference, cx), 0., 0.),
            Some(Ok(details)) => {
                if pull {
                    flow.push(self.title(&reference, cx), 16., 8.);
                    flow.push(self.byline(&reference, &details, cx), 0., 24.);
                } else {
                    flow.push(self.byline(&reference, &details, cx), 0., 0.);
                    flow.push(self.title(&reference, cx), 16., 16.);
                }
                if !details.facts.is_empty() || pull {
                    flow.push(self.facts(&reference, &details, cx), 0., 0.);
                }
                if pull {
                    let (tabs, content) = self.pull_content(cx);
                    flow.push(tabs, 0., 0.);
                    flow.push(content, 0., 0.);
                } else if let Some(body) = self.body.clone() {
                    let drawn = body_view::draw(
                        &body,
                        &body_view::Context {
                            id: format!("{}-{}", reference.url, self.attempt).into(),
                            media: self.media.clone(),
                        },
                        cx,
                    );
                    flow.push(drawn.element, drawn.top, drawn.bottom);
                }
            }
        }
        flow.push(
            div()
                .pt(px(12.))
                .border_t_1()
                .border_color(c.border_standard)
                .text_caption()
                .text_color(c.text_subtle)
                .child("Public GitHub data · Open on GitHub for private repositories and actions."),
            24.,
            0.,
        );
        root.child(flow.render()).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn capitalizes_states_as_css_does() {
        assert_eq!(capitalize("open"), "Open");
        assert_eq!(capitalize("closed"), "Closed");
        assert_eq!(capitalize("Merged"), "Merged");
        assert_eq!(capitalize(""), "");
    }
}

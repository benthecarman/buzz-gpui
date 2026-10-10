//! The Checks tab (`GitHubChecks.tsx`): a summary ring with counts, then
//! the checks grouped by outcome, actionable ones first.

use super::checks::{self, Category, Check, State, Summary};
use super::client::GitHub;
use super::copy_link;
use crate::ui::overlay::Dismiss;
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, Colors, Icon, IconButton, IconButtonSize,
    IconName, Menu, MenuItem, Tooltip, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;

pub struct ChecksView {
    repository: String,
    sha: Option<String>,
    result: Summary,
    loading: bool,
    attempt: usize,
    _task: Option<Task<()>>,
    /// Groups the reader closed; skipped checks start closed.
    collapsed: HashSet<Category>,
    /// The check whose actions menu is open.
    menu: Option<String>,
}

fn group_label(category: Category) -> &'static str {
    category.name()
}

/// Group order, as the accordion lists them.
const GROUPS: [Category; 7] = Category::COUNTS;

impl ChecksView {
    pub fn new(repository: String, sha: Option<String>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            result: if sha.is_some() {
                checks::loading()
            } else {
                checks::unavailable()
            },
            repository,
            sha,
            loading: false,
            attempt: 0,
            _task: None,
            collapsed: HashSet::from([Category::Skipped]),
            menu: None,
        };
        view.load(cx);
        view
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(sha) = self.sha.clone() else {
            return;
        };
        self.result = checks::loading();
        self.loading = true;
        let repository = self.repository.clone();
        let load = GitHub::global(cx).run(move |fetch| {
            Box::pin(async move { Ok(checks::load(fetch, repository, sha).await) })
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            let summary = load.await.unwrap_or_else(|_| checks::unavailable());
            let _ = this.update(cx, |this, cx| {
                this.result = summary;
                this.loading = false;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn needs_retry(&self) -> bool {
        self.result.state == State::Unavailable
            || self
                .result
                .checks
                .as_ref()
                .is_some_and(|checks| checks.iter().any(|c| c.category == Category::Unknown))
    }

    fn summary(&self, cx: &App) -> AnyElement {
        let c = cx.colors();
        let result = &self.result;
        let total = result.checks.as_ref().map_or(0, Vec::len);
        let graphic = if total > 0 && result.state != State::Unavailable {
            ring(result.checks.as_deref().unwrap_or_default(), c).into_any_element()
        } else {
            let (icon, color) = match result.state {
                State::Failure => (IconName::XCircle, c.text_danger),
                State::Pending => (IconName::WarningCircle, c.text_warning),
                State::Success => (IconName::CheckCircle, c.text_success),
                State::Neutral => (IconName::CircleDashed, c.text_subtle),
                State::Unavailable => (IconName::WarningCircle, c.text_subtle),
            };
            Icon::new(icon).size(32.).color(color).into_any_element()
        };
        let label = if result.state == State::Failure {
            "Some checks were not successful".to_string()
        } else {
            result.label.clone()
        };
        div()
            .id("checks-summary")
            .flex()
            .items_center()
            .gap(px(12.))
            .p(px(16.))
            .tooltip(Tooltip::text(result.description.clone()))
            .child(div().flex_none().size(px(32.)).child(graphic))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .min_w_0()
                    .child(div().text_label().child(label))
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .child(result.description.clone()),
                    ),
            )
            .into_any_element()
    }

    fn row(
        &self,
        check: &Check,
        first: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let color = category_color(check.category, &c);
        let icon = match check.category {
            Category::Failing | Category::Cancelled => IconName::XCircle,
            Category::Pending | Category::Unknown => IconName::WarningCircle,
            Category::Successful => IconName::Check,
            Category::Skipped | Category::Neutral => IconName::CircleDashed,
        };
        let group: SharedString = format!("check-row-{}", check.key).into();
        let name = match &check.url {
            Some(url) => {
                let url = url.clone();
                div()
                    .id(SharedString::from(format!("{}-name", check.key)))
                    .min_w_0()
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child(check.name.clone())
                    .into_any_element()
            }
            None => div().min_w_0().child(check.name.clone()).into_any_element(),
        };
        let menu_open = self.menu.as_deref() == Some(check.key.as_str());
        let actions = check.url.clone().map(|url| {
            let key = check.key.clone();
            let toggle = cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.menu = if this.menu.as_deref() == Some(key.as_str()) {
                    None
                } else {
                    Some(key.clone())
                };
                cx.notify();
            });
            let weak = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            });
            let close = dismiss.clone();
            div()
                .relative()
                .flex_none()
                .when(!menu_open, |d| {
                    d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                })
                .child(
                    IconButton::new(
                        SharedString::from(format!("{}-actions", check.key)),
                        IconName::DotsThree,
                    )
                    .size(IconButtonSize::Xs)
                    .tooltip(format!("Actions for {}", check.name))
                    .on_click(toggle),
                )
                .when(menu_open, |d| {
                    d.child(crate::ui::overlay::dropdown(
                        window,
                        crate::ui::overlay::Align::End,
                        Menu::new().child(
                            MenuItem::new("copy-link", "Copy link")
                                .icon(IconName::Link)
                                .on_click(move |_, window, cx| {
                                    copy_link(&url, "Link copied", cx);
                                    close(window, cx);
                                }),
                        ),
                        dismiss,
                    ))
                })
        });
        div()
            .group(group)
            .flex()
            .items_start()
            .gap(px(8.))
            .py(px(12.))
            .when(!first, |d| d.border_t_1().border_color(c.border_standard))
            .child(
                div()
                    .flex_none()
                    .mt(px(2.))
                    .child(Icon::new(icon).size(18.).color(color)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_baseline()
                            .gap_x(px(12.))
                            .gap_y(px(4.))
                            .child(name)
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .child(check.label.clone()),
                            ),
                    )
                    .when_some(check.description.clone(), |d, description| {
                        d.child(
                            div()
                                .mt(px(4.))
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child(description),
                        )
                    }),
            )
            .children(actions)
            .into_any_element()
    }
}

fn category_color(category: Category, c: &Colors) -> Hsla {
    match category {
        Category::Failing | Category::Cancelled => c.text_danger,
        Category::Pending => c.text_warning,
        Category::Successful => c.text_success,
        _ => c.text_subtle,
    }
}

/// The summary ring: one arc per outcome, in group order, from the top.
fn ring(checks: &[Check], c: &Colors) -> impl IntoElement {
    let total = checks.len() as f32;
    let segments: Vec<(f32, Hsla)> = GROUPS
        .iter()
        .filter_map(|category| {
            let count = checks
                .iter()
                .filter(|check| check.category == *category)
                .count();
            (count > 0).then(|| {
                let color = match category {
                    Category::Failing | Category::Cancelled => c.text_danger,
                    Category::Pending => c.text_warning,
                    Category::Successful => c.text_success,
                    _ => c.border_standard,
                };
                (count as f32 / total, color)
            })
        })
        .collect();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let scale = f32::from(bounds.size.width) / 32.;
            let center = bounds.center();
            let radius = 13. * scale;
            let mut start = 0f32;
            for (fraction, color) in &segments {
                let sweep = fraction * std::f32::consts::TAU;
                let steps = ((sweep / 0.05).ceil() as usize).max(2);
                let mut path = PathBuilder::stroke(px(3. * scale));
                for step in 0..=steps {
                    let angle =
                        start + sweep * step as f32 / steps as f32 - std::f32::consts::FRAC_PI_2;
                    let at = point(
                        center.x + px(radius * angle.cos()),
                        center.y + px(radius * angle.sin()),
                    );
                    if step == 0 {
                        path.move_to(at);
                    } else {
                        path.line_to(at);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, *color);
                }
                start += sweep;
            }
        },
    )
    .size(px(32.))
}

impl Render for ChecksView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let checks = self.result.checks.clone().unwrap_or_default();
        let retry =
            self.sha.is_some() && (self.needs_retry() || (self.attempt > 0 && self.loading));
        let groups: Vec<(Category, Vec<&Check>)> = GROUPS
            .iter()
            .map(|category| {
                (
                    *category,
                    checks
                        .iter()
                        .filter(|c| c.category == *category)
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, checks)| !checks.is_empty())
            .collect();
        let mut sections = Vec::new();
        for (category, group) in &groups {
            let open = !self.collapsed.contains(category);
            let count = group.len();
            let title = format!(
                "{count} {} {}",
                group_label(*category),
                if count == 1 { "check" } else { "checks" }
            );
            let category = *category;
            sections.push(
                div()
                    .id(SharedString::from(format!("group-{}", category.name())))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .py(px(8.))
                    .text_body()
                    .text_color(c.text_subtle)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&category) {
                            this.collapsed.insert(category);
                        }
                        cx.notify();
                    }))
                    .child(title)
                    .child(
                        Icon::new(if open {
                            IconName::CaretUp
                        } else {
                            IconName::CaretDown
                        })
                        .size(14.),
                    )
                    .into_any_element(),
            );
            if open {
                let rows: Vec<AnyElement> = group
                    .iter()
                    .enumerate()
                    .map(|(index, check)| self.row(check, index == 0, window, cx))
                    .collect();
                sections.push(div().flex().flex_col().children(rows).into_any_element());
            }
        }
        div()
            .border_1()
            .border_color(c.border_standard)
            .rounded(px(12.))
            .child(self.summary(cx))
            .when(retry, |d| {
                d.child(
                    div().flex().px(px(16.)).pb(px(12.)).child(
                        Button::new("retry-checks", "Retry")
                            .variant(ButtonVariant::Link)
                            .size(ButtonSize::Xs)
                            .loading(self.loading)
                            .tooltip("Retry checks")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.loading {
                                    this.attempt += 1;
                                    this.load(cx);
                                }
                            })),
                    ),
                )
            })
            .when(!groups.is_empty(), |d| {
                d.child(
                    div()
                        .border_t_1()
                        .border_color(c.border_standard)
                        .px(px(16.))
                        .py(px(8.))
                        .children(sections),
                )
            })
    }
}

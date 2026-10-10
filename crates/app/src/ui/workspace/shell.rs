//! The shell around the pages (`app/shell/AppShell.tsx`): the header with
//! its page pills, the navigation column with its resize handle, collapse
//! and narrow-window drawer, and the joined panel.

use super::Workspace;
use super::layout::{self, SIDEBAR_DEFAULT, SIDEBAR_MIN};
use crate::ui::pages::{self, Page, Placement};
use crate::ui::resize_handle::ResizeHandle;
use crate::ui::sidebar::{self, SidebarProps, SidebarView};
use crate::ui::{overlay, window_controls};
use buzz_ui::tokens::{duration, easing, shadow};
use buzz_ui::{
    ActiveTheme, IconButton, IconButtonSize, IconName, MaterialStyled, NavigationItem, Popover,
    PopoverPadding, PopoverWidth, Surface, Tab, TabVariant, Tabs,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// The header's groups as last laid out, for folding the page pills.
#[derive(Clone, Copy, Default)]
pub(super) struct HeaderSizes {
    left: f32,
    center: f32,
    right: f32,
}

/// The sidebar collapsing or expanding (`grid-template-columns` over
/// `--duration-settle`, ease-in-out).
#[derive(Clone, Copy)]
pub(super) struct Motion {
    started: Instant,
    opening: bool,
}

/// CSS `ease-in-out`.
const EASE_IN_OUT: (f32, f32, f32, f32) = (0.42, 0., 0.58, 1.);

impl Workspace {
    fn viewport_width(window: &Window) -> f32 {
        f32::from(window.viewport_size().width)
    }

    /// The joined panel's width: the window less the rail and the gutter.
    pub(super) fn content_width(window: &Window) -> f32 {
        let width = Self::viewport_width(window);
        width - layout::RAIL - layout::gutter(width)
    }

    /// The sidebar's width now: the saved one, clamped so main keeps its
    /// room, or 220 in the narrow drawer.
    pub(super) fn sidebar_width(&self, window: &Window) -> f32 {
        if layout::narrow(Self::viewport_width(window)) {
            return SIDEBAR_MIN;
        }
        layout::clamp_sidebar(
            self.sidebar_view.width.unwrap_or(SIDEBAR_DEFAULT),
            Self::content_width(window),
        )
    }

    /// Whether the header toggle collapses the sidebar on this page.
    fn collapsible(&self, cx: &App) -> bool {
        self.page == Page::SETTINGS
            || pages::spec(self.page, cx).is_some_and(|spec| spec.collapsible)
    }

    fn toggle_label(&self, narrow: bool) -> &'static str {
        let me = self.page == Page::ME;
        match (narrow, self.drawer_open, self.sidebar_open, me) {
            (true, true, _, _) => "Hide navigation",
            (true, false, _, _) => "Show navigation",
            (false, _, true, true) => "Hide Me sidebar",
            (false, _, true, false) => "Hide Channel sidebar",
            (false, _, false, true) => "Show Me sidebar",
            (false, _, false, false) => "Show Channel sidebar",
        }
    }

    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if layout::narrow(Self::viewport_width(window)) {
            self.drawer_open = !self.drawer_open;
        } else {
            self.sidebar_open = !self.sidebar_open;
            self.sidebar_motion = Some(Motion {
                started: Instant::now(),
                opening: self.sidebar_open,
            });
        }
        cx.notify();
    }

    /// Closes the narrow drawer; navigation does this on every visit.
    pub(super) fn close_drawer(&mut self, focus_toggle: bool, window: &mut Window, cx: &mut App) {
        if self.drawer_open {
            self.drawer_open = false;
            if focus_toggle {
                window.focus(&self.toggle_focus, cx);
            }
        }
    }

    pub(super) fn select_header_page(
        &mut self,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overflow_open = false;
        match page {
            Page::MESSAGES => self.go_home(window, cx),
            page => self.select_page(page, window, cx),
        }
    }

    pub(super) fn header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let width = Self::viewport_width(window);
        let narrow = layout::narrow(width);
        let (pad_left, pad_right) = layout::header_padding(width);
        let header_button = |id: &'static str, icon: IconName| {
            IconButton::new(id, icon)
                .size(IconButtonSize::Sm)
                .surface(Surface::Header)
        };
        let settings_open = self.page == Page::SETTINGS;
        self.account
            .update(cx, |account, _| account.settings_open = settings_open);
        let header_pages = pages::placed(Placement::Topbar, cx);
        let compact = self.compact_pages || narrow;
        let selected = self.page;

        // Page pills, measured even while folded so they can come back.
        let pills = header_pages
            .iter()
            .fold(Tabs::new(TabVariant::Chrome), |tabs, spec| {
                let page = spec.page;
                tabs.tab(
                    Tab::new(
                        SharedString::from(format!("pill-{}", page.key())),
                        spec.label,
                    )
                    .selected(selected == page)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_header_page(page, window, cx)
                    })),
                )
            });
        let sizes = self.header_sizes.clone();
        let center_sizes = sizes.clone();
        let right_sizes = sizes.clone();
        let center = div()
            .absolute()
            .top(rems(0.25))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .on_children_prepainted(move |bounds, _, _| {
                let mut current = center_sizes.get();
                current.center = bounds.first().map_or(0., |b| f32::from(b.size.width));
                center_sizes.set(current);
            })
            .child(
                window_controls::not_title_bar(div())
                    .when(compact, |this| this.invisible())
                    .child(pills),
            );

        let toggle = (self.collapsible(cx) || narrow).then(|| {
            let label = self.toggle_label(narrow);
            div().track_focus(&self.toggle_focus).child(
                header_button("toggle-sidebar", IconName::Sidebar)
                    .tooltip(label)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx))),
            )
        });
        let left = window_controls::not_title_bar(div().flex().ml(rems(1.)))
            .children(toggle)
            .child(
                header_button("back", IconName::ArrowLeft)
                    .tooltip("Go back")
                    .disabled(!self.history.can_go_back())
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
            .child(
                header_button("forward", IconName::ArrowRight)
                    .tooltip("Go forward")
                    .disabled(!self.history.can_go_forward())
                    .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
            );

        let overflow = (compact && !header_pages.is_empty()).then(|| {
            let open = self.overflow_open;
            let menu = open.then(|| {
                let rows = header_pages.iter().map(|spec| {
                    let page = spec.page;
                    NavigationItem::new(
                        SharedString::from(format!("more-{}", page.key())),
                        spec.label,
                    )
                    .icon(spec.icon)
                    .icon_stroke(2.5)
                    .surface(Surface::FloatingCompact)
                    .selected(selected == page)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_header_page(page, window, cx)
                    }))
                });
                let weak = cx.entity().downgrade();
                overlay::dropdown(
                    window,
                    overlay::Align::End,
                    Popover::new()
                        .width(PopoverWidth::Compact)
                        .padding(PopoverPadding::List)
                        .child(div().flex().flex_col().gap(rems(0.25)).children(rows)),
                    Rc::new(move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.overflow_open = false;
                            cx.notify();
                        });
                    }),
                )
            });
            div()
                .relative()
                .child(
                    header_button("more-pages", IconName::DotsThree)
                        .tooltip("More pages")
                        .selected(open)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.overflow_open = !open;
                            cx.notify();
                        })),
                )
                .children(menu)
        });
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(rems(0.5))
            .child(
                header_button("search", IconName::MagnifyingGlass)
                    .tooltip("Search Buzz (Ctrl+K)")
                    .on_click(cx.listener(|this, _, window, cx| this.open_search(window, cx))),
            )
            .child(self.account.clone())
            .child(window_controls::render(
                window_controls::Policy::current(),
                window,
            ));
        let right = window_controls::not_title_bar(div().flex().items_center().gap(rems(0.5)))
            .on_children_prepainted(move |bounds, _, _| {
                let mut current = right_sizes.get();
                current.right = bounds.last().map_or(0., |b| f32::from(b.size.width));
                right_sizes.set(current);
            })
            .children(overflow)
            .child(actions);

        let weak = cx.entity().downgrade();
        let bar = div()
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(layout::HEADER_GAP))
            .h(rems(3.))
            .pl(px(pad_left))
            .pr(px(pad_right))
            .on_children_prepainted(move |bounds, window, cx| {
                let mut current = sizes.get();
                // The left group's own 16px padding is its margin here.
                current.left =
                    bounds.first().map_or(0., |b| f32::from(b.size.width)) + 16. + pad_left;
                sizes.set(current);
                let width = Self::viewport_width(window);
                let compact = layout::compact_pages(
                    width,
                    current.left,
                    current.right + pad_right + layout::HEADER_GAP,
                    current.center,
                );
                let weak = weak.clone();
                cx.defer(move |cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if this.compact_pages != compact {
                            this.compact_pages = compact;
                            if !compact {
                                this.overflow_open = false;
                            }
                            cx.notify();
                        }
                    });
                });
            });
        window_controls::title_bar(bar.id("title-bar"))
            .child(left)
            .child(center)
            .child(right)
            .into_any_element()
    }

    /// What goes in the navigation column on this page, if anything.
    fn navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.onboarding.is_some() && self.page != Page::SETTINGS {
            return None;
        }
        if self.page == Page::SETTINGS {
            let settings = self.settings_page(window, cx);
            return Some(
                settings.update(cx, |settings, cx| settings.sidebar(cx).into_any_element()),
            );
        }
        if let Some(own) = self.own_sidebars.get(&self.page) {
            return Some(own.clone().into_any_element());
        }
        let community = self.community.clone()?;
        let snapshot = community.read(cx).snapshot.clone();
        let weak = cx.entity().downgrade();
        let weak_page = weak.clone();
        let (weak_create, weak_message) = (weak.clone(), weak.clone());
        let can_create = snapshot
            .relay
            .as_ref()
            .is_some_and(|relay| relay.supports_channel_creation);
        Some(sidebar::render(
            &snapshot,
            SidebarProps {
                page: self.page,
                selected: self
                    .channel
                    .as_ref()
                    .map(|c| c.read(cx).channel_id().to_string()),
                on_select_channel: Arc::new(move |channel, window, cx| {
                    let _ = weak.update(cx, |this, cx| this.select_channel(channel, window, cx));
                }),
                on_select_page: Arc::new(move |page, window, cx| {
                    let _ = weak_page.update(cx, |this, cx| this.select_page(page, window, cx));
                }),
                on_create_channel: can_create.then(|| {
                    Arc::new(move |window: &mut Window, cx: &mut App| {
                        let _ =
                            weak_create.update(cx, |this, cx| this.open_new_channel(window, cx));
                    }) as sidebar::ActionHandler
                }),
                on_new_message: Arc::new(move |window, cx| {
                    let _ = weak_message.update(cx, |this, cx| this.open_new_message(window, cx));
                }),
            },
            cx,
        ))
    }

    /// Saves the sidebar width the user chose for this community.
    fn save_sidebar_view(&self, cx: &App) {
        if let Some(community) = &self.community {
            self.sidebar_view.save(community.read(cx), cx);
        }
    }

    /// The joined panel: the navigation column, then the page.
    pub(super) fn body(
        &mut self,
        main: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let viewport = Self::viewport_width(window);
        let narrow = layout::narrow(viewport);
        if !narrow {
            self.drawer_open = false;
        }
        let collapsible = self.collapsible(cx);
        let width = self.sidebar_width(window);
        let navigation = self.navigation(window, cx);
        // Only the channel sidebar resizes; Settings and Me read its width.
        let resizable = !narrow
            && navigation.is_some()
            && self.page != Page::SETTINGS
            && !self.own_sidebars.contains_key(&self.page);

        // How much of the column shows while it collapses or expands.
        let mut shown = match (collapsible, self.sidebar_open) {
            (false, _) | (true, true) => 1.,
            (true, false) => 0.,
        };
        if let Some(motion) = self.sidebar_motion {
            let progress = motion.started.elapsed().as_secs_f32() / duration::SETTLE.as_secs_f32();
            if progress >= 1. || cx.reduce_motion() || !collapsible {
                self.sidebar_motion = None;
            } else {
                let eased = easing::cubic_bezier(EASE_IN_OUT, progress);
                shown = if motion.opening { eased } else { 1. - eased };
                window.request_animation_frame();
            }
        }

        let column = navigation.map(|navigation| {
            div()
                .id("shell-navigation")
                .flex_none()
                .h_full()
                .w(px(width))
                .border_r_1()
                .border_color(c.border_standard)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" && this.drawer_open {
                        cx.stop_propagation();
                        this.close_drawer(true, window, cx);
                        cx.notify();
                    }
                }))
                .child(navigation)
        });
        let (in_flow, drawer) = match (narrow, column) {
            (true, Some(column)) => (
                None,
                self.drawer_open.then(|| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .max_w(px(Self::content_width(window) - 16.))
                        .bg(c.surface_panel)
                        .shadow(shadow::sm())
                        .child(column)
                }),
            ),
            (false, Some(column)) => (
                Some(
                    div()
                        .relative()
                        .flex_none()
                        .h_full()
                        .w(px(width * shown))
                        .overflow_hidden()
                        .when(shown == 0., |this| this.invisible())
                        .child(column),
                ),
                None,
            ),
            (_, None) => (None, None),
        };
        let handle = (resizable && shown == 1.).then(|| {
            let max = layout::clamp_sidebar(f32::MAX, Self::content_width(window));
            let weak = cx.entity().downgrade();
            let commit = weak.clone();
            div().relative().w_0().h_full().child(
                ResizeHandle::new("sidebar-resize", width, SIDEBAR_MIN, max)
                    .on_resize(move |next, _, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.sidebar_view.width = next;
                            cx.notify();
                        });
                    })
                    .on_commit(move |_, cx| {
                        let _ = commit.update(cx, |this, cx| this.save_sidebar_view(cx));
                    }),
            )
        });

        div()
            .relative()
            .flex()
            .size_full()
            .overflow_hidden()
            .panel_surface(&c)
            .children(in_flow)
            .children(handle)
            .child(
                div()
                    .id("main-content")
                    .track_focus(&self.main_focus)
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(main),
            )
            .children(drawer)
            .into_any_element()
    }

    /// The saved sidebar view follows the community.
    pub(super) fn load_sidebar_view(&mut self, cx: &App) {
        self.sidebar_view = match &self.community {
            Some(community) => SidebarView::load(community.read(cx), cx),
            None => SidebarView::default(),
        };
    }
}

pub(super) fn header_sizes() -> Rc<Cell<HeaderSizes>> {
    Rc::new(Cell::new(HeaderSizes::default()))
}

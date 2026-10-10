//! Development only: `BUZZ_DEV_CLAUDE_CODE=claude` (or `settings`) opens a
//! window with the type's tabs for the selected community's first Claude
//! Code agent, made if there is none, framed as the Agents2 Build view
//! frames them. For seeing the tabs before that view exists; debug builds
//! only.

use super::{PLUGIN, TYPE_ID};
use crate::agents2::{AgentView, Agents2, Status, type_of};
use buzz_ui::{ActiveTheme, Tab, TabVariant, Tabs, TypeStyled, tokens::space};
use gpui_kit::*;
use std::collections::HashMap;

pub fn init(cx: &mut App) {
    let Some(tab) = std::env::var("BUZZ_DEV_CLAUDE_CODE").ok() else {
        return;
    };
    let key = format!("{PLUGIN}/{TYPE_ID}");
    cx.spawn(async move |cx| {
        let mut asked = false;
        for _ in 0..240 {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            let step = cx.update(|cx| {
                let agents = Agents2::global(cx)?;
                let read = agents.read(cx);
                let session = crate::community::AppState::current(cx)
                    .map(|community| format!("{:?}", community.read(cx).snapshot.status));
                let identity = matches!(
                    cx.global::<crate::community::AppState>().identity,
                    crate::community::IdentityState::Ready(_)
                );
                tracing::debug!(status = ?read.status(), bound = read.bound(), hosts = read.hosts(cx).len(), ?session, identity, "claude code preview waiting");
                if session.as_deref() == Some("Ready") && identity && !read.bound() {
                    // Agents2 binds on app state changes; nudge it once the
                    // session is up.
                    cx.update_global::<crate::community::AppState, _>(|_, _| {});
                    return None;
                }
                if *read.status() != Status::Ready || !read.bound() || read.hosts(cx).is_empty() {
                    return None;
                }
                let found = read
                    .agents()
                    .iter()
                    .find(|agent| agent.agent_type == key)
                    .map(|agent| agent.pubkey.clone());
                if let Some(pubkey) = found {
                    if std::env::var_os("BUZZ_DEV_CLAUDE_CODE_TURNS").is_some_and(|v| !v.is_empty())
                    {
                        turns(&pubkey, cx);
                    }
                    let tab = tab.clone();
                    let opened = gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                                None,
                                size(px(720.), px(860.)),
                                cx,
                            ))),
                            ..Default::default()
                        },
                        cx,
                        move |window, cx| cx.new(|cx| Preview::new(pubkey, &tab, window, cx)),
                    );
                    if let Err(error) = opened {
                        tracing::warn!(%error, "could not open the Claude Code preview");
                    }
                    return Some(());
                }
                if !asked {
                    asked = true;
                    let create = agents.update(cx, |agents, cx| agents.create(&key, "Claude", None, cx));
                    cx.spawn(async move |_| {
                        if let Err(error) = create.await {
                            tracing::warn!(%error, "could not make a Claude Code agent");
                        }
                    })
                    .detach();
                }
                None
            });
            if step.is_some() {
                return;
            }
        }
        tracing::warn!("no Claude Code agent to preview");
    })
    .detach();
}

/// `BUZZ_DEV_CLAUDE_CODE_TURNS`: hands the agent two mentions from its
/// owner in the selected channel, for the Running sessions list, without
/// waiting on live events.
fn turns(pubkey: &str, cx: &mut App) {
    use super::runtime::{Run, Wake};
    use crate::agents2::AgentHandle;
    use crate::agents2::runtime::HostLink;
    use crate::buzz_mcp::BuzzEvent;
    let Some(agents) = Agents2::global(cx) else {
        return;
    };
    let (Some(agent), Some(host)) = (
        agents.read(cx).find(pubkey).cloned(),
        agents.read(cx).host_of(pubkey),
    ) else {
        return;
    };
    let Some(state) = super::ClaudeCode::global(cx) else {
        return;
    };
    let channel = crate::community::AppState::current(cx)
        .and_then(|community| community.read(cx).remembered_channel(cx))
        .unwrap_or_default();
    let handle = AgentHandle {
        pubkey: agent.pubkey.clone(),
        name: agent.name.clone(),
        owner: agent.owner.clone(),
        host: HostLink {
            client: host.read(cx).client(),
            runtime: crate::runtime::Runtime::handle(cx),
        },
        wrote: Default::default(),
    };
    let spawn = host.read(cx).spawner(super::PROCESS_OWNER);
    let runtime = state.read(cx).runtime.clone();
    let config = agent.config.clone();
    crate::runtime::Runtime::handle(cx).spawn(async move {
        for content in [
            "Claude, what changed in the build?",
            "Claude, please look at the flaky test",
        ] {
            let event = BuzzEvent {
                id: format!("{:064x}", uuid::Uuid::new_v4().as_u128()),
                pubkey: handle.owner.clone(),
                created_at: chrono::Utc::now().timestamp() as u64,
                kind: 9,
                content: content.into(),
                tags: vec![vec!["h".into(), channel.clone()]],
                sig: String::new(),
            };
            runtime
                .run(Run {
                    wake: Wake::Mention { event },
                    channel: Some(channel.clone()),
                    agent: std::sync::Arc::new(handle.clone()),
                    config: config.clone(),
                    spawn: spawn.clone(),
                })
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

struct Preview {
    pubkey: String,
    selected: String,
    views: HashMap<String, AnyView>,
    _observe: Subscription,
}

impl Preview {
    fn new(pubkey: String, tab: &str, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let agents = Agents2::global(cx).expect("Agents2 is running");
        Self {
            pubkey,
            selected: tab.to_string(),
            views: HashMap::new(),
            _observe: cx.observe(&agents, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let agent =
            Agents2::global(cx).and_then(|agents| agents.read(cx).find(&self.pubkey).cloned());
        let Some(agent) = agent else {
            return div();
        };
        let kind = type_of(&agent.agent_type, cx);
        let type_tabs = kind
            .as_ref()
            .map(|kind| kind.kind.tabs())
            .unwrap_or_default();
        let summary = kind
            .as_ref()
            .map(|kind| kind.summary_of(&agent))
            .unwrap_or_default();
        let mut tabs = Tabs::new(TabVariant::Panel);
        for tab in &type_tabs {
            let id = tab.id.to_string();
            tabs = tabs.tab(
                Tab::new(SharedString::from(format!("dev-{id}")), tab.title.clone())
                    .selected(self.selected == id)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = id.clone();
                        cx.notify();
                    })),
            );
        }
        tabs = tabs.tab(Tab::new("dev-attention", "Attention"));
        let panel = type_tabs
            .iter()
            .find(|tab| tab.id.as_ref() == self.selected)
            .map(|tab| {
                self.views
                    .entry(self.selected.clone())
                    .or_insert_with(|| {
                        (tab.build)(
                            AgentView {
                                agent: agent.clone(),
                            },
                            window,
                            cx,
                        )
                    })
                    .clone()
            });
        div()
            .size_full()
            .bg(c.surface_panel)
            .text_color(c.text_standard)
            .p(space::S6)
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_heading().child(agent.name.clone()))
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(summary),
                    ),
            )
            .child(tabs)
            .children(panel)
    }
}

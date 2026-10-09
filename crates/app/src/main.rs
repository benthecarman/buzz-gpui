mod config;
mod hosts;
mod runtime;

use gpui_kit::*;
use hosts::{HostConnection, Hosts};

/// Temporary view listing hosts and their agents, to exercise the host
/// connections until the real shell lands.
struct Shell {
    hosts: Vec<Entity<HostConnection>>,
}

impl Shell {
    fn new(cx: &mut Context<Self>) -> Self {
        let hosts = cx.global::<Hosts>().hosts.clone();
        for host in &hosts {
            cx.observe(host, |_, _, cx| cx.notify()).detach();
        }
        Self { hosts }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hosts = self.hosts.clone();
        div()
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_4()
            .children(hosts.iter().map(|host| host_section(host.read(cx))))
    }
}

fn host_section(host: &HostConnection) -> impl IntoElement {
    let state = format!("{:?}", host.state);
    let harnesses = host
        .harnesses
        .as_ref()
        .map(|list| {
            list.iter()
                .map(|h| {
                    if h.missing.is_empty() {
                        format!("{} ✓", h.label)
                    } else {
                        format!("{} (missing {})", h.label, h.missing.join(", "))
                    }
                })
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_default();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_lg().child(host.label.clone()))
        .child(div().text_sm().child(state))
        .child(div().text_sm().child(harnesses))
        .children(host.agents.agents.iter().map(|agent| {
            div()
                .text_sm()
                .child(format!("{} — {:?}", agent.name, agent.status))
        }))
}

fn main() {
    buzz_host::dispatch_supervisor();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = config::Config::load();
    let app = gpui_kit::application();
    app.run(move |cx| {
        gpui_kit::init(cx);
        runtime::Runtime::init(cx);
        Hosts::init(&config, cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(Shell::new))
            .expect("open main window");
    });
}

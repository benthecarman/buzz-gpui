mod community;
mod config;
mod deep_links;
mod hosts;
mod http;
mod idle;
mod mirror;
mod notifications;
mod runtime;
mod ui;

use gpui_kit::*;
use hosts::Hosts;
use ui::navigation::{self, Request};

fn main() {
    buzz_host::dispatch_supervisor();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    // A `buzz://` link the OS launched us with (`Exec=buzz-gpui %U`).
    let link = deep_links::from_args(std::env::args().skip(1));
    let config = config::Config::load();
    let app = gpui_kit::application().with_assets(buzz_ui::Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        buzz_ui::init(cx);
        runtime::Runtime::init(cx);
        cx.set_http_client(std::sync::Arc::new(http::Http::new(
            runtime::Runtime::handle(cx),
        )));
        Hosts::init(&config, cx);
        community::AppState::init(cx);
        idle::Idle::init(cx);
        let requests = navigation::init(cx);
        if let Some(link) = link {
            let _ = requests.unbounded_send(Request::Url(link));
        }
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        let (window, workspace) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(400.))),
                app_id: Some("buzz-gpui".into()),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| ui::workspace::Workspace::new(window, cx)),
        )
        .expect("open main window");
        navigation::install(window, &workspace, cx);
    });
}

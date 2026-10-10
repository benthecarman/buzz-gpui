mod agent_activity;
mod agents2;
mod appearance;
mod community;
mod config;
mod deep_links;
mod hosts;
mod http;
mod idle;
mod image_clipboard;
mod main_window;
mod mirror;
mod network;
mod notifications;
mod oauth;
mod runtime;
mod shortcuts;
mod sign_out;
#[cfg(unix)]
mod single_instance;
mod snapshot;
mod sounds;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod tray;
mod ui;

use hosts::Hosts;
use ui::navigation::{self, Request};

fn main() {
    buzz_host::dispatch_supervisor();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    // A pending sign-out finishes before anything else reads or writes storage.
    sign_out::boot();
    // A `buzz://` link the OS launched us with (`Exec=buzz-gpui %U`).
    let link = deep_links::from_args(std::env::args().skip(1));
    #[cfg(unix)]
    let instance = {
        let path = single_instance::socket_path(&community::data_dir());
        let handoff = single_instance::Handoff {
            link: link.clone().unwrap_or_default(),
            // What the launcher gave us to come to the front with; the
            // running instance uses it instead.
            activation_token: std::env::var("XDG_ACTIVATION_TOKEN").ok(),
        };
        match single_instance::claim(&path, &handoff) {
            Ok(single_instance::Claim::Forwarded) => {
                tracing::info!("handed off to the running Buzz");
                return;
            }
            Ok(single_instance::Claim::Primary(listener)) => Some(listener),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "running without single-instance handoff");
                None
            }
        }
    };
    deep_links::register_scheme();
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
        notifications::Notifications::init(cx);
        appearance::init(cx);
        idle::Idle::init(cx);
        agents2::init(cx);
        ui::pages::init(cx);
        ui::agent_activity::init(cx);
        ui::channel_panels::dev_sample(cx);
        network::init(cx);
        let requests = navigation::init(cx);
        if let Some(link) = link {
            let _ = requests.unbounded_send(Request::Url(link).into());
        }
        #[cfg(unix)]
        if let Some(listener) = instance {
            single_instance::serve(listener, move |handoff| {
                let request = match handoff.link.is_empty() {
                    true => Request::Focus,
                    false => Request::Url(handoff.link),
                };
                let _ = requests.unbounded_send(navigation::Arrival {
                    request,
                    activation_token: handoff.activation_token,
                });
            });
        }
        shortcuts::init(cx);
        main_window::init(cx);
        main_window::open(cx);
    });
}

// Release builds open no console window beside the app on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent_activity;
mod agent_plugins;
mod agents2;
mod appearance;
mod buzz_mcp;
mod community;
mod config;
mod deep_links;
mod hosts;
mod http;
mod idle;
mod image_clipboard;
mod main_window;
mod mirror;
mod mission_control;
mod network;
mod notifications;
mod oauth;
mod perf;
mod plugins;
mod runtime;
mod shortcuts;
mod sign_out;
mod single_instance;
mod snapshot;
mod sounds;
mod tray;
mod ui;
mod updates;

use hosts::Hosts;
use ui::navigation::{self, Request};

fn main() {
    perf::init();
    buzz_host::dispatch_supervisor();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    // A pending sign-out finishes before anything else reads or writes storage.
    sign_out::boot();
    // Media fetched in earlier runs, kept where signing out erases it.
    if let Some(cache) = dirs::cache_dir() {
        buzz_relay_client::set_disk_cache(cache.join("buzz-gpui").join("media"));
    }
    // A `buzz://` link the OS launched us with (`Exec=buzz-gpui %U` on
    // Linux, the registered command on Windows). macOS sends links as
    // events instead (`on_open_urls` below).
    let link = deep_links::from_args(std::env::args().skip(1));
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
    // Launch Services opens the running app rather than a second copy, and
    // hands it links (on a cold start too) and Dock clicks as events.
    #[cfg(target_os = "macos")]
    let os_requests = {
        let (sender, requests) = futures::channel::mpsc::unbounded();
        let links = sender.clone();
        app.on_open_urls(move |urls| {
            if let Some(link) = deep_links::from_args(urls) {
                let _ = links.unbounded_send(Request::Url(link));
            }
        });
        app.on_reopen(move |_| {
            let _ = sender.unbounded_send(Request::Focus);
        });
        requests
    };
    app.run(move |cx| {
        // Windows shows toasts and groups taskbar buttons under this
        // AppUserModelID; it is the macOS bundle identifier too.
        #[cfg(windows)]
        cx.set_app_identity("com.benthecarman.buzz-gpui", "Buzz");
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
        plugins::Plugins::init(cx);
        agents2::init(cx);
        ui::agents::init(cx);
        agent_plugins::claude_code::init(cx);
        mission_control::init(cx);
        ui::pages::init(cx);
        ui::agent_activity::init(cx);
        ui::profile_panel::init(cx);
        ui::channel_usage::init(cx);
        ui::todos::init(cx);
        ui::github::init(cx);
        ui::projects::init(cx);
        ui::moderation::init(cx);
        ui::settings::personal_groups::init(cx);
        ui::settings::pairing::init(cx);
        ui::templates::init(cx);
        ui::channel_panels::dev_sample(cx);
        ui::terminal::init(cx);
        network::init(cx);
        let requests = navigation::init(cx);
        if let Some(link) = link {
            let _ = requests.unbounded_send(Request::Url(link).into());
        }
        #[cfg(target_os = "macos")]
        {
            let requests = requests.clone();
            cx.background_executor()
                .spawn(futures::StreamExt::for_each(os_requests, move |request| {
                    let _ = requests.unbounded_send(request.into());
                    std::future::ready(())
                }))
                .detach();
        }
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
        updates::init(cx);
        main_window::open(cx);
    });
}

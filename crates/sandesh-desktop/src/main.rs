//! The Sandesh desktop shell (`ROADMAP.md` Phase 9).
//!
//! A Tauri window over the *same* engine the CLI runs. This crate wires
//! nothing: it starts [`sandesh_app::Application`], asks it which port the OS
//! gave the HTTP listener, and opens a WebView on that address. Every listener,
//! every route and every asset is the one `sandesh` already serves.
//!
//! ## Why the WebView loads `127.0.0.1` rather than bundled assets
//!
//! Tauri can embed a frontend build in the binary and serve it from
//! `tauri://localhost`. This shell deliberately does not (ADR-092).
//! The engine already serves `web/dist` through `rust-embed`, so pointing the
//! WebView at the engine means:
//!
//! - **One asset build, not two.** "The React build is byte-identical to the
//!   web build" is then true by construction rather than by a check — there is
//!   only one build, served by the same code path in both products.
//! - **No cross-origin call, so no CORS and no configurable base URL.** The UI
//!   is unchanged, which is the criterion Phase 9 exists to test.
//! - **The origin semantics the UI was designed against are preserved.** The
//!   sandboxed HTML preview (ADR-030) reasons about origins; serving the same
//!   bytes from a different scheme would change that reasoning silently.
//!
//! ## Why the port is ephemeral
//!
//! A desktop app must not fail to launch because the user already has Sandesh
//! running on the default port — and must not quietly take it either. The
//! engine binds `127.0.0.1:0`, the OS chooses, and `addresses()` reports what
//! it chose. This is the mechanism every integration test already uses, so the
//! desktop path is not a new one.
//!
//! Loopback only, never `0.0.0.0`: installing a desktop app must not put a mail
//! server on the user's network.

use std::net::SocketAddr;

use sandesh_app::Application;

mod engine;
mod notify;
mod tray;

use engine::{desktop_config, webview_url};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Where the desktop app keeps its database and blobs.
///
/// The CLI defaults to a path under the working directory, which is right for a
/// server started from a terminal and wrong for a GUI app launched from a
/// desktop icon — there the working directory is arbitrary, so mail would land
/// somewhere different depending on how the app was started.
///
/// Tauri resolves the platform's own application-data location, which is what
/// ROADMAP means by "OS-appropriate data directory".
fn data_directory(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|error| format!("no application data directory is available: {error}"))
}

/// Starts the engine and opens a window on it.
///
/// Ordering matters and mirrors the CLI's: the engine is fully bound *before*
/// the window exists, so the WebView never races a listener that is not up yet
/// and there is no retry loop to get wrong. `Application::start` returning
/// means every port is bound.
fn main() {
    // The same log configuration the CLI uses, and the same `SANDESH_LOG`
    // override: a desktop user debugging their app should not need a different
    // incantation from a server one.
    sandesh_app::logging::init("info");

    tauri::Builder::default()
        .setup(|app| {
            let data_dir = data_directory(app.handle())?;
            std::fs::create_dir_all(&data_dir)?;

            let config = desktop_config(&data_dir);

            // The engine is async; Tauri's setup is not. A dedicated runtime
            // owned by the shell keeps the engine's tasks alive for the life of
            // the window, and hands back the bound addresses synchronously.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let engine = runtime.block_on(Application::start(config))?;
            let http = engine
                .addresses()
                .http
                .ok_or("the desktop shell requires the HTTP listener, which is disabled")?;
            let shutdown = engine.shutdown_token();
            let events: std::sync::Arc<dyn sandesh_core::EventSource> =
                std::sync::Arc::clone(engine.events())
                    as std::sync::Arc<dyn sandesh_core::EventSource>;
            // The tray labels itself with the SMTP address, which is the thing a
            // developer reads most often.
            let smtp = engine
                .addresses()
                .smtp
                .ok_or("the desktop shell requires the SMTP listener, which is disabled")?;

            tracing::info!(%http, %smtp, "desktop engine bound");

            // Serve for as long as the app lives. The handle is dropped
            // deliberately: the runtime below owns the work, and cancelling the
            // token is what stops it.
            runtime.spawn(async move {
                if let Err(error) = engine.run().await {
                    tracing::error!(%error, "the embedded engine stopped");
                }
            });

            let url = webview_url(http)
                .parse()
                .map_err(|error| format!("the engine's address is not a URL: {error}"))?;

            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Sandesh")
                .inner_size(1280.0, 860.0)
                .min_inner_size(375.0, 480.0)
                .build()?;

            // Notifications on new mail, over the same bus the Web UI's
            // WebSocket and IMAP IDLE consume (A2). Its own task, so a slow or
            // absent notification daemon can never delay a delivery.
            runtime.spawn(notify::run(events, shutdown.clone()));

            // The tray is what makes hiding the window reasonable: the engine
            // keeps running, and the connection details stay reachable.
            if let Err(error) = tray::install(app.handle(), smtp) {
                // A Linux desktop with no StatusNotifierItem host has no tray.
                // That costs the menu, not the app.
                tracing::warn!(%error, "no system tray available; continuing without one");
            }

            // The runtime must outlive `setup`, or dropping it here would abort
            // every engine task the moment the window opened.
            app.manage(EngineHandle {
                _runtime: runtime,
                shutdown,
                http,
                smtp,
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window *hides* it; the engine keeps running and the
            // tray keeps pointing at it. This changed in A2: while the window
            // was the only interface, closing it had to stop the listeners, but
            // a tray that outlived its server would offer a menu of dead items.
            // Quit, chosen explicitly from the tray, is now the only stop.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Err(error) = window.hide() {
                    tracing::warn!(%error, "could not hide the window");
                }
                tracing::info!("window hidden; the engine keeps running (quit from the tray)");
            }
        })
        .on_menu_event(|app, event| {
            if let Some(engine) = app.try_state::<EngineHandle>() {
                // Quit stops the listeners *before* the event loop exits.
                // Relying on process teardown alone would leave the sockets
                // bound for as long as the runtime took to drop, and a mail
                // server that cannot be restarted immediately after quitting
                // is a mail server that looks broken.
                if event.id().as_ref() == tray::QUIT {
                    engine.shutdown.cancel();
                }
                tray::handle_menu_event(app, &event, engine.smtp);
            }
        })
        .run(tauri::generate_context!())
        // The one place a desktop binary legitimately gives up: if the window
        // system refuses, there is no UI to report the failure through, and the
        // engine has nothing to serve.
        .unwrap_or_else(|error| {
            tracing::error!(%error, "the desktop shell could not start");
            std::process::exit(1);
        });
}

/// The running engine, kept alive for the life of the application.
///
/// Held in Tauri's state purely so it is not dropped: dropping the runtime
/// aborts every engine task, and dropping the token would leave the listeners
/// with no way to be stopped.
struct EngineHandle {
    _runtime: tokio::runtime::Runtime,
    shutdown: tokio_util::sync::CancellationToken,
    /// Where the UI and API are served. Reported in logs, and the address any
    /// test drives.
    #[allow(dead_code)]
    http: SocketAddr,
    /// Where SMTP listens — the address the tray menu offers to copy.
    smtp: SocketAddr,
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("http", &self.http)
            .field("smtp", &self.smtp)
            .finish_non_exhaustive()
    }
}

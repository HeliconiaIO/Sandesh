//! The tray icon and its menu (`ROADMAP.md` Phase 9, A2).
//!
//! A mail server is a background thing: the developer starts it once and wants
//! it out of the way until mail arrives. The tray is what makes closing the
//! window reasonable — and it is also what makes the *connection details*
//! reachable without one, which is the single thing a developer needs from
//! Sandesh most often and currently has to open the UI to read.
//!
//! ## Closing the window no longer stops the engine
//!
//! A1 cancelled the engine when the window was destroyed, which was right while
//! the window was the only interface. With a tray, closing the window has to
//! mean *hide*, or the tray would outlive the server it points at and every
//! menu item would be dead. Quit — explicitly chosen from the menu — is now the
//! only thing that stops the listeners.

use std::net::SocketAddr;

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

/// Menu item ids. Matched in [`handle_menu_event`], so they are constants
/// rather than strings repeated at both ends. [`QUIT`] is public because the
/// shell cancels the engine on it before Tauri tears the event loop down.
const SHOW: &str = "show";
const COPY_SMTP: &str = "copy-smtp";
pub const QUIT: &str = "quit";

/// Builds the tray icon, its menu, and the click behaviour.
///
/// # Errors
/// If the menu or tray cannot be created — on Linux, typically because no
/// StatusNotifierItem host is running.
pub fn install(app: &AppHandle, smtp: SocketAddr) -> tauri::Result<()> {
    // The address is in the item's label rather than behind a submenu: reading
    // it is the common case, copying it the occasional one, and a label that
    // states the port means the answer needs no click at all.
    let smtp_item = MenuItem::with_id(
        app,
        COPY_SMTP,
        format!("Copy SMTP address  ({smtp})"),
        true,
        None::<&str>,
    )?;
    let show = MenuItem::with_id(app, SHOW, "Open Sandesh", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit Sandesh", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[&show, &separator, &smtp_item, &separator, &quit])?;

    TrayIconBuilder::with_id("sandesh-tray")
        .icon(app.default_window_icon().cloned().ok_or_else(|| {
            tauri::Error::UnknownPath // the window icon is compiled in; absence is a build fault
        })?)
        .tooltip(format!("Sandesh — SMTP on {smtp}"))
        .menu(&menu)
        // Left-click shows the window. `false` means a left click does *not*
        // open the menu, which is the platform convention: the menu is the
        // right-click affordance, and a primary click on a tray icon restores.
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

/// Routes a tray menu selection.
///
/// `smtp` is passed in rather than read from state so the routing is a pure
/// function of (event, address) and can be reasoned about — and tested — without
/// an `AppHandle`.
pub fn handle_menu_event(app: &AppHandle, event: &MenuEvent, smtp: SocketAddr) {
    match event.id().as_ref() {
        SHOW => show_window(app),
        COPY_SMTP => {
            // Written to the window's clipboard through the WebView, because
            // that is the clipboard the rest of the app already uses; a second
            // clipboard integration would be a second thing to keep working.
            if let Some(window) = app.get_webview_window("main") {
                let script = format!(
                    "navigator.clipboard && navigator.clipboard.writeText({});",
                    serde_json_string(&smtp.to_string())
                );
                if let Err(error) = window.eval(&script) {
                    tracing::warn!(%error, "could not copy the SMTP address");
                }
            }
        }
        QUIT => {
            tracing::info!("quit chosen from the tray; stopping the engine");
            // `exit` runs Tauri's shutdown, which drops the managed state and
            // with it the engine handle — the same path as closing the last
            // window used to take in A1.
            app.exit(0);
        }
        other => tracing::debug!(id = other, "unhandled tray menu item"),
    }
}

/// Shows and focuses the main window, creating nothing.
///
/// The window always exists — it is built during setup and only ever hidden —
/// so a missing one is a bug worth logging rather than a case to recover from.
fn show_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("the main window is missing; cannot show it");
        return;
    };
    if let Err(error) = window.show() {
        tracing::warn!(%error, "could not show the window");
    }
    if let Err(error) = window.set_focus() {
        tracing::warn!(%error, "could not focus the window");
    }
}

/// Quotes a string as a JSON literal for embedding in `eval`.
///
/// Hand-written rather than pulling `serde_json` into this crate for one call.
/// The input is a socket address, but escaping is done properly regardless: a
/// string interpolated into JavaScript is exactly where "it can only ever be
/// digits and dots" stops being true one refactor later.
fn serde_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_quoted_as_a_json_string() {
        assert_eq!(serde_json_string("127.0.0.1:1025"), "\"127.0.0.1:1025\"");
    }

    #[test]
    fn quotes_and_backslashes_cannot_escape_the_literal() {
        // The reason this is not `format!("\"{value}\"")`: a value carrying a
        // quote would otherwise end the string and run as code.
        assert_eq!(serde_json_string(r#"a"b"#), r#""a\"b""#);
        assert_eq!(serde_json_string(r"a\b"), r#""a\\b""#);
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(serde_json_string("a\nb"), "\"a\\nb\"");
        // A raw control byte must come out as a \u escape, not as itself:
        // the point of this function is that nothing in the value can be read
        // as JavaScript syntax.
        assert_eq!(serde_json_string("a\u{1}b"), "\"a\\u0001b\"");
    }
}

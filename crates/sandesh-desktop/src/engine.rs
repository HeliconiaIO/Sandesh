//! Starting the embedded engine, separated from the window that displays it.
//!
//! The split is what makes Phase 9 testable. Everything a desktop install does
//! differently from the CLI — the data directory, the ephemeral loopback ports,
//! the lifecycle — lives here and is exercised without a WebView. `main.rs`
//! keeps only the Tauri window, which needs a display and a working renderer.
//!
//! That boundary is not a testing convenience: "the desktop app runs the
//! identical engine with no forked code" is a claim about *this* module, and a
//! window cannot make it true or false.

use std::net::SocketAddr;
use std::path::Path;

use sandesh_config::AppConfig;

/// The engine configuration a desktop install runs under.
///
/// Every difference from the CLI default follows from being a desktop app
/// rather than a server, and each is deliberate:
///
/// * **Ephemeral ports**, so launching never collides with a Sandesh the user
///   is already running, and never silently takes the port that server wanted.
/// * **Loopback binds**, so installing a desktop app never puts a mail server
///   on the user's network.
/// * **A platform data directory**, so mail does not land in whatever directory
///   the app happened to be launched from.
///
/// Everything else is `AppConfig::default()` — the same defaults the CLI runs,
/// which is what "no forked code" means at the configuration layer.
#[must_use]
pub fn desktop_config(data_dir: &Path) -> AppConfig {
    let loopback_ephemeral: SocketAddr = ([127, 0, 0, 1], 0).into();
    let defaults = AppConfig::default();

    AppConfig {
        database_path: data_dir.join("sandesh.db"),
        blob_path: data_dir.join("blobs"),
        http: sandesh_config::HttpConfig {
            bind: loopback_ephemeral,
            ..defaults.http
        },
        smtp: sandesh_config::SmtpConfig {
            bind: loopback_ephemeral,
            ..defaults.smtp
        },
        imap: sandesh_config::ImapConfig {
            bind: Some(loopback_ephemeral),
            ..defaults.imap
        },
        pop3: sandesh_config::Pop3Config {
            bind: Some(loopback_ephemeral),
            ..defaults.pop3
        },
        ..defaults
    }
}

/// The URL the WebView should load, given the engine's bound HTTP address.
///
/// Trivial by design, and that is the point: the desktop app reaches the engine
/// over the same loopback HTTP the browser does, so there is no second asset
/// pipeline and no cross-origin call (ADR-092).
#[must_use]
pub fn webview_url(http: SocketAddr) -> String {
    format!("http://{http}/")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// The bind of an optional listener, which the desktop config always sets.
    fn bind_of(bind: Option<SocketAddr>, name: &str) -> SocketAddr {
        bind.unwrap_or_else(|| panic!("{name} is enabled in the desktop configuration"))
    }

    #[test]
    fn desktop_binds_loopback_only() {
        // A desktop install must never listen on the network. Asserted for
        // every listener, not just HTTP: the mail ports are the ones that would
        // matter if this were wrong.
        let config = desktop_config(Path::new("/tmp/sandesh-test"));

        assert!(config.http.bind.ip().is_loopback(), "HTTP must be loopback");
        assert!(config.smtp.bind.ip().is_loopback(), "SMTP must be loopback");
        assert!(
            bind_of(config.imap.bind, "IMAP").ip().is_loopback(),
            "IMAP must be loopback"
        );
        assert!(
            bind_of(config.pop3.bind, "POP3").ip().is_loopback(),
            "POP3 must be loopback"
        );
    }

    #[test]
    fn desktop_uses_ephemeral_ports() {
        // Port 0 throughout: launching the app must not fail because the user
        // already has Sandesh running, nor quietly steal that server's port.
        let config = desktop_config(Path::new("/tmp/sandesh-test"));

        assert_eq!(config.http.bind.port(), 0);
        assert_eq!(config.smtp.bind.port(), 0);
        assert_eq!(bind_of(config.imap.bind, "IMAP").port(), 0);
        assert_eq!(bind_of(config.pop3.bind, "POP3").port(), 0);
    }

    #[test]
    fn storage_lives_under_the_given_data_directory() {
        // Not the working directory: a GUI app is launched from somewhere
        // arbitrary, so mail would otherwise land in a different place
        // depending on how it was started.
        let dir = Path::new("/somewhere/app-data");
        let config = desktop_config(dir);

        assert!(config.database_path.starts_with(dir));
        assert!(config.blob_path.starts_with(dir));
    }

    #[test]
    fn the_webview_loads_the_engines_own_address() {
        let address: SocketAddr = "127.0.0.1:41234".parse().expect("a valid address");
        let url = webview_url(address);
        assert_eq!(url, "http://127.0.0.1:41234/");
    }
}

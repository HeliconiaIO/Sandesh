//! Logging setup.
//!
//! ## What is deliberately not logged
//!
//! Message bodies, raw email, headers and credentials never reach the log. A
//! developer mail server holds exactly the material people paste into bug
//! reports — password reset links, one-time codes, personal addresses — and a
//! log file is far easier to leak than a database. Protocol-level tracing of
//! `DATA` content stays off by default (`SMTP_DESIGN.md`); the message is in
//! the database, where the API can show it deliberately.
//!
//! Events are logged as identifiers only, which is the same reason the event
//! payloads carry identifiers rather than content (`EVENT_MODEL.md` §1).

use std::sync::{Arc, LazyLock};

use sandesh_events::{LogBuffer, LogLayer};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// The environment variable that overrides the configured level.
pub const LOG_ENV: &str = "SANDESH_LOG";

/// The process-wide capture buffer the log stream reads (Phase 7 A6).
///
/// Global for the same reason the subscriber is: `tracing` installs one
/// subscriber per process, so the layer feeding this buffer is installed once
/// and the buffer must outlive any single `Application`. Threading it through
/// `Application::start` instead would change a signature twenty-one call
/// sites use, to hand every one of them the same value.
static LOG_BUFFER: LazyLock<Arc<LogBuffer>> = LazyLock::new(|| Arc::new(LogBuffer::new()));

/// The buffer the capture layer writes to.
///
/// Populated only once [`init`] has installed the layer. Before that — and in
/// a test binary where another subscriber won the race — it is simply empty,
/// which the endpoint reports as an empty list rather than an error: "nothing
/// has been logged yet" and "capture is not installed" look the same to a
/// reader, and neither is a failure.
#[must_use]
pub fn buffer() -> Arc<LogBuffer> {
    Arc::clone(&LOG_BUFFER)
}

/// Initialises logging.
///
/// The configured level is the default; `SANDESH_LOG` overrides it, so a
/// developer can raise verbosity without editing configuration.
///
/// Returns whether initialisation happened. A second call is a no-op rather
/// than an error: a global subscriber can only be set once, and a test binary
/// that starts several applications must not fail on the second one.
///
/// The return value is safe to ignore: a caller that does not care whether it
/// was first simply gets whatever subscriber is already installed.
#[allow(clippy::must_use_candidate)]
pub fn init(level: &str) -> bool {
    let filter = EnvFilter::try_from_env(LOG_ENV)
        .or_else(|_| EnvFilter::try_new(level))
        // An unusable filter must not stop the program from starting; fall back
        // to something sane and carry on.
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // The fmt layer keeps the terminal output a developer running the binary
    // expects; the capture layer adds a second reader. A6 adds a consumer of
    // the log, it does not move the log somewhere else.
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(LogLayer::new(buffer()))
        .try_init()
        .is_ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn initialising_twice_is_not_an_error() {
        // A test binary may start several applications in one process.
        let _ = init("info");
        let _ = init("info");
    }

    /// The layer must actually capture, not merely install.
    ///
    /// `init` returns false when another test already installed a subscriber,
    /// and in that case this process has no capture layer — so the assertion
    /// is conditional on having won the race rather than skipped outright.
    /// Run alone (`--test-threads=1`) it always exercises the real path.
    #[test]
    fn an_installed_layer_captures_an_emitted_line() {
        if !init("info") {
            // Another test installed the subscriber first. Nothing to assert
            // about a layer this process never installed.
            return;
        }

        tracing::info!(marker = "a6-capture-probe", "log capture probe");

        let captured = buffer().recent(100, None, None);
        assert!(
            captured
                .iter()
                .any(|line| line.message.contains("log capture probe")),
            "the emitted line never reached the buffer: {captured:?}"
        );
    }

    #[test]
    fn an_unusable_filter_does_not_prevent_startup() {
        // A bad log directive is a configuration mistake, not a reason to
        // refuse to run a mail server. `init` must never panic.
        let _ = init("this is not a valid=filter=!!!");
    }
}

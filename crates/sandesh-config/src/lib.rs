//! # Sandesh configuration
//!
//! The configuration model and its defaults. Phase 2 covers only what the SMTP
//! server needs; layered loading from file, environment and CLI arrives with
//! the application crate in a later phase.
//!
//! This crate deliberately depends on nothing but `thiserror`. It is read by
//! protocol adapters and by the composition root, so anything heavier here
//! would propagate into all of them.
//!
//! ## Safety posture
//!
//! Listeners bind to **loopback by default** ([`SmtpConfig::default`]). Sandesh
//! accepts mail without authentication (ADR-005), so a default of `0.0.0.0`
//! would put an open mail server on the network the moment someone started it.
//! Binding elsewhere is possible and supported — it just has to be deliberate,
//! and [`SmtpConfig::warnings`] reports the risk so the operator is told.

#![doc(html_root_url = "https://docs.rs/sandesh-config")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

/// The loopback address every listener binds to unless told otherwise.
pub const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Default SMTP port.
///
/// 1025 is the de facto developer convention (`README.md`), and being above
/// 1024 means no elevated privileges are needed.
pub const DEFAULT_SMTP_PORT: u16 = 1025;

/// How Sandesh treats a recipient that matches no local identity.
///
/// Implements the routing policy in `CORE_SPEC.md` §7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnknownRecipientPolicy {
    /// Deliver to the catch-all account. The default: a development mail server
    /// that bounces unrecognised recipients creates friction with no benefit.
    #[default]
    CatchAll,
    /// Create an account and identity on demand.
    AutoCreate,
    /// Reject with `550`. For testing an application's bounce handling.
    Strict,
}

/// Whether the SMTP server requires authentication.
///
/// Phase 2 implements neither mechanism; the type exists so the session state
/// machine has a real authentication boundary from the start rather than
/// hard-coding "unauthenticated forever" (ADR-005 and the Phase 4.5 roadmap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// No authentication. Any client may deliver. The Phase 2 default.
    #[default]
    Disabled,
    /// Authentication offered but not required.
    Optional,
    /// Authentication required before `MAIL FROM`.
    Required,
}

impl AuthMode {
    /// Whether the server should accept `AUTH` at all.
    ///
    /// Disabled remains the default (ADR-005): Sandesh is a local development
    /// mail server, and requiring credentials before it will accept a test
    /// message would add friction with no security benefit on loopback.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Optional | Self::Required)
    }

    /// Whether a client must authenticate before `MAIL FROM`.
    #[must_use]
    pub const fn is_required(self) -> bool {
        matches!(self, Self::Required)
    }

    /// Whether `AUTH` may be offered on a connection with the given encryption.
    ///
    /// `PLAIN` and `LOGIN` both send the password, so offering them in the
    /// clear would put a credential on the wire (ADR-033). RFC 8314 §4.1 says
    /// not to, and a client that sees no `AUTH` will not try — which is a far
    /// better outcome than one that tries and succeeds insecurely.
    #[must_use]
    pub const fn is_offered_over(self, encrypted: bool) -> bool {
        self.is_enabled() && encrypted
    }
}

/// Whether the SMTP server offers TLS.
///
/// Phase 4.5 implements this; the type exists so the capability list and
/// session model already have a place for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsMode {
    /// No TLS. The Phase 2 default.
    #[default]
    Disabled,
    /// `STARTTLS` offered on the cleartext port.
    StartTls,
    /// TLS from the first byte.
    Implicit,
}

impl TlsMode {
    /// Whether `STARTTLS` should be advertised.
    ///
    /// Only when it actually works: advertising a capability a client cannot
    /// complete is the worst failure mode an SMTP server has.
    #[must_use]
    pub const fn advertises_starttls(self) -> bool {
        matches!(self, Self::StartTls)
    }

    /// Whether TLS is available at all, by either route.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::StartTls | Self::Implicit)
    }
}

/// SMTP server configuration.
///
/// Every limit here exists to bound what an untrusted client can make the
/// server allocate or hold open. Defaults are chosen to be generous enough that
/// ordinary development never notices them.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    /// Address to bind. Defaults to loopback.
    pub bind: SocketAddr,

    /// Hostname announced in the greeting and `EHLO` response.
    pub hostname: String,

    /// Largest message accepted, in bytes. Advertised via `SIZE` and enforced
    /// during `DATA` collection rather than after buffering.
    pub max_message_size: usize,

    /// Longest single command line, in bytes.
    ///
    /// RFC 5321 §4.5.3.1.4 sets 512 octets for the command line; this is
    /// deliberately far larger, because a developer tool that rejects a long
    /// address is annoying, and the limit exists to bound memory, not to police
    /// conformance.
    pub max_line_length: usize,

    /// Largest number of recipients in one transaction.
    ///
    /// RFC 5321 §4.5.3.1.8 requires at least 100.
    pub max_recipients: usize,

    /// Largest number of simultaneous connections.
    pub max_connections: usize,

    /// How long to wait for a command before closing the connection.
    pub command_timeout: Duration,

    /// How long to wait for `DATA` content before abandoning the transaction.
    pub data_timeout: Duration,

    /// Authentication posture.
    pub auth_mode: AuthMode,

    /// TLS posture.
    pub tls_mode: TlsMode,

    /// What to do with a recipient that matches no local identity.
    pub unknown_recipient: UnknownRecipientPolicy,

    /// Whether `AUTH` may be offered and accepted on an unencrypted connection
    /// (developer mode / localhost testing). Defaults to `false` for strict
    /// conformance tests, but enabled in the application server for local clients.
    pub allow_cleartext_auth: bool,
}

impl Default for SmtpConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(DEFAULT_BIND, DEFAULT_SMTP_PORT),
            hostname: String::from("sandesh"),
            // 25 MiB, matching PROTOCOL_CAPABILITIES §3.2.
            max_message_size: 25 * 1024 * 1024,
            // 64 KiB: far above any legitimate command, low enough to bound a
            // client that never sends CRLF.
            max_line_length: 64 * 1024,
            max_recipients: 100,
            max_connections: 64,
            // RFC 5321 §4.5.3.2 sets 5 minutes for command timeouts. Matching
            // it keeps well-behaved clients working while bounding idle holds.
            command_timeout: Duration::from_mins(5),
            // 10 minutes, per RFC 5321 §4.5.3.2 for DATA termination.
            data_timeout: Duration::from_mins(10),
            auth_mode: AuthMode::Disabled,
            tls_mode: TlsMode::Disabled,
            unknown_recipient: UnknownRecipientPolicy::CatchAll,
            allow_cleartext_auth: false,
        }
    }
}

impl SmtpConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a limit is set to a value that cannot work.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.hostname.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "hostname",
                reason: String::from("must not be empty; it appears in the SMTP greeting"),
            });
        }
        if self.max_message_size == 0 {
            return Err(ConfigError::Invalid {
                field: "max_message_size",
                reason: String::from("must be greater than zero"),
            });
        }
        // A command line shorter than the longest fixed-form command could not
        // accept valid input at all.
        if self.max_line_length < 512 {
            return Err(ConfigError::Invalid {
                field: "max_line_length",
                reason: String::from("must be at least 512, the RFC 5321 minimum"),
            });
        }
        if self.max_recipients == 0 {
            return Err(ConfigError::Invalid {
                field: "max_recipients",
                reason: String::from("must be at least 1 or no mail could be delivered"),
            });
        }
        if self.max_connections == 0 {
            return Err(ConfigError::Invalid {
                field: "max_connections",
                reason: String::from("must be at least 1 or no client could connect"),
            });
        }
        if self.command_timeout.is_zero() || self.data_timeout.is_zero() {
            return Err(ConfigError::Invalid {
                field: "timeouts",
                reason: String::from("must be non-zero"),
            });
        }

        Ok(())
    }

    /// Whether the listener is reachable from outside this machine.
    #[must_use]
    pub fn is_externally_bound(&self) -> bool {
        !self.bind.ip().is_loopback()
    }

    /// Warnings the operator should see at startup.
    ///
    /// Returning these rather than logging them keeps the crate free of a
    /// logging dependency and lets the caller decide how to present them.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.is_externally_bound() && !self.auth_mode.is_enabled() {
            warnings.push(format!(
                "SMTP is bound to {} with authentication disabled: any host that \
                 can reach this address can deliver mail to Sandesh. Bind to \
                 127.0.0.1 unless you intend this.",
                self.bind
            ));
        }
        if self.is_externally_bound() && self.tls_mode == TlsMode::Disabled {
            warnings.push(format!(
                "SMTP is bound to {} without TLS: message content and any \
                 credentials cross the network in cleartext.",
                self.bind
            ));
        }
        // Auth without TLS is not merely weak here — it is inert, because AUTH
        // is never advertised on an unencrypted connection (ADR-033) unless
        // allow_cleartext_auth is enabled.
        if self.auth_mode.is_enabled() && !self.tls_mode.is_enabled() && !self.allow_cleartext_auth
        {
            warnings.push(String::from(
                "SMTP authentication is enabled but TLS is not, so AUTH will \
                 never be offered: PLAIN and LOGIN send the password and are \
                 refused on an unencrypted connection. Set tls_mode to \
                 start_tls to use authentication, or enable allow_cleartext_auth.",
            ));
        }
        warnings
    }
}

/// A configuration error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A field holds a value that cannot work.
    #[error("invalid configuration for {field}: {reason}")]
    Invalid {
        /// The offending field.
        field: &'static str,
        /// Why it was rejected.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bind_to_loopback() {
        // The single most important default in the crate: Sandesh accepts mail
        // without authentication, so it must not be reachable by default.
        let config = SmtpConfig::default();
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.bind.port(), 1025);
        assert!(!config.is_externally_bound());
    }

    #[test]
    fn defaults_are_valid() {
        assert!(SmtpConfig::default().validate().is_ok());
    }

    #[test]
    fn defaults_produce_no_warnings() {
        assert!(SmtpConfig::default().warnings().is_empty());
    }

    #[test]
    fn external_binding_without_auth_warns() {
        let config = SmtpConfig {
            bind: "0.0.0.0:1025".parse().unwrap_or_else(|_| {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_SMTP_PORT)
            }),
            ..SmtpConfig::default()
        };
        let warnings = config.warnings();
        assert!(config.is_externally_bound());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("authentication disabled")),
            "an open relay-shaped default must be called out: {warnings:?}"
        );
    }

    #[test]
    fn auth_and_tls_are_advertised_only_when_they_work() {
        // Advertising a capability that is not implemented is the worst thing
        // an SMTP server can do to a client. Phase 4.5 implements both, so the
        // invariant becomes "advertised exactly when configured on" rather than
        // the Phase 2 "never advertised".
        assert!(!AuthMode::Disabled.is_enabled());
        assert!(AuthMode::Optional.is_enabled());
        assert!(AuthMode::Required.is_enabled());

        // Only `Required` forces a client to authenticate.
        assert!(!AuthMode::Disabled.is_required());
        assert!(!AuthMode::Optional.is_required());
        assert!(AuthMode::Required.is_required());

        // STARTTLS is an in-band upgrade, so implicit TLS must not advertise it.
        assert!(!TlsMode::Disabled.advertises_starttls());
        assert!(TlsMode::StartTls.advertises_starttls());
        assert!(!TlsMode::Implicit.advertises_starttls());
    }

    #[test]
    fn auth_is_never_offered_on_an_unencrypted_connection() {
        // The credential-exposure guard. PLAIN and LOGIN both send the
        // password, so an unencrypted connection must not even be told they
        // exist (ADR-033).
        for mode in [AuthMode::Optional, AuthMode::Required] {
            assert!(
                !mode.is_offered_over(false),
                "{mode:?} offered in the clear"
            );
            assert!(mode.is_offered_over(true), "{mode:?} withheld over TLS");
        }

        // Disabled stays disabled regardless of encryption.
        assert!(!AuthMode::Disabled.is_offered_over(true));
    }

    #[test]
    fn enabling_auth_without_tls_warns_that_it_will_do_nothing() {
        // A configuration that silently has no effect is worse than one that
        // fails, so it must be reported at startup.
        let config = SmtpConfig {
            auth_mode: AuthMode::Required,
            tls_mode: TlsMode::Disabled,
            ..Default::default()
        };

        let warnings = config.warnings();
        assert!(
            warnings.iter().any(|w| w.contains("never be offered")),
            "expected a warning about inert auth, got {warnings:?}"
        );
    }

    #[test]
    fn auth_with_tls_produces_no_such_warning() {
        let config = SmtpConfig {
            auth_mode: AuthMode::Required,
            tls_mode: TlsMode::StartTls,
            ..Default::default()
        };

        assert!(
            !config
                .warnings()
                .iter()
                .any(|w| w.contains("never be offered")),
            "a working configuration must not warn"
        );
    }

    #[test]
    fn authentication_is_disabled_by_default() {
        // ADR-005, restated as a test because Phase 4.5 makes it possible to
        // turn on: the default must not have changed as a side effect.
        assert_eq!(SmtpConfig::default().auth_mode, AuthMode::Disabled);
        assert!(!SmtpConfig::default().auth_mode.is_enabled());
        assert_eq!(SmtpConfig::default().tls_mode, TlsMode::Disabled);
    }

    #[test]
    fn validation_rejects_unusable_limits() {
        let cases: Vec<(&str, SmtpConfig)> = vec![
            (
                "hostname",
                SmtpConfig {
                    hostname: String::new(),
                    ..Default::default()
                },
            ),
            (
                "max_message_size",
                SmtpConfig {
                    max_message_size: 0,
                    ..Default::default()
                },
            ),
            (
                "max_line_length",
                SmtpConfig {
                    max_line_length: 10,
                    ..Default::default()
                },
            ),
            (
                "max_recipients",
                SmtpConfig {
                    max_recipients: 0,
                    ..Default::default()
                },
            ),
            (
                "max_connections",
                SmtpConfig {
                    max_connections: 0,
                    ..Default::default()
                },
            ),
            (
                "timeouts",
                SmtpConfig {
                    command_timeout: Duration::ZERO,
                    ..Default::default()
                },
            ),
        ];
        for (field, config) in cases {
            assert!(config.validate().is_err(), "{field} must be rejected");
        }
    }

    #[test]
    fn unknown_recipient_defaults_to_catch_all() {
        // CORE_SPEC §7: rejecting unknown recipients by default would create
        // friction with no benefit in a development mail server.
        assert_eq!(
            SmtpConfig::default().unknown_recipient,
            UnknownRecipientPolicy::CatchAll
        );
    }
}

/// Default IMAP port.
///
/// 1143 mirrors the SMTP choice of 1025: the privileged default (143) would
/// need elevation, and 1143 is the established local-development convention.
pub const DEFAULT_IMAP_PORT: u16 = 1143;

/// Default implicit-TLS IMAP port.
///
/// 1993 stands to 993 (IMAPS) as 1143 stands to 143. Implicit TLS needs a
/// *separate* listener from the cleartext one, because the mode is defined by
/// which port a client connects to — the handshake happens before any IMAP is
/// spoken, so a single port cannot serve both (ADR-041).
pub const DEFAULT_IMAPS_PORT: u16 = 1993;

/// IMAP server configuration (RFC 9051).
///
/// Same posture as [`SmtpConfig`]: loopback by default, and every limit present
/// to bound what an untrusted client can make the server allocate or hold open.
///
/// # Why two addresses
///
/// `bind` carries cleartext IMAP (optionally upgradable with `STARTTLS`) and
/// `implicit_tls_bind` carries IMAP-over-TLS. Unlike SMTP, which selects one
/// posture per process, IMAP conventionally offers both at once — 143 and 993 —
/// because mail clients differ in which they will use. Modelling them as two
/// optional listeners rather than one `TlsMode` is what lets a deployment serve
/// both, and lets either be switched off entirely (ADR-041).
#[derive(Debug, Clone)]
pub struct ImapConfig {
    /// Cleartext listener address, or `None` to serve no cleartext port.
    ///
    /// Optional because a deployment that wants TLS only should be able to have
    /// no cleartext listener at all, rather than one it must remember not to
    /// use.
    pub bind: Option<SocketAddr>,

    /// Implicit-TLS listener address, or `None` to serve no TLS port.
    pub implicit_tls_bind: Option<SocketAddr>,

    /// Hostname announced in the greeting.
    pub hostname: String,

    /// Longest single command line, in bytes.
    ///
    /// RFC 9051 sets no limit. This bounds a client that never sends CRLF, and
    /// is deliberately generous: long `FETCH` and `SEARCH` commands are normal.
    pub max_line_length: usize,

    /// Largest literal accepted, in bytes (RFC 9051 §4.3).
    ///
    /// Enforced against the *declared* length before anything is allocated, so
    /// `{4294967295}` costs a rejection rather than four gigabytes (ADR-040).
    pub max_literal_size: usize,

    /// Largest number of simultaneous connections.
    ///
    /// IMAP clients hold connections open — often several per account — so this
    /// is higher than the SMTP equivalent, where connections are transient.
    pub max_connections: usize,

    /// How long to wait for a command before closing the connection.
    ///
    /// RFC 9051 §5.4 requires servers to allow at least 30 minutes of
    /// inactivity for an authenticated session, which this honours.
    pub command_timeout: Duration,

    /// How long a session may stay in `IDLE` before the server ends it.
    ///
    /// RFC 2177 requires a client to re-issue `IDLE` at least every 29 minutes
    /// and permits the server to end an idle session that does not. The default
    /// sits just inside that window, so a conforming client is never cut off
    /// and an abandoned connection does not idle forever holding a subscription.
    pub idle_timeout: Duration,

    /// Authentication posture.
    pub auth_mode: AuthMode,

    /// Whether `STARTTLS` is offered on the cleartext listener.
    ///
    /// A plain `bool` rather than [`TlsMode`]: implicit TLS is expressed by
    /// `implicit_tls_bind`, so the only remaining question for the cleartext
    /// port is whether it advertises an upgrade. Reusing `TlsMode` here would
    /// admit `TlsMode::Implicit` on the cleartext listener, which is a
    /// contradiction the type should not be able to express.
    pub start_tls: bool,
}

impl Default for ImapConfig {
    fn default() -> Self {
        Self {
            bind: Some(SocketAddr::new(DEFAULT_BIND, DEFAULT_IMAP_PORT)),
            // Off by default: enabling it would generate a certificate and open
            // a second port on first run, neither of which a developer asked
            // for. Opt in with `SANDESH_IMAP_TLS_ADDR`.
            implicit_tls_bind: None,
            hostname: String::from("sandesh"),
            // 64 KiB, matching the SMTP limit for the same reason: far above
            // any legitimate command, low enough to bound a silent client.
            max_line_length: 64 * 1024,
            // 25 MiB, matching `SmtpConfig::max_message_size` — a literal
            // carries a message, so a limit below it would refuse mail that
            // SMTP accepted (relevant once APPEND lands).
            max_literal_size: 25 * 1024 * 1024,
            max_connections: 128,
            // 30 minutes: the RFC 9051 §5.4 minimum for an authenticated
            // session. Shorter would break clients that legitimately idle.
            command_timeout: Duration::from_mins(30),
            // 29 minutes: the RFC 2177 §3 ceiling. A conforming client renews
            // before this, so only an abandoned IDLE reaches it.
            idle_timeout: Duration::from_mins(29),
            auth_mode: AuthMode::Disabled,
            start_tls: false,
        }
    }
}

impl ImapConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] naming the field and why, so a bad value is
    /// actionable at startup rather than at first connection.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.bind.is_none() && self.implicit_tls_bind.is_none() {
            return Err(ConfigError::Invalid {
                field: "bind",
                reason: String::from(
                    "IMAP is enabled with neither a cleartext nor an implicit-TLS \
                     address: set one, or disable IMAP",
                ),
            });
        }
        if self.hostname.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "hostname",
                reason: String::from("must not be empty; it appears in the IMAP greeting"),
            });
        }
        // Below this a client could not send the longest fixed-form command.
        if self.max_line_length < 512 {
            return Err(ConfigError::Invalid {
                field: "max_line_length",
                reason: String::from("must be at least 512 to accept valid commands"),
            });
        }
        if self.max_literal_size == 0 {
            return Err(ConfigError::Invalid {
                field: "max_literal_size",
                reason: String::from("must be greater than zero or no literal could be sent"),
            });
        }
        if self.max_connections == 0 {
            return Err(ConfigError::Invalid {
                field: "max_connections",
                reason: String::from("must be at least 1 or no client could connect"),
            });
        }
        if self.command_timeout.is_zero() {
            return Err(ConfigError::Invalid {
                field: "command_timeout",
                reason: String::from("must be non-zero"),
            });
        }
        // A zero idle timeout would end every IDLE the instant it began, which
        // looks like a server that does not support IDLE at all.
        if self.idle_timeout.is_zero() {
            return Err(ConfigError::Invalid {
                field: "idle_timeout",
                reason: String::from("must be non-zero"),
            });
        }
        // `STARTTLS` with no cleartext listener is not dangerous, but it is
        // certainly a mistake: the flag can have no effect.
        if self.start_tls && self.bind.is_none() {
            return Err(ConfigError::Invalid {
                field: "start_tls",
                reason: String::from(
                    "STARTTLS is enabled but there is no cleartext listener to \
                     upgrade: set an IMAP address, or use implicit TLS instead",
                ),
            });
        }
        Ok(())
    }

    /// Whether TLS material is needed to start.
    ///
    /// Either posture requires a certificate, and the composition root uses
    /// this to decide whether to load one before any listener binds.
    #[must_use]
    pub const fn needs_tls(&self) -> bool {
        self.start_tls || self.implicit_tls_bind.is_some()
    }

    /// Whether either listener is reachable from outside this machine.
    #[must_use]
    pub fn is_externally_bound(&self) -> bool {
        [self.bind, self.implicit_tls_bind]
            .into_iter()
            .flatten()
            .any(|addr| !addr.ip().is_loopback())
    }

    /// Warnings the operator should see at startup.
    ///
    /// Returned rather than logged so this crate stays free of a logging
    /// dependency and the caller decides how to present them.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.is_externally_bound() && !self.auth_mode.is_enabled() {
            warnings.push(String::from(
                "IMAP is bound to a non-loopback address with authentication \
                 disabled: any host that can reach this address can read mail \
                 from Sandesh. Bind to 127.0.0.1 unless you intend this.",
            ));
        }

        // The case that silently defeats authentication: PLAIN and LOGIN are
        // only ever offered over TLS (ADR-033), so with no TLS on a cleartext
        // listener an `auth_mode` of `required` locks every client out rather
        // than securing anything.
        if self.auth_mode.is_enabled() && self.bind.is_some() && !self.start_tls {
            warnings.push(String::from(
                "IMAP authentication is enabled on the cleartext listener but \
                 STARTTLS is not: credentials are never accepted in the clear, \
                 so clients on that port cannot authenticate. Enable STARTTLS \
                 or use the implicit-TLS listener.",
            ));
        }

        warnings
    }
}

/// Default POP3 port.
///
/// 1110 mirrors 110: the privileged default (110) would need elevation, and
/// 1110 is the established local-development convention (like 1025 for SMTP
/// and 1143 for IMAP).
pub const DEFAULT_POP3_PORT: u16 = 1110;

/// Default implicit-TLS POP3 port.
///
/// 1995 stands to 995 (POP3S) as 1110 stands to 110.
pub const DEFAULT_POP3S_PORT: u16 = 1995;

/// POP3 server configuration (RFC 1939, RFC 2449, RFC 2595, RFC 5034).
///
/// Same posture as [`SmtpConfig`] and [`ImapConfig`]: loopback by default, and every
/// limit present to bound what an untrusted client can make the server allocate.
#[derive(Debug, Clone)]
pub struct Pop3Config {
    /// Cleartext listener address, or `None` to serve no cleartext port.
    pub bind: Option<SocketAddr>,

    /// Implicit-TLS listener address, or `None` to serve no TLS port.
    pub implicit_tls_bind: Option<SocketAddr>,

    /// Hostname announced in the greeting.
    pub hostname: String,

    /// Longest single command line, in bytes.
    pub max_line_length: usize,

    /// Largest number of simultaneous connections.
    pub max_connections: usize,

    /// How long to wait for a command before closing the connection.
    pub command_timeout: Duration,

    /// Authentication posture.
    pub auth_mode: AuthMode,

    /// Whether `STLS` is offered on the cleartext listener.
    pub start_tls: bool,
}

impl Default for Pop3Config {
    fn default() -> Self {
        Self {
            bind: Some(SocketAddr::new(DEFAULT_BIND, DEFAULT_POP3_PORT)),
            implicit_tls_bind: None,
            hostname: String::from("sandesh"),
            max_line_length: 64 * 1024,
            max_connections: 128,
            command_timeout: Duration::from_mins(10),
            auth_mode: AuthMode::Disabled,
            start_tls: false,
        }
    }
}

impl Pop3Config {
    /// Validates the configuration.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] naming the field and why.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.bind.is_none() && self.implicit_tls_bind.is_none() {
            return Err(ConfigError::Invalid {
                field: "bind",
                reason: String::from(
                    "POP3 is enabled with neither a cleartext nor an implicit-TLS \
                     address: set one, or disable POP3",
                ),
            });
        }
        if self.hostname.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "hostname",
                reason: String::from("must not be empty; it appears in the POP3 greeting"),
            });
        }
        if self.max_line_length < 512 {
            return Err(ConfigError::Invalid {
                field: "max_line_length",
                reason: String::from("must be at least 512 to accept valid commands"),
            });
        }
        if self.max_connections == 0 {
            return Err(ConfigError::Invalid {
                field: "max_connections",
                reason: String::from("must be at least 1 or no client could connect"),
            });
        }
        if self.command_timeout.is_zero() {
            return Err(ConfigError::Invalid {
                field: "command_timeout",
                reason: String::from("must be non-zero"),
            });
        }
        if self.start_tls && self.bind.is_none() {
            return Err(ConfigError::Invalid {
                field: "start_tls",
                reason: String::from(
                    "STLS is enabled but there is no cleartext listener to \
                     upgrade: set a POP3 address, or use implicit TLS instead",
                ),
            });
        }
        Ok(())
    }

    /// Whether TLS material is needed to start.
    #[must_use]
    pub const fn needs_tls(&self) -> bool {
        self.start_tls || self.implicit_tls_bind.is_some()
    }

    /// Whether either listener is reachable from outside this machine.
    #[must_use]
    pub fn is_externally_bound(&self) -> bool {
        [self.bind, self.implicit_tls_bind]
            .into_iter()
            .flatten()
            .any(|addr| !addr.ip().is_loopback())
    }

    /// Warnings the operator should see at startup.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.is_externally_bound() && !self.auth_mode.is_enabled() {
            warnings.push(String::from(
                "POP3 is bound to a non-loopback address with authentication \
                 disabled: any host that can reach this address can retrieve and delete mail \
                 from Sandesh. Bind to 127.0.0.1 unless you intend this.",
            ));
        }

        if self.auth_mode.is_enabled() && self.bind.is_some() && !self.start_tls {
            warnings.push(String::from(
                "POP3 authentication is enabled on the cleartext listener but \
                 STLS is not: credentials are never accepted in the clear, \
                 so clients on that port cannot authenticate. Enable STLS \
                 or use the implicit-TLS listener.",
            ));
        }

        warnings
    }
}

/// Default HTTP port.
///
/// 8080 is the conventional local-development HTTP port and needs no
/// privileges. `README.md` documents it as where the API and future UI live.
pub const DEFAULT_HTTP_PORT: u16 = 8080;

/// How the HTTP API authenticates callers.
///
/// Separate from [`AuthMode`], which governs SMTP only. The two were long
/// conflated in reports of "auth is off": `SANDESH_SMTP_AUTH` never had any
/// effect on the API, which was unauthenticated regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApiAuthMode {
    /// No authentication. Any caller may read and mutate mail.
    ///
    /// The default, and deliberately so (ADR-005): Sandesh binds to loopback,
    /// and requiring a credential before `curl` can read a test message would
    /// add friction with no security benefit there. It stops being acceptable
    /// the moment the listener is bound off loopback, which is why
    /// [`HttpConfig::validate`] refuses that combination.
    #[default]
    Disabled,
    /// Every request must carry a credential, except the exempt paths.
    Required,
}

impl ApiAuthMode {
    /// Whether a credential is demanded.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Required)
    }

    /// The stable wire word, as reported by `/info`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Required => "required",
        }
    }
}

/// A bearer token, held so it cannot be printed by accident.
///
/// `sandesh-config` is reached by every layer and must stay dependency-free
/// (`tests/architecture.rs::config_is_dependency_free`), so this is a local
/// type rather than `sandesh_proto::Secret`. It exists for one reason: the
/// surrounding config derives `Debug`, and a `String` token would be printed
/// by any `{:?}` of the configuration — into logs, into a panic message, into
/// a bug report.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken(String);

impl ApiToken {
    /// Wraps a token value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reads the token.
    ///
    /// Named to be conspicuous, matching `sandesh_proto::Secret::expose`: every
    /// call site is somewhere a credential is handled in the clear.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the token is empty, which is never a usable credential.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ApiToken {
    /// Renders a placeholder. There is no way to print the value by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiToken(***)")
    }
}

/// HTTP/WebSocket server configuration.
///
/// Same posture as [`SmtpConfig`]: loopback by default, every limit present to
/// bound what an untrusted client can make the server allocate.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Address to bind. Defaults to loopback.
    pub bind: SocketAddr,

    /// Largest accepted request body, in bytes.
    ///
    /// An unbounded body is a memory-exhaustion vector reachable with a single
    /// request, so there is a cap even though few endpoints accept bodies.
    pub max_body_bytes: usize,

    /// Per-connection WebSocket event queue length.
    ///
    /// When a client falls this far behind, the oldest events are dropped and
    /// it is told how many it missed (`EVENT_MODEL.md` §6). Bounded so a
    /// stalled browser tab cannot grow memory without limit — or block SMTP.
    pub websocket_queue: usize,

    /// Largest simultaneous WebSocket connections.
    pub max_websocket_connections: usize,

    /// Origins permitted by CORS.
    ///
    /// Empty by default, which disables CORS entirely. The Phase 4 UI is served
    /// from this same origin and needs none; a separate dev server is the only
    /// real use, and it is named explicitly. There is deliberately no wildcard:
    /// `Allow-Origin: *` on a local mail server would let any page a developer
    /// visits read their mail.
    pub cors_origins: Vec<String>,

    /// Whether the API demands a credential.
    pub auth_mode: ApiAuthMode,

    /// The accepted bearer token, when one is configured.
    ///
    /// `None` with `auth_mode = Required` still authenticates: basic auth
    /// against the accounts table remains available, so an operator may choose
    /// per-user credentials and no shared token.
    pub auth_token: Option<ApiToken>,

    /// Whether to serve HTTPS rather than HTTP.
    ///
    /// Uses the same certificate as the mail listeners (ADR-034), so enabling
    /// it needs no second identity and no extra configuration.
    pub tls_enabled: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(DEFAULT_BIND, DEFAULT_HTTP_PORT),
            // 32 MiB: comfortably above the 25 MiB message limit, so injecting
            // the largest acceptable message is not rejected by the body cap.
            max_body_bytes: 32 * 1024 * 1024,
            websocket_queue: 1000,
            max_websocket_connections: 256,
            cors_origins: Vec::new(),
            // Off by default (ADR-005). See `ApiAuthMode::Disabled`.
            auth_mode: ApiAuthMode::default(),
            auth_token: None,
            tls_enabled: false,
        }
    }
}

impl HttpConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a limit cannot work.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_body_bytes == 0 {
            return Err(ConfigError::Invalid {
                field: "max_body_bytes",
                reason: String::from("must be greater than zero or no request could be accepted"),
            });
        }
        if self.websocket_queue == 0 {
            return Err(ConfigError::Invalid {
                field: "websocket_queue",
                reason: String::from(
                    "must be at least 1; a zero-length queue would drop every event",
                ),
            });
        }
        if self.max_websocket_connections == 0 {
            return Err(ConfigError::Invalid {
                field: "max_websocket_connections",
                reason: String::from("must be at least 1 or no client could subscribe"),
            });
        }
        if self.cors_origins.iter().any(|origin| origin == "*") {
            return Err(ConfigError::Invalid {
                field: "cors_origins",
                reason: String::from(
                    "'*' is not permitted: it would let any page a developer visits read \
                     their mail. List the development origin explicitly.",
                ),
            });
        }

        // Auth demanded but no way to satisfy it would lock the operator out of
        // their own API with a 401 they cannot resolve. Basic auth against the
        // accounts table is always available, so this only rejects an empty
        // token — a value that looks configured and authenticates nobody.
        if let Some(token) = &self.auth_token
            && token.is_empty()
        {
            return Err(ConfigError::Invalid {
                field: "auth_token",
                reason: String::from(
                    "an empty token is not a credential; unset it to use basic auth alone",
                ),
            });
        }
        Ok(())
    }

    /// Whether the listener is reachable from outside this machine.
    #[must_use]
    pub fn is_externally_bound(&self) -> bool {
        !self.bind.ip().is_loopback()
    }

    /// Warnings the operator should see at startup.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.is_externally_bound() && !self.auth_mode.is_enabled() {
            warnings.push(format!(
                "HTTP is bound to {}: the API exposes every message Sandesh has \
                 received, without authentication. Set SANDESH_API_AUTH=required \
                 or bind to 127.0.0.1.",
                self.bind
            ));
        }
        if self.is_externally_bound() && !self.tls_enabled {
            warnings.push(format!(
                "HTTP is bound to {} without TLS: credentials and mail contents \
                 cross the network in the clear. Set SANDESH_API_TLS=true.",
                self.bind
            ));
        }
        warnings
    }
}

/// The whole application's configuration.
///
/// One struct owned by `sandesh-config` rather than a config type per crate:
/// duplicated configuration is how two subsystems end up disagreeing about the
/// same setting.
// The four `*_enabled` switches are deliberately independent: each listener
// must be able to run with any of the others disabled, so collapsing them into
// an enum or bitflag would encode a coupling the design explicitly rejects.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// SMTP listener settings.
    pub smtp: SmtpConfig,
    /// IMAP listener settings.
    pub imap: ImapConfig,
    /// POP3 listener settings.
    pub pop3: Pop3Config,
    /// HTTP and WebSocket settings.
    pub http: HttpConfig,
    /// Where the `SQLite` database lives.
    pub database_path: std::path::PathBuf,
    /// Where out-of-line message blobs live.
    pub blob_path: std::path::PathBuf,
    /// The `tracing` filter directive, e.g. `info` or `sandesh_smtp=debug`.
    pub log_level: String,
    /// Whether the SMTP listener runs.
    pub smtp_enabled: bool,
    /// Whether the IMAP listener runs.
    pub imap_enabled: bool,
    /// Whether the POP3 listener runs.
    pub pop3_enabled: bool,
    /// Whether the HTTP listener runs.
    ///
    /// Independent switches: SMTP must keep working with the API disabled, and
    /// the reverse. Neither subsystem may depend on the other being up.
    pub http_enabled: bool,
    /// Whether per-connection protocol transcripts are recorded (ADR-066).
    ///
    /// On by default: traces are the point of the Diagnostics view, and a
    /// feature nobody discovers is a feature nobody has. This is not the
    /// auth-disabled-by-default case (ADR-005) — that default governs who may
    /// *reach* the server, while a transcript records only the commands the
    /// server already handled. Message content and credentials never enter one.
    pub tracing_enabled: bool,
    /// How many connection records to keep before the oldest are evicted.
    ///
    /// Transcripts are produced more often than messages and nothing else
    /// bounds them, so they get a cap in the same spirit as the 5000-message
    /// default (`STORAGE_DESIGN.md` §5).
    pub trace_retention: u32,
}

/// The default number of connection records kept.
pub const DEFAULT_TRACE_RETENTION: u32 = 1000;

/// Default database filename, relative to the working directory.
pub const DEFAULT_DATABASE_PATH: &str = "sandesh.db";

/// Default blob directory.
pub const DEFAULT_BLOB_PATH: &str = "sandesh-blobs";

/// The default is the local-development configuration.
///
/// Deliberately not a derived `Default`: a derived one would produce empty
/// paths and disabled listeners, which `validate` rejects — a "default" that
/// cannot start is a trap.
impl Default for AppConfig {
    fn default() -> Self {
        Self::local()
    }
}

impl AppConfig {
    /// The default local-development configuration.
    ///
    /// Everything on, everything on loopback, database in the working
    /// directory — what `cargo run` should give a developer with no arguments.
    #[must_use]
    pub fn local() -> Self {
        // Cleartext AUTH is allowed only in this local profile: a developer on
        // loopback with no TLS must still be able to exercise authentication.
        let smtp = SmtpConfig {
            allow_cleartext_auth: true,
            ..SmtpConfig::default()
        };
        Self {
            smtp,
            imap: ImapConfig::default(),
            pop3: Pop3Config::default(),
            http: HttpConfig::default(),
            database_path: std::path::PathBuf::from(DEFAULT_DATABASE_PATH),
            blob_path: std::path::PathBuf::from(DEFAULT_BLOB_PATH),
            log_level: String::from("info"),
            tracing_enabled: true,
            trace_retention: DEFAULT_TRACE_RETENTION,
            smtp_enabled: true,
            imap_enabled: true,
            pop3_enabled: true,
            http_enabled: true,
        }
    }

    /// Validates every subsystem's configuration.
    ///
    /// # Errors
    /// Returns the first [`ConfigError`] found. Startup calls this before
    /// opening anything, so an unusable configuration fails cleanly rather than
    /// half-initialising the application.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.smtp_enabled {
            self.smtp.validate()?;
        }
        if self.imap_enabled {
            self.imap.validate()?;
        }
        if self.pop3_enabled {
            self.pop3.validate()?;
        }
        if self.http_enabled {
            self.http.validate()?;
        }
        if self.database_path.as_os_str().is_empty() {
            return Err(ConfigError::Invalid {
                field: "database_path",
                reason: String::from("must not be empty"),
            });
        }
        if self.log_level.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "log_level",
                reason: String::from("must not be empty"),
            });
        }
        Ok(())
    }

    /// Every warning the operator should see at startup.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.smtp_enabled {
            warnings.extend(self.smtp.warnings());
        }
        if self.imap_enabled {
            warnings.extend(self.imap.warnings());
        }
        if self.pop3_enabled {
            warnings.extend(self.pop3.warnings());
        }
        if self.http_enabled {
            warnings.extend(self.http.warnings());
        }
        warnings
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod app_config_tests {
    use super::*;

    #[test]
    fn http_defaults_bind_to_loopback() {
        // The API exposes every message received; it must not be reachable by
        // default any more than SMTP is.
        let config = HttpConfig::default();
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.bind.port(), DEFAULT_HTTP_PORT);
        assert!(config.warnings().is_empty());
    }

    #[test]
    fn http_defaults_are_valid() {
        assert!(HttpConfig::default().validate().is_ok());
    }

    #[test]
    fn cors_wildcard_is_refused() {
        // Not merely discouraged: a wildcard here lets any page a developer
        // visits read their mail.
        let config = HttpConfig {
            cors_origins: vec![String::from("*")],
            ..HttpConfig::default()
        };
        let error = config.validate().expect_err("must be rejected");
        assert!(error.to_string().contains("cors_origins"));
    }

    #[test]
    fn a_named_cors_origin_is_accepted() {
        let config = HttpConfig {
            cors_origins: vec![String::from("http://localhost:5173")],
            ..HttpConfig::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn external_http_binding_warns() {
        let config = HttpConfig {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_HTTP_PORT),
            ..HttpConfig::default()
        };
        assert!(config.is_externally_bound());
        assert!(
            config
                .warnings()
                .iter()
                .any(|w| w.contains("without authentication") || w.contains("every message")),
            "an exposed API must be called out"
        );
    }

    #[test]
    fn http_validation_rejects_unusable_limits() {
        for config in [
            HttpConfig {
                max_body_bytes: 0,
                ..Default::default()
            },
            HttpConfig {
                websocket_queue: 0,
                ..Default::default()
            },
            HttpConfig {
                max_websocket_connections: 0,
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn the_local_default_is_valid_and_quiet() {
        // `cargo run` with no arguments must work and warn about nothing.
        let config = AppConfig::local();
        assert!(config.validate().is_ok());
        assert!(config.warnings().is_empty());
        assert!(config.smtp_enabled && config.http_enabled);
    }

    #[test]
    fn subsystems_can_be_disabled_independently() {
        // SMTP must work with the API off, and the reverse.
        let smtp_only = AppConfig {
            http_enabled: false,
            ..AppConfig::local()
        };
        assert!(smtp_only.validate().is_ok());

        let api_only = AppConfig {
            smtp_enabled: false,
            ..AppConfig::local()
        };
        assert!(api_only.validate().is_ok());
    }

    #[test]
    fn a_disabled_subsystems_configuration_is_not_validated() {
        // An unusable setting for a subsystem that will not run must not block
        // startup of the ones that will.
        let config = AppConfig {
            http_enabled: false,
            http: HttpConfig {
                max_body_bytes: 0,
                ..HttpConfig::default()
            },
            ..AppConfig::local()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn an_empty_database_path_is_rejected() {
        let config = AppConfig {
            database_path: std::path::PathBuf::new(),
            ..AppConfig::local()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn pop3_defaults_bind_to_loopback() {
        let config = Pop3Config::default();
        let bind = config.bind.expect("default pop3 bind is Some");
        assert!(bind.ip().is_loopback());
        assert_eq!(bind.port(), DEFAULT_POP3_PORT);
        assert!(config.warnings().is_empty());
    }

    #[test]
    fn pop3_defaults_are_valid() {
        assert!(Pop3Config::default().validate().is_ok());
    }

    #[test]
    fn external_pop3_binding_warns() {
        let config = Pop3Config {
            bind: Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                DEFAULT_POP3_PORT,
            )),
            ..Pop3Config::default()
        };
        assert!(config.is_externally_bound());
        assert!(
            config
                .warnings()
                .iter()
                .any(|w| w.contains("without authentication") || w.contains("retrieve and delete")),
            "an exposed POP3 listener must be warned"
        );
    }

    #[test]
    fn pop3_validation_rejects_unusable_limits() {
        assert!(
            Pop3Config {
                bind: None,
                implicit_tls_bind: None,
                ..Pop3Config::default()
            }
            .validate()
            .is_err()
        );

        assert!(
            Pop3Config {
                max_line_length: 10,
                ..Pop3Config::default()
            }
            .validate()
            .is_err()
        );

        assert!(
            Pop3Config {
                max_connections: 0,
                ..Pop3Config::default()
            }
            .validate()
            .is_err()
        );

        assert!(
            Pop3Config {
                command_timeout: Duration::ZERO,
                ..Pop3Config::default()
            }
            .validate()
            .is_err()
        );
    }
}

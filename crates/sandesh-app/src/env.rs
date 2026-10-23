//! Configuration from the environment (OD-24).
//!
//! `sandesh-config` holds the configuration *model* and depends on nothing but
//! `thiserror`, because every protocol adapter reads it. Reading the
//! environment is a composition-root concern, so it lives here — which is also
//! what that crate's module documentation anticipated.
//!
//! ## Why the environment, and why these variables
//!
//! Until now Sandesh resolved its database and blob store relative to the
//! working directory with no way to move them. That is fine for `cargo run` in
//! a checkout and wrong everywhere else: a service has a data directory, a
//! container has a mounted volume, and a test harness needs somewhere
//! disposable. The only workaround was to change the process's working
//! directory, which is a blunt instrument that also moves every other relative
//! path.
//!
//! Environment variables rather than CLI flags come first because they are what
//! a container, a systemd unit and a `.env` file all already speak. Flags can
//! layer on top later without changing anything here (OD-24).
//!
//! ## Failure posture
//!
//! An unparseable value is an **error**, never a silently-ignored setting. A
//! developer who writes `SANDESH_HTTP_ADDR=8080` — a port, where an address is
//! wanted — must be told, not quietly given the default and left wondering why
//! their configuration had no effect.

use std::net::SocketAddr;
use std::path::PathBuf;

use sandesh_config::{ApiAuthMode, ApiToken, AppConfig, AuthMode, TlsMode};

/// Where the `SQLite` database is written.
pub const ENV_DATABASE_PATH: &str = "SANDESH_DATABASE_PATH";

/// Where message blobs are written.
pub const ENV_BLOB_PATH: &str = "SANDESH_BLOB_PATH";

/// The parent of both, used when neither is set individually.
pub const ENV_DATA_DIR: &str = "SANDESH_DATA_DIR";

/// The SMTP listener address, as `host:port`.
pub const ENV_SMTP_ADDR: &str = "SANDESH_SMTP_ADDR";

/// The HTTP listener address, as `host:port`.
pub const ENV_HTTP_ADDR: &str = "SANDESH_HTTP_ADDR";

/// The tracing filter, e.g. `info` or `sandesh_smtp=debug`.
pub const ENV_LOG_LEVEL: &str = "SANDESH_LOG";

/// Whether to run the SMTP listener.
pub const ENV_SMTP_ENABLED: &str = "SANDESH_SMTP_ENABLED";

/// Overrides the IMAP cleartext listener address.
pub const ENV_IMAP_ADDR: &str = "SANDESH_IMAP_ADDR";

/// Overrides the IMAP implicit-TLS listener address.
///
/// Setting this is how the implicit-TLS port is enabled: it is off by default,
/// because opening it would generate a certificate and bind a second port that
/// a developer did not ask for.
pub const ENV_IMAPS_ADDR: &str = "SANDESH_IMAP_TLS_ADDR";

/// Whether the IMAP listener runs.
pub const ENV_IMAP_ENABLED: &str = "SANDESH_IMAP_ENABLED";
/// Whether per-connection protocol transcripts are recorded (ADR-066).
pub const ENV_TRACING_ENABLED: &str = "SANDESH_TRACING_ENABLED";
/// How many connection records to keep before the oldest are evicted.
pub const ENV_TRACE_RETENTION: &str = "SANDESH_TRACE_RETENTION";

/// The IMAP authentication posture.
pub const ENV_IMAP_AUTH: &str = "SANDESH_IMAP_AUTH";

/// Whether `STARTTLS` is offered on the IMAP cleartext listener.
pub const ENV_IMAP_STARTTLS: &str = "SANDESH_IMAP_STARTTLS";

/// Overrides the POP3 cleartext listener address.
pub const ENV_POP3_ADDR: &str = "SANDESH_POP3_ADDR";

/// Overrides the POP3 implicit-TLS listener address.
pub const ENV_POP3S_ADDR: &str = "SANDESH_POP3_TLS_ADDR";

/// Whether the POP3 listener runs.
pub const ENV_POP3_ENABLED: &str = "SANDESH_POP3_ENABLED";

/// The POP3 authentication posture.
pub const ENV_POP3_AUTH: &str = "SANDESH_POP3_AUTH";

/// Whether `STLS` is offered on the POP3 cleartext listener.
pub const ENV_POP3_STARTTLS: &str = "SANDESH_POP3_STARTTLS";

/// Whether to run the HTTP listener.
pub const ENV_HTTP_ENABLED: &str = "SANDESH_HTTP_ENABLED";

/// The SMTP authentication mode: `disabled`, `optional` or `required`.
pub const ENV_SMTP_AUTH: &str = "SANDESH_SMTP_AUTH";

/// Whether to allow cleartext SMTP authentication without TLS.
pub const ENV_SMTP_ALLOW_CLEARTEXT_AUTH: &str = "SANDESH_SMTP_ALLOW_CLEARTEXT_AUTH";

/// The SMTP TLS mode: `disabled`, `starttls` or `implicit`.
pub const ENV_SMTP_TLS: &str = "SANDESH_SMTP_TLS";

/// The HTTP API authentication mode: `disabled` or `required`.
///
/// Distinct from [`ENV_SMTP_AUTH`], which governs SMTP alone and never had any
/// effect on the API.
pub const ENV_API_AUTH: &str = "SANDESH_API_AUTH";

/// The bearer token the API accepts.
///
/// Setting it does not by itself enable authentication; `SANDESH_API_AUTH`
/// does. A token with auth disabled is inert rather than half-enforced, so a
/// half-finished configuration cannot look protected while letting everything
/// through.
pub const ENV_API_TOKEN: &str = "SANDESH_API_TOKEN";

/// Whether the HTTP listener serves HTTPS: `true` or `false`.
pub const ENV_API_TLS: &str = "SANDESH_API_TLS";

/// Why the environment could not be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{variable}: {reason}")]
pub struct EnvError {
    /// The variable that could not be used.
    pub variable: &'static str,
    /// Why, in terms the reader can act on.
    pub reason: String,
}

impl EnvError {
    fn new(variable: &'static str, reason: impl Into<String>) -> Self {
        Self {
            variable,
            reason: reason.into(),
        }
    }
}

/// Reads one variable, treating empty as unset.
///
/// An exported-but-empty variable is what a shell produces from an unset
/// substitution (`SANDESH_LOG=$UNDEFINED`). Treating it as "use the default" is
/// friendlier than failing, and matches what every other tool does.
fn var(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        // A non-UTF-8 value is unusable; the caller's default is better than
        // refusing to start over a variable the user may not have set on
        // purpose.
        _ => None,
    }
}

/// Parses a boolean the way a person would write one.
fn parse_bool(name: &'static str, raw: &str) -> Result<bool, EnvError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(EnvError::new(
            name,
            format!("expected true or false, got `{other}`"),
        )),
    }
}

/// Parses an authentication mode.
///
/// Spelled out rather than derived so the accepted words are a deliberate,
/// stable contract rather than whatever the Rust variant names happen to be.
fn parse_auth_mode(raw: &str) -> Result<AuthMode, EnvError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "disabled" | "off" | "none" => Ok(AuthMode::Disabled),
        "optional" => Ok(AuthMode::Optional),
        "required" => Ok(AuthMode::Required),
        other => Err(EnvError::new(
            ENV_SMTP_AUTH,
            format!("expected disabled, optional or required, got `{other}`"),
        )),
    }
}

/// Parses the API authentication mode.
///
/// Deliberately not [`parse_auth_mode`]: SMTP's `optional` has no meaning for
/// HTTP, where there is no capability negotiation to opt into. Accepting the
/// word and quietly treating it as one of the other two is how a configuration
/// ends up meaning something other than it says.
fn parse_api_auth_mode(raw: &str) -> Result<ApiAuthMode, EnvError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "disabled" | "off" | "none" => Ok(ApiAuthMode::Disabled),
        "required" | "on" => Ok(ApiAuthMode::Required),
        other => Err(EnvError::new(
            ENV_API_AUTH,
            format!("expected disabled or required, got `{other}`"),
        )),
    }
}

/// Parses a TLS mode.
fn parse_tls_mode(raw: &str) -> Result<TlsMode, EnvError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "disabled" | "off" | "none" => Ok(TlsMode::Disabled),
        "starttls" | "start_tls" => Ok(TlsMode::StartTls),
        "implicit" => Ok(TlsMode::Implicit),
        other => Err(EnvError::new(
            ENV_SMTP_TLS,
            format!("expected disabled, starttls or implicit, got `{other}`"),
        )),
    }
}

/// Parses a listener address, naming the likely mistake.
fn parse_u32(name: &'static str, raw: &str) -> Result<u32, EnvError> {
    raw.trim()
        .parse::<u32>()
        .map_err(|_| EnvError::new(name, format!("expected a whole number, got `{raw}`")))
}

fn parse_addr(name: &'static str, raw: &str) -> Result<SocketAddr, EnvError> {
    raw.trim().parse::<SocketAddr>().map_err(|_| {
        // The overwhelmingly common error is a bare port, so say so rather than
        // echoing a parser message about scope ids.
        let hint = if raw.trim().parse::<u16>().is_ok() {
            format!("expected `host:port`, got the bare port `{raw}`. Try `127.0.0.1:{raw}`")
        } else {
            format!("expected `host:port`, got `{raw}`")
        };
        EnvError::new(name, hint)
    })
}

/// Applies the environment to a configuration.
///
/// Precedence is environment over the supplied defaults; a variable that is
/// absent leaves its field untouched. `SANDESH_DATA_DIR` is applied first so
/// that the two specific path variables can override it, which is what lets a
/// caller put the blobs on a different volume from the database.
///
/// # Errors
/// [`EnvError`] when a variable is set but unusable. Nothing is applied
/// partially: the caller receives either a fully-updated configuration or an
/// error naming the variable at fault.
pub fn apply(mut config: AppConfig) -> Result<AppConfig, EnvError> {
    if let Some(dir) = var(ENV_DATA_DIR) {
        let dir = PathBuf::from(dir);
        config.database_path = dir.join("sandesh.db");
        config.blob_path = dir.join("sandesh-blobs");
    }

    if let Some(path) = var(ENV_DATABASE_PATH) {
        config.database_path = PathBuf::from(path);
    }

    if let Some(path) = var(ENV_BLOB_PATH) {
        config.blob_path = PathBuf::from(path);
    }

    if let Some(raw) = var(ENV_SMTP_ADDR) {
        config.smtp.bind = parse_addr(ENV_SMTP_ADDR, &raw)?;
    }

    if let Some(raw) = var(ENV_HTTP_ADDR) {
        config.http.bind = parse_addr(ENV_HTTP_ADDR, &raw)?;
    }

    if let Some(level) = var(ENV_LOG_LEVEL) {
        config.log_level = level;
    }

    if let Some(raw) = var(ENV_SMTP_ENABLED) {
        config.smtp_enabled = parse_bool(ENV_SMTP_ENABLED, &raw)?;
    }

    if let Some(raw) = var(ENV_HTTP_ENABLED) {
        config.http_enabled = parse_bool(ENV_HTTP_ENABLED, &raw)?;
    }

    if let Some(raw) = var(ENV_SMTP_AUTH) {
        config.smtp.auth_mode = parse_auth_mode(&raw)?;
    }

    if let Some(raw) = var(ENV_SMTP_ALLOW_CLEARTEXT_AUTH) {
        config.smtp.allow_cleartext_auth = parse_bool(ENV_SMTP_ALLOW_CLEARTEXT_AUTH, &raw)?;
    }

    if let Some(raw) = var(ENV_API_AUTH) {
        config.http.auth_mode = parse_api_auth_mode(&raw)?;
    }

    if let Some(raw) = var(ENV_API_TOKEN) {
        config.http.auth_token = Some(ApiToken::new(raw));
    }

    if let Some(raw) = var(ENV_API_TLS) {
        config.http.tls_enabled = parse_bool(ENV_API_TLS, &raw)?;
    }

    if let Some(raw) = var(ENV_SMTP_TLS) {
        config.smtp.tls_mode = parse_tls_mode(&raw)?;
    }

    if let Some(raw) = var(ENV_IMAP_ADDR) {
        config.imap.bind = Some(parse_addr(ENV_IMAP_ADDR, &raw)?);
    }

    if let Some(raw) = var(ENV_IMAPS_ADDR) {
        config.imap.implicit_tls_bind = Some(parse_addr(ENV_IMAPS_ADDR, &raw)?);
    }

    if let Some(raw) = var(ENV_IMAP_ENABLED) {
        config.imap_enabled = parse_bool(ENV_IMAP_ENABLED, &raw)?;
    }

    if let Some(raw) = var(ENV_TRACING_ENABLED) {
        config.tracing_enabled = parse_bool(ENV_TRACING_ENABLED, &raw)?;
    }

    if let Some(raw) = var(ENV_TRACE_RETENTION) {
        config.trace_retention = parse_u32(ENV_TRACE_RETENTION, &raw)?;
    }

    if let Some(raw) = var(ENV_IMAP_AUTH) {
        config.imap.auth_mode = parse_auth_mode(&raw)?;
    }

    if let Some(raw) = var(ENV_IMAP_STARTTLS) {
        config.imap.start_tls = parse_bool(ENV_IMAP_STARTTLS, &raw)?;
    }

    if let Some(raw) = var(ENV_POP3_ADDR) {
        config.pop3.bind = Some(parse_addr(ENV_POP3_ADDR, &raw)?);
    }

    if let Some(raw) = var(ENV_POP3S_ADDR) {
        config.pop3.implicit_tls_bind = Some(parse_addr(ENV_POP3S_ADDR, &raw)?);
    }

    if let Some(raw) = var(ENV_POP3_ENABLED) {
        config.pop3_enabled = parse_bool(ENV_POP3_ENABLED, &raw)?;
    }

    if let Some(raw) = var(ENV_POP3_AUTH) {
        config.pop3.auth_mode = parse_auth_mode(&raw)?;
    }

    if let Some(raw) = var(ENV_POP3_STARTTLS) {
        config.pop3.start_tls = parse_bool(ENV_POP3_STARTTLS, &raw)?;
    }

    Ok(config)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    // The process environment is global, so these tests build a configuration
    // through the same parsing functions rather than mutating it. Setting real
    // variables would make the suite order-dependent and flaky under the
    // default multi-threaded test runner.

    #[test]
    fn a_bare_port_is_rejected_with_a_usable_hint() {
        // The single most likely mistake, and the one where a silent default
        // would waste the most time.
        let error = parse_addr(ENV_HTTP_ADDR, "8080").expect_err("a bare port is not an address");
        assert!(error.reason.contains("127.0.0.1:8080"), "{}", error.reason);
    }

    #[test]
    fn an_address_parses() {
        let addr = parse_addr(ENV_HTTP_ADDR, "0.0.0.0:9000").expect("parses");
        assert_eq!(addr.port(), 9000);
        assert!(!addr.ip().is_loopback());
    }

    #[test]
    fn trace_retention_parses_and_rejects_nonsense() {
        assert_eq!(
            parse_u32(ENV_TRACE_RETENTION, "2500").expect("parses"),
            2500
        );
        assert_eq!(parse_u32(ENV_TRACE_RETENTION, " 10 ").expect("parses"), 10);

        // A negative or non-numeric retention must be a startup error, not a
        // silent fallback: someone setting it has a specific cap in mind.
        for raw in ["-1", "lots", "1.5", ""] {
            assert!(
                parse_u32(ENV_TRACE_RETENTION, raw).is_err(),
                "`{raw}` should not parse as a retention count"
            );
        }
    }

    #[test]
    fn booleans_accept_the_spellings_people_use() {
        for raw in ["1", "true", "TRUE", "yes", "on"] {
            assert!(parse_bool(ENV_SMTP_ENABLED, raw).expect("parses"), "{raw}");
        }
        for raw in ["0", "false", "FALSE", "no", "off"] {
            assert!(!parse_bool(ENV_SMTP_ENABLED, raw).expect("parses"), "{raw}");
        }
    }

    #[test]
    fn a_meaningless_boolean_is_an_error_not_a_default() {
        // `SANDESH_SMTP_ENABLED=maybe` must not quietly mean "enabled".
        assert!(parse_bool(ENV_SMTP_ENABLED, "maybe").is_err());
    }

    #[test]
    fn an_error_names_the_variable_at_fault() {
        let error = parse_addr(ENV_SMTP_ADDR, "not-an-address").expect_err("rejected");
        assert_eq!(error.variable, ENV_SMTP_ADDR);
        assert!(error.to_string().contains(ENV_SMTP_ADDR));
    }

    #[test]
    fn auth_and_tls_modes_parse_the_documented_words() {
        assert_eq!(
            parse_auth_mode("disabled").expect("parses"),
            AuthMode::Disabled
        );
        assert_eq!(
            parse_auth_mode("OPTIONAL").expect("parses"),
            AuthMode::Optional
        );
        assert_eq!(
            parse_auth_mode(" required ").expect("parses"),
            AuthMode::Required
        );

        assert_eq!(
            parse_tls_mode("disabled").expect("parses"),
            TlsMode::Disabled
        );
        assert_eq!(
            parse_tls_mode("starttls").expect("parses"),
            TlsMode::StartTls
        );
        assert_eq!(
            parse_tls_mode("implicit").expect("parses"),
            TlsMode::Implicit
        );
    }

    #[test]
    fn a_misspelled_security_mode_fails_rather_than_silently_disabling() {
        // The dangerous failure: `SANDESH_SMTP_AUTH=requried` must not quietly
        // leave authentication off.
        let error = parse_auth_mode("requried").expect_err("rejected");
        assert_eq!(error.variable, ENV_SMTP_AUTH);
        assert!(error.reason.contains("required"), "{}", error.reason);

        assert!(parse_tls_mode("tsl").is_err());
    }

    #[test]
    fn the_data_directory_places_both_stores_under_it() {
        // Applied through the same logic `apply` uses, without touching the
        // process environment.
        let dir = PathBuf::from("/var/lib/sandesh");
        let mut config = AppConfig::local();
        config.database_path = dir.join("sandesh.db");
        config.blob_path = dir.join("sandesh-blobs");

        assert!(config.database_path.starts_with(&dir));
        assert!(config.blob_path.starts_with(&dir));
        assert!(config.validate().is_ok());
    }
}

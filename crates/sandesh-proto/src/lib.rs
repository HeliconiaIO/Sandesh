//! # Sandesh protocol scaffolding
//!
//! The transport-and-session substrate shared by Sandesh's line protocols.
//!
//! Because Sandesh implements SMTP, IMAP and POP3 itself (ADR-002), the parts
//! those three have in common are worth writing once: TCP acceptance,
//! connection limiting, line framing that survives arbitrary packet
//! boundaries, timeouts, and eventually SASL and the TLS upgrade.
//!
//! ## What belongs here
//!
//! Anything protocol-*agnostic*. [`Connection`], [`LineReader`], [`Listener`],
//! [`ProtocolError`].
//!
//! ## What does not
//!
//! Anything that names a protocol's vocabulary. There is no `MailFrom` here and
//! there never should be: an SMTP verb in this crate would make it useless to
//! IMAP, which is the entire reason it exists.
//!
//! ## Bytes, not strings
//!
//! Lines are `&[u8]`. Mail content is not necessarily UTF-8, and decoding it at
//! the framing layer would destroy the raw-fidelity guarantee the product rests
//! on. Callers decode only what they know to be text.

#![doc(html_root_url = "https://docs.rs/sandesh-proto")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]

mod connection;
mod credential;
mod error;
mod line;
mod listener;
mod recorder;
mod sasl;
mod secret;
mod tls;
mod transcript;

pub use connection::Connection;
pub use credential::{CredentialError, hash_password, verify_optional, verify_password};
pub use error::{ProtocolError, ProtocolResult};
pub use line::{CRLF, LineReader};
pub use listener::{Accepted, BindError, Listener};
pub use recorder::{ConnectionRecorder, OpenConnection};
pub use sasl::{
    Credentials, Mechanism, SaslError, decode_login_field, decode_plain, login_prompts,
};
pub use secret::Secret;
pub use tls::{CERTIFICATE_FILE, Identity, KEY_FILE, TlsError, paths as tls_paths};
pub use transcript::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, Direction, Transcript};

/// Re-exported so adapters share one cancellation type without each depending
/// on `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;

//! # POP3 server adapter for Sandesh
//!
//! Implements RFC 1939 (POP3), RFC 2449 (CAPA), RFC 2595 (STLS), and RFC 5034 (SASL AUTH).
//!
//! ## Architecture
//!
//! Following `docs/ARCHITECTURE.md` §12, `sandesh-pop3` is an inward-facing protocol
//! adapter. It depends on `sandesh-core`, `sandesh-proto`, and `sandesh-config`,
//! with no direct database (SQL) or HTTP dependencies.
//!
//! In accordance with **ADR-006**, Sandesh POP3 does not enforce an exclusive lock on
//! the user's mailbox: the snapshot model taken at authentication guarantees stable 1..N
//! message numbering and isolation for the duration of the POP3 session without locking out
//! concurrent IMAP clients or the Web UI.

#![doc(html_root_url = "https://docs.rs/sandesh-pop3")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]

pub mod command;
pub mod response;
pub mod server;
pub mod session;
pub mod state;

pub use command::{ParseError, Pop3Command};
pub use server::{BoundServer, Pop3Server, ServerError, ServerStatus};
pub use session::{Outcome, Pop3Message, Session};
pub use state::Pop3State;

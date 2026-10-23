//! # Sandesh SMTP
//!
//! The SMTP server adapter — the first protocol adapter, and the one that
//! proves the architecture: *mail core first, protocols are adapters*.
//!
//! ## What this crate is
//!
//! An **ingress-only** SMTP server. It accepts mail and hands it to the domain.
//! It is not an MTA, it has no queue, and it never relays: every accepted
//! message is stored locally, which is a product safety property rather than a
//! missing feature.
//!
//! ## Boundaries
//!
//! This crate depends on `sandesh-core` (the domain), `sandesh-proto`
//! (transport) and `sandesh-config`. It has **no database dependency** and
//! issues no SQL — everything it does with an accepted message goes through
//! [`DeliveryService`], which talks to the core's storage ports. The
//! architecture test enforces this mechanically.
//!
//! ## Layering
//!
//! ```text
//! TCP bytes  →  LineReader   (sandesh-proto: framing)
//!            →  command      (verbs and arguments)
//!            →  state        (sequencing)
//!            →  session      (dispatch)
//!            →  DeliveryService  (domain operations)
//! ```
//!
//! Each layer is separately testable, and the state machine never sees a byte
//! of TCP buffering. IMAP and POP3 will reuse the same shape.

#![doc(html_root_url = "https://docs.rs/sandesh-smtp")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]
#![allow(clippy::module_name_repetitions)]

mod address;
mod command;
mod data;
mod reply;
mod server;
mod session;
mod state;

pub use command::{BodyType, Command, MailCommand, ParseError, RcptCommand};
// `DeliveryService` moved to `sandesh-core` (ADR-071): its routing is
// `CORE_SPEC.md` §7, not SMTP. Re-exported here so existing callers and tests
// keep one import path, and because the SMTP session is still its main user.
pub use reply::{Code, Reply};
pub use sandesh_core::{DeliveryReceipt, DeliveryRequest, DeliveryService, RecipientRejection};
pub use server::{BoundServer, ServerError, ServerStatus, SmtpServer};
pub use session::Session;
pub use state::{State, Transition};

/// Command parsing, exposed for protocol-level tests.
pub mod parser {
    pub use crate::address::{ParsedPath, PathError, parse_path};
    pub use crate::command::parse;
}

/// `DATA` framing, exposed for protocol-level tests.
pub mod framing {
    pub use crate::data::{DataOutcome, collect, stuff};
}

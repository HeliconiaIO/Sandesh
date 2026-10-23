//! Domain services: the operations that orchestrate several ports.
//!
//! A port says *what can be stored*; a service says *what an operation means*.
//! `CORE_SPEC.md` §6 and §7 specify the second, and until this module existed
//! there was nowhere in the domain to put it — so each protocol adapter wrote
//! its own version against the raw ports, and the versions drifted apart
//! (ADR-068).
//!
//! Services hold their ports as `Arc<dyn …>` trait objects, which is what lets
//! a test drive one against in-memory doubles with no database present.
//!
//! What must *not* move in here is set out in `CORE_SPEC.md` §9: session state,
//! wire encodings and per-protocol response codes stay in the adapters. The
//! test of whether logic belongs here is whether both IMAP and HTTP would give
//! the same answer; if they must differ, it is protocol semantics and it stays
//! out.

mod account;
mod delivery;
mod mailbox;
mod message;

pub use account::{AccountService, NewAccount};
pub use delivery::{DeliveryReceipt, DeliveryRequest, DeliveryService, RecipientRejection};
pub use mailbox::MailboxService;
pub use message::{Discarded, MessageService};

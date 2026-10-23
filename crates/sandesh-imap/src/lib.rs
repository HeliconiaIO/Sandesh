//! # Sandesh `IMAP4rev2`
//!
//! The `IMAP4rev2` protocol adapter, implementing RFC 9051.
//!
//! ## Scope
//!
//! This crate is a *protocol adapter*: it speaks IMAP and delegates every
//! decision about stored mail to the ports `sandesh-core` defines. It contains
//! no SQL, no database driver and no HTTP, and `tests/architecture.rs` enforces
//! that mechanically.
//!
//! It also contains no second implementation of anything Phase 4.5 already
//! built. Authentication is `sandesh_proto::verify_optional` over
//! `sandesh_proto::Credentials`; TLS is `sandesh_proto::Identity`; framing,
//! connection limits and timeouts are `sandesh_proto::{Listener, Connection}`.
//!
//! ## Layering
//!
//! ```text
//!   parser   ── octets → Command                   (pure, synchronous, no I/O)
//!   sequence ── text → SequenceSet → MessageEntry   (pure)
//!   fetch    ── tokens → FetchRequest               (pure)
//!   store    ── tokens → StoreRequest → FlagUpdate  (pure)
//!   search   ── tokens → SearchQuery                (pure; the plan is core's)
//!   append   ── tokens → AppendRequest              (pure)
//!   move     ── SequenceMap → MovePlan              (pure)
//!   idle     ── DomainEvent → Update → response     (pure)
//!   state    ── (State, CommandKind) → Transition   (pure)
//!   session  ── drives them against a Connection and the storage ports
//! ```
//!
//! Keeping the parser and the state machine free of I/O is what makes the
//! boundary cases — malformed literals, commands in the wrong state — testable
//! without a socket, which is where the bulk of RFC risk lives.
//!
//! ## Deliberate omissions
//!
//! `LSUB` is absent — RFC 9051 removed it in favour of `LIST` with selection
//! options, and accepting it would mean advertising `IMAP4rev1` behaviour this
//! server does not implement. `IDLE` was absent for the same reason until it was
//! implemented (ADR-053): a command this crate cannot honour is not a variant of
//! [`CommandKind`], so it cannot be parsed and cannot be advertised.

#![doc(html_root_url = "https://docs.rs/sandesh-imap")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]

mod append;
mod command;
mod copy;
mod enable;
mod expunge;
mod fetch;
mod fetch_response;
mod idle;
mod mailbox;
mod move_entry;
mod parser;
mod response;
mod search;
mod sequence;
mod server;
mod session;
mod state;
mod store;
mod utf7;

pub use append::{AppendParseError, AppendRequest};
pub use command::{Command, CommandKind, Tag, Token};
pub use copy::{CopiedUid, CopyResult, render_uid_set};
pub use expunge::{Doomed, ExpungePlan};
pub use fetch::{FetchItem, FetchParseError, FetchRequest, Partial, Section, SectionKind};
pub use fetch_response::{MessageData, ResponseBuilder, internal_date};
pub use idle::{Update, apply as apply_update, exists_line, expunge_line, flags_line};
pub use mailbox::{SequenceMap, matches_pattern, quote_mailbox, resolve_reference};
pub use move_entry::{MovePlan, Moving};
pub use parser::{MAX_NESTING_DEPTH, MAX_TOKENS, ParseError, Parsed, PendingLiteral, parse_line};
pub use response::{Capabilities, Status};
pub use search::{CompiledSearch, MAX_SEARCH_DEPTH, SearchParseError, compile as compile_search};
pub use sequence::{MAX_NUMBER, MAX_SET_ITEMS, SequenceError, SequenceSet};
pub use server::{BoundServer, ImapServer, ServerError, ServerStatus};
pub use session::{Outcome, Session};
pub use state::{Refusal, State, Transition, transition};
pub use store::{MAX_FLAGS, StoreOperation, StoreParseError, StoreRequest};

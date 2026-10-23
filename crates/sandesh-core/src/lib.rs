//! # Sandesh core
//!
//! The canonical mail domain model.
//!
//! This crate is deliberately free of infrastructure. It has no protocol
//! implementation, no HTTP framework, no database, and no async runtime — the
//! architecture test in `tests/architecture.rs` enforces that mechanically.
//!
//! ## The shape of the model
//!
//! Three protocol requirements determined the design (`docs/CORE_SPEC.md` §1):
//!
//! | Requirement | Consequence |
//! | --- | --- |
//! | A message may sit in several mailboxes with independent UIDs and flags | [`Message`] and [`MessageEntry`] are separate entities |
//! | UIDs are per-mailbox, monotonic, never reused | UID assignment belongs to [`Mailbox`], not a global counter |
//! | POP3 deletion is session-scoped until `QUIT` | Deletion is two-stage; staging lives in the adapter |
//!
//! ## Layers
//!
//! * **Value objects** — [`EmailAddress`], [`Uid`], [`MessageFlags`],
//!   [`ByteRange`], [`Timestamp`]. Immutable, validated, no identity.
//! * **Entities** — [`Account`], [`Mailbox`], [`Message`], [`MessageEntry`],
//!   [`Delivery`].
//! * **Ports** — [`MessageStore`], [`MailboxStore`], [`AccountStore`],
//!   [`BlobStore`], [`EventSink`]. Defined here, implemented outside.
//! * **Services** — [`AccountService`], [`DeliveryService`], [`MailboxService`],
//!   [`MessageService`]. The
//!   operations `CORE_SPEC.md` §6 and §7
//!   specify, orchestrating ports so that every protocol surface gets the same
//!   answer. Protocol-specific behaviour stays in the adapters (§9).
//!
//! ## Raw bytes are authoritative
//!
//! [`RawMessage`] holds the exact octets received. Everything derived —
//! headers, MIME parts, attachments — is a regenerable cache, and MIME parts
//! are [`ByteRange`]s *into* those octets rather than copies. This is what makes
//! `FETCH BODY[2.1]` byte-exact by construction.

#![doc(html_root_url = "https://docs.rs/sandesh-core")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]
#![allow(clippy::module_name_repetitions)]

mod account;
mod address;
mod analysis;
mod connection;
mod delivery;
mod entry;
mod error;
mod event;
mod faults;
mod flags;
mod id;
mod links;
mod mailbox;
mod mbox;
mod message;
mod mime;
mod parser;
mod ports;
mod routing;
mod rules;
mod search;
mod service;
mod time;
mod uid;
mod webhook;
mod workspace;

pub use account::{Account, Credential, DEFAULT_MAILBOXES, EmailIdentity};
pub use address::EmailAddress;
pub use analysis::{AuthResult, DkimSignature, HeaderAnalysis, ReceivedHop, SpamHeader, SpfRecord};
pub use connection::{ConnectionDisposition, ConnectionRecord, Protocol};
pub use delivery::{Delivery, DeliveryOutcome, RecipientOutcome};
pub use entry::MessageEntry;
pub use error::{CoreError, CoreResult, EntityKind, StoreError};
pub use event::{
    DomainEvent, EventKind, EventPayload, EventSink, EventSource, EventStream, NullEventSink,
};
pub use faults::{FaultMatch, FaultMode, FaultPoint, FaultRegistry, FaultRule};
pub use flags::{Keyword, MessageFlags, SystemFlags};
pub use id::{
    AccountId, AttachmentId, ConnectionId, DeliveryId, EndpointId, EntryId, EventId, IdentityId,
    MailboxId, MessageId, OutboxId, RuleId,
};
pub use links::{ExtractedLink, LinkKind, LinkReport};
pub use mailbox::{
    HIERARCHY_DELIMITER, INBOX, InvalidMailboxName, Mailbox, MailboxAttributes, MailboxName,
    SPECIAL_USE_ATTRIBUTES,
};
pub use mbox::{
    parse as parse_mbox, quote_from_lines, unquote_from_lines, write as write_mbox,
    write_entry as write_mbox_entry,
};
pub use message::{
    Attachment, DeliverySource, Envelope, Header, Message, ParsedMessage, RawMessage,
};
pub use mime::{
    ByteRange, DiagnosticSeverity, Disposition, MimePart, ParseDiagnostic, PartPath,
    TransferEncoding,
};
pub use parser::{PARSE_VERSION, decoded_part_body, parse};
pub use ports::{
    AccountStore, BlobStore, ConnectionStore, CredentialVerifier, DELIVERY_DURATION_BUCKETS_US,
    DeliveryStore, DeliveryTally, DurationHistogram, FlagChangeStatus, FlagOutcome, FlagUpdate,
    InsertedMessage, MailboxCounts, MailboxStore, MessageStore, MetricsSnapshot, MetricsSource,
    NewMessage, NewPlacement, NullSearchIndex, OutboxStore, ProtocolTally, RuleStore,
    SearchCandidates, SearchIndex, WorkspaceStore,
};
pub use routing::UnknownRecipient;
pub use rules::{Rule, RuleAction, RuleCondition, RuleMatch, RuleSet};
pub use search::{
    DateComparison, DateField, FlagKey, SearchQuery, TextField, parse_date_text, parse_filter_query,
};
pub use service::{
    AccountService, DeliveryReceipt, DeliveryRequest, DeliveryService, Discarded, MailboxService,
    MessageService, NewAccount, RecipientRejection,
};
pub use time::Timestamp;
pub use uid::{ModSeq, Uid, UidValidity};
pub use webhook::{
    OutboxEntry, OutboxStatus, Retry, RetryPolicy, SIGNATURE_HEADER, WebhookEndpoint,
    WebhookSecret, sign_payload, signature_hex, verify_signature,
};
pub use workspace::{
    InitialAccountSetup, PurgeMode, PurgeOptions, PurgeResult, Workspace, WorkspaceUpdate,
};

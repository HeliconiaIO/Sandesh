//! API-facing types (`API_DESIGN.md` §4).
//!
//! These exist so the domain model is not accidentally a public contract.
//! `sandesh-core` deliberately does not derive `serde` (`DEPENDENCIES.md`), so
//! every wire shape is written out here and converted explicitly. That is more
//! typing than `#[derive(Serialize)]` on the entities, and it is the point: a
//! field cannot appear in the JSON because someone added it to a struct, and
//! renaming a domain field cannot silently break a client.
//!
//! Conventions, all from `API_DESIGN.md` §2:
//!
//! * identifiers are UUID v7 rendered as strings;
//! * timestamps are RFC 3339 UTC;
//! * field names are `snake_case`;
//! * raw bytes never appear in JSON — they have their own endpoint.
//!
//! ## Message and entry stay distinct
//!
//! A [`MessageSummary`] carries **all** of its [`EntryRef`]s. A message in three
//! mailboxes is one summary with three entries, never three messages and never
//! one with a fictional "the" mailbox. This is the domain distinction between
//! immutable content and placement (`CORE_SPEC.md` §2) surfacing in the API,
//! and collapsing it here would make the multi-mailbox behaviour invisible to
//! the UI.

use sandesh_core::{
    Account, Attachment, DiagnosticSeverity, EmailAddress, Header, Mailbox, MailboxCounts, Message,
    MessageEntry, MimePart, ParseDiagnostic, Timestamp,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Renders a timestamp as RFC 3339 UTC.
///
/// Falls back to the Unix epoch rather than failing the whole response: a
/// formatting failure is not a reason to deny a client its mail, and an
/// obviously wrong timestamp is easier to diagnose than a 500.
/// The serde default for a flag that means "on unless you said otherwise".
pub(crate) const fn yes() -> bool {
    true
}

pub(crate) fn rfc3339(timestamp: Timestamp) -> String {
    timestamp
        .to_datetime()
        .and_then(|dt| {
            dt.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| String::from("1970-01-01T00:00:00Z"))
}

/// An email address as the API renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AddressDto {
    /// The addr-spec.
    pub address: String,
    /// The display name, when the header carried one.
    pub display_name: Option<String>,
}

impl AddressDto {
    /// Builds a DTO from a domain address.
    #[must_use]
    pub fn from_core(address: &EmailAddress) -> Self {
        Self {
            address: address.normalized(),
            display_name: address.display_name().map(ToOwned::to_owned),
        }
    }
}

/// One placement of a message, as it appears inside a message summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EntryRef {
    /// The placement.
    pub entry_id: String,
    /// The mailbox it sits in.
    pub mailbox_id: String,
    /// That mailbox's name, so a list view needs no second request.
    pub mailbox_name: Option<String>,
    /// The per-mailbox UID.
    pub uid: u32,
    /// IMAP flag names, e.g. `\Seen`.
    pub flags: Vec<String>,
}

impl EntryRef {
    /// Builds a reference from an entry, optionally naming its mailbox.
    #[must_use]
    pub fn from_core(entry: &MessageEntry, mailbox_name: Option<String>) -> Self {
        Self {
            entry_id: entry.id.to_string(),
            mailbox_id: entry.mailbox_id.to_string(),
            mailbox_name,
            uid: entry.uid.get(),
            flags: entry.flags.all_names(),
        }
    }
}

/// A message as it appears in a list (`API_DESIGN.md` §4).
///
/// Deliberately excludes bodies and MIME structure. A list of 500 messages must
/// not carry 500 message bodies; the detail endpoint exists for that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MessageSummary {
    /// The message.
    pub id: String,
    /// Decoded `Subject`, if present.
    pub subject: Option<String>,
    /// The `From:` header, decoded — what the sending application wrote,
    /// display name included.
    ///
    /// Falls back to the envelope sender (SMTP `MAIL FROM`) only when the
    /// message has no `From:` mailbox. The envelope is usually a bounce
    /// address with no name, so preferring it would list most application mail
    /// under an address its author never typed; it remains available from
    /// `GET /messages/{id}/delivery`.
    pub from: Option<AddressDto>,
    /// The `To:` header, decoded.
    ///
    /// Deliberately the header, not the envelope recipients: envelope
    /// recipients are a property of a *delivery*, not of the immutable message
    /// — one message delivered to three addresses has one set of bytes and
    /// three envelope recipients. They are reported by
    /// `GET /messages/{id}/delivery`, and the placements in `entries` show
    /// where the message actually landed, which is what makes `Bcc` visible.
    pub to: Vec<AddressDto>,
    /// When Sandesh accepted it.
    pub received_at: String,
    /// `RFC822.SIZE` in bytes.
    pub size: u64,
    /// Whether the message has any attachment part.
    pub has_attachments: bool,
    /// Whether MIME parsing fully succeeded.
    pub parse_ok: bool,
    /// Every placement of this message.
    pub entries: Vec<EntryRef>,
}

impl MessageSummary {
    /// Builds a summary from a message and all of its placements.
    #[must_use]
    pub fn from_core(message: &Message, entries: Vec<EntryRef>) -> Self {
        Self {
            id: message.id.to_string(),
            subject: message.subject().map(ToOwned::to_owned),
            from: header_addresses(message, "From")
                .into_iter()
                .next()
                .or_else(|| {
                    message
                        .envelope
                        .mail_from
                        .as_ref()
                        .map(AddressDto::from_core)
                }),
            to: header_addresses(message, "To"),
            received_at: rfc3339(message.received_at),
            size: message.size(),
            has_attachments: !message.parsed.attachments.is_empty(),
            parse_ok: message.parsed.parse_ok,
            entries,
        }
    }
}

/// The addresses in a named header, decoded.
///
/// Duplicated headers are all read, because a message with two `To:` fields is
/// exactly the kind of thing a mail-debugging tool must not hide.
fn header_addresses(message: &Message, name: &str) -> Vec<AddressDto> {
    message
        .parsed
        .headers_named(name)
        .flat_map(Header::addresses)
        .map(|address| AddressDto::from_core(&address))
        .collect()
}

/// One header, raw and decoded (`API_DESIGN.md` §3.1).
///
/// Duplicates are preserved and order is the order they appeared in: a tool for
/// debugging mail must show what actually arrived, not a tidied map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct HeaderDto {
    /// The field name as it appeared.
    pub name: String,
    /// The decoded value.
    pub value: String,
    /// The undecoded bytes, lossily rendered for JSON transport. The raw
    /// endpoint remains the authority for exact octets.
    pub raw_value: String,
}

impl HeaderDto {
    /// Builds a header DTO.
    #[must_use]
    pub fn from_core(header: &sandesh_core::Header) -> Self {
        Self {
            name: header.name.clone(),
            value: header.decoded_value.clone(),
            raw_value: String::from_utf8_lossy(&header.raw_value).into_owned(),
        }
    }
}

/// A message with everything the detail view needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MessageDetail {
    /// The list-row fields, flattened in so a client has one shape to learn.
    #[serde(flatten)]
    pub summary: MessageSummary,
    /// Every header, in order, duplicates preserved.
    pub headers: Vec<HeaderDto>,
    /// Attachment metadata; bytes come from the attachment endpoint.
    pub attachments: Vec<AttachmentDto>,
    /// Where parsing struggled. Non-empty is normal and interesting.
    pub diagnostics: Vec<DiagnosticDto>,
    /// How the message reached Sandesh.
    pub source: String,
}

/// Attachment metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AttachmentDto {
    /// The attachment.
    pub id: String,
    /// Its MIME part path, e.g. `2.1`.
    pub part_path: String,
    /// The declared filename.
    pub filename: Option<String>,
    /// The declared content type.
    pub content_type: String,
    /// Decoded size in bytes.
    pub size: u64,
    /// Whether it is `inline` rather than an attachment proper.
    pub is_inline: bool,
}

impl AttachmentDto {
    /// Builds an attachment DTO.
    #[must_use]
    pub fn from_core(attachment: &Attachment) -> Self {
        Self {
            id: attachment.id.to_string(),
            part_path: attachment.part_path.to_string(),
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            size: attachment.size,
            is_inline: attachment.is_inline,
        }
    }
}

/// A parse diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct DiagnosticDto {
    /// `warning` or `error`.
    pub severity: String,
    /// What went wrong.
    pub message: String,
}

impl DiagnosticDto {
    /// Builds a diagnostic DTO.
    #[must_use]
    pub fn from_core(diagnostic: &ParseDiagnostic) -> Self {
        Self {
            severity: String::from(match diagnostic.severity {
                DiagnosticSeverity::Info => "info",
                DiagnosticSeverity::Warning => "warning",
                DiagnosticSeverity::Error => "error",
            }),
            message: diagnostic.message.clone(),
        }
    }
}

/// One node of the MIME tree (`API_DESIGN.md` §3.1).
///
/// Offsets are ranges **into the raw bytes**, matching the storage model
/// (`CORE_SPEC.md`): there is no second copy of the MIME content anywhere, and
/// a client can correlate any part with the raw endpoint exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MimePartDto {
    /// Dotted part path, `""` for the root.
    pub path: String,
    /// How to address this part in a URL.
    ///
    /// The same as `path` except for the root, whose domain rendering is the
    /// empty string and which is therefore spelled `root` in a URL.
    pub url_path: String,
    /// The content type.
    pub content_type: String,
    /// The declared charset.
    pub charset: Option<String>,
    /// The transfer encoding name.
    pub encoding: String,
    /// The declared filename.
    pub filename: Option<String>,
    /// Byte offset of the part body within the raw message.
    pub body_offset: usize,
    /// Length of the part body in raw bytes.
    pub body_length: usize,
    /// Size after decoding, when known.
    pub decoded_size: Option<u64>,
    /// Whether this part is an attachment.
    pub is_attachment: bool,
    /// Child part paths.
    pub children: Vec<String>,
}

impl MimePartDto {
    /// Builds a part DTO.
    #[must_use]
    pub fn from_core(part: &MimePart) -> Self {
        let path = part.path.to_string();
        Self {
            url_path: if path.is_empty() {
                String::from("root")
            } else {
                path.clone()
            },
            path,
            content_type: part.content_type.clone(),
            charset: part.charset.clone(),
            encoding: part.encoding.as_str().to_owned(),
            filename: part.filename.clone(),
            body_offset: part.body_range.start(),
            body_length: part.body_range.len(),
            decoded_size: part.decoded_size,
            is_attachment: part.is_attachment,
            children: part.children.iter().map(ToString::to_string).collect(),
        }
    }
}

/// The MIME structure of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MimeTree {
    /// Every part, flattened; `children` reconstructs the tree.
    pub parts: Vec<MimePartDto>,
    /// Paths of the plain-text bodies.
    pub text_parts: Vec<String>,
    /// Paths of the HTML bodies.
    pub html_parts: Vec<String>,
    /// Whether parsing fully succeeded.
    pub parse_ok: bool,
    /// Parse diagnostics.
    pub diagnostics: Vec<DiagnosticDto>,
}

/// A mailbox with its counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MailboxDto {
    /// The mailbox.
    pub id: String,
    /// The owning account.
    pub account_id: String,
    /// Its hierarchical name, e.g. `INBOX/Work`.
    pub name: String,
    /// Whether the client is subscribed.
    pub subscribed: bool,
    /// UID-space generation.
    pub uid_validity: u32,
    /// The next UID to be assigned.
    pub uid_next: u32,
    /// Total entries.
    pub total: u64,
    /// Entries without `\Seen`.
    pub unseen: u64,
}

impl MailboxDto {
    /// Builds a mailbox DTO from a mailbox and its counts.
    #[must_use]
    pub fn from_core(mailbox: &Mailbox, counts: MailboxCounts) -> Self {
        Self {
            id: mailbox.id.to_string(),
            account_id: mailbox.account_id.to_string(),
            name: mailbox.name.as_str().to_owned(),
            subscribed: mailbox.subscribed,
            uid_validity: mailbox.uid_validity.get(),
            uid_next: counts.uid_next,
            total: counts.total,
            unseen: counts.unseen,
        }
    }
}

/// An entry addressed in its own right (`API_DESIGN.md` §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EntryDto {
    /// The placement.
    pub id: String,
    /// The message placed.
    pub message_id: String,
    /// The mailbox.
    pub mailbox_id: String,
    /// The per-mailbox UID.
    pub uid: u32,
    /// IMAP flag names.
    pub flags: Vec<String>,
    /// The IMAP `INTERNALDATE`.
    pub internal_date: String,
}

impl EntryDto {
    /// Builds an entry DTO.
    #[must_use]
    pub fn from_core(entry: &MessageEntry) -> Self {
        Self {
            id: entry.id.to_string(),
            message_id: entry.message_id.to_string(),
            mailbox_id: entry.mailbox_id.to_string(),
            uid: entry.uid.get(),
            flags: entry.flags.all_names(),
            internal_date: rfc3339(entry.internal_date),
        }
    }
}

/// An account and its identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AccountDto {
    /// The account.
    pub id: String,
    /// Its login name.
    pub username: String,
    /// Whether it receives mail for unmatched recipients.
    pub is_catch_all: bool,
    /// The addresses that route to it.
    pub addresses: Vec<String>,
    /// Whether this account has a configured password.
    pub has_password: bool,
}

impl AccountDto {
    /// Builds an account DTO.
    #[must_use]
    pub fn from_core(account: &Account, addresses: Vec<String>) -> Self {
        Self {
            id: account.id.to_string(),
            username: account.username.clone(),
            is_catch_all: account.is_catch_all,
            addresses,
            has_password: account.credential.is_some(),
        }
    }
}

/// One recipient's outcome within a delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RecipientOutcomeDto {
    /// The recipient address, normalised.
    pub address: String,
    /// The domain's own stable outcome name, never `Debug` output.
    pub outcome: String,
    /// Why, when the outcome was not a plain success.
    pub reason: Option<String>,
    /// The account this address resolved to, when it resolved to one. `null`
    /// for a rejected recipient, and for a catch-all placement the id is the
    /// catch-all account rather than one matching the address.
    pub resolved_account_id: Option<String>,
    /// The placement this recipient produced, when one was made. Lets the view
    /// link a recipient to the entry it created rather than re-deriving it.
    pub entry_id: Option<String>,
}

/// How a message reached Sandesh (`GET /api/v1/messages/{id}/delivery`).
///
/// Envelope recipients live here rather than on the message because they are a
/// property of the *delivery*, not of the immutable bytes (ADR-019). This is
/// what makes `Bcc` visible: a recipient that appears in no header still
/// appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct DeliveryDto {
    /// The delivery record.
    pub id: String,
    /// The message it produced, when one was stored.
    pub message_id: Option<String>,
    /// SMTP `MAIL FROM`.
    pub mail_from: Option<String>,
    /// SMTP `RCPT TO`, every recipient the envelope carried.
    pub rcpt_to: Vec<String>,
    /// Which ingress accepted it.
    pub source: String,
    /// The client address, when the ingress had one.
    pub peer: Option<String>,
    /// The domain the client gave in `EHLO`/`HELO`. Unverified, as SMTP makes
    /// it unverifiable: it is what the client claimed, not what it is.
    pub helo: Option<String>,
    /// When the delivery was received, RFC 3339.
    pub received_at: String,
    /// How long the transaction took, in microseconds.
    pub duration_us: i64,
    /// Per-recipient results.
    pub recipients: Vec<RecipientOutcomeDto>,
    /// The rules that fired, and what each added (ADR-087).
    ///
    /// The A3 criterion is that rules are visible in the delivery record; this
    /// is where the Delivery tab reads them from.
    pub rules_applied: Vec<RuleMatchDto>,
    /// The message this one was replayed from, when it was a replay (A5).
    ///
    /// The queryable half of replay provenance. The same link is written into
    /// the replayed message as an `X-Sandesh-Replay-Of` header, which travels
    /// with the bytes; this field is what a consumer filters on. `null` for
    /// every ordinary delivery.
    pub replay_of: Option<String>,
}

/// One connection's protocol transcript (`GET /api/v1/connections/{id}`).
///
/// The retrieval half of ADR-066: the capture side has been storing these since
/// A2's first half, with no way to read them back, so the Phase 7 criterion
/// "every connection produces a **retrievable** trace" was only half met.
///
/// The transcript is already redacted at capture time — credentials never reach
/// storage, and message bodies become a byte count (ADR-066) — so this DTO does
/// no filtering of its own. Redacting here instead would put the guarantee on
/// the read path, where a second reader (a future export, a log stream) would
/// not inherit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConnectionDto {
    /// The connection record.
    pub id: String,
    /// Which protocol server accepted it: `smtp`, `imap` or `pop3`.
    pub protocol: String,
    /// The client address, when the transport had one.
    pub peer: Option<String>,
    /// Whether the transport was encrypted **when the connection closed**.
    ///
    /// A `STARTTLS` connection is one record that begins in cleartext and ends
    /// encrypted, so this is the final state. The transcript shows where the
    /// upgrade happened.
    pub encrypted: bool,
    /// When the socket was accepted, RFC 3339.
    pub opened_at: String,
    /// When it closed, RFC 3339. `null` while still open.
    pub closed_at: Option<String>,
    /// How it ended: `closed`, `disconnected`, `server_closed` or
    /// `shutting_down`. `null` while still open.
    pub disposition: Option<String>,
    /// How long it lasted, in microseconds. `null` while still open.
    pub duration_us: Option<i64>,
    /// The transcript, one entry per line.
    ///
    /// Always present, including in the list response. A transcript is the
    /// whole point of the record and is already capped at capture time, so
    /// omitting it from the list would mean a second round trip per row to see
    /// the only field anyone opened the view for.
    pub transcript: Vec<String>,
    /// Whether the transcript hit its cap and was cut short.
    ///
    /// Reported so a reader can tell a quiet connection from a truncated one —
    /// without it, a trace that stops mid-session looks like a client that went
    /// silent.
    pub truncated: bool,
}

/// The envelope every list endpoint returns (`API_DESIGN.md` §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ListResponse<T> {
    /// This page.
    pub items: Vec<T>,
    /// Cursor for the next page, or `null` at the end.
    pub next_cursor: Option<String>,
    /// An **estimate** of the total. Named honestly: an exact count under
    /// continuous insertion is expensive and stale by the time it is read.
    pub total_estimate: u64,
}

/// Names one concrete `ListResponse<T>` for the `OpenAPI` document.
///
/// `OpenAPI` 3.1 has no generics, so every instantiation the API actually returns
/// needs a name of its own. These are contract-only shapes: handlers keep
/// returning `ListResponse<T>`, and because each alias is a real type whose
/// fields must line up, a list endpoint added without its alias fails to
/// compile rather than quietly leaving a hole in the contract.
macro_rules! list_response_alias {
    ($name:ident, $item:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
        pub struct $name {
            /// This page.
            pub items: Vec<$item>,
            /// Cursor for the next page, or `null` at the end.
            pub next_cursor: Option<String>,
            /// An estimate of the total; see [`ListResponse::total_estimate`].
            pub total_estimate: u64,
        }
    };
}

list_response_alias!(ListResponseAccountDto, AccountDto, "A page of accounts.");
list_response_alias!(ListResponseMailboxDto, MailboxDto, "A page of mailboxes.");
list_response_alias!(ListResponseEntryDto, EntryDto, "A page of placements.");
list_response_alias!(
    ListResponseMessageSummary,
    MessageSummary,
    "A page of message summaries."
);
list_response_alias!(
    ListResponseAttachmentDto,
    AttachmentDto,
    "A page of message attachments."
);
list_response_alias!(
    ListResponseDeliveryDto,
    DeliveryDto,
    "A page of delivery records, newest first."
);
list_response_alias!(
    ListResponseConnectionDto,
    ConnectionDto,
    "A page of connection transcripts, newest first."
);
list_response_alias!(
    ListResponseLogLineDto,
    LogLineDto,
    "Recent log lines, oldest first."
);

/// Liveness (`GET /api/v1/health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct HealthDto {
    /// Always `ok` if the process can answer at all.
    pub status: &'static str,
    /// The running version.
    pub version: &'static str,
}

/// Readiness (`GET /api/v1/health/ready`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReadyDto {
    /// `ready` or `not_ready`.
    pub status: &'static str,
    /// Whether the database answered a trivial query.
    pub database: bool,
}

/// Server and connection information (`GET /api/v1/info`).
///
/// The connection strings are real product surface: they are what a developer
/// copies into their application's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct InfoDto {
    /// The running version.
    pub version: &'static str,
    /// Where the SMTP listener is bound, if it is running.
    pub smtp_address: Option<String>,
    /// Where the IMAP listener is bound, if it is running.
    pub imap_address: Option<String>,
    /// Where the POP3 listener is bound, if it is running.
    pub pop3_address: Option<String>,
    /// Where the HTTP listener is bound.
    pub http_address: Option<String>,
    /// The `WebSocket` route.
    pub websocket_path: &'static str,
    /// Where the database lives.
    pub database: String,
    /// The SMTP authentication posture: `disabled`, `optional` or `required`.
    ///
    /// Reported so the UI can state it plainly. A developer must never be
    /// unsure whether their local mail server is accepting anonymous mail
    /// (`UI_UX_DIRECTION` §5) — and the honest answer by default is that it is.
    pub smtp_auth: String,
    /// The SMTP TLS posture: `disabled`, `starttls` or `implicit`.
    pub smtp_tls: String,
    /// The certificate's SHA-256 fingerprint, when TLS is enabled.
    ///
    /// The only way to trust a self-signed certificate deliberately rather
    /// than by clicking through a warning is to compare this against what the
    /// client reports.
    pub tls_fingerprint: Option<String>,
    /// Whether any SMTP fault is currently injected (ADR-084).
    ///
    /// Read live from the registry on every request rather than from the
    /// startup `ServerInfo`, because unlike every other field here it can
    /// change while the server runs — that being the entire point of faults
    /// being runtime state. Surfaced so an injected failure is never invisible
    /// to someone wondering why delivery stopped working.
    pub faults_active: bool,
    /// The **API** authentication posture: `disabled` or `required`.
    ///
    /// Separate from `smtp_auth`, which never governed the HTTP surface. Before
    /// this existed, a deployment with `smtp_auth: required` looked protected
    /// while the API served every message to anyone who asked (ADR-075).
    pub api_auth: String,
    /// Whether the API is served over HTTPS.
    pub api_tls: bool,
}

/// Request body for composing and sending an email (`POST /api/v1/send`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SendMessageRequest {
    /// Sender address (e.g. `sender@example.com` or `Name <sender@example.com>`).
    pub from: String,
    /// List of recipient addresses.
    pub to: Vec<String>,
    /// Optional CC recipients.
    #[serde(default)]
    pub cc: Vec<String>,
    /// Optional BCC recipients.
    #[serde(default)]
    pub bcc: Vec<String>,
    /// Message subject.
    pub subject: String,
    /// Plain-text message body.
    #[serde(default)]
    pub text_body: Option<String>,
    /// HTML message body.
    #[serde(default)]
    pub html_body: Option<String>,
    /// Message-ID being replied to (if any).
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// Thread references header (if any).
    #[serde(default)]
    pub references: Option<String>,
    /// Whether this message should be saved as a draft in Drafts instead of delivered.
    #[serde(default)]
    pub is_draft: Option<bool>,
}

/// Request body for moving an entry to another mailbox (`POST /api/v1/entries/{id}/move`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MoveEntryRequest {
    /// Target mailbox UUID.
    #[serde(default)]
    pub target_mailbox_id: Option<String>,
    /// Target mailbox name (e.g. "Trash", "Archive", "Junk", "INBOX").
    #[serde(default)]
    pub mailbox_name: Option<String>,
}

/// Request body for copying an entry to another mailbox (`POST /api/v1/entries/{id}/copy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CopyEntryRequest {
    /// Target mailbox UUID.
    #[serde(default)]
    pub target_mailbox_id: Option<String>,
    /// Target mailbox name (e.g. "Archive").
    #[serde(default)]
    pub mailbox_name: Option<String>,
}

/// Request body for creating a mailbox (`POST /api/v1/mailboxes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateMailboxRequest {
    /// Optional account UUID. Defaults to the catch-all or primary account if omitted.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Mailbox name (e.g. "Projects/Sandesh").
    pub name: String,
}

/// Request body for updating a mailbox (`PATCH /api/v1/mailboxes/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PatchMailboxRequest {
    /// New mailbox name (for rename).
    #[serde(default)]
    pub name: Option<String>,
    /// Whether the mailbox is subscribed.
    #[serde(default)]
    pub subscribed: Option<bool>,
}

/// Request body for creating an account (`POST /api/v1/accounts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateAccountRequest {
    /// Username for login.
    pub username: String,
    /// Optional password (will be securely hashed).
    #[serde(default)]
    pub password: Option<String>,
    /// Optional addresses routing to this account.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// Whether this is the catch-all account.
    #[serde(default)]
    pub is_catch_all: Option<bool>,
}

/// Request body for updating an account (`PATCH /api/v1/accounts/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PatchAccountRequest {
    /// New password (will be securely hashed).
    #[serde(default)]
    pub password: Option<String>,
    /// Additional addresses to route to this account.
    #[serde(default)]
    pub add_addresses: Vec<String>,
    /// Addresses to remove from this account.
    #[serde(default)]
    pub remove_addresses: Vec<String>,
    /// Update catch-all status.
    #[serde(default)]
    pub is_catch_all: Option<bool>,
}

/// Response returned after successfully composing and delivering an email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SendMessageResponse {
    /// The generated UUID of the new message.
    pub id: String,
    /// The RFC 5322 Message-ID header value.
    pub message_id: String,
    /// Subject of the message.
    pub subject: String,
    /// Number of mailbox placements created (e.g. recipient INBOX + sender Sent).
    pub placements: usize,
    /// When the message was accepted.
    pub created_at: String,
}
/// Action to perform across multiple entries (`POST /api/v1/entries/bulk`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkAction {
    /// Mark all specified entries as seen (`\Seen`).
    MarkRead,
    /// Mark all specified entries as unseen (remove `\Seen`).
    MarkUnread,
    /// Delete all specified placements.
    Delete,
    /// Move all specified entries to target mailbox.
    Move {
        /// Target mailbox UUID.
        #[serde(default)]
        target_mailbox_id: Option<String>,
        /// Target mailbox name (e.g. "Trash").
        #[serde(default)]
        mailbox_name: Option<String>,
    },
}

/// Request body for bulk operations on entries (`POST /api/v1/entries/bulk`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BulkEntriesRequest {
    /// Placements to operate on.
    pub entry_ids: Vec<String>,
    /// Action to execute.
    pub action: BulkAction,
}

/// Response returned from a bulk operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BulkEntriesResponse {
    /// Number of entries affected.
    pub affected: usize,
}

/// Query plan explanation returned from `GET /api/v1/search/explain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SearchExplainResponse {
    /// The user query string as received.
    pub query: String,
    /// Debug representation of the compiled `SearchQuery` domain plan.
    pub plan: String,
}

/// The analysis of one message's headers (`ROADMAP.md` Phase 7 A3).
///
/// **Parsed, never verified.** Every authentication field here is reproduced as
/// the upstream server wrote it. Sandesh performs no DKIM, SPF or DMARC
/// verification and this response makes no claim about validity — see
/// [`HeaderAnalysisDto::disclaimer`], which is part of the payload precisely so
/// that a client cannot render these fields without the caveat travelling with
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct HeaderAnalysisDto {
    /// The `Received:` chain, oldest hop first.
    pub hops: Vec<ReceivedHopDto>,
    /// Parsed `DKIM-Signature:` fields.
    pub dkim: Vec<DkimSignatureDto>,
    /// Entries extracted from `Authentication-Results:` fields.
    pub auth_results: Vec<AuthResultDto>,
    /// `Received-SPF:` fields.
    pub spf: Vec<SpfRecordDto>,
    /// `X-Spam-*` headers, surfaced without interpretation.
    pub spam: Vec<SpamHeaderDto>,
    /// A fixed statement that nothing here was verified.
    pub disclaimer: String,
}

/// The statement that accompanies every analysis response.
const DISCLAIMER: &str = "Headers are parsed and displayed as written. Sandesh performs no DKIM, SPF or DMARC \
     verification and makes no claim that any signature or policy is valid.";

/// One hop of the `Received:` chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReceivedHopDto {
    /// Zero-based position in chronological order; hop 0 is the oldest.
    pub index: usize,
    /// The `from` clause.
    pub from: Option<String>,
    /// The `by` clause.
    pub by: Option<String>,
    /// The `with` clause: the protocol used.
    pub with: Option<String>,
    /// The `for` clause: the envelope recipient this hop saw.
    pub for_recipient: Option<String>,
    /// The receiving host's queue id.
    pub id: Option<String>,
    /// The hop timestamp, RFC 3339 UTC, when it parsed.
    pub timestamp: Option<String>,
    /// Microseconds since the previous hop. May be negative when relay clocks
    /// disagree; that is reported rather than hidden.
    pub delay_micros: Option<i64>,
    /// The whole field value, for when the parse missed something.
    pub raw: String,
}

/// A parsed `DKIM-Signature:` field. Not verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct DkimSignatureDto {
    /// `v=` — version.
    pub version: Option<String>,
    /// `a=` — signing algorithm.
    pub algorithm: Option<String>,
    /// `d=` — signing domain.
    pub domain: Option<String>,
    /// `s=` — selector.
    pub selector: Option<String>,
    /// `c=` — canonicalisation.
    pub canonicalization: Option<String>,
    /// `h=` — the signed header names, in the order signed.
    pub signed_headers: Vec<String>,
    /// `bh=` — the body hash.
    pub body_hash: Option<String>,
    /// `t=` — signature timestamp, RFC 3339 UTC.
    pub timestamp: Option<String>,
    /// `x=` — expiry, RFC 3339 UTC.
    pub expiration: Option<String>,
    /// Every tag as written, including ones not named above.
    pub tags: Vec<TagDto>,
}

/// A single `name=value` pair, used for DKIM tags and auth-result properties.
///
/// A list of pairs rather than a map: order is meaningful in a DKIM signature,
/// and a duplicate tag must stay visible rather than silently overwrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TagDto {
    /// The tag name.
    pub name: String,
    /// The tag value as written.
    pub value: String,
}

/// One method's entry from an `Authentication-Results:` field.
///
/// `result` is the upstream server's word, reproduced. Sandesh did not check it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AuthResultDto {
    /// The host claiming to have performed the check.
    pub authserv_id: String,
    /// The method, e.g. `dkim`, `spf`, `dmarc`.
    pub method: String,
    /// The result as written upstream, e.g. `pass`. Not checked here.
    pub result: String,
    /// Trailing properties for this method.
    pub properties: Vec<TagDto>,
}

/// A `Received-SPF:` field, as written upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SpfRecordDto {
    /// The result keyword as written upstream. Not checked here.
    pub result: String,
    /// The free-text remainder.
    pub detail: String,
}

/// An `X-Spam-*` header, surfaced without interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SpamHeaderDto {
    /// The field name as it appeared.
    pub name: String,
    /// The decoded value.
    pub value: String,
}

impl HeaderAnalysisDto {
    /// Projects a core analysis.
    #[must_use]
    pub fn from_core(analysis: &sandesh_core::HeaderAnalysis) -> Self {
        Self {
            hops: analysis
                .hops
                .iter()
                .map(|hop| ReceivedHopDto {
                    index: hop.index,
                    from: hop.from.clone(),
                    by: hop.by.clone(),
                    with: hop.with.clone(),
                    for_recipient: hop.for_recipient.clone(),
                    id: hop.id.clone(),
                    timestamp: hop.timestamp.map(rfc3339),
                    delay_micros: hop.delay_micros,
                    raw: hop.raw.clone(),
                })
                .collect(),
            dkim: analysis
                .dkim
                .iter()
                .map(|signature| DkimSignatureDto {
                    version: signature.version.clone(),
                    algorithm: signature.algorithm.clone(),
                    domain: signature.domain.clone(),
                    selector: signature.selector.clone(),
                    canonicalization: signature.canonicalization.clone(),
                    signed_headers: signature.signed_headers.clone(),
                    body_hash: signature.body_hash.clone(),
                    timestamp: signature.timestamp.map(rfc3339),
                    expiration: signature.expiration.map(rfc3339),
                    tags: signature.tags.iter().map(TagDto::from_pair).collect(),
                })
                .collect(),
            auth_results: analysis
                .auth_results
                .iter()
                .map(|result| AuthResultDto {
                    authserv_id: result.authserv_id.clone(),
                    method: result.method.clone(),
                    result: result.result.clone(),
                    properties: result.properties.iter().map(TagDto::from_pair).collect(),
                })
                .collect(),
            spf: analysis
                .spf
                .iter()
                .map(|record| SpfRecordDto {
                    result: record.result.clone(),
                    detail: record.detail.clone(),
                })
                .collect(),
            spam: analysis
                .spam
                .iter()
                .map(|header| SpamHeaderDto {
                    name: header.name.clone(),
                    value: header.value.clone(),
                })
                .collect(),
            disclaimer: DISCLAIMER.to_string(),
        }
    }
}

impl TagDto {
    /// Projects a `(name, value)` pair.
    fn from_pair(pair: &(String, String)) -> Self {
        Self {
            name: pair.0.clone(),
            value: pair.1.clone(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use sandesh_core::{EntryId, MailboxId, MessageFlags, MessageId, Uid};

    fn entry(mailbox: MailboxId, uid: u32) -> MessageEntry {
        MessageEntry {
            id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: mailbox,
            uid: Uid::new(uid).unwrap_or(Uid::MIN),
            flags: MessageFlags::default(),
            internal_date: Timestamp::now(),
            added_at: Timestamp::now(),
            mod_seq: sandesh_core::ModSeq::MIN,
        }
    }

    #[test]
    fn an_entry_ref_renders_ids_as_strings() {
        // UUIDs cross the wire as strings, never as byte arrays or integers.
        let dto = EntryRef::from_core(&entry(MailboxId::new(), 42), Some(String::from("INBOX")));
        assert_eq!(dto.uid, 42);
        assert_eq!(dto.mailbox_name.as_deref(), Some("INBOX"));
        assert_eq!(dto.entry_id.len(), 36, "canonical UUID string");
    }

    #[test]
    fn a_summary_carries_every_placement() {
        // The multi-mailbox property: one message, several entries, no
        // pretending there is a single home.
        let json = serde_json::to_value(ListResponse {
            items: vec![EntryRef::from_core(
                &entry(MailboxId::new(), 1),
                Some(String::from("INBOX")),
            )],
            next_cursor: None,
            total_estimate: 1,
        })
        .expect("serialises");

        assert!(json.get("items").is_some());
        assert!(
            json.get("next_cursor")
                .is_some_and(serde_json::Value::is_null)
        );
    }

    #[test]
    fn field_names_are_snake_case() {
        let dto = EntryRef::from_core(&entry(MailboxId::new(), 1), None);
        let json = serde_json::to_value(&dto).expect("serialises");
        let object = json.as_object().expect("an object");
        for key in object.keys() {
            assert!(
                !key.chars().any(char::is_uppercase),
                "{key} is not snake_case"
            );
        }
        assert!(object.contains_key("entry_id"));
        assert!(object.contains_key("mailbox_id"));
    }

    #[test]
    fn timestamps_render_as_rfc3339_utc() {
        let dto = EntryDto::from_core(&entry(MailboxId::new(), 1));
        assert!(
            dto.internal_date.ends_with('Z'),
            "must be UTC: {}",
            dto.internal_date
        );
        assert!(dto.internal_date.contains('T'));
    }

    #[test]
    fn no_dto_exposes_raw_bytes() {
        // Raw octets have their own endpoint; putting them in JSON would both
        // corrupt them and unbound the response size.
        let dto = EntryDto::from_core(&entry(MailboxId::new(), 1));
        let json = serde_json::to_string(&dto).expect("serialises");
        assert!(!json.contains("raw"));
    }

    #[test]
    fn the_analysis_response_always_carries_the_not_verified_disclaimer() {
        // The caveat travels inside the payload, not only in the docs, so a
        // client cannot render `dkim=pass` without it.
        let dto = HeaderAnalysisDto::from_core(&sandesh_core::HeaderAnalysis::default());
        assert!(
            dto.disclaimer
                .contains("no DKIM, SPF or DMARC verification")
        );
    }

    #[test]
    fn no_analysis_field_or_value_claims_verification() {
        let analysis = sandesh_core::HeaderAnalysis::analyze(&[]);
        let dto = HeaderAnalysisDto::from_core(&analysis);
        let json = serde_json::to_string(&dto).expect("serialises");

        // The disclaimer is the one place the word may appear, and it appears
        // there to deny it. Everything else must be free of the claim.
        let without_disclaimer = json.replace(
            &serde_json::to_string(&dto.disclaimer).expect("serialises"),
            "\"\"",
        );
        for claim in ["verified", "valid", "authentic", "trusted"] {
            assert!(
                !without_disclaimer.to_lowercase().contains(claim),
                "the analysis payload must not claim `{claim}`"
            );
        }
    }

    #[test]
    fn dkim_tags_project_as_ordered_pairs_not_a_map() {
        // Order is meaningful in a signature and duplicate tags must stay
        // visible, which a JSON object could not preserve.
        let core = sandesh_core::HeaderAnalysis::analyze(&[header_for_test(
            "DKIM-Signature",
            "v=1; d=example.com; d=evil.example",
        )]);
        let dto = HeaderAnalysisDto::from_core(&core);

        let tags = &dto.dkim[0].tags;
        assert_eq!(tags.len(), 3, "a repeated tag is not collapsed");
        assert_eq!(tags[0].name, "v");
        assert_eq!(tags[1].value, "example.com");
        assert_eq!(tags[2].value, "evil.example");
    }

    /// Builds a header for the analysis DTO tests.
    fn header_for_test(name: &str, value: &str) -> sandesh_core::Header {
        sandesh_core::Header {
            name: name.to_string(),
            raw_value: value.as_bytes().to_vec(),
            decoded_value: value.to_string(),
            range: sandesh_core::ByteRange::empty_at(0),
        }
    }
}

/// One captured log line (Phase 7 A6).
///
/// The log carries no message content by construction: bodies, headers and
/// credentials never reach a log line in the first place (`logging.rs`), so
/// this DTO — like [`ConnectionDto`] — does no filtering of its own. Putting
/// redaction on the read path would place the guarantee where a second reader
/// could not inherit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct LogLineDto {
    /// Monotonic identity, so a reader can tell a gap from a quiet period.
    pub seq: u64,
    /// When it was captured, RFC 3339.
    pub at: String,
    /// Severity: `error`, `warn`, `info`, `debug` or `trace`.
    pub level: String,
    /// The emitting module path.
    pub target: String,
    /// The rendered message.
    pub message: String,
    /// The event's structured fields, rendered `key=value`.
    pub fields: String,
}

impl From<sandesh_events::LogLine> for LogLineDto {
    fn from(line: sandesh_events::LogLine) -> Self {
        Self {
            seq: line.seq,
            at: line.at,
            level: line.level,
            target: line.target,
            message: line.message,
            fields: line.fields,
        }
    }
}

/// What an injected fault does (ADR-084).
///
/// A tagged representation rather than a flat string plus optional fields: the
/// delay belongs to `slow_response` and to no other mode, and a shape that
/// allowed `{"mode": "connection_drop", "delay_ms": 500}` would have to either
/// reject it or silently ignore the delay. This makes it unconstructable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum FaultModeDto {
    /// Refuse the recipient permanently (`550`).
    RejectAtRcpt,
    /// Accept the recipients, then refuse the message (`554`).
    RejectAtData,
    /// Refuse temporarily (`451`), so a client retries.
    TemporaryFailure,
    /// Delay before the ordinary reply.
    SlowResponse {
        /// How long to wait, in milliseconds.
        delay_ms: u64,
    },
    /// Close the connection with no reply.
    ConnectionDrop,
}

impl From<FaultModeDto> for sandesh_core::FaultMode {
    fn from(dto: FaultModeDto) -> Self {
        match dto {
            FaultModeDto::RejectAtRcpt => Self::RejectAtRcpt,
            FaultModeDto::RejectAtData => Self::RejectAtData,
            FaultModeDto::TemporaryFailure => Self::TemporaryFailure,
            FaultModeDto::SlowResponse { delay_ms } => {
                Self::SlowResponse(std::time::Duration::from_millis(delay_ms))
            }
            FaultModeDto::ConnectionDrop => Self::ConnectionDrop,
        }
    }
}

impl From<sandesh_core::FaultMode> for FaultModeDto {
    fn from(mode: sandesh_core::FaultMode) -> Self {
        match mode {
            sandesh_core::FaultMode::RejectAtRcpt => Self::RejectAtRcpt,
            sandesh_core::FaultMode::RejectAtData => Self::RejectAtData,
            sandesh_core::FaultMode::TemporaryFailure => Self::TemporaryFailure,
            sandesh_core::FaultMode::SlowResponse(delay) => Self::SlowResponse {
                // Saturating rather than `as`: a delay beyond u64 milliseconds
                // is not expressible, and silently wrapping it would report a
                // short delay for a long one.
                delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            },
            sandesh_core::FaultMode::ConnectionDrop => Self::ConnectionDrop,
        }
    }
}

/// Which conversations a fault applies to (ADR-084).
///
/// Every field absent means every conversation, which is the common case for a
/// CI scenario and needs no ceremony to express.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FaultMatchDto {
    /// Fires only for this recipient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Fires only for this sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
}

/// One configured fault (`GET`/`POST /api/v1/faults`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FaultRuleDto {
    /// What this fault does.
    #[serde(flatten)]
    pub mode: FaultModeDto,
    /// Which conversations it applies to.
    #[serde(default)]
    pub selector: FaultMatchDto,
}

impl From<FaultRuleDto> for sandesh_core::FaultRule {
    fn from(dto: FaultRuleDto) -> Self {
        Self {
            mode: dto.mode.into(),
            selector: sandesh_core::FaultMatch {
                recipient: dto.selector.recipient,
                sender: dto.selector.sender,
            },
        }
    }
}

impl From<sandesh_core::FaultRule> for FaultRuleDto {
    fn from(rule: sandesh_core::FaultRule) -> Self {
        Self {
            mode: rule.mode.into(),
            selector: FaultMatchDto {
                recipient: rule.selector.recipient,
                sender: rule.selector.sender,
            },
        }
    }
}

/// The configured faults (`GET /api/v1/faults`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FaultsDto {
    /// Whether any fault is currently configured.
    ///
    /// Redundant against `rules` being non-empty, and deliberately so: it is
    /// what `/info` and the UI indicator read, and a single boolean is harder
    /// to misinterpret than an array's length.
    pub active: bool,
    /// Every configured rule, in the order they are evaluated.
    pub rules: Vec<FaultRuleDto>,
}

// --- Webhooks (Phase 8 A2, ADR-085, ADR-086) ---------------------------------

/// A registered webhook destination (`GET`/`POST /api/v1/webhooks`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookEndpointDto {
    /// Identity, for deletion.
    pub id: String,
    /// Where notifications are `POST`ed.
    pub url: String,
    /// Whether new notifications are enqueued for it.
    pub enabled: bool,
    /// When it was registered, RFC 3339.
    pub created_at: String,
    /// Whether payloads for this endpoint are signed.
    ///
    /// A boolean, never the key. The secret is returned exactly once, by the
    /// create response, and is unreadable afterwards: an endpoint listing is
    /// the sort of thing that ends up in a log or a screen share, and a
    /// signing key that appears there has stopped being a signing key.
    pub has_secret: bool,
}

impl From<sandesh_core::WebhookEndpoint> for WebhookEndpointDto {
    fn from(endpoint: sandesh_core::WebhookEndpoint) -> Self {
        Self {
            id: endpoint.id.to_string(),
            url: endpoint.url,
            enabled: endpoint.enabled,
            created_at: rfc3339(endpoint.created_at),
            has_secret: endpoint.secret.is_some(),
        }
    }
}

/// The response to registering an endpoint (`POST /api/v1/webhooks`).
///
/// Distinct from [`WebhookEndpointDto`] for one reason: it carries `secret`,
/// and this is the only response that ever will. Giving the read type an
/// optional field that is populated exactly once would make every consumer
/// handle a value they cannot otherwise obtain, and would put the key one
/// forgotten `null` check away from a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreatedWebhookDto {
    /// The endpoint as it will read from now on.
    #[serde(flatten)]
    pub endpoint: WebhookEndpointDto,
    /// The signing secret, in clear, for the only time.
    ///
    /// `None` when the endpoint is unsigned. Store it now: it cannot be
    /// recovered, only replaced by registering the endpoint again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// A request to register an endpoint (`POST /api/v1/webhooks`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NewWebhookDto {
    /// Where to POST notifications.
    pub url: String,
    /// Whether to start enabled. Defaults to true — registering an endpoint
    /// that receives nothing is not what anyone means by registering one.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Turn on payload signing.
    ///
    /// Sandesh generates the secret and returns it once. Omitted or false
    /// leaves the endpoint unsigned, which stays the default: a webhook
    /// pointed at a local test receiver has nothing to spoof, and requiring a
    /// key there would add a step to the case Sandesh exists to serve.
    #[serde(default)]
    pub sign: bool,
    /// Use this secret instead of a generated one.
    ///
    /// For pointing Sandesh at a receiver that already knows a key. Ignored
    /// unless `sign` is set, so a secret cannot be configured on an endpoint
    /// that will never use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// One queued or finished notification (`GET /api/v1/webhooks/outbox`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OutboxEntryDto {
    /// Identity.
    pub id: String,
    /// Which endpoint it is bound for.
    pub endpoint_id: String,
    /// The JSON body, as rendered at enqueue time.
    pub payload: String,
    /// How many attempts have been made.
    pub attempts: u32,
    /// `pending`, `delivered` or `failed`.
    pub status: String,
    /// When the next attempt becomes due, RFC 3339.
    pub next_attempt_at: String,
    /// Why the most recent attempt failed, if one did.
    ///
    /// The reason a terminally-failed entry is kept rather than deleted: a
    /// webhook that silently stopped is the opacity Phase 7 exists to remove.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When it was enqueued, RFC 3339.
    pub created_at: String,
}

impl From<sandesh_core::OutboxEntry> for OutboxEntryDto {
    fn from(entry: sandesh_core::OutboxEntry) -> Self {
        Self {
            id: entry.id.to_string(),
            endpoint_id: entry.endpoint_id.to_string(),
            payload: entry.payload,
            attempts: entry.attempts,
            status: entry.status.as_str().to_owned(),
            next_attempt_at: rfc3339(entry.next_attempt_at),
            last_error: entry.last_error,
            created_at: rfc3339(entry.created_at),
        }
    }
}

// --- Message rules (Phase 8 A3, ADR-087) -------------------------------------

/// One condition of a rule.
///
/// Flattened into a `kind`-tagged object, matching how `FaultModeDto` carries
/// its variants, so the wire form reads as one object rather than a wrapper
/// around a single-key map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleConditionDto {
    /// A named header contains this text, case-insensitively.
    HeaderContains {
        /// Header name.
        name: String,
        /// Substring to find in the decoded value.
        value: String,
    },
    /// The envelope sender contains this text.
    SenderContains {
        /// Substring to find.
        value: String,
    },
    /// Any envelope recipient contains this text.
    RecipientContains {
        /// Substring to find.
        value: String,
    },
}

impl From<RuleConditionDto> for sandesh_core::RuleCondition {
    fn from(dto: RuleConditionDto) -> Self {
        match dto {
            RuleConditionDto::HeaderContains { name, value } => {
                Self::HeaderContains { name, value }
            }
            RuleConditionDto::SenderContains { value } => Self::SenderContains(value),
            RuleConditionDto::RecipientContains { value } => Self::RecipientContains(value),
        }
    }
}

impl From<sandesh_core::RuleCondition> for RuleConditionDto {
    fn from(condition: sandesh_core::RuleCondition) -> Self {
        match condition {
            sandesh_core::RuleCondition::HeaderContains { name, value } => {
                Self::HeaderContains { name, value }
            }
            sandesh_core::RuleCondition::SenderContains(value) => Self::SenderContains { value },
            sandesh_core::RuleCondition::RecipientContains(value) => {
                Self::RecipientContains { value }
            }
        }
    }
}

/// A configured rule (`GET`/`POST /api/v1/rules`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleDto {
    /// Identity.
    pub id: String,
    /// The name shown in the delivery record when this rule fires.
    pub name: String,
    /// Every condition must hold for the rule to apply.
    pub conditions: Vec<RuleConditionDto>,
    /// The keyword added to each placement when it does.
    pub add_keyword: String,
    /// Whether the rule is evaluated.
    pub enabled: bool,
    /// When it was created, RFC 3339.
    pub created_at: String,
}

impl From<sandesh_core::Rule> for RuleDto {
    fn from(rule: sandesh_core::Rule) -> Self {
        let sandesh_core::RuleAction::AddKeyword(keyword) = rule.action;
        Self {
            id: rule.id.to_string(),
            name: rule.name,
            conditions: rule.conditions.into_iter().map(Into::into).collect(),
            add_keyword: keyword.to_string(),
            enabled: rule.enabled,
            created_at: rfc3339(rule.created_at),
        }
    }
}

/// A request to create a rule (`POST /api/v1/rules`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NewRuleDto {
    /// A human name, shown wherever the rule is reported.
    pub name: String,
    /// Every condition must hold. An empty list matches nothing, not
    /// everything — a half-configured rule must not tag all mail.
    pub conditions: Vec<RuleConditionDto>,
    /// The keyword to add. Must be a valid IMAP keyword.
    pub add_keyword: String,
    /// Whether to start enabled. Defaults to true.
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// One rule firing, as reported on a delivery.
///
/// Carries the rule's name as it stood when it fired, not a live lookup: a rule
/// edited or deleted afterwards must not rewrite history (ADR-087).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleMatchDto {
    /// Which rule fired.
    pub rule_id: String,
    /// Its name at the time.
    pub rule_name: String,
    /// The keyword it added.
    pub keyword: String,
}

impl From<sandesh_core::RuleMatch> for RuleMatchDto {
    fn from(applied: sandesh_core::RuleMatch) -> Self {
        Self {
            rule_id: applied.rule_id.to_string(),
            rule_name: applied.rule_name,
            keyword: applied.keyword.to_string(),
        }
    }
}

/// The links and images one message's HTML parts reference.
///
/// **Nothing here was fetched.** Sandesh enumerates the URLs a message
/// contains and never resolves them — see [`LinkReportDto::disclaimer`], which
/// is part of the payload so a client cannot render the list as if the
/// destinations had been checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct LinkReportDto {
    /// Every URL found, in document order, parts in tree order. Duplicates are
    /// preserved.
    pub links: Vec<ExtractedLinkDto>,
    /// Distinct hosts referenced, lowercased and sorted.
    pub hosts: Vec<String>,
    /// Count of `<a>`-style hyperlinks.
    pub link_count: usize,
    /// Count of images the client would load.
    pub image_count: usize,
    /// Count of other referenced resources (scripts, stylesheets, frames).
    pub resource_count: usize,
    /// A fixed statement that no URL here was resolved.
    pub disclaimer: String,
}

/// The statement that accompanies every link report.
const LINK_DISCLAIMER: &str = "URLs are extracted from the message's HTML as written. Sandesh performs no network \
     resolution: no URL here has been fetched, and no claim is made that any destination exists, \
     resolves, or is safe.";

/// One URL found in an HTML part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ExtractedLinkDto {
    /// The URL as the message wrote it, with HTML entities decoded. Not
    /// normalised or resolved against a base.
    pub url: String,
    /// `link`, `image` or `resource`.
    pub kind: String,
    /// The lowercased element that carried it, e.g. `a`, `img`.
    pub element: String,
    /// The lowercased attribute that carried it, e.g. `href`, `src`.
    pub attribute: String,
    /// Visible link text, for anchors with any.
    pub text: Option<String>,
    /// The `alt` attribute, for images that declare one.
    pub alt: Option<String>,
    /// The lowercased scheme, when the URL has one.
    pub scheme: Option<String>,
    /// The lowercased host, when the URL has an authority component.
    pub host: Option<String>,
    /// The dotted path of the HTML part this URL came from.
    pub part_path: String,
    /// Whether the content is carried by the message itself (`cid:`, `data:`)
    /// rather than fetched.
    pub embedded: bool,
    /// Whether loading it would use plain `http:`.
    pub insecure: bool,
}

impl LinkReportDto {
    /// Project a core report.
    #[must_use]
    pub fn from_core(report: &sandesh_core::LinkReport) -> Self {
        let (link_count, image_count, resource_count) = report.counts();
        Self {
            links: report
                .links
                .iter()
                .map(ExtractedLinkDto::from_core)
                .collect(),
            hosts: report.hosts(),
            link_count,
            image_count,
            resource_count,
            disclaimer: LINK_DISCLAIMER.to_string(),
        }
    }
}

impl ExtractedLinkDto {
    /// Project one core link.
    #[must_use]
    pub fn from_core(link: &sandesh_core::ExtractedLink) -> Self {
        Self {
            url: link.url.clone(),
            kind: link.kind.as_str().to_string(),
            element: link.element.clone(),
            attribute: link.attribute.clone(),
            text: link.text.clone(),
            alt: link.alt.clone(),
            scheme: link.scheme.clone(),
            host: link.host.clone(),
            part_path: link.part_path.clone(),
            embedded: link.is_embedded(),
            insecure: link.is_insecure(),
        }
    }
}

/// A workspace summary returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceDto {
    /// Slug identifier (e.g. "default", "billing-service").
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Created timestamp formatted as RFC 3339 UTC.
    pub created_at: String,
    /// Whether this workspace is currently active.
    pub is_active: bool,
    /// Whether this workspace requires authentication for SMTP/IMAP/POP3.
    pub auth_required: bool,
    /// Total count of messages currently in this workspace.
    pub message_count: u64,
    /// Size in bytes on disk for this workspace's database and blobs.
    pub size_bytes: u64,
    /// Database file path on disk.
    pub database_path: String,
}

impl WorkspaceDto {
    /// Converts from a core workspace.
    #[must_use]
    pub fn from_core(ws: &sandesh_core::Workspace) -> Self {
        Self {
            id: ws.id.clone(),
            name: ws.name.clone(),
            description: ws.description.clone(),
            created_at: rfc3339(ws.created_at),
            is_active: ws.is_active,
            auth_required: ws.auth_required,
            message_count: ws.message_count,
            size_bytes: ws.size_bytes,
            database_path: ws.database_path.clone(),
        }
    }
}

/// Initial account configuration when creating a new workspace from scratch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct InitialAccountSetupDto {
    /// Account username (defaults to "dev" if omitted).
    #[serde(default = "default_account_username")]
    pub username: String,
    /// Bound email addresses to associate with this account.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// Whether this account receives all unmatched incoming recipient addresses.
    #[serde(default = "yes")]
    pub is_catch_all: bool,
}

fn default_account_username() -> String {
    "dev".to_string()
}

/// Request payload to create a new workspace.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct CreateWorkspaceRequest {
    /// Display name.
    pub name: String,
    /// Optional slug identifier. If omitted, generated from name.
    pub id: Option<String>,
    /// Optional description.
    pub description: Option<String>,
    /// Optional source workspace identifier to clone accounts and routing rules from.
    #[serde(default)]
    pub clone_from: Option<String>,
    /// Optional password for initial account authentication.
    #[serde(default)]
    pub password: Option<String>,
    /// Whether to require authentication for SMTP/IMAP/POP3 in this workspace.
    #[serde(default)]
    pub auth_required: bool,
    /// Optional initial account setup when not cloning from an existing workspace.
    #[serde(default)]
    pub initial_account: Option<InitialAccountSetupDto>,
    /// Whether to immediately switch to this workspace upon creation.
    #[serde(default)]
    pub set_active: bool,
}

/// Request payload to update an existing workspace.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct UpdateWorkspaceRequest {
    /// Updated display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Updated description (null or omitted preserves/clears).
    #[serde(default)]
    pub description: Option<Option<String>>,
    /// Updated authentication requirement toggle.
    #[serde(default)]
    pub auth_required: Option<bool>,
}

/// Options for purging data in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct PurgeWorkspaceRequest {
    /// Purge mode: `mail_and_traces` (default) or `factory_reset`.
    #[serde(default = "default_purge_mode")]
    pub mode: String,
    /// Whether to clear protocol connection traces.
    #[serde(default = "yes")]
    pub clear_traces: bool,
    /// Whether to clear webhook outbox logs.
    #[serde(default = "yes")]
    pub clear_webhooks: bool,
}

fn default_purge_mode() -> String {
    "mail_and_traces".to_string()
}

/// Summary returned after a purge operation completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PurgeResultDto {
    /// The workspace that was purged.
    pub workspace_id: String,
    /// Number of messages deleted.
    pub deleted_messages: u64,
    /// Number of connection traces deleted.
    pub deleted_traces: u64,
    /// Reclaimed disk bytes.
    pub reclaimed_bytes: u64,
    /// Purge timestamp formatted as RFC 3339 UTC.
    pub purged_at: String,
}

impl PurgeResultDto {
    /// Converts from a core `PurgeResult`.
    #[must_use]
    pub fn from_core(res: &sandesh_core::PurgeResult) -> Self {
        Self {
            workspace_id: res.workspace_id.clone(),
            deleted_messages: res.deleted_messages,
            deleted_traces: res.deleted_traces,
            reclaimed_bytes: res.reclaimed_bytes,
            purged_at: rfc3339(res.purged_at),
        }
    }
}

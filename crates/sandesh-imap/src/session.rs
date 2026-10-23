//! One IMAP connection, from greeting to close.
//!
//! # What this module owns
//!
//! The command loop: read a line, parse it, ask the state machine whether it is
//! allowed, run it, write a tagged response. It owns the connection and the
//! session's mutable state, and it is the only place that performs I/O.
//!
//! ```text
//! Connection ──▶ parse_line ──▶ transition ──▶ handler ──▶ response
//!      ▲                            │
//!      └──── continuation (+) ◀──────┘  (literals, SASL challenges)
//! ```
//!
//! # What it deliberately does not own
//!
//! *Policy.* Whether a command is legal in the current state is
//! [`crate::transition`]; whether `AUTH` may be offered is
//! [`sandesh_config::AuthMode::is_offered_over`]; whether a password matches is
//! [`sandesh_proto::verify_optional`]. This module sequences those decisions —
//! it does not re-make them. That is what keeps a second, subtly different
//! security policy from growing here (phase brief §6).
//!
//! # Scope of this increment
//!
//! `CAPABILITY`, `NOOP`, `ID`, `STARTTLS`, `AUTHENTICATE`, `LOGIN`, `LOGOUT`,
//! the mailbox commands — `SELECT`, `EXAMINE`, `LIST`, `STATUS`, `CREATE`,
//! `DELETE`, `RENAME`, `SUBSCRIBE`, `UNSUBSCRIBE`, `CLOSE`/`UNSELECT` — and
//! `FETCH`/`UID FETCH`, `STORE`/`UID STORE`, `SEARCH`/`UID SEARCH`,
//! `EXPUNGE`/`UID EXPUNGE`, `COPY`/`UID COPY`, `MOVE`/`UID MOVE`, `APPEND`
//! and `IDLE`.
//!
//! `NAMESPACE` (RFC 2342) and `ENABLE` (RFC 5161) join them, and both are
//! advertised. `ENABLE` is real but currently enables nothing: no extension
//! this server recognises is implemented yet, so it answers `OK` with an empty
//! `* ENABLED` (RFC 5161 §3.1). Every command the state machine permits is now
//! implemented — there is no longer a parsed-but-refused verb.

use std::collections::BTreeSet;
use std::sync::Arc;

use sandesh_config::ImapConfig;
use sandesh_core::{
    Account, AccountId, AccountStore, CoreError, DeliverySource, DomainEvent, EntryId, Envelope,
    EventSource, EventStream, FlagUpdate, HIERARCHY_DELIMITER, Mailbox, MailboxAttributes,
    MailboxCounts, MailboxId, MailboxName, MailboxService, MailboxStore, MessageEntry,
    MessageFlags, MessageStore, ModSeq, NewMessage, NewPlacement, RawMessage,
    SPECIAL_USE_ATTRIBUTES, SystemFlags, Timestamp, Uid, UidValidity,
};
use sandesh_proto::{
    Connection, Credentials, Mechanism, ProtocolError, SaslError, Secret, decode_login_field,
    decode_plain, login_prompts, verify_optional,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::append::AppendRequest;
use crate::command::{Command, CommandKind, Tag, Token};
use crate::copy::CopyResult;
use crate::expunge::ExpungePlan;
use crate::fetch::{FetchItem, FetchRequest};
use crate::fetch_response::{self, MessageData};
use crate::idle::{self, Update};
use crate::mailbox::{self, SequenceMap};
use crate::move_entry::{MovePlan, Moving};
use crate::parser::{ParseError, Parsed, parse_line};
use crate::response::{self, Capabilities, Status};
use crate::search;
use crate::sequence::SequenceSet;
use crate::state::{Refusal, State, Transition, transition};
use crate::store::StoreRequest;

/// Why a session ended.
///
/// `UpgradeToTls` is not a failure: it is how the session tells the server that
/// the client accepted `STARTTLS` and the socket must now be handed to the TLS
/// handshake. Returning it rather than performing the handshake here is what
/// keeps `rustls` out of this module (ADR-036 applies the same split to SMTP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The connection is finished.
    Closed,
    /// The client issued `STARTTLS` and was told to proceed.
    UpgradeToTls,
}

/// Whether the loop continues after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop(Outcome),
}

/// How an `IDLE` ended.
///
/// A disconnect is not here: it arrives as an error from the read and ends the
/// session, which is the correct handling of a client that vanished mid-IDLE.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IdleEnd {
    /// The client sent `DONE` (RFC 2177 §3).
    Done,
    /// The server ended an IDLE the client did not renew.
    Timeout,
    /// The client sent something other than `DONE`, truncated for echoing.
    Unexpected(String),
}

/// The mailbox this session has selected, and everything later message
/// commands need about it.
///
/// Held per session, never globally: two connections selecting the same
/// mailbox get two independent snapshots, which is what makes concurrent
/// sessions safe (phase brief §16).
#[derive(Debug, Clone)]
struct SelectedMailbox {
    /// Identity of the selected mailbox.
    mailbox_id: MailboxId,
    /// Its name, kept current across a `RENAME`.
    name: MailboxName,
    /// The UID epoch this snapshot belongs to.
    uid_validity: UidValidity,
    /// The canonical sequence-number ↔ UID mapping.
    sequences: SequenceMap,
    /// Whether the mailbox was opened with `EXAMINE`.
    read_only: bool,
}

impl SelectedMailbox {
    /// The UID epoch this snapshot belongs to.
    ///
    /// Checked before acting on a client's cached UIDs: a snapshot taken
    /// under a stale validity must not be used to resolve them.
    #[allow(dead_code)]
    const fn uid_validity(&self) -> UidValidity {
        self.uid_validity
    }

    /// The canonical sequence-number ↔ UID mapping (phase brief §12).
    ///
    /// `FETCH` resolves message identifiers through this and nowhere else;
    /// the later message commands must do the same.
    const fn sequences(&self) -> &SequenceMap {
        &self.sequences
    }

    /// The canonical mapping, for the one caller that must change it while a
    /// command is not running: `IDLE` applying an unsolicited update.
    ///
    /// Mutable access is deliberately narrow. Every *command* goes through
    /// [`record_flags`](Self::record_flags) or
    /// [`remove_expunged`](Self::remove_expunged), which state what they change
    /// and why; this exists so `crate::idle` can make the same two kinds of
    /// change plus an insertion, in one place, paired with the response line
    /// that announces it.
    fn sequences_mut(&mut self) -> &mut SequenceMap {
        &mut self.sequences
    }

    /// Whether this selection refuses mutation (`EXAMINE`).
    ///
    /// The state machine already refuses mutating commands on a read-only
    /// selection; this is the same fact where a handler needs it directly.
    const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Records a placement's new flags in this session's snapshot.
    ///
    /// Both writers of flags call this: `FETCH BODY[...]` when it sets `\Seen`,
    /// and `STORE` for any mutation. Storage is authoritative — the value
    /// passed here is whatever `MessageStore::update_flags` returned — and this
    /// keeps the snapshot in step so a subsequent `FETCH FLAGS` in the *same*
    /// session reports what is actually stored rather than the flags captured
    /// at `SELECT` (ADR-047).
    ///
    /// It updates only the one placement, and only its flags and modification
    /// sequence. The snapshot's *membership* and ordering stay fixed at
    /// selection time (ADR-046); changing them here would renumber messages
    /// under a client that is mid-conversation.
    ///
    /// The sequence travels with the flags because storage allocates one for
    /// exactly these mutations; see [`SequenceMap::set_flags`].
    fn record_flags(&mut self, uid: Uid, flags: MessageFlags, mod_seq: Option<ModSeq>) {
        self.sequences.set_flags(uid, flags, mod_seq);
    }

    /// Removes expunged placements from this session's snapshot and renumbers
    /// what remains (ADR-049).
    ///
    /// The one operation that changes a snapshot's *membership*, and the one
    /// place `EXPUNGE` differs from every other command in this module.
    /// [`record_flags`](Self::record_flags) is careful not to renumber; this
    /// must, because the client has just been told each removal as
    /// `* <seq> EXPUNGE` and its next `FETCH 1:*` addresses the mailbox as it
    /// now is. Leaving the snapshot alone would make every subsequent sequence
    /// number in this session refer to the wrong message.
    ///
    /// UIDs are untouched: they are stable identities, so `UID FETCH` keeps
    /// working across an expunge exactly as it did before.
    fn remove_expunged(&mut self, uids: &BTreeSet<u32>) -> usize {
        self.sequences.remove_uids(uids)
    }
}

/// The `FLAGS` line, identical for every mailbox this server serves.
const FLAGS_LINE: &str = "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n";

/// `PERMANENTFLAGS` for a writable selection.
const PERMANENTFLAGS_READ_WRITE: &str =
    "* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] Flags permitted\r\n";

/// `PERMANENTFLAGS` for a read-only selection.
///
/// Empty, because an `EXAMINE`d mailbox permits no flag change at all — RFC
/// 9051 §6.3.3. Reporting the writable list here would invite a client to
/// attempt a `STORE` this server must refuse.
const PERMANENTFLAGS_READ_ONLY: &str = "* OK [PERMANENTFLAGS ()] No permanent flags permitted\r\n";

/// The `LIST` name attributes for a mailbox.
///
/// Three sources, combined here so every `LIST` response — plain or extended —
/// derives its attributes from one place:
///
/// * stored attributes, which carry the RFC 6154 special uses (`\Sent`,
///   `\Trash`, …) and `\Noselect`;
/// * `\HasChildren` or `\HasNoChildren` (RFC 5258 §3), computed from
///   `siblings` rather than stored, because being a parent is a fact about the
///   mailbox *set* and would otherwise need updating on every create, delete
///   and rename;
/// * `\Subscribed` (RFC 5258 §3.1), included only when `with_subscribed` — a
///   client that did not ask for it must not receive it.
///
/// `siblings` is the full mailbox list for the account, not the filtered one:
/// a child excluded by the pattern still makes its parent a parent. Filtering
/// first would report `\HasNoChildren` for a mailbox that plainly has children,
/// which is worse than omitting the attribute.
fn name_attributes(mailbox: &Mailbox, siblings: &[Mailbox], with_subscribed: bool) -> Vec<String> {
    let mut attributes: Vec<String> = mailbox.attributes.iter().map(str::to_owned).collect();

    attributes.push(String::from(if has_children(mailbox, siblings) {
        r"\HasChildren"
    } else {
        r"\HasNoChildren"
    }));

    if with_subscribed && mailbox.subscribed {
        attributes.push(String::from(r"\Subscribed"));
    }

    attributes.sort();
    attributes.dedup();
    attributes
}

/// Whether any mailbox in `siblings` is a child of `mailbox`.
///
/// A child is a name with the parent's name plus the delimiter as its prefix —
/// `Work/Deep` is a child of `Work`, at any depth. `Workshop` is not, which is
/// why the delimiter is part of the prefix rather than matching the bare name.
fn has_children(mailbox: &Mailbox, siblings: &[Mailbox]) -> bool {
    let prefix = format!("{}{HIERARCHY_DELIMITER}", mailbox.name.as_str());
    siblings
        .iter()
        .any(|candidate| candidate.name.as_str().starts_with(&prefix))
}

/// The `LIST` selection and return options of RFC 5258 / RFC 6154.
///
/// Parsed away from the handler so the command body stays the reference and
/// pattern resolution plain `LIST` already had. Every field is a decision the
/// client made explicitly; nothing here is inferred.
struct ListOptions<'args> {
    /// Restrict the listing to subscribed mailboxes (RFC 5258 §3.1).
    only_subscribed: bool,
    /// Restrict the listing to special-use mailboxes (RFC 6154 §3).
    special_use_only: bool,
    /// Report `\Subscribed` on each result (RFC 5258 §3.2).
    return_subscribed: bool,
    /// `STATUS` items to report per mailbox, validated by [`StatusItem`].
    status_items: Vec<StatusItem>,
    /// The arguments after the selection options: reference, pattern, and any
    /// `RETURN` clause already consumed.
    remaining: &'args [Token],
}

impl<'args> ListOptions<'args> {
    /// Parses both option groups from a `LIST` argument list.
    ///
    /// # Errors
    ///
    /// A message for the `BAD` response. Every unsupported option is refused
    /// rather than ignored: a selection option changes *which* mailboxes are
    /// listed and a return option changes *what is reported about them*, so
    /// silently dropping either answers a different question in a response the
    /// client cannot audit.
    fn parse(args: &'args [Token]) -> Result<Self, String> {
        let mut options = Self {
            only_subscribed: false,
            special_use_only: false,
            return_subscribed: false,
            status_items: Vec::new(),
            remaining: args,
        };

        if let Some(Token::List(selection)) = args.first() {
            for option in selection {
                let Some(name) = option.as_text() else {
                    return Err(String::from("LIST selection options must be atoms"));
                };
                match name.to_ascii_uppercase().as_str() {
                    // RFC 5258 §3.1: this is `LSUB` expressed properly, and is
                    // the rev2 replacement for it.
                    "SUBSCRIBED" => options.only_subscribed = true,
                    "SPECIAL-USE" => options.special_use_only = true,
                    // `RECURSIVEMATCH` and `REMOTE` each change which
                    // mailboxes are listed, so neither may be ignored.
                    other => {
                        return Err(format!("unsupported LIST selection option: {other}"));
                    }
                }
            }
            options.remaining = &args[1..];
        }

        options.parse_return()?;
        Ok(options)
    }

    /// Parses the `RETURN (...)` clause following the pattern (RFC 5258 §3.2).
    fn parse_return(&mut self) -> Result<(), String> {
        let args = self.remaining;
        let Some(keyword) = next_argument(args, 2) else {
            return Ok(());
        };
        if !keyword
            .as_text()
            .is_some_and(|text| text.eq_ignore_ascii_case("RETURN"))
        {
            return Err(String::from("unexpected argument after LIST pattern"));
        }
        let Some(Token::List(requested)) = next_argument(args, 3) else {
            return Err(String::from(
                "LIST RETURN requires a parenthesised option list",
            ));
        };

        let mut index = 0;
        while index < requested.len() {
            let Some(name) = requested[index].as_text() else {
                return Err(String::from("LIST RETURN options must be atoms"));
            };
            match name.to_ascii_uppercase().as_str() {
                "SUBSCRIBED" => self.return_subscribed = true,
                // `CHILDREN` sets no flag: `\HasChildren`/`\HasNoChildren` are
                // computed for every listing, because RFC 5258 §3.2 permits a
                // server to return them unconditionally and a client that did
                // not ask is not harmed by knowing the hierarchy shape. The
                // option is still *accepted* so a client that asks is not
                // refused — it simply requests what already happens.
                "CHILDREN" => {}
                // `STATUS` takes its own parenthesised item list, validated by
                // the *same* `StatusItem::parse` the `STATUS` command uses — so
                // `RETURN (STATUS (RECENT))` is refused for exactly the reason
                // `STATUS RECENT` is.
                "STATUS" => {
                    index += 1;
                    let Some(Token::List(items)) = requested.get(index) else {
                        return Err(String::from("LIST RETURN STATUS requires an item list"));
                    };
                    for item in items {
                        let Some(parsed) = item.as_text().and_then(StatusItem::parse) else {
                            return Err(String::from("unsupported STATUS item in LIST RETURN"));
                        };
                        if !self.status_items.contains(&parsed) {
                            self.status_items.push(parsed);
                        }
                    }
                }
                other => return Err(format!("unsupported LIST RETURN option: {other}")),
            }
            index += 1;
        }
        Ok(())
    }

    /// Whether `mailbox` belongs in the listing for `pattern`.
    fn selects(&self, pattern: &str, mailbox: &Mailbox) -> bool {
        mailbox::matches_pattern(pattern, mailbox.name.as_str())
            && (!self.only_subscribed || mailbox.subscribed)
            && (!self.special_use_only || mailbox.attributes.has_special_use())
    }

    /// Whether `\Subscribed` should appear on each result.
    ///
    /// True through either route: `RETURN (SUBSCRIBED)` requests the attribute,
    /// and a `(SUBSCRIBED)` selection already implies every result carries it
    /// (RFC 5258 §3.1).
    const fn reports_subscribed(&self) -> bool {
        self.return_subscribed || self.only_subscribed
    }
}

/// The argument at `index`, skipping the line parser's trailing terminator.
///
/// The tokeniser leaves the command line's `CRLF` attached as a final atom, so
/// a handler reading optional trailing arguments by position would see it as
/// one. Every optional-argument check goes through this rather than
/// `args.get`, so a client sending nothing extra is not told it sent junk.
fn next_argument(args: &[Token], index: usize) -> Option<&Token> {
    let argument = args.get(index)?;
    let is_terminator = argument
        .as_text()
        .is_some_and(|text| text.trim_matches(|c| c == '\r' || c == '\n').is_empty());
    if is_terminator { None } else { Some(argument) }
}

/// Renders the `ESEARCH` return data for `results` (RFC 9051 §7.3.4).
///
/// `results` are the numbers `SEARCH` already computed — sequence numbers or
/// UIDs according to the command — so every option here is a *view* of one
/// result set rather than a second search.
///
/// # Empty results
///
/// RFC 9051 §7.3.4 omits a key rather than sending it empty: there is no
/// minimum of nothing, and no set naming no messages. So `MIN`, `MAX` and
/// `ALL` disappear when nothing matched, leaving `* ESEARCH (TAG "A1")` — or
/// `COUNT 0` if it was asked for, because zero *is* a correct count. Sending
/// `MIN 0` instead would name a message, since 0 is not a valid sequence
/// number or UID.
fn esearch_items(returns: &search::ReturnOptions, results: &[u32]) -> Vec<String> {
    use search::ReturnOption;

    let mut items = Vec::new();
    for option in returns.in_response_order() {
        match option {
            // `results` is built in ascending order by the search loop, but
            // min/max are taken rather than indexed so this stays correct if
            // that ever changes.
            ReturnOption::Min => {
                if let Some(min) = results.iter().min() {
                    items.push(format!("MIN {min}"));
                }
            }
            ReturnOption::Max => {
                if let Some(max) = results.iter().max() {
                    items.push(format!("MAX {max}"));
                }
            }
            ReturnOption::All => {
                let set = SequenceSet::from_numbers(results);
                if !set.is_empty() {
                    items.push(format!("ALL {set}"));
                }
            }
            // Always present, including as `COUNT 0`.
            ReturnOption::Count => items.push(format!("COUNT {}", results.len())),
        }
    }
    items
}

/// The tagged completion for a `STORE`, carrying `MODIFIED` when needed.
///
/// RFC 7162 §3.2.5: a conditional store that refused some messages still
/// succeeds — it is a report, not an error. The refused UIDs travel in the
/// response code so the client knows exactly which of its changes did not
/// happen, coalesced by [`SequenceSet::from_numbers`] rather than by any
/// second set-formatting path (ADR-057).
fn store_reply(command: &Command, modified: &[u32]) -> String {
    let summary = format!(
        "{}{} completed",
        if command.uid_mode { "UID " } else { "" },
        command.kind
    );
    if modified.is_empty() {
        return response::tagged(&command.tag, Status::Ok, &summary);
    }
    let set = SequenceSet::from_numbers(modified);
    response::tagged_with_code(
        &command.tag,
        Status::Ok,
        &format!("MODIFIED {set}"),
        &summary,
    )
}

/// Builds a mailbox for `CREATE`.
///
/// UIDVALIDITY and UIDNEXT come from their domain defaults: a fresh mailbox
/// starts a new UID epoch, and its first UID is 1 (never 0, which
/// [`sandesh_core::Uid`] cannot represent). HIGHESTMODSEQ likewise starts at
/// its floor: a mailbox that has never changed has nothing above 1 to report
/// A `STATUS` data item this server can answer truthfully (RFC 9051 §6.3.11).
///
/// `RECENT` is deliberately absent. `IMAP4rev2` removed it, this server keeps
/// no `\Recent` state, and reporting a constant would be a fabricated value
/// (phase brief §6). A client asking for it gets `BAD`, which is honest,
/// rather than `0`, which is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusItem {
    Messages,
    UidNext,
    UidValidity,
    Unseen,
    Size,
    /// `HIGHESTMODSEQ` (RFC 7162 §3.1.2).
    HighestModSeq,
}

impl StatusItem {
    /// Parses an item name, case-insensitively.
    fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "MESSAGES" => Some(Self::Messages),
            "UIDNEXT" => Some(Self::UidNext),
            "UIDVALIDITY" => Some(Self::UidValidity),
            "UNSEEN" => Some(Self::Unseen),
            "SIZE" => Some(Self::Size),
            "HIGHESTMODSEQ" => Some(Self::HighestModSeq),
            _ => None,
        }
    }

    /// Renders `NAME value` from the mailbox and its counts.
    fn render(self, mailbox: &Mailbox, counts: MailboxCounts) -> String {
        match self {
            Self::Messages => format!("MESSAGES {}", counts.total),
            Self::UidNext => format!("UIDNEXT {}", mailbox.uid_next.get()),
            Self::UidValidity => format!("UIDVALIDITY {}", mailbox.uid_validity.get()),
            Self::Unseen => format!("UNSEEN {}", counts.unseen),
            // RFC 9051 §6.3.11 SIZE is the mailbox's total octets. Not tracked
            // separately, so it is reported as 0 rather than guessed.
            Self::Size => String::from("SIZE 0"),
            // RFC 7162 §3.1.2. Taken from the counts, which read the mailbox's
            // own high-water mark, so `STATUS` and `SELECT` cannot disagree.
            Self::HighestModSeq => format!("HIGHESTMODSEQ {}", counts.highest_mod_seq.get()),
        }
    }
}

/// One IMAP session over one connection.
pub struct Session<R, W> {
    connection: Connection<R, W>,
    config: Arc<ImapConfig>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Option<Arc<dyn MailboxStore>>,
    messages_store: Option<Arc<dyn MessageStore>>,
    state: State,
    /// True once TLS is established, by either route.
    encrypted: bool,
    /// The authenticated username, for logging. Never a password.
    identity: Option<String>,
    /// Extensions this connection has enabled with `ENABLE` (RFC 5161 §3).
    ///
    /// Per session by construction. Nothing outside this struct holds a
    /// reference, so one client's `ENABLE` cannot change what another is sent
    /// — the isolation is structural rather than a rule to remember.
    enabled: crate::enable::Enabled,
    peer: Option<String>,
    selected_mailbox: Option<SelectedMailbox>,
    /// The live event stream, when the composition root supplied one.
    ///
    /// Optional for the same reason the stores are: a session can be
    /// constructed without one, and then simply has no `IDLE` to offer.
    events: Option<Arc<dyn EventSource>>,
    /// The full-text index, when one was supplied.
    ///
    /// Purely an optimisation: it narrows which messages `SEARCH` loads and
    /// never which ones it returns. `None` is the pre-index behaviour, a full
    /// scan, and must stay indistinguishable in results (`STORAGE_DESIGN.md` §7).
    search_index: Option<Arc<dyn sandesh_core::SearchIndex>>,
}

impl<R, W> std::fmt::Debug for Session<R, W> {
    /// Renders no credential and no connection internals.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("encrypted", &self.encrypted)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl<R, W> Session<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Creates a session.
    pub fn new(
        connection: Connection<R, W>,
        config: Arc<ImapConfig>,
        accounts: Arc<dyn AccountStore>,
        encrypted: bool,
    ) -> Self {
        Self::with_stores(connection, config, accounts, None, None, encrypted)
    }

    /// Creates a session with mailbox and message stores.
    pub fn with_stores(
        connection: Connection<R, W>,
        config: Arc<ImapConfig>,
        accounts: Arc<dyn AccountStore>,
        mailboxes: Option<Arc<dyn MailboxStore>>,
        messages_store: Option<Arc<dyn MessageStore>>,
        encrypted: bool,
    ) -> Self {
        let peer = connection.peer().map(|addr| addr.to_string());
        Self {
            connection,
            config,
            accounts,
            mailboxes,
            messages_store,
            state: State::NotAuthenticated,
            encrypted,
            identity: None,
            enabled: crate::enable::Enabled::new(),
            peer,
            selected_mailbox: None,
            events: None,
            search_index: None,
        }
    }

    /// Supplies the live event stream that `IDLE` reports from.
    ///
    /// A builder step rather than a constructor parameter: a session without
    /// one is entirely valid — it simply refuses `IDLE` — and threading an
    /// `Option` through every call site would say less about intent than this
    /// does. The port is `sandesh_core::EventSource`, so this crate still knows
    /// nothing about the bus that implements it.
    #[must_use]
    pub fn with_events(mut self, events: Option<Arc<dyn EventSource>>) -> Self {
        self.events = events;
        self
    }

    /// Attaches a full-text index for `SEARCH` to narrow with.
    ///
    /// A builder step for the same reason as [`Self::with_events`], and
    /// optional for a stronger one: the index may never change a result, so a
    /// session without one must behave identically and only more slowly.
    #[must_use]
    pub fn with_search_index(
        mut self,
        search_index: Option<Arc<dyn sandesh_core::SearchIndex>>,
    ) -> Self {
        self.search_index = search_index;
        self
    }

    /// What this connection may currently do.
    ///
    /// Recomputed on every use rather than cached, so it cannot fall out of
    /// step with the encryption state after a `STARTTLS` upgrade (phase brief
    /// §5). `start_tls` is reported as configured; [`Capabilities::new`]
    /// suppresses it once encrypted.
    fn capabilities(&self) -> Capabilities {
        Capabilities::new(self.config.auth_mode, self.config.start_tls, self.encrypted)
            .with_idle(self.events.is_some())
    }

    /// Runs the session to completion.
    ///
    /// `greet` is false only on the second half of a `STARTTLS` upgrade, where
    /// the pre-handshake greeting already happened.
    ///
    /// # Errors
    /// Propagates an unrecoverable [`ProtocolError`]. A *recoverable* one — an
    /// over-long line — is answered with `BAD` and the session continues, which
    /// is what RFC 9051 §7.1.3 expects.
    pub async fn run(mut self, greet: bool) -> Result<Outcome, ProtocolError> {
        if greet {
            let greeting = response::greeting(self.capabilities(), &self.config.hostname);
            self.connection.write_flush(greeting.as_bytes()).await?;
        }

        loop {
            let line = match self
                .connection
                .read_line_owned(self.config.command_timeout)
                .await
            {
                Ok(line) => line,
                Err(error) if error.is_disconnect() => return Ok(Outcome::Closed),
                Err(error) if error.is_recoverable() => {
                    // An over-long command line. The tag is unknown — it may
                    // never have arrived — so the refusal is untagged, and the
                    // buffered remainder is dropped so the next read starts at
                    // a line boundary rather than mid-command.
                    self.connection.discard_input();
                    let reply = response::untagged_bad("command line too long");
                    self.connection.write_flush(reply.as_bytes()).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };

            match self.handle_line(&line).await? {
                Flow::Continue => {}
                Flow::Stop(outcome) => return Ok(outcome),
            }
        }
    }

    /// Parses and dispatches one command line, including any literals.
    async fn handle_line(&mut self, line: &[u8]) -> Result<Flow, ProtocolError> {
        let Some(command) = self.read_command(line).await? else {
            return Ok(Flow::Continue);
        };
        self.dispatch(command).await
    }

    /// Assembles a complete command, reading literal payloads as needed.
    ///
    /// Returns `None` when the input was rejected and a response already sent.
    ///
    /// This is where `Parsed::NeedLiteral` meets the network: the parser stops
    /// at a literal, this sends the continuation request, reads *exactly* the
    /// declared number of octets, and resumes. The declared length was already
    /// checked against `max_literal_size` by the parser, so the read below is
    /// bounded before it is attempted (ADR-040).
    async fn read_command(&mut self, line: &[u8]) -> Result<Option<Command>, ProtocolError> {
        let mut parsed = match parse_line(line, self.config.max_literal_size) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.reject_parse(line, &error).await?;
                return Ok(None);
            }
        };

        loop {
            match parsed {
                Parsed::Complete(command) => return Ok(Some(command)),
                Parsed::NeedLiteral(pending) => {
                    // A non-synchronising literal (`{n+}`, RFC 7888) must NOT
                    // be answered: the client is already sending the octets, so
                    // a continuation here would desynchronise the stream.
                    if !pending.is_non_synchronising() {
                        let request = response::continuation("");
                        self.connection.write_flush(request.as_bytes()).await?;
                    }

                    let payload = match self
                        .connection
                        .read_exact_owned(
                            pending.length(),
                            self.config.max_literal_size,
                            self.config.command_timeout,
                        )
                        .await
                    {
                        Ok(payload) => payload,
                        Err(error) if error.is_disconnect() => return Err(error),
                        Err(error) => return Err(error),
                    };

                    // The remainder of the command follows the literal on the
                    // same logical line.
                    let rest = match self
                        .connection
                        .read_line_owned(self.config.command_timeout)
                        .await
                    {
                        Ok(rest) => rest,
                        Err(error) => return Err(error),
                    };

                    parsed = match pending.resume(payload, &rest, self.config.max_literal_size) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            self.reject_parse(line, &error).await?;
                            return Ok(None);
                        }
                    };
                }
            }
        }
    }

    /// Answers a parse failure.
    ///
    /// The tag is echoed when it can be recovered, because a client matches
    /// responses to commands by tag and an untagged `BAD` leaves the command
    /// outstanding forever. When the tag itself was the problem there is
    /// nothing safe to echo, so the response is untagged.
    async fn reject_parse(&mut self, line: &[u8], error: &ParseError) -> Result<(), ProtocolError> {
        let text = error.to_string();
        let reply = match recover_tag(line) {
            Some(tag) if !matches!(error, ParseError::InvalidTag) => {
                response::tagged(&tag, Status::Bad, &text)
            }
            _ => response::untagged_bad(&text),
        };
        self.connection.write_flush(reply.as_bytes()).await
    }

    /// Runs one parsed command, after checking it against the state machine.
    async fn dispatch(&mut self, command: Command) -> Result<Flow, ProtocolError> {
        // The state machine decides first, always. No handler re-checks, and
        // none may run before this returns something other than `Rejected`.
        match transition(self.state, command.kind) {
            Transition::Rejected { reason } => {
                self.refuse(&command.tag, reason).await?;
                return Ok(Flow::Continue);
            }
            // Commands reached in this increment carry their own state changes
            // below; SELECT and friends are not implemented yet.
            Transition::Stay
            | Transition::Authenticate
            | Transition::Select { .. }
            | Transition::Deselect
            | Transition::Logout => {}
        }

        match command.kind {
            CommandKind::Capability => self.capability(&command.tag).await,
            CommandKind::Noop => self.noop(&command.tag).await,
            CommandKind::Check => self.check(&command.tag).await,
            CommandKind::Id => self.id(&command.tag).await,
            CommandKind::Logout => self.logout(&command.tag).await,
            CommandKind::StartTls => self.start_tls(&command.tag).await,
            CommandKind::Authenticate => self.authenticate(&command).await,
            CommandKind::Login => self.login(&command).await,
            CommandKind::Select => self.select(&command, false).await,
            CommandKind::Examine => self.select(&command, true).await,
            CommandKind::List => self.list(&command).await,
            CommandKind::Status => self.status(&command).await,
            CommandKind::Create => self.create(&command).await,
            CommandKind::Delete => self.delete(&command).await,
            CommandKind::Rename => self.rename(&command).await,
            CommandKind::Subscribe => self.set_subscribed(&command, true).await,
            CommandKind::Unsubscribe => self.set_subscribed(&command, false).await,
            CommandKind::Close | CommandKind::Unselect => self.close(&command.tag).await,
            CommandKind::Fetch => self.fetch(&command).await,
            CommandKind::Store => self.store(&command).await,
            CommandKind::Search => self.search(&command).await,
            CommandKind::Expunge => self.expunge(&command).await,
            CommandKind::Copy => self.copy(&command).await,
            CommandKind::Move => self.move_messages(&command).await,
            CommandKind::Append => self.append(&command).await,
            CommandKind::Idle => self.idle(&command).await,

            CommandKind::Namespace => self.namespace(&command).await,
            CommandKind::Enable => self.enable(&command).await,
        }
    }

    /// Answers a command refused by the state machine.
    ///
    /// `BAD` for a sequencing error, per RFC 9051 §7.1.3.
    async fn refuse(&mut self, tag: &Tag, reason: Refusal) -> Result<(), ProtocolError> {
        let reply = response::tagged(tag, Status::Bad, reason.message());
        self.connection.write_flush(reply.as_bytes()).await
    }

    /// `CAPABILITY` (RFC 9051 §6.1.1).
    async fn capability(&mut self, tag: &Tag) -> Result<Flow, ProtocolError> {
        let caps = self.capabilities();
        let mut out = response::capability_data(caps);
        out.push_str(&response::tagged(tag, Status::Ok, "CAPABILITY completed"));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `NAMESPACE` (RFC 2342 §5).
    ///
    /// Takes no arguments and needs no storage access: the answer is a
    /// property of the mailbox *model*, not of any account's contents.
    /// [`MailboxStore::list`](sandesh_core::MailboxStore::list) is scoped to a
    /// single `AccountId`, so an authenticated client can reach exactly one
    /// namespace — its own, at no prefix — and there is nothing to look up to
    /// discover that. The response is therefore deterministic, which is what
    /// [`response::namespace_line`] documents and its tests pin.
    ///
    /// Trailing arguments are rejected. RFC 2342 §5 defines `NAMESPACE` with
    /// none, and accepting junk would leave a client believing it had asked a
    /// narrower question than it did.
    async fn namespace(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        if !command.args.is_empty() {
            let reply = response::tagged(&command.tag, Status::Bad, "NAMESPACE takes no arguments");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        let mut out = response::namespace_line(HIERARCHY_DELIMITER);
        out.push_str(&response::tagged(
            &command.tag,
            Status::Ok,
            "NAMESPACE completed",
        ));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `ENABLE` (RFC 5161 §3.1).
    ///
    /// Enables the named capabilities this server actually implements and
    /// reports back exactly those. A name it does not implement is dropped
    /// rather than refused — RFC 5161 §3.1 requires `OK` regardless, and the
    /// capability's absence from `* ENABLED` is the refusal the client reads.
    /// [`crate::enable::Extension::is_implemented`] is the one place that
    /// decides which is which.
    ///
    /// An empty capability list is a syntax error (RFC 5161 §3.1 requires one
    /// or more), and is the only way this command answers anything but `OK`.
    ///
    /// The enabled set lives in `self`, so it dies with the connection and
    /// cannot reach another session.
    async fn enable(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        if command.args.is_empty() {
            let reply = response::tagged(
                &command.tag,
                Status::Bad,
                "ENABLE requires at least one capability",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        // Capability names are atoms (RFC 9051 §9). A list or a literal where
        // one belongs is malformed, and is refused rather than skipped — an
        // ignored argument would look to the client like a capability this
        // server declined, which is a different and misleading answer.
        let mut names: Vec<&str> = Vec::with_capacity(command.args.len());
        for arg in &command.args {
            if let Some(name) = arg.as_text() {
                names.push(name);
            } else {
                let reply = response::tagged(
                    &command.tag,
                    Status::Bad,
                    "ENABLE capability names must be atoms",
                );
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        }
        let enabled = self.enabled.enable(names);

        let mut out = response::enabled_line(&enabled);
        out.push_str(&response::tagged(
            &command.tag,
            Status::Ok,
            "ENABLE completed",
        ));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `NOOP` (RFC 9051 §6.1.2).
    ///
    /// Does nothing, deliberately. Once a mailbox can be selected this is also
    /// where pending untagged updates would be flushed; there are none yet, and
    /// inventing some would be worse than none.
    async fn noop(&mut self, tag: &Tag) -> Result<Flow, ProtocolError> {
        let reply = response::tagged(tag, Status::Ok, "NOOP completed");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `CHECK` (RFC 9051 §6.4.1).
    ///
    /// A checkpoint of the selected mailbox: the RFC's example is a server
    /// writing through in-memory state to disk. Every mutation here is already
    /// committed inside its own transaction before the tagged reply is sent, so
    /// there is nothing deferred to flush and the honest implementation is
    /// `NOOP`'s — which §6.4.1 explicitly permits ("if the server has no such
    /// housekeeping, CHECK is equivalent to NOOP").
    ///
    /// It is answered rather than omitted because real clients send it: `mbsync`
    /// issues `CHECK` after propagating flags, and an unrecognised-command `BAD`
    /// there aborts the sync — found by running the real binary, not by review.
    async fn check(&mut self, tag: &Tag) -> Result<Flow, ProtocolError> {
        let reply = response::tagged(tag, Status::Ok, "CHECK completed");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `ID` (RFC 2971).
    ///
    /// Replies with the server name and nothing else. RFC 2971 §3.3 warns
    /// against version disclosure, and the client's own parameters are
    /// deliberately not logged: they are a fingerprinting surface.
    async fn id(&mut self, tag: &Tag) -> Result<Flow, ProtocolError> {
        let mut out = String::from("* ID (\"name\" \"Sandesh\")\r\n");
        out.push_str(&response::tagged(tag, Status::Ok, "ID completed"));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `LOGOUT` (RFC 9051 §6.1.3).
    ///
    /// Untagged `BYE` first, then the tagged `OK`, in that order — RFC 9051
    /// §7.1.5 requires the `BYE` to precede the completion.
    async fn logout(&mut self, tag: &Tag) -> Result<Flow, ProtocolError> {
        self.state = State::Logout;
        let mut out = response::bye("Sandesh IMAP4rev2 signing off");
        out.push_str(&response::tagged(tag, Status::Ok, "LOGOUT completed"));
        self.connection.write_flush(out.as_bytes()).await?;
        // Flushed above; the caller closes the socket. A half-close here would
        // race the client's read of the response it was just sent.
        Ok(Flow::Stop(Outcome::Closed))
    }

    /// `STARTTLS` (RFC 9051 §6.2.1).
    ///
    /// Refused unless it is actually advertised, so the answer always matches
    /// the capability list. On success the tagged `OK` is the last cleartext
    /// byte written: the caller performs the handshake and starts a *new*
    /// session, discarding everything learned before it (ADR-036).
    async fn start_tls(&mut self, tag: &Tag) -> Result<Flow, ProtocolError> {
        if !self.capabilities().advertises_start_tls() {
            let reply = response::tagged(tag, Status::Bad, "STARTTLS is not available");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        let reply = response::tagged(tag, Status::Ok, "begin TLS negotiation now");
        self.connection.write_flush(reply.as_bytes()).await?;

        // Anything the client pipelined behind STARTTLS is discarded unread. It
        // arrived in cleartext and must not be interpreted after the upgrade —
        // that is the STARTTLS command-injection vulnerability.
        self.connection.discard_input();

        Ok(Flow::Stop(Outcome::UpgradeToTls))
    }

    /// `AUTHENTICATE` (RFC 9051 §6.2.2).
    async fn authenticate(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        // Policy before mechanism: on a connection where AUTH is not offered,
        // the mechanism name is irrelevant and must not be probed for.
        if !self.capabilities().offers_auth() {
            let text = if self.config.auth_mode.is_enabled() {
                "AUTHENTICATE requires TLS"
            } else {
                "authentication is not enabled"
            };
            let reply = response::tagged(&command.tag, Status::No, text);
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        let Some(name) = command.args.first().and_then(Token::as_text) else {
            let reply = response::tagged(
                &command.tag,
                Status::Bad,
                "AUTHENTICATE requires a mechanism",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let Some(mechanism) = Mechanism::parse(name) else {
            // RFC 9051 §6.2.2: an unsupported mechanism is `NO`, and the
            // session stays usable so the client can try another.
            let reply = response::tagged(
                &command.tag,
                Status::No,
                "unsupported authentication mechanism",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // SASL-IR (RFC 4959): the initial response may ride along with the
        // command, saving a round trip. Held as a `Secret` from the first
        // moment, because for PLAIN it *is* the password (ADR-035).
        let initial = command
            .args
            .get(1)
            .and_then(Token::as_text)
            .map(Secret::new);

        let credentials = match mechanism {
            Mechanism::Plain => self.collect_plain(initial).await?,
            Mechanism::Login => self.collect_login(initial).await?,
        };

        match credentials {
            Ok(credentials) => self.verify(&command.tag, credentials).await,
            Err(error) => {
                self.reject_sasl(&command.tag, error).await?;
                Ok(Flow::Continue)
            }
        }
    }

    /// `LOGIN` (RFC 9051 §6.2.3).
    ///
    /// Accepted only where `AUTHENTICATE` would be — the same TLS policy, since
    /// `LOGIN` sends the password in the clear as an argument. When AUTH is
    /// unavailable the capability list says `LOGINDISABLED`, so this refusal is
    /// what the client was already told to expect.
    /// When `AuthMode::Disabled` is configured (developer mode), `LOGIN` is
    /// accepted in cleartext without TLS (ADR-042).
    async fn login(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        if !self.capabilities().offers_auth()
            && self.config.auth_mode != sandesh_config::AuthMode::Disabled
        {
            let text = if self.config.auth_mode.is_enabled() {
                "LOGIN requires TLS"
            } else {
                "authentication is not enabled"
            };
            let reply = response::tagged(&command.tag, Status::No, text);
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        let (Some(username), Some(password)) = (
            command.args.first().and_then(Token::as_text),
            command.args.get(1).and_then(Token::as_text),
        ) else {
            let reply = response::tagged(
                &command.tag,
                Status::Bad,
                "LOGIN requires a username and password",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        if self.config.auth_mode == sandesh_config::AuthMode::Disabled {
            let stored = self.credential_hash(username).await;
            if stored.is_none() {
                self.state = State::Authenticated;
                self.identity = Some(username.to_owned());
                tracing::info!(
                    peer = self.peer.as_deref().unwrap_or("unknown"),
                    username = %username,
                    "IMAP login succeeded (auth disabled)"
                );
                let reply = response::tagged_with_code(
                    &command.tag,
                    Status::Ok,
                    &format!("CAPABILITY {}", self.capabilities().to_line()),
                    "LOGIN completed",
                );
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        }

        let credentials = Credentials {
            username: username.to_owned(),
            password: Secret::new(password),
        };
        self.verify(&command.tag, credentials).await
    }

    /// Resolves the account whose mailboxes this session operates on.
    ///
    /// Sandesh is a single-user developer tool: a session is scoped to exactly
    /// one account and every mailbox command resolves through it. Returning
    /// `None` rather than falling back to "any account" is what stops one
    /// session from reaching another account's mailboxes (phase brief §18).
    async fn current_account(&self) -> Option<Account> {
        if let Some(username) = self.identity.as_deref()
            && let Ok(Some(account)) = self.accounts.get_by_username(username).await
        {
            return Some(account);
        }

        // The catch-all fallback is reachable only when authentication is
        // disabled — developer mode (ADR-042), where `LOGIN` accepts any name
        // and the catch-all account *is* the user, so a name that matches no
        // account still gets a usable mailbox.
        //
        // When authentication is enabled this must never fire: falling back
        // there would hand a session whose identity failed to resolve someone
        // else's mailboxes (phase brief §18).
        if self.config.auth_mode == sandesh_config::AuthMode::Disabled
            && let Ok(Some(account)) = self.accounts.catch_all().await
        {
            return Some(account);
        }
        None
    }

    /// Validates a client-supplied mailbox name argument.
    ///
    /// Returns `None` after answering the client, so the caller returns
    /// immediately. Every mailbox command funnels through here: a name is
    /// parsed once, by the domain's validator, and never interpolated into a
    /// query or a path (phase brief §18).
    async fn mailbox_argument(
        &mut self,
        command: &Command,
        index: usize,
    ) -> Result<Option<MailboxName>, ProtocolError> {
        let Some(raw) = command.args.get(index).and_then(Token::as_text) else {
            let reply = response::tagged(
                &command.tag,
                Status::Bad,
                &format!("{} requires a mailbox name", command.kind),
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(None);
        };

        // Modified UTF-7 in, Unicode onwards (RFC 3501 §5.1.3). A rev1 client
        // encodes non-ASCII names; without this the encoded form would become a
        // literal name and `CREATE "Wichtig-&APw-ber"` would land in a
        // *different* mailbox from the rev2 client's `CREATE "Wichtig-über"`.
        // A rev2 name is already Unicode and survives `decode` untouched.
        let decoded = crate::utf7::decode(raw);

        match MailboxName::new(decoded) {
            Ok(name) => Ok(Some(name)),
            Err(error) => {
                let reply = response::tagged(&command.tag, Status::No, &error.to_string());
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(None)
            }
        }
    }

    /// Answers a command that needs storage when no store is configured.
    async fn no_store(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let reply = response::tagged(&command.tag, Status::No, "mailbox storage is unavailable");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `SELECT` and `EXAMINE` (RFC 9051 §6.3.2, §6.3.3).
    ///
    /// The two commands are one handler because they differ in exactly one
    /// respect — whether the resulting selection permits mutation — and RFC
    /// 9051 §6.3.3 defines `EXAMINE` as "identical to SELECT except". Writing
    /// them separately would invite the two response sets to drift apart.
    ///
    /// # A failed SELECT deselects
    ///
    /// RFC 9051 §6.3.2 is explicit: if the command fails, the session is left
    /// with **no** mailbox selected, not with its previous one. Every failure
    /// path below therefore clears the selection before answering.
    async fn select(&mut self, command: &Command, read_only: bool) -> Result<Flow, ProtocolError> {
        let Some(name) = self.mailbox_argument(command, 0).await? else {
            self.deselect();
            return Ok(Flow::Continue);
        };

        let (Some(mailboxes), Some(messages)) =
            (self.mailboxes.clone(), self.messages_store.clone())
        else {
            self.deselect();
            return self.no_store(command).await;
        };

        let Some(account) = self.current_account().await else {
            self.deselect();
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // Existence and selectability come from the service. Deselecting on
        // every failure is RFC 9051 §6.3.2's requirement and session state, so
        // it stays here (CORE_SPEC §9).
        let mailbox = match MailboxService::new(Arc::clone(&mailboxes))
            .resolve_selectable(account.id, &name)
            .await
        {
            Ok(mailbox) => mailbox,
            Err(error) => {
                self.deselect();
                let text = match error {
                    CoreError::NotFound { .. } => "no such mailbox",
                    CoreError::Conflict { .. } => "mailbox is not selectable",
                    _ => "mailbox is unavailable",
                };
                let reply = response::tagged(&command.tag, Status::No, text);
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        };

        let entries = messages.list_entries(mailbox.id).await.unwrap_or_default();
        let sequences = SequenceMap::new(entries);

        // UIDNEXT comes from the mailbox's own counter rather than from
        // `max(uid) + 1`: an empty mailbox that once held messages must not
        // hand out a UID it has already issued (phase brief §11).
        let uid_next = mailbox.uid_next.get();
        let uid_validity = mailbox.uid_validity.get();

        let mut out = String::new();
        out.push_str(&response::count_line(sequences.len(), "EXISTS"));
        out.push_str(FLAGS_LINE);
        out.push_str(if read_only {
            PERMANENTFLAGS_READ_ONLY
        } else {
            PERMANENTFLAGS_READ_WRITE
        });
        out.push_str(&response::untagged_ok_with_code(
            &format!("UIDVALIDITY {uid_validity}"),
            "UIDs valid",
        ));
        out.push_str(&response::untagged_ok_with_code(
            &format!("UIDNEXT {uid_next}"),
            "Predicted next UID",
        ));
        if let Some(first_unseen) = sequences.first_unseen() {
            out.push_str(&response::untagged_ok_with_code(
                &format!("UNSEEN {first_unseen}"),
                "First unseen message",
            ));
        }

        // RFC 7162 §3.1.1: `HIGHESTMODSEQ` is part of the SELECT/EXAMINE
        // response for a CONDSTORE-capable server. It is sent unconditionally
        // rather than only to sessions that issued `ENABLE CONDSTORE`, because
        // it is an `OK` response *code* — a client that does not know it is
        // required to ignore it (RFC 9051 §7.1), so this cannot break an
        // older client, whereas withholding it would force a CONDSTORE client
        // into a `STATUS` round-trip it should not need.
        //
        // Read from the mailbox row, not derived from the entries: a mailbox
        // whose only recent change was an `EXPUNGE` has a HIGHESTMODSEQ above
        // every surviving entry's value.
        out.push_str(&response::untagged_ok_with_code(
            &format!("HIGHESTMODSEQ {}", mailbox.highest_mod_seq.get()),
            "Highest modification sequence",
        ));

        out.push_str(&response::tagged_with_code(
            &command.tag,
            Status::Ok,
            if read_only { "READ-ONLY" } else { "READ-WRITE" },
            &format!("{} completed", command.kind),
        ));

        self.connection.write_flush(out.as_bytes()).await?;

        // State changes only after the response is safely written, and it
        // replaces any previous selection wholesale.
        self.state = State::Selected { read_only };
        self.selected_mailbox = Some(SelectedMailbox {
            mailbox_id: mailbox.id,
            name,
            uid_validity: mailbox.uid_validity,
            sequences,
            read_only,
        });
        Ok(Flow::Continue)
    }

    /// Drops any selection and returns to Authenticated.
    ///
    /// Used by `CLOSE`/`UNSELECT`, by a failed `SELECT`, and when the selected
    /// mailbox is deleted or renamed out from under the session.
    fn deselect(&mut self) {
        self.selected_mailbox = None;
        if self.state.is_selected() {
            self.state = State::Authenticated;
        }
    }

    /// `LIST` (RFC 9051 §6.3.9).
    ///
    /// ```text
    /// LIST "" "*"      → every mailbox
    /// LIST "" "%"      → top level only
    /// LIST "" "Work/%" → the children of Work
    /// LIST "" ""       → the delimiter probe: one \Noselect root
    /// ```
    async fn list(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        // RFC 5258 §3: an optional `(selection options)` precedes the
        // reference, and an optional `RETURN (options)` follows the pattern.
        // Both are parsed off here so the reference/pattern resolution below
        // is exactly the one plain `LIST` already used.
        let options = match ListOptions::parse(&command.args) {
            Ok(options) => options,
            Err(message) => return self.bad(command, &message).await,
        };
        let args = options.remaining;

        let reference = args.first().and_then(Token::as_text).unwrap_or_default();
        let Some(pattern) = args.get(1).and_then(Token::as_text) else {
            let reply = response::tagged(
                &command.tag,
                Status::Bad,
                "LIST requires a reference and a pattern",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // RFC 9051 §6.3.9: an empty pattern is a request for the hierarchy
        // delimiter, answered with the root and nothing else. It precedes every
        // option: there is no mailbox to report attributes or status for.
        if pattern.is_empty() {
            let mut out =
                response::list_line(&[String::from(r"\Noselect")], HIERARCHY_DELIMITER, "");
            out.push_str(&response::tagged(
                &command.tag,
                Status::Ok,
                "LIST completed",
            ));
            self.connection.write_flush(out.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        let Some(mailboxes) = self.mailboxes.clone() else {
            return self.no_store(command).await;
        };
        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let resolved = mailbox::resolve_reference(reference, pattern);
        let mut listed = mailboxes.list(account.id).await.unwrap_or_default();
        // Deterministic ordering, independent of what the store returns
        // (phase brief §5).
        listed.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));

        // Children are computed against the *whole* account, not the filtered
        // result: a child the pattern excludes still makes its parent a parent.
        let all = listed.clone();

        let mut out = String::new();
        for mailbox in listed
            .iter()
            .filter(|mailbox| options.selects(&resolved, mailbox))
        {
            out.push_str(&response::list_line(
                &name_attributes(mailbox, &all, options.reports_subscribed()),
                HIERARCHY_DELIMITER,
                mailbox.name.as_str(),
            ));

            // `RETURN (STATUS (...))` emits a `* STATUS` line per listed
            // mailbox, reusing `StatusItem::render` so the values cannot
            // diverge from what the `STATUS` command reports.
            if !options.status_items.is_empty() {
                // The same port the STATUS command reads, so the two cannot
                // report different numbers for one mailbox.
                let counts = mailboxes.counts(mailbox.id).await.unwrap_or_default();
                let items: Vec<String> = options
                    .status_items
                    .iter()
                    .map(|item| item.render(mailbox, counts))
                    .collect();
                out.push_str(&response::status_line(mailbox.name.as_str(), &items));
            }
        }

        // An empty result is a successful LIST with no data lines, never an
        // error — a client uses it to learn the mailbox is absent.
        out.push_str(&response::tagged(
            &command.tag,
            Status::Ok,
            "LIST completed",
        ));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `STATUS` (RFC 9051 §6.3.11).
    ///
    /// Reports only items the storage model can answer truthfully. `RECENT` is
    /// **not** supported: RFC 9051 removed it from `IMAP4rev2`, and this server
    /// has no `\Recent` concept to report — answering `0` would be a fabricated
    /// value (phase brief §6).
    async fn status(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(name) = self.mailbox_argument(command, 0).await? else {
            return Ok(Flow::Continue);
        };

        let Some(Token::List(requested)) = command.args.get(1) else {
            let reply = response::tagged(
                &command.tag,
                Status::Bad,
                "STATUS requires a parenthesised item list",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        if requested.is_empty() {
            let reply = response::tagged(
                &command.tag,
                Status::Bad,
                "STATUS requires at least one item",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        // Validate every requested item *before* touching storage, so an
        // unsupported item is a protocol error rather than a partial answer.
        let mut items = Vec::with_capacity(requested.len());
        for token in requested {
            let Some(text) = token.as_text() else {
                let reply = response::tagged(&command.tag, Status::Bad, "malformed STATUS item");
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            };
            let Some(item) = StatusItem::parse(text) else {
                let reply = response::tagged(
                    &command.tag,
                    Status::Bad,
                    &format!("unsupported STATUS item {text}"),
                );
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            };
            items.push(item);
        }

        let Some(mailboxes) = self.mailboxes.clone() else {
            return self.no_store(command).await;
        };
        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // `STATUS` reports on a mailbox without selecting it, so a `\Noselect`
        // name is a legitimate target and plain resolution is what is wanted.
        // A store failure is no longer reported as "no such mailbox".
        let mailbox = match MailboxService::new(Arc::clone(&mailboxes))
            .resolve(account.id, &name)
            .await
        {
            Ok(mailbox) => mailbox,
            Err(error) => {
                let text = match error {
                    CoreError::NotFound { .. } => "no such mailbox",
                    _ => "mailbox is unavailable",
                };
                let reply = response::tagged(&command.tag, Status::No, text);
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        };

        let counts = mailboxes.counts(mailbox.id).await.unwrap_or_default();
        let rendered: Vec<String> = items
            .iter()
            .map(|item| item.render(&mailbox, counts))
            .collect();

        let mut out = response::status_line(name.as_str(), &rendered);
        out.push_str(&response::tagged(
            &command.tag,
            Status::Ok,
            "STATUS completed",
        ));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `CREATE` (RFC 9051 §6.3.4).
    ///
    /// The store creates missing parents, so `CREATE a/b/c` in one call yields
    /// the whole chain — RFC 9051 §6.3.4 permits either behaviour and creating
    /// them is what clients expect.
    async fn create(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(name) = self.mailbox_argument(command, 0).await? else {
            return Ok(Flow::Continue);
        };

        // RFC 6154 §3: `CREATE name (USE (\Sent …))` requests special uses for
        // the new mailbox. The tokeniser delivers the whole thing as one
        // nested list — `List([Atom("USE"), List([...])])` — so it is read as
        // such rather than as two arguments.
        //
        // Parsed and validated before anything is created, so a refused
        // request leaves no mailbox behind.
        let mut uses: Vec<String> = Vec::new();
        if let Some(extra) = next_argument(&command.args, 1) {
            let Token::List(parts) = extra else {
                return self
                    .bad(command, "unexpected argument after CREATE mailbox name")
                    .await;
            };
            let keyword = parts.first().and_then(Token::as_text);
            if !keyword.is_some_and(|text| text.eq_ignore_ascii_case("USE")) {
                return self
                    .bad(command, "unexpected argument after CREATE mailbox name")
                    .await;
            }
            let Some(Token::List(requested)) = parts.get(1) else {
                return self
                    .bad(
                        command,
                        "CREATE USE requires a parenthesised attribute list",
                    )
                    .await;
            };
            for token in requested {
                let Some(text) = token.as_text() else {
                    return self
                        .bad(command, "CREATE USE attributes must be atoms")
                        .await;
                };
                // Matched case-insensitively against the closed RFC 6154 set,
                // then stored in the RFC's own spelling — so `\sent` persists
                // as `\Sent` and every later comparison is exact.
                let Some(canonical) = SPECIAL_USE_ATTRIBUTES
                    .iter()
                    .find(|attribute| attribute.eq_ignore_ascii_case(text))
                else {
                    // RFC 6154 §3 gives `NO [USEATTR]` for a use this server
                    // will not assign — a refusal of the *attribute*, not a
                    // syntax error, so the client can retry without it.
                    let reply = response::tagged_with_code(
                        &command.tag,
                        Status::No,
                        "USEATTR",
                        &format!("unsupported special-use attribute {text}"),
                    );
                    self.connection.write_flush(reply.as_bytes()).await?;
                    return Ok(Flow::Continue);
                };
                if !uses.iter().any(|held| held == *canonical) {
                    uses.push((*canonical).to_owned());
                }
            }
        }

        let Some(mailboxes) = self.mailbox_service() else {
            return self.no_store(command).await;
        };
        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let attributes = if uses.is_empty() {
            MailboxAttributes::none()
        } else {
            MailboxAttributes::from_names(uses)
        };

        // The INBOX refusal (RFC 9051 §6.3.4) and the duplicate check are the
        // service's, so HTTP enforces them identically (ADR-068). Note the
        // duplicate check now distinguishes a taken name from a store that
        // could not answer — it previously matched `Ok(Some(_))` and created
        // the mailbox anyway when the probe failed.
        let (status, text) = match mailboxes.create(account.id, &name, attributes).await {
            Ok(_) => (Status::Ok, "CREATE completed"),
            Err(CoreError::InvalidArgument { .. }) => (Status::No, "INBOX already exists"),
            Err(CoreError::AlreadyExists { .. }) => (Status::No, "mailbox already exists"),
            Err(_) => (Status::No, "cannot create mailbox"),
        };
        let reply = response::tagged(&command.tag, status, text);
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `DELETE` (RFC 9051 §6.3.5).
    ///
    /// Deleting the mailbox this session has selected leaves no stale
    /// selection: the session is returned to Authenticated before the response
    /// goes out (phase brief §8).
    async fn delete(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(name) = self.mailbox_argument(command, 0).await? else {
            return Ok(Flow::Continue);
        };

        let Some(mailboxes) = self.mailbox_service() else {
            return self.no_store(command).await;
        };
        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // The INBOX refusal (RFC 9051 §6.3.5) and the existence check are the
        // service's. Dropping a stale selection is not: that is session state
        // (CORE_SPEC §9) and stays here.
        match mailboxes.delete(account.id, &name).await {
            Ok(deleted_id) => {
                if self
                    .selected_mailbox
                    .as_ref()
                    .is_some_and(|selected| selected.mailbox_id == deleted_id)
                {
                    self.deselect();
                }
                let reply = response::tagged(&command.tag, Status::Ok, "DELETE completed");
                self.connection.write_flush(reply.as_bytes()).await?;
            }
            Err(error) => {
                let text = match error {
                    CoreError::InvalidArgument { .. } => "INBOX cannot be deleted",
                    CoreError::NotFound { .. } => "no such mailbox",
                    _ => "cannot delete mailbox",
                };
                let reply = response::tagged(&command.tag, Status::No, text);
                self.connection.write_flush(reply.as_bytes()).await?;
            }
        }
        Ok(Flow::Continue)
    }

    /// `RENAME` (RFC 9051 §6.3.6).
    ///
    /// The mailbox keeps its identity, and therefore its UIDVALIDITY: a rename
    /// is not a new UID epoch, and telling a client otherwise would make it
    /// discard and refetch the whole mailbox (phase brief §11). Descendants
    /// move with the parent, which the storage port handles atomically.
    async fn rename(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(from) = self.mailbox_argument(command, 0).await? else {
            return Ok(Flow::Continue);
        };
        let Some(to) = self.mailbox_argument(command, 1).await? else {
            return Ok(Flow::Continue);
        };

        let Some(mailboxes) = self.mailbox_service() else {
            return self.no_store(command).await;
        };
        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // INBOX in either position (RFC 9051 §6.3.6), the source lookup and the
        // duplicate-target check all belong to the service, so HTTP cannot
        // rename a mailbox onto INBOX while IMAP refuses to (ADR-068).
        let renamed = match mailboxes.rename(account.id, &from, &to).await {
            Ok(id) => Ok(id),
            Err(error) => {
                let text = match error {
                    CoreError::InvalidArgument { .. } => "INBOX cannot be renamed",
                    CoreError::NotFound { .. } => "no such mailbox",
                    CoreError::AlreadyExists { .. } => "mailbox already exists",
                    _ => "cannot rename mailbox",
                };
                let reply = response::tagged(&command.tag, Status::No, text);
                self.connection.write_flush(reply.as_bytes()).await?;
                Err(())
            }
        };
        let Ok(mailbox_id) = renamed else {
            return Ok(Flow::Continue);
        };

        // The selection survives a rename — same mailbox, same UID space — but
        // the name it reports must follow. Session state, so it stays here
        // (CORE_SPEC §9).
        if let Some(selected) = self.selected_mailbox.as_mut()
            && selected.mailbox_id == mailbox_id
        {
            selected.name = to;
        }
        let reply = response::tagged(&command.tag, Status::Ok, "RENAME completed");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `SUBSCRIBE` and `UNSUBSCRIBE` (RFC 9051 §6.3.7, §6.3.8).
    ///
    /// Subscription is persisted by the store — the domain already models it —
    /// rather than held in a per-session map, so it survives reconnection the
    /// way clients assume (phase brief §10).
    async fn set_subscribed(
        &mut self,
        command: &Command,
        subscribed: bool,
    ) -> Result<Flow, ProtocolError> {
        let Some(name) = self.mailbox_argument(command, 0).await? else {
            return Ok(Flow::Continue);
        };

        let Some(mailboxes) = self.mailbox_service() else {
            return self.no_store(command).await;
        };
        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let reply = match mailboxes
            .set_subscribed(account.id, &name, subscribed)
            .await
        {
            Ok(()) => response::tagged(
                &command.tag,
                Status::Ok,
                &format!("{} completed", command.kind),
            ),
            Err(CoreError::NotFound { .. }) => {
                response::tagged(&command.tag, Status::No, "no such mailbox")
            }
            Err(_) => response::tagged(&command.tag, Status::No, "cannot change subscription"),
        };
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `CLOSE` (RFC 9051 §6.4.2).
    async fn close(&mut self, tag: &Tag) -> Result<Flow, ProtocolError> {
        self.deselect();
        let reply = response::tagged(tag, Status::Ok, "CLOSE completed");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `FETCH` and `UID FETCH` (RFC 9051 §6.4.5, §6.4.8).
    ///
    /// ```text
    /// sequence set ──▶ SequenceMap ──▶ MessageEntry (UID, flags, date)
    ///                                        │
    ///                                        └─▶ MessageStore::get ──▶ raw + MIME
    ///                                                                      │
    ///                                                          FETCH response line
    /// ```
    ///
    /// # Identifier resolution
    ///
    /// Both modes resolve through the session's [`SequenceMap`] and nowhere
    /// else (ADR-044). `FETCH` treats the set as positions, `UID FETCH` as
    /// UIDs; neither consults a database id, and neither re-derives the
    /// ordering. Because the map holds only the *selected* mailbox's
    /// placements, a UID naming a message in another mailbox resolves to
    /// nothing — mailbox scoping is a property of the data structure rather
    /// than a check that could be forgotten (phase brief §18).
    ///
    /// # Read-only
    ///
    /// `FETCH` mutates nothing except, for a non-`.PEEK` body fetch, the
    /// `\Seen` flag that RFC 9051 §6.4.5 requires it to set. That single
    /// mutation goes through [`MessageStore::update_flags`] like any other
    /// flag change; this module never touches storage directly.
    async fn fetch(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        // The sequence set is the first argument, the item list the rest.
        let Some(set_text) = command
            .args
            .first()
            .and_then(Token::as_text)
            .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        else {
            return self
                .bad(
                    command,
                    &format!("{} requires a sequence set", command.kind),
                )
                .await;
        };

        let set = match SequenceSet::parse(&set_text) {
            Ok(set) => set,
            Err(error) => return self.bad(command, error.message()).await,
        };

        let request = match FetchRequest::parse(&command.args[1..]) {
            Ok(request) => request,
            Err(error) => return self.bad(command, &error.message()).await,
        };

        // The state machine has already established that a mailbox is
        // selected, so this is a consistency check rather than a policy one.
        let Some(selected) = self.selected_mailbox.clone() else {
            return self
                .bad(command, Refusal::NeedsSelectedMailbox.message())
                .await;
        };
        let Some(messages) = self.messages_store.clone() else {
            return self.no_store(command).await;
        };

        // Resolve identifiers against the canonical mapping. Out-of-range
        // members are not errors (RFC 9051 §6.4.8), so an empty result is a
        // successful FETCH that returns no data.
        let resolved: Vec<(usize, MessageEntry)> = if command.uid_mode {
            set.resolve_uids(selected.sequences())
        } else {
            set.resolve(selected.sequences())
        }
        .into_iter()
        .map(|(sequence, entry)| (sequence, entry.clone()))
        .collect();

        let needs_raw = request.needs_raw();
        let sets_seen = request.sets_seen() && !selected.is_read_only();

        // RFC 7162 §3.1.4.1: `CHANGEDSINCE` implies `MODSEQ` in the response,
        // so a client filtering by modification sequence learns the new values
        // and can advance the bound it filters on. Appended rather than
        // inserted, so the client's requested item order is preserved.
        let mut items = request.items().to_vec();
        if request.implies_mod_seq() {
            items.push(crate::fetch::FetchItem::ModSeq);
        }

        for (sequence, entry) in resolved {
            // RFC 7162 §3.1.4: report only what changed after the client's
            // value. Filtered here rather than in the sequence resolution
            // because the set still names the messages — they are simply not
            // reported, and an unmatched message is not an error.
            if request
                .changed_since()
                .is_some_and(|since| entry.mod_seq <= since)
            {
                continue;
            }
            // The message body is loaded only when an item actually needs it,
            // and once per message however many items were asked for
            // (phase brief §21).
            let message = if needs_raw {
                match messages.get(entry.message_id).await {
                    Ok(message) => message,
                    // An entry whose message cannot be read is skipped rather
                    // than failing the whole command: the other messages in
                    // the set are still answerable, and the error text must not
                    // describe storage internals (phase brief §22).
                    Err(_) => continue,
                }
            } else {
                None
            };

            // RFC 9051 §6.4.5: a non-peek body fetch sets \Seen. The flag is
            // set *before* the response is rendered so that the FLAGS the
            // client sees match what is now stored — a client that asked for
            // both FLAGS and BODY[] in one command must not be told the
            // message is unseen by a response that just made it seen.
            let mut entry = entry;
            if sets_seen && !entry.is_seen() {
                let update = FlagUpdate::Add {
                    system: SystemFlags::SEEN,
                    keywords: BTreeSet::new(),
                };
                // The conditional port, with no precondition: an implicit
                // \Seen is unconditional, but it must still report the modseq
                // it allocated, or the MODSEQ in this very response would name
                // the value from before the flag was set.
                if let Ok(outcome) = messages
                    .update_flags_conditional(entry.id, update, None)
                    .await
                {
                    entry.flags = outcome.flags;
                    entry.mod_seq = outcome.mod_seq;
                    // Keep the session's snapshot consistent with the store, so
                    // a second FETCH in the same session reports the new flag.
                    if let Some(selection) = self.selected_mailbox.as_mut() {
                        selection.record_flags(entry.uid, entry.flags.clone(), Some(entry.mod_seq));
                    }
                }
            }

            let data = MessageData {
                entry: &entry,
                message: message.as_ref(),
            };
            let line = fetch_response::fetch_line(sequence, &data, &items, command.uid_mode);
            // Written without flushing: the untagged lines and the tagged
            // completion go out together on the final `write_flush`, so a
            // multi-message FETCH is one syscall batch rather than one per
            // message.
            self.connection.write(&line).await?;
        }

        let reply = response::tagged(
            &command.tag,
            Status::Ok,
            &format!(
                "{}{} completed",
                if command.uid_mode { "UID " } else { "" },
                command.kind
            ),
        );
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `STORE` and `UID STORE` (RFC 9051 §6.4.6, §6.4.8).
    ///
    /// ```text
    /// sequence set ──▶ SequenceMap ──▶ MessageEntry.id (a placement)
    ///                                        │
    ///                                        ▼
    ///                        MessageStore::update_flags (one transaction,
    ///                                        │           emits EntryFlagsChanged)
    ///                                        ▼
    ///                          snapshot updated  +  untagged FETCH FLAGS
    ///                                               (unless .SILENT)
    /// ```
    ///
    /// # Identifier resolution
    ///
    /// The same two resolvers `FETCH` uses, over the same `SequenceMap`
    /// (ADR-044). `STORE` treats the set as positions, `UID STORE` as UIDs.
    /// Because the map holds only the selected mailbox's placements, a UID
    /// naming a message in another mailbox resolves to nothing — and the
    /// mutation targets `MessageEntry.id`, one *placement*, so another
    /// mailbox's placement of the same message is untouched by construction.
    ///
    /// # Read-only selections
    ///
    /// Refused by the state machine before this runs (`EXAMINE` yields
    /// `Refusal::MailboxIsReadOnly`), so no mutation is even attempted. The
    /// check here is a consistency assertion, not the policy.
    ///
    /// # Nothing is partially applied on a malformed command
    ///
    /// The sequence set and the flag argument are both parsed *before* any
    /// entry is touched. A syntax error therefore mutates nothing, which is
    /// what RFC 9051 §6.4.6 requires and what a client relies on when it
    /// retries.
    async fn store(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(set_text) = command
            .args
            .first()
            .and_then(Token::as_text)
            .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        else {
            return self
                .bad(
                    command,
                    &format!("{} requires a sequence set", command.kind),
                )
                .await;
        };

        let set = match SequenceSet::parse(&set_text) {
            Ok(set) => set,
            Err(error) => return self.bad(command, error.message()).await,
        };

        let request = match StoreRequest::parse(&command.args[1..]) {
            Ok(request) => request,
            Err(error) => return self.bad(command, &error.message()).await,
        };

        let Some(selected) = self.selected_mailbox.clone() else {
            return self
                .bad(command, Refusal::NeedsSelectedMailbox.message())
                .await;
        };

        // The state machine already refused a read-only selection. Re-checking
        // here means a future routing change cannot turn EXAMINE into a
        // writable selection silently.
        if selected.is_read_only() {
            return self
                .bad(command, Refusal::MailboxIsReadOnly.message())
                .await;
        }

        let Some(messages) = self.messages_store.clone() else {
            return self.no_store(command).await;
        };

        // Out-of-range members are not errors (RFC 9051 §6.4.8): an empty
        // result is a successful STORE that changed nothing.
        let resolved: Vec<(usize, MessageEntry)> = if command.uid_mode {
            set.resolve_uids(selected.sequences())
        } else {
            set.resolve(selected.sequences())
        }
        .into_iter()
        .map(|(sequence, entry)| (sequence, entry.clone()))
        .collect();

        let update = request.to_update();
        let unchanged_since = request.unchanged_since();
        let mut out: Vec<u8> = Vec::new();
        // UIDs whose `UNCHANGEDSINCE` precondition failed, for `MODIFIED`.
        let mut modified: Vec<u32> = Vec::new();

        for (sequence, entry) in resolved {
            // One transaction per placement, through the port. The storage
            // layer applies add/remove/replace, evaluates the precondition and
            // emits `EntryFlagsChanged`; this adapter issues no SQL and
            // computes no resulting flag set of its own — the value it reports
            // is the value storage returned.
            let Ok(outcome) = messages
                .update_flags_conditional(entry.id, update.clone(), unchanged_since)
                .await
            else {
                // A placement that has since been removed is skipped rather
                // than failing the whole command, matching how FETCH degrades
                // (ADR-046). The error text must not describe storage.
                continue;
            };

            // RFC 7162 §3.2.5: a placement whose modseq exceeded the client's
            // value is named in `MODIFIED` and gets no untagged FETCH — the
            // client asked not to overwrite a newer change, and reporting
            // flags here would suggest the store had been applied.
            if outcome.status == sandesh_core::FlagChangeStatus::Failed {
                modified.push(entry.uid.get());
                continue;
            }

            // The snapshot is updated whether or not the client is told, so a
            // later FETCH in this session is never stale (ADR-047). Doing this
            // for `.SILENT` too is the whole point: silence suppresses the
            // *report*, not the change.
            if let Some(selection) = self.selected_mailbox.as_mut() {
                selection.record_flags(entry.uid, outcome.flags.clone(), Some(outcome.mod_seq));
            }

            if request.is_silent() {
                continue;
            }

            // RFC 9051 §6.4.6: report the new flags as an untagged FETCH. In
            // UID mode the UID is included, because the client addressed the
            // message by UID and needs to match the response to its request
            // (§6.4.8) — the same rule FETCH follows.
            //
            // RFC 7162 §3.2 additionally requires `MODSEQ` in the untagged
            // FETCH of a *conditional* store, so the client can record the
            // revision its own change produced.
            let mut updated = entry;
            updated.flags = outcome.flags;
            updated.mod_seq = outcome.mod_seq;
            let data = MessageData {
                entry: &updated,
                message: None,
            };
            let items: &[FetchItem] = if unchanged_since.is_some() {
                &[FetchItem::Flags, FetchItem::ModSeq]
            } else {
                &[FetchItem::Flags]
            };
            let line = fetch_response::fetch_line(sequence, &data, items, command.uid_mode);
            out.extend_from_slice(&line);
        }

        out.extend_from_slice(store_reply(command, &modified).as_bytes());
        self.connection.write_flush(&out).await?;
        Ok(Flow::Continue)
    }

    /// `SEARCH` and `UID SEARCH` (RFC 9051 §6.4.4, §6.4.8).
    ///
    /// ```text
    /// criteria ──▶ CompiledSearch ──▶ SearchQuery        (pure, in sandesh-core)
    ///                                      │
    /// SequenceMap ──▶ candidate placements │
    ///                            │         │
    ///                            ▼         ▼
    ///                   SearchQuery::matches(message, entry)
    ///                            │
    ///                            ▼
    ///           * SEARCH <sequence numbers | UIDs>
    /// ```
    ///
    /// # Mailbox scoping is structural
    ///
    /// The candidate set is the selected mailbox's `SequenceMap` and nothing
    /// else, so a message in another mailbox cannot appear in a result. There
    /// is no mailbox filter to forget, and no query a client could write to
    /// widen the scope.
    ///
    /// # What is returned
    ///
    /// `SEARCH` returns **sequence numbers**; `UID SEARCH` returns **UIDs**.
    /// Both come from the same `SequenceMap` entry, so they cannot disagree
    /// about which message matched — only about how to name it.
    ///
    /// Results are ascending and deduplicated: they are produced by walking the
    /// mailbox in sequence order, so the ordering is a property of the walk
    /// rather than a sort applied afterwards.
    ///
    /// # Read-only is fine
    ///
    /// `SEARCH` mutates nothing — not even `\Seen`, unlike `FETCH BODY[]`. It
    /// is therefore permitted on an `EXAMINE`d selection.
    /// Asks the index which messages a search need even look at.
    ///
    /// Returns a **superset** of the matches, or `Unsupported` meaning "scan
    /// everything". Either way `SearchQuery::matches` still decides the result,
    /// so this can only change how much work a `SEARCH` does — never its
    /// answer. An index error is treated as `Unsupported` for the same reason:
    /// degrading to a full scan is always correct, and failing a client's
    /// command because an optimisation broke would not be
    /// (`STORAGE_DESIGN.md` §7).
    async fn search_candidates(
        &self,
        query: &sandesh_core::SearchQuery,
    ) -> sandesh_core::SearchCandidates {
        match self.search_index.as_ref() {
            Some(index) => index
                .candidates(query)
                .await
                .unwrap_or(sandesh_core::SearchCandidates::Unsupported),
            None => sandesh_core::SearchCandidates::Unsupported,
        }
    }

    async fn search(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let compiled = match search::compile(&command.args, command.uid_mode) {
            Ok(compiled) => compiled,
            Err(error) => {
                // RFC 9051 §6.4.4: an unsupported charset is `NO [BADCHARSET]`
                // rather than `BAD`, because the command was well formed and
                // the client can retry in US-ASCII.
                if error.is_bad_charset() {
                    let reply = response::tagged_with_code(
                        &command.tag,
                        Status::No,
                        "BADCHARSET (US-ASCII UTF-8)",
                        &error.message(),
                    );
                    self.connection.write_flush(reply.as_bytes()).await?;
                    return Ok(Flow::Continue);
                }
                return self.bad(command, &error.message()).await;
            }
        };

        let Some(selected) = self.selected_mailbox.clone() else {
            return self
                .bad(command, Refusal::NeedsSelectedMailbox.message())
                .await;
        };
        let Some(messages) = self.messages_store.clone() else {
            return self.no_store(command).await;
        };

        // Sequence-number sets in the criteria are resolved here, against this
        // session's snapshot — a sequence number is a position in it, which the
        // domain query deliberately cannot express.
        let permitted: Option<Vec<Uid>> = if compiled.sequence_sets().is_empty() {
            None
        } else {
            let mut allowed: Option<Vec<Uid>> = None;
            for set in compiled.sequence_sets() {
                let uids: Vec<Uid> = set
                    .resolve(selected.sequences())
                    .into_iter()
                    .map(|(_, entry)| entry.uid)
                    .collect();
                allowed = Some(match allowed {
                    // Several sets are ANDed, so each narrows the last.
                    Some(previous) => previous
                        .into_iter()
                        .filter(|uid| uids.contains(uid))
                        .collect(),
                    None => uids,
                });
            }
            allowed
        };

        // Loading the message is only necessary for content keys. A
        // `SEARCH UNSEEN` over a large mailbox therefore reads no bodies at
        // all, which is the difference between a constant-cost search and one
        // proportional to the mailbox's total size.
        let needs_message = compiled.query().needs_message();
        let query = compiled.query().clone();

        let candidates = self.search_candidates(&query).await;

        let mut results: Vec<u32> = Vec::new();

        for (sequence, entry) in selected.sequences().enumerated() {
            if let Some(permitted) = permitted.as_ref()
                && !permitted.contains(&entry.uid)
            {
                continue;
            }

            // Excluded by the index means it provably cannot match, so it is
            // never loaded — the saving the index exists for.
            if !candidates.admits(entry.message_id) {
                continue;
            }

            let message = if needs_message {
                // A message that cannot be read simply does not match, rather
                // than failing the whole command (ADR-046) — and the error is
                // never described to the client.
                messages.get(entry.message_id).await.ok().flatten()
            } else {
                None
            };

            if !query.matches(message.as_ref(), entry) {
                continue;
            }

            // The one place the two modes differ: how a match is named.
            let number = if command.uid_mode {
                entry.uid.get()
            } else {
                // A sequence number always fits in `u32` — a mailbox cannot
                // hold more messages than UIDs exist — but the conversion is
                // checked rather than cast, so an absurd count cannot wrap
                // into a valid-looking small number.
                match u32::try_from(sequence) {
                    Ok(number) => number,
                    Err(_) => continue,
                }
            };
            results.push(number);
        }

        // The results are identical either way; only the response form
        // differs (RFC 9051 §7.3.4). Both branches read the same `results`
        // vector, so an `ESEARCH` can never disagree with the `SEARCH` it
        // would have been.
        let mut out = match compiled.returns() {
            Some(returns) => {
                let items = esearch_items(returns, &results);
                response::esearch_line(&command.tag, command.uid_mode, &items)
            }
            None => response::search_line(&results),
        };
        out.push_str(&response::tagged(
            &command.tag,
            Status::Ok,
            &format!(
                "{}{} completed",
                if command.uid_mode { "UID " } else { "" },
                command.kind
            ),
        ));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `EXPUNGE` and `UID EXPUNGE` (RFC 9051 §6.4.3, RFC 4315 §2.1).
    ///
    /// ```text
    /// EXPUNGE                 ──▶ every \Deleted placement
    /// UID EXPUNGE <uid-set>   ──▶ every \Deleted placement in the set
    ///                                        │
    ///                    ExpungePlan (built against the unmutated snapshot)
    ///                                        │
    ///                     delete_entry per placement, through the port
    ///                          (emits EntryDeleted; no SQL here)
    ///                                        │
    ///                     ┌──────────────────┴───────────────────┐
    ///                     ▼                                      ▼
    ///        * <seq> EXPUNGE, highest first        snapshot membership updated
    ///                                              (remaining messages renumber)
    ///                                        │
    ///                              collect_orphans (emits MessageDeleted)
    /// ```
    ///
    /// # Only `\Deleted` is removed
    ///
    /// `EXPUNGE` takes no argument and removes what is *marked*, not what a
    /// client names. `UID EXPUNGE` intersects that with the set, so a UID
    /// without `\Deleted` survives — which is what lets a client expunge a set
    /// it captured earlier without racing its own flag state.
    ///
    /// # The whole command is validated before anything is removed
    ///
    /// A `UID EXPUNGE` with a malformed set is `BAD` and removes nothing, and
    /// the plan is computed in full before the first `delete_entry`. That is
    /// also what makes the reported sequence numbers correct: they are
    /// positions in the snapshot as it stood when the command arrived.
    ///
    /// # Read-only selections
    ///
    /// The state machine refuses `EXPUNGE` on an `EXAMINE`d selection before
    /// this runs. The check repeated here is a consistency assertion — it
    /// guarantees no mutation is attempted even if routing changes — and it is
    /// deliberately made *before* the plan is applied, not after.
    ///
    /// # Placement isolation
    ///
    /// The plan carries `EntryId`s, and `delete_entry` removes one placement.
    /// Another mailbox's placement of the same message is a different
    /// placement and is untouched; the message itself survives until no
    /// placement references it, at which point `collect_orphans` reclaims it.
    async fn expunge(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        // `UID EXPUNGE` requires a set; plain `EXPUNGE` must not carry one.
        // Both are decided here, before any state is consulted, so a malformed
        // command cannot reach storage.
        let set = if command.uid_mode {
            let Some(text) = command
                .args
                .first()
                .and_then(Token::as_text)
                .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
            else {
                return self.bad(command, "UID EXPUNGE requires a UID set").await;
            };
            match SequenceSet::parse(&text) {
                Ok(set) => Some(set),
                Err(error) => return self.bad(command, error.message()).await,
            }
        } else {
            None
        };

        let Some(selected) = self.selected_mailbox.clone() else {
            return self
                .bad(command, Refusal::NeedsSelectedMailbox.message())
                .await;
        };

        // Already refused by the state machine; restated so a future routing
        // change cannot make EXAMINE mutate.
        if selected.is_read_only() {
            return self
                .bad(command, Refusal::MailboxIsReadOnly.message())
                .await;
        }

        let Some(messages) = self.messages_store.clone() else {
            return self.no_store(command).await;
        };

        let mut plan = match set.as_ref() {
            Some(set) => ExpungePlan::deleted_in_set(selected.sequences(), set),
            None => ExpungePlan::all_deleted(selected.sequences()),
        };

        // One transaction per placement, through the port. Storage emits
        // `EntryDeleted` for each removal, so this adapter publishes no events
        // of its own and invents none: a placement storage did not remove is
        // dropped from the plan below, and therefore produces neither an
        // untagged line nor a snapshot change.
        let mut removed: BTreeSet<EntryId> = BTreeSet::new();
        for doomed in plan.iter() {
            if messages.delete_entry(doomed.entry_id).await == Ok(true) {
                removed.insert(doomed.entry_id);
            }
        }
        plan.retain_removed(&removed);

        // Reported highest sequence number first, so each number is still
        // valid when the client applies it (RFC 9051 §7.4.1).
        let mut out = String::new();
        for doomed in plan.report_order() {
            out.push_str(&response::count_line(doomed.sequence, "EXPUNGE"));
        }

        // The acting session's own view changes now (ADR-049): membership
        // drops the removed placements and the survivors renumber. Other
        // sessions keep their snapshots until they re-select — this increment
        // implements no unsolicited cross-session synchronisation.
        if let Some(selection) = self.selected_mailbox.as_mut() {
            selection.remove_expunged(&plan.uids());
        }

        // A message whose last placement is gone is reclaimed, which emits
        // `MessageDeleted`. Run after the removals rather than inside them, and
        // a failure here is not reported to the client: the expunge itself
        // succeeded, and an uncollected orphan is reclaimable later.
        if !plan.is_empty()
            && let Err(error) = messages.collect_orphans().await
        {
            tracing::warn!(%error, "cannot collect orphaned messages after EXPUNGE");
        }

        out.push_str(&response::tagged(
            &command.tag,
            Status::Ok,
            &format!(
                "{}{} completed",
                if command.uid_mode { "UID " } else { "" },
                command.kind
            ),
        ));
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `COPY` and `UID COPY` (RFC 9051 §6.4.7, §6.4.8; `COPYUID` per RFC 4315 §3).
    ///
    /// ```text
    /// COPY <set> <mailbox>       ──▶ set read as sequence numbers
    /// UID COPY <uid-set> <mbox>  ──▶ set read as UIDs
    ///                │
    ///        resolved through SequenceMap (never re-derived)
    ///                │
    ///        copy_entry per placement, through the port
    ///        (new EntryId, new destination UID, emits EntryCreated)
    ///                │
    ///     (source uid, destination uid) recorded as storage returned it
    ///                │
    ///                ▼
    ///   OK [COPYUID <dest uidvalidity> <source set> <dest set>] COPY completed
    /// ```
    ///
    /// # The source mailbox is not touched
    ///
    /// `COPY` creates placements; it removes and renumbers nothing. The source
    /// `SequenceMap`, its UIDs and its flags are all unchanged, so this handler
    /// deliberately makes no call into `record_flags` or `remove_expunged`. The
    /// underlying `Message` is *shared*, not duplicated — `copy_entry` reuses
    /// `message_id`, which is what keeps the content-addressed blob single-copy.
    ///
    /// # Destination UIDs come from storage, never from arithmetic
    ///
    /// Each `copy_entry` allocates a UID inside its own transaction and returns
    /// the placement it created. `[COPYUID]` reports those values. Computing
    /// them from the destination's `UIDNEXT` instead would name a concurrent
    /// writer's messages when two clients copy into one mailbox (ADR-050).
    ///
    /// # Everything is validated before anything is copied
    ///
    /// The set, the mailbox name, the destination's existence and its
    /// selectability are all checked first, so a `COPY` that is going to fail
    /// creates no placement. RFC 9051 §6.4.7 requires exactly this: a failed
    /// `COPY` must leave no message copied.
    async fn copy(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(set_text) = command
            .args
            .first()
            .and_then(Token::as_text)
            .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        else {
            return self
                .bad(
                    command,
                    &format!("{} requires a sequence set", command.kind),
                )
                .await;
        };

        let set = match SequenceSet::parse(&set_text) {
            Ok(set) => set,
            Err(error) => return self.bad(command, error.message()).await,
        };

        let Some(destination_name) = self.mailbox_argument(command, 1).await? else {
            return Ok(Flow::Continue);
        };

        let Some(selected) = self.selected_mailbox.clone() else {
            return self
                .bad(command, Refusal::NeedsSelectedMailbox.message())
                .await;
        };

        let (Some(mailboxes), Some(messages)) =
            (self.mailboxes.clone(), self.messages_store.clone())
        else {
            return self.no_store(command).await;
        };

        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let Some(destination) = self
            .resolve_destination(command, &mailboxes, account.id, &destination_name)
            .await?
        else {
            return Ok(Flow::Continue);
        };

        // A message may be placed in a given mailbox only once — an invariant
        // of the domain model (`CORE_SPEC.md` §7, enforced by a unique index on
        // `(mailbox_id, message_id)`), not a limitation of this handler. A
        // same-mailbox `COPY` therefore cannot create the second placement RFC
        // 9051 §6.4.7 would allow, so it is refused explicitly rather than
        // reported as a success that silently copied nothing (ADR-050).
        if destination.id == selected.mailbox_id {
            let reply = response::tagged(
                &command.tag,
                Status::No,
                "cannot copy a message into its own mailbox",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        // Out-of-range members are not errors (RFC 9051 §6.4.8): an empty
        // result is a successful COPY that copied nothing.
        let resolved: Vec<MessageEntry> = if command.uid_mode {
            set.resolve_uids(selected.sequences())
        } else {
            set.resolve(selected.sequences())
        }
        .into_iter()
        .map(|(_, entry)| entry.clone())
        .collect();

        // One transaction per placement, through the port. Storage assigns the
        // UID and emits `EntryCreated`; this adapter issues no SQL, computes no
        // UID of its own, and publishes no event.
        let mut result = CopyResult::new();
        for entry in resolved {
            let Ok(created) = messages.copy_entry(entry.id, destination.id).await else {
                // A placement removed since this snapshot was taken is skipped
                // rather than failing the whole command, matching how FETCH and
                // STORE degrade (ADR-046). It contributes no `COPYUID` pair.
                continue;
            };
            result.record(entry.uid, created.uid);
        }

        // `[COPYUID]` is omitted when nothing was copied: there are no UIDs to
        // report, and an empty set would be malformed (RFC 4315 §3).
        let text = format!(
            "{}{} completed",
            if command.uid_mode { "UID " } else { "" },
            command.kind
        );
        let reply = match result.copyuid(destination.uid_validity) {
            Some(code) => response::tagged_with_code(&command.tag, Status::Ok, &code, &text),
            None => response::tagged(&command.tag, Status::Ok, &text),
        };
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `MOVE` and `UID MOVE` (RFC 6851).
    ///
    /// ```text
    /// MOVE <set> <mailbox>       ──▶ set read as sequence numbers
    /// UID MOVE <uid-set> <mbox>  ──▶ set read as UIDs
    ///                │
    ///                ▼
    ///        MovePlan over the selected mailbox's SequenceMap
    ///                │
    ///                ▼
    ///        move_entry per placement          ← storage assigns the UID
    ///                │                           and removes the source
    ///     ┌──────────┼──────────────┐
    ///     ▼          ▼              ▼
    /// [COPYUID]  * <seq> EXPUNGE   snapshot loses the moved placements
    /// ```
    ///
    /// `MOVE` is the one command that both writes a destination *and* mutates
    /// the selected mailbox, so it is the only place all three mechanisms meet:
    /// the `[COPYUID]` pairing of [`COPY`](Self::copy), the `* <seq> EXPUNGE`
    /// reporting of [`EXPUNGE`](Self::expunge), and the snapshot membership
    /// change of ADR-049.
    async fn move_messages(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(set_text) = command
            .args
            .first()
            .and_then(Token::as_text)
            .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        else {
            return self
                .bad(
                    command,
                    &format!("{} requires a sequence set", command.kind),
                )
                .await;
        };

        let set = match SequenceSet::parse(&set_text) {
            Ok(set) => set,
            Err(error) => return self.bad(command, error.message()).await,
        };

        let Some(destination_name) = self.mailbox_argument(command, 1).await? else {
            return Ok(Flow::Continue);
        };

        let Some(selected) = self.selected_mailbox.clone() else {
            return self
                .bad(command, Refusal::NeedsSelectedMailbox.message())
                .await;
        };

        // Already refused by the state machine, which groups MOVE with the
        // writers; restated so a future routing change cannot make EXAMINE
        // remove messages.
        if selected.is_read_only() {
            return self
                .bad(command, Refusal::MailboxIsReadOnly.message())
                .await;
        }

        let (Some(mailboxes), Some(messages)) =
            (self.mailboxes.clone(), self.messages_store.clone())
        else {
            return self.no_store(command).await;
        };

        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let Some(destination) = self
            .resolve_destination(command, &mailboxes, account.id, &destination_name)
            .await?
        else {
            return Ok(Flow::Continue);
        };

        // A message is placed in a given mailbox at most once — an invariant of
        // the domain model (`CORE_SPEC.md` §7.3, a unique index on
        // `(mailbox_id, message_id)`), not a limitation of this handler. A
        // same-mailbox `MOVE` therefore has nowhere to move a message *to*: the
        // destination placement it would create is the source placement it
        // would remove. Storage's `move_entry` recognises this and returns the
        // source unchanged, which is the right thing for storage to do and the
        // wrong thing to report as a move — `OK [COPYUID]` would name a
        // destination UID equal to the source UID and an `EXPUNGE` for a
        // message that is still there, corrupting the client's snapshot.
        //
        // It is refused explicitly instead. RFC 6851 §3.3 does not require a
        // server to support moving a message onto itself, and a `NO` a client
        // can act on beats a success that silently did nothing (ADR-051,
        // following ADR-050's treatment of the same invariant for `COPY`).
        if destination.id == selected.mailbox_id {
            let reply = response::tagged(
                &command.tag,
                Status::No,
                "cannot move a message into its own mailbox",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        // Planned against the snapshot as it is *now*, before anything moves:
        // every reported sequence number must come from one unmutated view.
        let mut plan = MovePlan::resolve(selected.sequences(), &set, command.uid_mode);

        // One transaction per placement, through the port. Storage assigns the
        // destination UID, removes the source, and emits `EntryDeleted` plus
        // `EntryCreated`; this adapter issues no SQL, computes no UID of its
        // own, and publishes no event.
        let attempts: Vec<Moving> = plan.iter().copied().collect();
        for moving in attempts {
            let Ok(created) = messages.move_entry(moving.entry_id, destination.id).await else {
                // A placement removed since this snapshot was taken, or one
                // storage refused, is skipped rather than aborting the rest.
                // It is recorded nowhere, so it contributes neither a
                // `[COPYUID]` pair nor an `EXPUNGE` line — and it makes the
                // command partial, which the tagged response below reports.
                continue;
            };
            plan.record(moving, created.uid);
        }

        // Reported highest sequence number first, so each number is still valid
        // when the client applies it (RFC 9051 §7.4.1).
        let mut out = String::new();
        for moving in plan.report_order() {
            out.push_str(&response::count_line(moving.sequence, "EXPUNGE"));
        }

        // The acting session's own view changes now, exactly as it does for
        // `EXPUNGE` (ADR-049): the moved placements leave and the survivors
        // renumber. Only placements that actually moved are removed.
        if let Some(selection) = self.selected_mailbox.as_mut() {
            selection.remove_expunged(&plan.moved_uids());
        }

        // `[COPYUID]` is omitted when nothing moved: there are no UIDs to
        // report, and an empty set would be malformed (RFC 4315 §3).
        let code = plan.copyuid(destination.uid_validity);
        let prefix = if command.uid_mode { "UID " } else { "" };

        // A partial move is not a success. The moves that happened are already
        // committed and are reported — the client must know about them — but
        // the tagged response is `NO`, because `OK` would assert that every
        // named message moved (ADR-052).
        out.push_str(&if plan.is_partial() {
            let text = format!(
                "{}{} moved {} of {} messages",
                prefix,
                command.kind,
                plan.moved_len(),
                plan.planned_len()
            );
            match code {
                Some(code) => response::tagged_with_code(&command.tag, Status::No, &code, &text),
                None => response::tagged(&command.tag, Status::No, &text),
            }
        } else {
            let text = format!("{prefix}{} completed", command.kind);
            match code {
                Some(code) => response::tagged_with_code(&command.tag, Status::Ok, &code, &text),
                None => response::tagged(&command.tag, Status::Ok, &text),
            }
        });

        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `APPEND` (RFC 9051 §6.3.12).
    ///
    /// ```text
    /// APPEND <mailbox> [(flags)] [date-time] {literal}
    ///                        │
    ///                        ▼
    ///              AppendRequest (pure parse)
    ///                        │
    ///                        ▼
    ///          MessageStore::insert — one transaction
    ///                        │        assigns the UID, emits the events
    ///                        ▼
    ///        OK [APPENDUID <uidvalidity> <uid>] APPEND completed
    /// ```
    ///
    /// # The literal is already bounded when this runs
    ///
    /// [`read_command`](Self::read_command) reads the literal, and the parser
    /// checked its declared length against `max_literal_size` *before* a single
    /// octet was read (ADR-040). `APPEND` therefore adds no new way to exhaust
    /// memory and provides no route around the existing message-size limit: the
    /// same ceiling applies as to every other literal.
    ///
    /// # The bytes are authoritative
    ///
    /// The submitted octets are stored exactly as received and are never
    /// re-encoded or line-ending-normalised. Structure is derived from them by
    /// the one MIME parser (`sandesh_core::parse`), never re-derived here.
    async fn append(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        let Some(mailbox_name) = self.mailbox_argument(command, 0).await? else {
            return Ok(Flow::Continue);
        };

        // Parsed before anything is looked up, so a malformed APPEND cannot
        // reach storage and cannot report a mailbox's existence through the
        // shape of its error.
        let request = match AppendRequest::parse(command.args.get(1..).unwrap_or_default()) {
            Ok(request) => request,
            Err(error) => return self.bad(command, &error.message()).await,
        };

        // An empty literal is well-formed IMAP but is not a message: it has no
        // headers, so it can carry no envelope. Refused rather than stored as a
        // placement nothing can render.
        if request.is_empty() {
            return self
                .bad(command, "APPEND requires a non-empty message")
                .await;
        }

        let (Some(mailboxes), Some(messages)) =
            (self.mailboxes.clone(), self.messages_store.clone())
        else {
            return self.no_store(command).await;
        };

        let Some(account) = self.current_account().await else {
            let reply = response::tagged(&command.tag, Status::No, "no account for this session");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // RFC 9051 §6.3.12: a nonexistent destination is `NO [TRYCREATE]`. The
        // RFC is explicit that the server must *not* create the mailbox itself,
        // which is why the shared resolver does not.
        let Some(destination) = self
            .resolve_destination(command, &mailboxes, account.id, &mailbox_name)
            .await?
        else {
            return Ok(Flow::Continue);
        };

        let received_at = Timestamp::now();
        let raw = RawMessage::new(request.message().to_vec());
        // The one MIME parser. `APPEND` derives structure exactly as delivery
        // does, so a message appended and a message delivered are indistinguishable
        // once stored.
        let parsed = sandesh_core::parse(raw.bytes());

        // An appended message has no SMTP envelope: nobody sent it anywhere.
        // Recording the authenticated identity rather than inventing a
        // `MAIL FROM` keeps the delivery record honest about how it arrived.
        let envelope = Envelope {
            mail_from: None,
            rcpt_to: Vec::new(),
            helo_domain: None,
            peer: self.peer.clone(),
            tls: None,
            auth_identity: self.identity.clone(),
        };

        // One placement, in one transaction, through the port. Storage assigns
        // the UID and emits `EntryCreated`/`MessageReceived`; this adapter
        // computes no UID and publishes no event. A failure here therefore
        // emits nothing at all — there is no phantom event for an APPEND that
        // did not happen.
        let inserted = match messages
            .insert(&NewMessage {
                raw,
                envelope,
                source: DeliverySource::ImapAppend,
                parsed,
                received_at,
                placements: vec![NewPlacement {
                    mailbox_id: destination.id,
                    flags: request.flags().clone(),
                    internal_date: request.internal_date(received_at),
                }],
            })
            .await
        {
            Ok(inserted) => inserted,
            Err(error) => {
                // The message itself is never logged, at any level: it is user
                // content, and this is the one path that has it in hand.
                tracing::warn!(%error, mailbox = %mailbox_name, "APPEND failed");
                let reply = response::tagged(&command.tag, Status::No, "cannot store the message");
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        };

        // `[APPENDUID]` reports the UID storage assigned (RFC 4315 §3), never a
        // value computed from `UIDNEXT`: a concurrent APPEND into the same
        // mailbox interleaves allocations, and a guess would name someone
        // else's message.
        let created = inserted
            .entries
            .iter()
            .find(|entry| entry.mailbox_id == destination.id);

        let reply = match created {
            Some(entry) => response::tagged_with_code(
                &command.tag,
                Status::Ok,
                &format!(
                    "APPENDUID {} {}",
                    destination.uid_validity.get(),
                    entry.uid.get()
                ),
                "APPEND completed",
            ),
            // Storage reported success but created no placement here. Not a
            // success to report: saying OK would tell the client a message is
            // in a mailbox it is not in.
            None => response::tagged(&command.tag, Status::No, "the message was not placed"),
        };
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// `IDLE` (RFC 2177).
    ///
    /// ```text
    ///  C: a1 IDLE
    ///  S: + idling                     ← continuation: the client may now wait
    ///     ...
    ///  S: * 4 EXISTS                   ← unsolicited, as the mailbox changes
    ///  S: * 2 EXPUNGE
    ///  C: DONE                         ← untagged, and the only word accepted
    ///  S: a1 OK IDLE terminated
    /// ```
    ///
    /// # How the wait works
    ///
    /// One `select!` over two futures: the next line from the client, and the
    /// next event from the [`EventSource`](sandesh_core::EventSource). Both are
    /// genuinely asynchronous, so the task is parked until one of them is ready
    /// — there is no polling loop, no sleep, and nothing that occupies the
    /// runtime while a session idles. A thousand idle sessions cost a thousand
    /// parked tasks and their bounded event queues, and no CPU at all.
    ///
    /// # Why the subscription is taken here and not at SELECT
    ///
    /// It exists only for the duration of the `IDLE`, so it is created on entry
    /// and dropped on exit. Dropping it releases the queue behind it, which is
    /// what stops an abandoned connection from accumulating events forever
    /// (phase brief §9). A session that never idles holds no subscription.
    ///
    /// # `DONE`, and only `DONE`
    ///
    /// RFC 2177 §3 defines exactly one way out: the untagged line `DONE`. While
    /// idling, the client is not sending commands, so anything else is a
    /// protocol violation rather than a command to run — and treating it as one
    /// would let a client smuggle a command past the tag it never sent. Any
    /// other line ends the `IDLE` with `BAD`, leaving the session in a defined
    /// state rather than half-idling.
    async fn idle(&mut self, command: &Command) -> Result<Flow, ProtocolError> {
        // A session with no event source can still IDLE — it simply has nothing
        // to report — but saying so is more useful than a silent wait that can
        // never produce anything.
        let Some(events) = self.events.clone() else {
            let reply = response::tagged(
                &command.tag,
                Status::No,
                "IDLE is unavailable on this server",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // Subscribed *before* the continuation is sent. A change that lands
        // between the two would otherwise be missed: the client believes it is
        // idling from the moment it sees `+`, and the server must already be
        // listening by then.
        let mut stream = events.subscribe();

        let request = response::continuation("idling");
        self.connection.write_flush(request.as_bytes()).await?;

        let outcome = self.idle_loop(&mut stream).await;

        // The subscription is dropped here however the loop ended — normally,
        // by timeout, or by a disconnect — so no queue outlives the IDLE.
        drop(stream);

        match outcome? {
            IdleEnd::Done => {
                let reply = response::tagged(&command.tag, Status::Ok, "IDLE terminated");
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(Flow::Continue)
            }
            IdleEnd::Timeout => {
                // RFC 2177 §3 permits ending an IDLE the client did not renew.
                // Reported as a tagged completion rather than a dropped
                // connection, so the client can simply IDLE again.
                let reply = response::tagged(
                    &command.tag,
                    Status::Ok,
                    "IDLE terminated; re-issue IDLE to continue",
                );
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(Flow::Continue)
            }
            IdleEnd::Unexpected(line) => {
                let reply = response::tagged(
                    &command.tag,
                    Status::Bad,
                    &format!("expected DONE while idling, got: {line}"),
                );
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(Flow::Continue)
            }
        }
    }

    /// Waits for `DONE` or for events, whichever comes first, until the IDLE ends.
    ///
    /// Split from [`idle`](Self::idle) so the subscription's lifetime is
    /// explicit at the call site: this borrows the stream, and the caller drops
    /// it. A disconnect propagates as an error, which ends the session — the
    /// clean shutdown RFC 2177 requires of an abandoned IDLE.
    async fn idle_loop(
        &mut self,
        stream: &mut Box<dyn EventStream>,
    ) -> Result<IdleEnd, ProtocolError> {
        let deadline = tokio::time::Instant::now() + self.config.idle_timeout;

        loop {
            tokio::select! {
                // Biased so a pending line is always taken before an event.
                // Without it, a client that sent DONE could be made to wait
                // behind an arbitrarily long run of events.
                biased;

                line = self.connection.read_line_owned(self.config.command_timeout) => {
                    let line = line?;
                    let text = String::from_utf8_lossy(&line);
                    let trimmed = text.trim_end_matches(['\r', '\n']);
                    // Case-insensitive: RFC 2177 §3 writes `DONE` as an atom.
                    if trimmed.eq_ignore_ascii_case("DONE") {
                        return Ok(IdleEnd::Done);
                    }
                    // Truncated before it is echoed: an unbounded echo of client
                    // input into a response is how a log or a client gets flooded.
                    let shown: String = trimmed.chars().take(32).collect();
                    return Ok(IdleEnd::Unexpected(shown));
                }

                event = stream.next() => {
                    // `None` means the source is gone. Nothing further can
                    // arrive, so the IDLE keeps waiting for DONE rather than
                    // ending: the client asked to idle, and the connection is
                    // still perfectly usable.
                    let Some(event) = event else {
                        continue;
                    };
                    self.report(&event).await?;
                }

                () = tokio::time::sleep_until(deadline) => {
                    return Ok(IdleEnd::Timeout);
                }
            }
        }
    }

    /// Writes the unsolicited response an event calls for, if any.
    ///
    /// Everything mailbox-scoped happens in [`crate::idle::Update::from_event`]:
    /// an event naming a mailbox this session has not selected produces no
    /// update and therefore no byte on the wire. A session with nothing
    /// selected reports nothing at all.
    async fn report(&mut self, event: &DomainEvent) -> Result<(), ProtocolError> {
        let Some(selected) = self.selected_mailbox.as_ref() else {
            return Ok(());
        };
        let Some(update) = Update::from_event(event, selected.mailbox_id) else {
            return Ok(());
        };

        // An arrival needs the placement itself, not just its UID: the snapshot
        // must be able to answer a following `FETCH` for it. It is read through
        // the port like any other lookup.
        let entry = match (&update, self.messages_store.as_ref()) {
            (Update::Exists { uid }, Some(messages)) => messages
                .get_entry_by_uid(selected.mailbox_id, *uid)
                .await
                .ok()
                .flatten(),
            _ => None,
        };

        // Read before the mutable borrow below, and from this session's own
        // enabled set: whether the client sees `MODSEQ` is a property of the
        // connection, never of the server (RFC 5161 §3).
        let condstore = self.enabled.has(crate::enable::Extension::CondStore);

        let Some(selection) = self.selected_mailbox.as_mut() else {
            return Ok(());
        };
        // The snapshot change and the response line are produced together, so
        // the client's view and this session's view cannot diverge.
        if let Some(line) = idle::apply(
            &update,
            selection.sequences_mut(),
            entry.as_ref(),
            condstore,
        ) {
            self.connection.write_flush(line.as_bytes()).await?;
        }
        Ok(())
    }

    /// Resolves the destination mailbox shared by `COPY`, `MOVE` and `APPEND`.
    ///
    /// Returns `None` when the destination cannot receive messages *and* the
    /// refusal has already been written, so a caller's only job is to stop.
    ///
    /// The three commands differ in what they do with a destination but not in
    /// what makes one usable, so the checks live here once. RFC 9051 §6.4.7
    /// defines the important one: a nonexistent destination is
    /// `NO [TRYCREATE]`, the code that tells a client it may create the mailbox
    /// and retry. It is *not* `BAD` — the command was well formed — and this
    /// server does not create the mailbox implicitly.
    /// The domain mailbox service over this session's store.
    ///
    /// Built per call rather than held as a field: the store is optional (a
    /// session can run without one and refuse the commands that need it), and
    /// `MailboxService` is a thin wrapper over an `Arc` clone, so there is
    /// nothing to cache. Keeping it out of the struct also means the wiring in
    /// `server.rs` is unchanged.
    fn mailbox_service(&self) -> Option<MailboxService> {
        self.mailboxes.clone().map(MailboxService::new)
    }

    async fn resolve_destination(
        &mut self,
        command: &Command,
        mailboxes: &Arc<dyn MailboxStore>,
        account_id: AccountId,
        name: &MailboxName,
    ) -> Result<Option<Mailbox>, ProtocolError> {
        // Existence and the `\Noselect` refusal are the service's; the
        // `[TRYCREATE]` response code is RFC 9051 §6.4.7's and stays here,
        // which is the whole shape of the split (CORE_SPEC §9).
        match MailboxService::new(Arc::clone(mailboxes))
            .resolve_selectable(account_id, name)
            .await
        {
            Ok(destination) => Ok(Some(destination)),
            Err(CoreError::NotFound { .. }) => {
                let reply = response::tagged_with_code(
                    &command.tag,
                    Status::No,
                    "TRYCREATE",
                    "no such mailbox",
                );
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(None)
            }
            Err(CoreError::Conflict { .. }) => {
                let reply = response::tagged(
                    &command.tag,
                    Status::No,
                    "destination mailbox is not selectable",
                );
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(None)
            }
            Err(_) => {
                let reply = response::tagged(&command.tag, Status::No, "mailbox is unavailable");
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(None)
            }
        }
    }

    /// Answers a command with `BAD` and the given text.
    async fn bad(&mut self, command: &Command, text: &str) -> Result<Flow, ProtocolError> {
        let reply = response::tagged(&command.tag, Status::Bad, text);
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// Collects a `PLAIN` credential, prompting if there was no initial response.
    async fn collect_plain(
        &mut self,
        initial: Option<Secret>,
    ) -> Result<Result<Credentials, SaslError>, ProtocolError> {
        let encoded = if let Some(encoded) = initial {
            encoded
        } else {
            // No initial response, so the client is waiting for a continuation
            // request before it sends anything (RFC 9051 §7.5). Reading first
            // would deadlock: both ends would be waiting for the other.
            // PLAIN's challenge is empty, so this is a bare `+ `.
            let challenge = response::continuation("");
            self.connection.write_flush(challenge.as_bytes()).await?;
            match self.read_sasl_line().await? {
                Ok(line) => line,
                Err(error) => return Ok(Err(error)),
            }
        };
        Ok(decode_plain(encoded.expose()))
    }

    /// Collects a `LOGIN` credential through its two prompts.
    async fn collect_login(
        &mut self,
        initial: Option<Secret>,
    ) -> Result<Result<Credentials, SaslError>, ProtocolError> {
        let [username_prompt, password_prompt] = login_prompts();

        // LOGIN's initial response, when present, is the username.
        let username_encoded = if let Some(encoded) = initial {
            encoded
        } else {
            let challenge = response::continuation(&username_prompt);
            self.connection.write_flush(challenge.as_bytes()).await?;
            match self.read_sasl_line().await? {
                Ok(line) => line,
                Err(error) => return Ok(Err(error)),
            }
        };

        let username = match decode_login_field(username_encoded.expose()) {
            Ok(username) => username,
            Err(error) => return Ok(Err(error)),
        };

        let challenge = response::continuation(&password_prompt);
        self.connection.write_flush(challenge.as_bytes()).await?;
        let password_encoded = match self.read_sasl_line().await? {
            Ok(line) => line,
            Err(error) => return Ok(Err(error)),
        };

        let password = match decode_login_field(password_encoded.expose()) {
            Ok(password) => password,
            Err(error) => return Ok(Err(error)),
        };

        Ok(Ok(Credentials {
            username,
            password: Secret::new(password),
        }))
    }

    /// Reads one line of a SASL exchange.
    ///
    /// Wrapped in a [`Secret`] immediately: for `PLAIN` this line *is* the
    /// password, and a `String` here would be one `tracing` call away from a
    /// credential on disk (ADR-035). A lone `*` is the RFC 9051 §6.2.2
    /// cancellation token.
    async fn read_sasl_line(&mut self) -> Result<Result<Secret, SaslError>, ProtocolError> {
        let line = self
            .connection
            .read_line_owned(self.config.command_timeout)
            .await?;

        if line.as_slice() == b"*" {
            return Ok(Err(SaslError::Cancelled));
        }

        Ok(Ok(Secret::new(String::from_utf8_lossy(&line).into_owned())))
    }

    /// Answers a malformed or cancelled SASL exchange.
    ///
    /// Every decode failure is one indistinguishable `NO` on the wire: telling
    /// a client *how* its payload was wrong is a probing oracle.
    async fn reject_sasl(&mut self, tag: &Tag, error: SaslError) -> Result<(), ProtocolError> {
        let text = if matches!(error, SaslError::Cancelled) {
            "authentication cancelled"
        } else {
            "authentication failed"
        };
        let reply = response::tagged(tag, Status::No, text);
        self.connection.write_flush(reply.as_bytes()).await
    }

    /// Verifies a credential and moves the session on success.
    ///
    /// Reuses [`verify_optional`], which performs the hash comparison even when
    /// the account does not exist so that both cases take the same time
    /// (ADR-032). That is what makes an unknown user indistinguishable from a
    /// wrong password — the anti-enumeration property — and it is why this
    /// method must not short-circuit on a missing account.
    async fn verify(&mut self, tag: &Tag, credentials: Credentials) -> Result<Flow, ProtocolError> {
        let stored = self.credential_hash(&credentials.username).await;

        if verify_optional(&credentials.password, stored.as_deref()) {
            self.state = State::Authenticated;
            self.identity = Some(credentials.username.clone());

            tracing::info!(
                peer = self.peer.as_deref().unwrap_or("unknown"),
                username = %credentials.username,
                "IMAP authentication succeeded"
            );

            // The capability list changes meaning once authenticated, so it is
            // returned with the completion — RFC 9051 §6.2.2 recommends this so
            // the client need not ask again.
            let reply = response::tagged_with_code(
                tag,
                Status::Ok,
                &format!("CAPABILITY {}", self.capabilities().to_line()),
                "AUTHENTICATE completed",
            );
            self.connection.write_flush(reply.as_bytes()).await?;
        } else {
            // The username is logged; the password is a `Secret` and cannot be.
            tracing::warn!(
                peer = self.peer.as_deref().unwrap_or("unknown"),
                username = %credentials.username,
                "IMAP authentication failed"
            );

            // One message for a wrong password and for an unknown user.
            let reply = response::tagged(tag, Status::No, "authentication failed");
            self.connection.write_flush(reply.as_bytes()).await?;
        }

        Ok(Flow::Continue)
    }

    /// Looks up a stored password hash, if the account has one.
    ///
    /// Reaches storage only through [`AccountStore`], the port the core defines.
    /// A storage failure yields `None`, which `verify_optional` treats as a
    /// failed authentication — the safe direction.
    async fn credential_hash(&self, username: &str) -> Option<String> {
        match self.accounts.get_by_username(username).await {
            Ok(Some(account)) => account.credential.map(|credential| credential.hash),
            Ok(None) => None,
            Err(error) => {
                tracing::error!(%error, "cannot read the account for IMAP authentication");
                None
            }
        }
    }
}

/// Recovers the tag from a line whose arguments failed to parse.
///
/// Best effort, and validated through [`Tag::parse`] so an unparseable tag is
/// never echoed — echoing raw client bytes is how a response line gets forged.
fn recover_tag(line: &[u8]) -> Option<Tag> {
    let text = core::str::from_utf8(line).ok()?;
    let candidate = text.trim_start().split(' ').next()?;
    Tag::parse(candidate)
}

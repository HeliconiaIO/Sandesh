//! IMAP command vocabulary (RFC 9051 §6).

use core::fmt;

/// Which IMAP command was issued.
///
/// Only commands Sandesh actually implements appear here. That is deliberate:
/// [`CommandKind`] drives both the state machine and the capability list, so a
/// variant that existed without an implementation would let the server
/// advertise a command it cannot honour — the failure mode the phase brief §6
/// and §14 both forbid.
///
/// `IDLE` is present as of the increment that implemented it (RFC 2177); it was
/// previously absent by decision (ADR-039) for exactly the reason this list
/// exists — an unimplemented variant would let the server advertise a command
/// it cannot honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandKind {
    // Any state (RFC 9051 §6.1)
    /// `CAPABILITY`.
    Capability,
    /// `NOOP`.
    Noop,
    /// `LOGOUT`.
    Logout,
    /// `ID` (RFC 2971).
    Id,
    /// `ENABLE` (RFC 5161).
    Enable,
    /// `CHECK` (RFC 9051 §6.4.1).
    Check,

    // Not authenticated (RFC 9051 §6.2)
    /// `STARTTLS`.
    StartTls,
    /// `AUTHENTICATE`.
    Authenticate,
    /// `LOGIN`.
    Login,

    // Authenticated (RFC 9051 §6.3)
    /// `SELECT`.
    Select,
    /// `EXAMINE`.
    Examine,
    /// `CREATE`.
    Create,
    /// `DELETE`.
    Delete,
    /// `RENAME`.
    Rename,
    /// `SUBSCRIBE`.
    Subscribe,
    /// `UNSUBSCRIBE`.
    Unsubscribe,
    /// `LIST`.
    List,
    /// `NAMESPACE`.
    Namespace,
    /// `STATUS`.
    Status,
    /// `APPEND`.
    Append,

    // Selected (RFC 9051 §6.4)
    /// `CLOSE`.
    Close,
    /// `UNSELECT`.
    Unselect,
    /// `EXPUNGE`, including `UID EXPUNGE`.
    Expunge,
    /// `SEARCH`, including `UID SEARCH`.
    Search,
    /// `FETCH`, including `UID FETCH`.
    Fetch,
    /// `STORE`, including `UID STORE`.
    Store,
    /// `COPY`, including `UID COPY`.
    Copy,
    /// `MOVE` (RFC 6851), including `UID MOVE`.
    Move,
    /// `IDLE` (RFC 2177).
    Idle,
}

impl CommandKind {
    /// Parses a command name, case-insensitively (RFC 9051 §9 `atom`).
    ///
    /// Returns `None` for anything unimplemented, which the session reports as
    /// `BAD`. `LSUB` is intentionally not accepted: RFC 9051 removed it in
    /// favour of `LIST` with selection options, and accepting it would mean
    /// advertising `IMAP4rev1` behaviour this server does not implement.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "CAPABILITY" => Some(Self::Capability),
            "NOOP" => Some(Self::Noop),
            "LOGOUT" => Some(Self::Logout),
            "ID" => Some(Self::Id),
            "ENABLE" => Some(Self::Enable),
            "CHECK" => Some(Self::Check),
            "STARTTLS" => Some(Self::StartTls),
            "AUTHENTICATE" => Some(Self::Authenticate),
            "LOGIN" => Some(Self::Login),
            "SELECT" => Some(Self::Select),
            "EXAMINE" => Some(Self::Examine),
            "CREATE" => Some(Self::Create),
            "DELETE" => Some(Self::Delete),
            "RENAME" => Some(Self::Rename),
            "SUBSCRIBE" => Some(Self::Subscribe),
            "UNSUBSCRIBE" => Some(Self::Unsubscribe),
            "LIST" => Some(Self::List),
            "NAMESPACE" => Some(Self::Namespace),
            "STATUS" => Some(Self::Status),
            "APPEND" => Some(Self::Append),
            "CLOSE" => Some(Self::Close),
            "UNSELECT" => Some(Self::Unselect),
            "EXPUNGE" => Some(Self::Expunge),
            "SEARCH" => Some(Self::Search),
            "FETCH" => Some(Self::Fetch),
            "STORE" => Some(Self::Store),
            "COPY" => Some(Self::Copy),
            "MOVE" => Some(Self::Move),
            "IDLE" => Some(Self::Idle),
            _ => None,
        }
    }

    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Capability => "CAPABILITY",
            Self::Noop => "NOOP",
            Self::Logout => "LOGOUT",
            Self::Id => "ID",
            Self::Enable => "ENABLE",
            Self::Check => "CHECK",
            Self::StartTls => "STARTTLS",
            Self::Authenticate => "AUTHENTICATE",
            Self::Login => "LOGIN",
            Self::Select => "SELECT",
            Self::Examine => "EXAMINE",
            Self::Create => "CREATE",
            Self::Delete => "DELETE",
            Self::Rename => "RENAME",
            Self::Subscribe => "SUBSCRIBE",
            Self::Unsubscribe => "UNSUBSCRIBE",
            Self::List => "LIST",
            Self::Namespace => "NAMESPACE",
            Self::Status => "STATUS",
            Self::Append => "APPEND",
            Self::Close => "CLOSE",
            Self::Unselect => "UNSELECT",
            Self::Expunge => "EXPUNGE",
            Self::Search => "SEARCH",
            Self::Fetch => "FETCH",
            Self::Store => "STORE",
            Self::Copy => "COPY",
            Self::Move => "MOVE",
            Self::Idle => "IDLE",
        }
    }

    /// Whether `UID` may prefix this command (RFC 9051 §6.4.9).
    #[must_use]
    pub const fn accepts_uid_prefix(self) -> bool {
        matches!(
            self,
            Self::Copy | Self::Fetch | Self::Store | Self::Search | Self::Expunge | Self::Move
        )
    }
}

impl fmt::Display for CommandKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed command: its tag, its verb, and its still-unparsed arguments.
///
/// Arguments stay as [`Token`]s rather than becoming a per-command struct.
/// Command-specific interpretation (a sequence set, a fetch item list) belongs
/// to the handler that knows the grammar for it, and keeping the split means
/// the parser has exactly one job: turn octets into well-formed tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The client's tag, echoed in the tagged response (RFC 9051 §2.2.1).
    pub tag: Tag,
    /// The command.
    pub kind: CommandKind,
    /// Whether the command was prefixed with `UID`.
    pub uid_mode: bool,
    /// The arguments, in order.
    pub args: Vec<Token>,
}

/// A validated command tag.
///
/// Wrapped rather than left as a `String` so that an unvalidated tag cannot be
/// echoed back: a tag containing CRLF would let a client inject a forged
/// response line into the stream it is reading (response splitting).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tag(String);

impl Tag {
    /// The longest tag accepted.
    ///
    /// RFC 9051 sets no maximum. Real clients use a handful of octets; this
    /// bounds what must be buffered and echoed.
    pub const MAX_LEN: usize = 32;

    /// Validates a tag against RFC 9051 §9 `tag = 1*<any ASTRING-CHAR except "+">`.
    ///
    /// Rejects the empty tag, over-long tags, `+` (reserved for continuations),
    /// and any character outside the permitted set — which excludes CR, LF,
    /// space and the list/literal delimiters, and so makes injection
    /// impossible by construction.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.is_empty() || raw.len() > Self::MAX_LEN {
            return None;
        }
        let permitted = raw.bytes().all(|b| {
            b.is_ascii_graphic()
                && !matches!(
                    b,
                    b'+' | b'(' | b')' | b'{' | b'}' | b'"' | b'\\' | b'%' | b'*'
                )
        });
        permitted.then(|| Self(raw.to_owned()))
    }

    /// The tag as it should appear on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One argument in a command.
///
/// # Why `Literal` holds `Vec<u8>` and the others hold `String`
///
/// Atoms and quoted strings are constrained by the grammar to text. A literal
/// is explicitly arbitrary binary (RFC 9051 §4.3) — it carries message bytes,
/// and decoding it as UTF-8 would corrupt the octets the product guarantees to
/// preserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// An unquoted atom, e.g. `INBOX`, `FLAGS`, `1:*`.
    Atom(String),
    /// A quoted string with escapes resolved, e.g. `"Sent Items"`.
    Quoted(String),
    /// A literal's payload — arbitrary octets.
    Literal(Vec<u8>),
    /// `NIL` (RFC 9051 §4.5).
    Nil,
    /// A parenthesised list, which may nest.
    List(Vec<Token>),
}

impl Token {
    /// The token's text, when it has an unambiguous textual form.
    ///
    /// `NIL` yields `None` rather than `"NIL"`: the distinction between the
    /// absent value and the string `"NIL"` is meaningful in IMAP, and
    /// flattening it here would lose it.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Atom(s) | Self::Quoted(s) => Some(s),
            Self::Literal(bytes) => core::str::from_utf8(bytes).ok(),
            Self::Nil | Self::List(_) => None,
        }
    }

    /// The token's raw octets, for arguments that may be binary.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Atom(s) | Self::Quoted(s) => Some(s.as_bytes()),
            Self::Literal(bytes) => Some(bytes),
            Self::Nil | Self::List(_) => None,
        }
    }
}

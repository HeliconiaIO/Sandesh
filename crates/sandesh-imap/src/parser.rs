//! The IMAP command parser (RFC 9051 §9).
//!
//! # Shape of the problem
//!
//! An IMAP command is not a line. A command containing a literal spans several
//! lines, and the server must send a continuation request (`+`) and then read
//! an exact octet count before it can finish parsing:
//!
//! ```text
//! C: A001 LOGIN {5}
//! S: + ready for literal data
//! C: alice LOGIN-password
//! S: A001 OK ...
//! ```
//!
//! So parsing cannot be a pure `&[u8] -> Command` function. Instead
//! [`parse_line`] returns [`Parsed::NeedLiteral`] when it stops at a literal,
//! and the session feeds the payload back with [`Parsed::resume`]. The parser
//! stays free of I/O — it never reads a socket — which is what keeps it
//! synchronously testable against the boundary cases in §10.
//!
//! # Bounds before allocation
//!
//! A literal declares its own length. The declared length is checked against
//! the configured limit *before* anything is allocated, so `{4294967295}` costs
//! a rejection rather than 4 GiB (phase brief §10, §23). Nesting depth and
//! token count are bounded for the same reason: `((((…))))` must not recurse
//! until the stack is gone.

use crate::command::{Command, CommandKind, Tag, Token};

/// How deeply parenthesised lists may nest.
///
/// Real commands nest two or three deep (a `FETCH BODY[HEADER.FIELDS (...)]`).
/// The limit exists so that a client cannot drive unbounded recursion.
pub const MAX_NESTING_DEPTH: usize = 16;

/// The most tokens accepted in one command.
///
/// Bounds a client that sends a million single-character atoms.
pub const MAX_TOKENS: usize = 1024;

/// Why a command could not be parsed.
///
/// Every variant becomes a `BAD` response. None of them can panic the session:
/// that is the point of returning an error rather than indexing optimistically.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The line was empty or whitespace only.
    #[error("empty command")]
    Empty,

    /// The tag was missing or violated RFC 9051 §9 `tag`.
    #[error("invalid tag")]
    InvalidTag,

    /// No command name followed the tag.
    #[error("missing command name")]
    MissingCommand,

    /// The command name is not implemented by this server.
    #[error("unrecognised command")]
    UnknownCommand,

    /// `UID` prefixed a command that does not accept it (RFC 9051 §6.4.9).
    #[error("UID is not valid with this command")]
    UidNotPermitted,

    /// A quoted string was never closed.
    #[error("unterminated quoted string")]
    UnterminatedQuoted,

    /// A quoted string contained a raw CR or LF, which RFC 9051 §9 forbids.
    #[error("illegal character in quoted string")]
    IllegalQuotedChar,

    /// A `(` had no matching `)`.
    #[error("unterminated list")]
    UnterminatedList,

    /// A `)` appeared with no open list.
    #[error("unbalanced parenthesis")]
    UnbalancedParen,

    /// Lists nested deeper than [`MAX_NESTING_DEPTH`].
    #[error("list nested too deeply")]
    NestingTooDeep,

    /// More than [`MAX_TOKENS`] tokens in one command.
    #[error("too many arguments")]
    TooManyTokens,

    /// A literal's `{...}` was malformed — no `}`, not a number, or trailing
    /// junk before the line ended.
    #[error("malformed literal")]
    MalformedLiteral,

    /// The declared literal length exceeded the configured maximum.
    ///
    /// Carries the limit, not the request: the requested size is attacker
    /// controlled and unbounded.
    #[error("literal exceeds the {limit} byte limit")]
    LiteralTooLarge {
        /// The configured maximum.
        limit: usize,
    },

    /// The command line was not valid UTF-8 where the grammar requires text.
    #[error("invalid encoding")]
    InvalidEncoding,
}

/// The outcome of parsing one line of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// A complete command.
    Complete(Command),
    /// Parsing stopped at a literal; the session must supply its payload.
    NeedLiteral(PendingLiteral),
}

/// A command that is waiting for literal octets.
///
/// Holds the tokens parsed so far plus the declared length. The session sends a
/// continuation request (unless [`Self::is_non_synchronising`]), reads exactly
/// [`Self::length`] octets, and calls [`Self::resume`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLiteral {
    tag: Tag,
    kind: CommandKind,
    uid_mode: bool,
    tokens: Vec<Token>,
    open_lists: Vec<Vec<Token>>,
    length: usize,
    non_synchronising: bool,
}

impl PendingLiteral {
    /// How many octets the client declared. Already validated against the limit.
    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }

    /// Whether this is a `LITERAL+` non-synchronising literal (`{n+}`).
    ///
    /// Such a literal must **not** be answered with a continuation request: the
    /// client is already sending the octets (RFC 7888). Answering anyway would
    /// desynchronise the stream.
    #[must_use]
    pub const fn is_non_synchronising(&self) -> bool {
        self.non_synchronising
    }

    /// Supplies the literal payload and continues parsing the rest of the line.
    ///
    /// `rest` is whatever followed the literal on the continuation line, which
    /// may itself contain another literal — hence the return type is [`Parsed`]
    /// again, and multiple literals in one command work by iteration.
    ///
    /// # Errors
    ///
    /// Propagates any parse error from the remainder of the command.
    pub fn resume(
        mut self,
        payload: Vec<u8>,
        rest: &[u8],
        max_literal: usize,
    ) -> Result<Parsed, ParseError> {
        push_token(
            &mut self.tokens,
            &mut self.open_lists,
            Token::Literal(payload),
        )?;
        let mut cursor = Cursor::new(rest);
        continue_parse(
            &mut cursor,
            self.tag,
            self.kind,
            self.uid_mode,
            self.tokens,
            self.open_lists,
            max_literal,
        )
    }
}

/// Parses one command line.
///
/// `line` must already have its trailing CRLF removed by the framing layer.
/// `max_literal` bounds any literal the line declares.
///
/// # Errors
///
/// Returns a [`ParseError`] for any input the grammar rejects. Never panics,
/// however malformed the input.
pub fn parse_line(line: &[u8], max_literal: usize) -> Result<Parsed, ParseError> {
    let mut cursor = Cursor::new(line);
    cursor.skip_spaces();
    if cursor.is_empty() {
        return Err(ParseError::Empty);
    }

    let tag_bytes = cursor.take_while(|b| b != b' ');
    let tag_text = core::str::from_utf8(tag_bytes).map_err(|_| ParseError::InvalidTag)?;
    let tag = Tag::parse(tag_text).ok_or(ParseError::InvalidTag)?;

    cursor.skip_spaces();
    let name_bytes = cursor.take_while(|b| b != b' ');
    if name_bytes.is_empty() {
        return Err(ParseError::MissingCommand);
    }
    let name = core::str::from_utf8(name_bytes).map_err(|_| ParseError::InvalidEncoding)?;

    // `UID` is a prefix, not a command: `UID FETCH` is a FETCH whose sequence
    // set is UIDs (RFC 9051 §6.4.9).
    let (kind, uid_mode) = if name.eq_ignore_ascii_case("UID") {
        cursor.skip_spaces();
        let inner = cursor.take_while(|b| b != b' ');
        if inner.is_empty() {
            return Err(ParseError::MissingCommand);
        }
        let inner = core::str::from_utf8(inner).map_err(|_| ParseError::InvalidEncoding)?;
        let kind = CommandKind::parse(inner).ok_or(ParseError::UnknownCommand)?;
        if !kind.accepts_uid_prefix() {
            return Err(ParseError::UidNotPermitted);
        }
        (kind, true)
    } else {
        (
            CommandKind::parse(name).ok_or(ParseError::UnknownCommand)?,
            false,
        )
    };

    continue_parse(
        &mut cursor,
        tag,
        kind,
        uid_mode,
        Vec::new(),
        Vec::new(),
        max_literal,
    )
}

/// Tokenises the argument portion, stopping at a literal.
fn continue_parse(
    cursor: &mut Cursor<'_>,
    tag: Tag,
    kind: CommandKind,
    uid_mode: bool,
    mut tokens: Vec<Token>,
    mut open_lists: Vec<Vec<Token>>,
    max_literal: usize,
) -> Result<Parsed, ParseError> {
    loop {
        cursor.skip_spaces();
        if cursor.is_empty() {
            if !open_lists.is_empty() {
                return Err(ParseError::UnterminatedList);
            }
            return Ok(Parsed::Complete(Command {
                tag,
                kind,
                uid_mode,
                args: tokens,
            }));
        }

        match cursor.peek() {
            Some(b'(') => {
                cursor.advance(1);
                if open_lists.len() >= MAX_NESTING_DEPTH {
                    return Err(ParseError::NestingTooDeep);
                }
                open_lists.push(Vec::new());
            }
            Some(b')') => {
                cursor.advance(1);
                let finished = open_lists.pop().ok_or(ParseError::UnbalancedParen)?;
                push_token(&mut tokens, &mut open_lists, Token::List(finished))?;
            }
            Some(b'"') => {
                let text = parse_quoted(cursor)?;
                push_token(&mut tokens, &mut open_lists, Token::Quoted(text))?;
            }
            Some(b'{') => {
                let (length, non_synchronising) = parse_literal_header(cursor, max_literal)?;
                return Ok(Parsed::NeedLiteral(PendingLiteral {
                    tag,
                    kind,
                    uid_mode,
                    tokens,
                    open_lists,
                    length,
                    non_synchronising,
                }));
            }
            Some(_) => {
                let raw = cursor.take_while(|b| !matches!(b, b' ' | b'(' | b')'));
                let text = core::str::from_utf8(raw).map_err(|_| ParseError::InvalidEncoding)?;
                let token = if text.eq_ignore_ascii_case("NIL") {
                    Token::Nil
                } else {
                    Token::Atom(text.to_owned())
                };
                push_token(&mut tokens, &mut open_lists, token)?;
            }
            // Unreachable: the cursor was checked non-empty above. Handled as
            // a normal completion rather than a panic, because this crate
            // forbids production panics and a parser must never be the thing
            // that takes the server down.
            None => return Err(ParseError::Empty),
        }
    }
}

/// Appends a token to the innermost open list, or to the top level.
fn push_token(
    tokens: &mut Vec<Token>,
    open_lists: &mut [Vec<Token>],
    token: Token,
) -> Result<(), ParseError> {
    let total: usize = tokens.len() + open_lists.iter().map(Vec::len).sum::<usize>();
    if total >= MAX_TOKENS {
        return Err(ParseError::TooManyTokens);
    }
    match open_lists.last_mut() {
        Some(innermost) => innermost.push(token),
        None => tokens.push(token),
    }
    Ok(())
}

/// Parses `"..."` with `\\` and `\"` escapes (RFC 9051 §9 `quoted`).
fn parse_quoted(cursor: &mut Cursor<'_>) -> Result<String, ParseError> {
    cursor.advance(1); // opening quote
    let mut out = Vec::new();
    loop {
        let byte = cursor.next_byte().ok_or(ParseError::UnterminatedQuoted)?;
        match byte {
            b'"' => break,
            // RFC 9051 §9 permits only `\` and `"` after a backslash.
            b'\\' => {
                let escaped = cursor.next_byte().ok_or(ParseError::UnterminatedQuoted)?;
                if !matches!(escaped, b'\\' | b'"') {
                    return Err(ParseError::IllegalQuotedChar);
                }
                out.push(escaped);
            }
            // A bare CR or LF inside a quoted string is a framing violation.
            b'\r' | b'\n' => return Err(ParseError::IllegalQuotedChar),
            other => out.push(other),
        }
    }
    String::from_utf8(out).map_err(|_| ParseError::InvalidEncoding)
}

/// Parses `{n}` or `{n+}`, validating `n` against `max_literal`.
///
/// The `}` must be the final octet of the line: RFC 9051 §9 puts the CRLF
/// immediately after it, so trailing junk means the client is confused and the
/// stream can no longer be trusted to be in sync.
fn parse_literal_header(
    cursor: &mut Cursor<'_>,
    max_literal: usize,
) -> Result<(usize, bool), ParseError> {
    cursor.advance(1); // `{`
    let digits = cursor.take_while(|b| b.is_ascii_digit());
    if digits.is_empty() {
        return Err(ParseError::MalformedLiteral);
    }

    let non_synchronising = cursor.peek() == Some(b'+');
    if non_synchronising {
        cursor.advance(1);
    }
    if cursor.next_byte() != Some(b'}') || !cursor.is_empty() {
        return Err(ParseError::MalformedLiteral);
    }

    let text = core::str::from_utf8(digits).map_err(|_| ParseError::MalformedLiteral)?;
    // `parse::<usize>` rejects an over-long run of digits rather than wrapping,
    // so `{99999999999999999999}` is a malformed literal, not an overflow.
    let length: usize = text.parse().map_err(|_| ParseError::MalformedLiteral)?;
    if length > max_literal {
        return Err(ParseError::LiteralTooLarge { limit: max_literal });
    }
    Ok((length, non_synchronising))
}

/// A non-allocating byte cursor.
///
/// Every accessor is bounds-checked and returns `Option`; there is no indexing
/// operation in this file that could panic on malformed input.
struct Cursor<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn advance(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n).min(self.input.len());
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
    }

    fn take_while(&mut self, predicate: impl Fn(u8) -> bool) -> &'a [u8] {
        let start = self.pos;
        while self.peek().is_some_and(&predicate) {
            self.pos += 1;
        }
        self.input.get(start..self.pos).unwrap_or_default()
    }
}

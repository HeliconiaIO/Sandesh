//! `FETCH` data items: what a client may ask for, and what it means.
//!
//! This module is pure. It turns tokens into a [`FetchRequest`] and renders
//! already-gathered values into wire form; it performs no I/O and touches no
//! storage. The session gathers the data, because only it has the ports.
//!
//! ```text
//!   tokens ──▶ FetchRequest ──▶ (session gathers) ──▶ FetchResponse ──▶ wire
//!   (parse)      pure            ports + raw bytes      typed items
//! ```
//!
//! # Why the items are a type and not a list of strings
//!
//! Two reasons the phase brief §16 asks for directly. A `BODY[2.1.TEXT]<0.512>`
//! carries a section path, a section kind and a partial range; matching on the
//! original string at every use site is how one of those three silently stops
//! being honoured. And the *response* must echo the item name in a specific
//! form — `BODY[2.1.TEXT]<0>` on a partial fetch, note the offset without the
//! count — which is a rendering rule, not a substring of the request.

use core::fmt;
use core::fmt::Write as _;

use crate::command::Token;

/// Why a `FETCH` item list could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchParseError {
    /// No data items were given.
    NoItems,
    /// An item name this server does not implement.
    UnknownItem(String),
    /// A `BODY[...]` section specifier that is not valid syntax.
    InvalidSection(String),
    /// A `<offset.count>` partial specifier that is not valid syntax.
    InvalidPartial(String),
    /// A macro (`ALL`, `FAST`, `FULL`) used alongside explicit items.
    MacroNotAlone,
}

impl FetchParseError {
    /// The text accompanying the `BAD` response.
    ///
    /// Names the offending item so a client author can fix it, and nothing
    /// else: no storage detail, no path, no message content.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NoItems => String::from("FETCH requires at least one data item"),
            Self::UnknownItem(name) => format!("unsupported FETCH data item: {name}"),
            Self::InvalidSection(section) => format!("invalid FETCH section: {section}"),
            Self::InvalidPartial(partial) => format!("invalid FETCH partial range: {partial}"),
            Self::MacroNotAlone => {
                String::from("a FETCH macro may not be combined with other items")
            }
        }
    }
}

impl fmt::Display for FetchParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// A `<offset.count>` partial specifier (RFC 9051 §6.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partial {
    /// First octet returned, 0-based.
    pub offset: u32,
    /// How many octets at most.
    pub count: u32,
}

impl Partial {
    /// Parses the inside of `<...>`: `offset "." count`.
    ///
    /// RFC 9051 §9 `partial` requires both halves; `<0>` alone is a *response*
    /// form, never a request form, so it is rejected here.
    fn parse(text: &str) -> Result<Self, FetchParseError> {
        let invalid = || FetchParseError::InvalidPartial(text.to_owned());

        let (offset, count) = text.split_once('.').ok_or_else(invalid)?;
        if offset.is_empty()
            || count.is_empty()
            || !offset.bytes().all(|b| b.is_ascii_digit())
            || !count.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }

        // Parsing into `u32` is itself the bound: RFC 9051 §9 `number` is a
        // 32-bit value, so anything larger is a syntax error rather than a
        // silently wrapped small number.
        let offset: u32 = offset.parse().map_err(|_| invalid())?;
        let count: u32 = count.parse().map_err(|_| invalid())?;
        // A zero count is legal and means "no octets": RFC 9051 §6.4.5 bounds
        // it only from above. It yields an empty literal, not an error.
        Ok(Self { offset, count })
    }

    /// Applies the range to `data`, saturating at its end.
    ///
    /// # The bounds guarantee
    ///
    /// Every arithmetic step here is saturating or checked, and the result is
    /// always a sub-slice of `data`:
    ///
    /// * an offset beyond the end yields an empty slice, not a panic;
    /// * a count longer than what remains yields only what remains;
    /// * `offset + count` cannot overflow, because the sum is computed in
    ///   `usize` after both have been widened, and clamped to `data.len()`.
    ///
    /// This is the function phase brief §14 asks to be unable to index out of
    /// bounds, so it never indexes: it slices with a checked range.
    #[must_use]
    pub fn apply(self, data: &[u8]) -> &[u8] {
        let offset = self.offset as usize;
        if offset >= data.len() {
            return &[];
        }
        let remaining = data.len() - offset;
        let take = (self.count as usize).min(remaining);
        let end = offset.saturating_add(take);
        data.get(offset..end).unwrap_or(&[])
    }
}

impl fmt::Display for Partial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}.{}>", self.offset, self.count)
    }
}

/// Which part of a section is wanted (RFC 9051 §6.4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionKind {
    /// The whole thing: `BODY[]` or `BODY[2.1]`.
    Full,
    /// `HEADER` — all header fields, verbatim.
    Header,
    /// `HEADER.FIELDS (A B)` — only the named fields.
    HeaderFields(Vec<String>),
    /// `HEADER.FIELDS.NOT (A B)` — every field except the named ones.
    HeaderFieldsNot(Vec<String>),
    /// `TEXT` — the body after the header.
    Text,
    /// `MIME` — a non-root part's own MIME headers.
    ///
    /// Only meaningful with a part path: RFC 9051 §6.4.5 has no `BODY[MIME]`,
    /// because the message's own headers are `BODY[HEADER]`.
    Mime,
}

impl SectionKind {
    /// The wire spelling, as it must appear in the response.
    fn render(&self) -> String {
        match self {
            Self::Full => String::new(),
            Self::Header => String::from("HEADER"),
            Self::HeaderFields(fields) => format!("HEADER.FIELDS ({})", render_fields(fields)),
            Self::HeaderFieldsNot(fields) => {
                format!("HEADER.FIELDS.NOT ({})", render_fields(fields))
            }
            Self::Text => String::from("TEXT"),
            Self::Mime => String::from("MIME"),
        }
    }
}

/// Strips the line terminator from a token, recursing into lists.
///
/// Trailing CRLF is a framing artefact, not part of any data item: an atom of
/// `BODY[]\r\n` names the same section as `BODY[]`, and leaving the terminator
/// attached would make every final item unrecognised.
fn trim_token(token: &Token) -> Token {
    match token {
        Token::Atom(text) => Token::Atom(text.trim_end_matches(['\r', '\n']).to_owned()),
        Token::Quoted(text) => Token::Quoted(text.clone()),
        Token::List(inner) => Token::List(inner.iter().map(trim_token).collect()),
        Token::Literal(bytes) => Token::Literal(bytes.clone()),
        Token::Nil => Token::Nil,
    }
}

/// Renders a header-field name list, uppercased as clients expect to see it.
fn render_fields(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| field.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `BODY[...]` section specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The MIME part path; empty for the message itself.
    pub path: Vec<u32>,
    /// Which portion of it.
    pub kind: SectionKind,
}

impl Section {
    /// Parses the inside of `BODY[...]`.
    ///
    /// The grammar interleaves a dotted part path with a trailing keyword:
    ///
    /// ```text
    /// []                     → the whole message
    /// [HEADER]               → the message's headers
    /// [TEXT]                 → the message's body
    /// [1]                    → part 1, entire
    /// [1.TEXT]               → part 1's body
    /// [1.MIME]               → part 1's own MIME headers
    /// [2.1.HEADER.FIELDS (SUBJECT)]
    /// ```
    ///
    /// Leading numeric components are the path; the first non-numeric component
    /// begins the keyword. A component that is `0` is rejected: part numbering
    /// is 1-based, and `BODY[0]` names nothing.
    fn parse(text: &str) -> Result<Self, FetchParseError> {
        let invalid = || FetchParseError::InvalidSection(text.to_owned());

        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Self {
                path: Vec::new(),
                kind: SectionKind::Full,
            });
        }

        // Split off a `HEADER.FIELDS (...)` argument list before splitting on
        // dots, since the field names inside the parentheses may contain them.
        let (spec, fields) = match trimmed.split_once('(') {
            Some((before, after)) => {
                let inside = after.strip_suffix(')').ok_or_else(invalid)?;
                let names: Vec<String> = inside
                    .split_whitespace()
                    .map(str::to_ascii_uppercase)
                    .collect();
                if names.is_empty() {
                    // `HEADER.FIELDS ()` names no fields. RFC 9051 §9 requires
                    // at least one, and an empty list is more likely a client
                    // bug than an intent to fetch nothing.
                    return Err(invalid());
                }
                (before.trim(), Some(names))
            }
            None => (trimmed, None),
        };

        let mut path = Vec::new();
        let mut components = spec.split('.').peekable();

        // Leading numeric components form the part path.
        while let Some(component) = components.peek() {
            if component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()) {
                break;
            }
            let number: u32 = component.parse().map_err(|_| invalid())?;
            if number == 0 {
                // Part numbering is 1-based; `BODY[0]` addresses nothing.
                return Err(invalid());
            }
            path.push(number);
            components.next();
        }

        // Whatever remains is the keyword.
        let keyword: Vec<String> = components.map(str::to_ascii_uppercase).collect();
        let keyword: Vec<&str> = keyword.iter().map(String::as_str).collect();

        let kind = match (keyword.as_slice(), fields) {
            ([], None) => SectionKind::Full,
            (["HEADER"], None) => SectionKind::Header,
            (["TEXT"], None) => SectionKind::Text,
            (["MIME"], None) => SectionKind::Mime,
            (["HEADER", "FIELDS"], Some(names)) => SectionKind::HeaderFields(names),
            (["HEADER", "FIELDS", "NOT"], Some(names)) => SectionKind::HeaderFieldsNot(names),
            _ => return Err(invalid()),
        };

        // `MIME` requires a part: the message's own headers are `HEADER`.
        if kind == SectionKind::Mime && path.is_empty() {
            return Err(invalid());
        }
        // A bare part path with no keyword must actually have a path.
        Ok(Self { path, kind })
    }

    /// The wire spelling of the whole specifier, without brackets.
    #[must_use]
    pub fn render(&self) -> String {
        let path: Vec<String> = self.path.iter().map(u32::to_string).collect();
        let kind = self.kind.render();
        match (path.is_empty(), kind.is_empty()) {
            (true, true) => String::new(),
            (true, false) => kind,
            (false, true) => path.join("."),
            (false, false) => format!("{}.{}", path.join("."), kind),
        }
    }
}

/// One `FETCH` data item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchItem {
    /// `UID` — the placement's UID.
    Uid,
    /// `FLAGS` — the placement's flags.
    Flags,
    /// `INTERNALDATE` — when the message was placed here.
    InternalDate,
    /// `RFC822.SIZE` — the raw message's octet count.
    Rfc822Size,
    /// `ENVELOPE` — the parsed header summary.
    Envelope,
    /// `BODYSTRUCTURE` — the full MIME tree, with extension data.
    BodyStructure,
    /// `BODY` with no section — the MIME tree without extension data.
    Body,
    /// `MODSEQ` — the placement's modification sequence (RFC 7162 §3.1.4).
    ModSeq,
    /// `BODY[...]`, `BODY.PEEK[...]`, and the `RFC822*` aliases.
    Section {
        /// Which section.
        section: Section,
        /// Whether the fetch is a peek, which must not set `\Seen`.
        peek: bool,
        /// An optional `<offset.count>` window.
        partial: Option<Partial>,
        /// How to spell this item in the response.
        ///
        /// `RFC822.TEXT` and `BODY[TEXT]` return the same octets but must echo
        /// their own name, so the request's spelling is carried through rather
        /// than reconstructed.
        alias: Option<&'static str>,
    },
}

impl FetchItem {
    /// Whether answering this item requires the raw message bytes.
    ///
    /// Used to fetch the raw octets once per message rather than once per
    /// item: `FETCH 1 (RFC822.SIZE BODY[HEADER] BODY[TEXT])` reads storage a
    /// single time (phase brief §21).
    #[must_use]
    pub const fn needs_raw(&self) -> bool {
        matches!(
            self,
            Self::Rfc822Size
                | Self::Envelope
                | Self::BodyStructure
                | Self::Body
                | Self::Section { .. }
        )
    }

    /// Whether answering this item implicitly sets `\Seen`.
    ///
    /// RFC 9051 §6.4.5: a `BODY[...]` fetch that is not `.PEEK` sets `\Seen`.
    /// Nothing else does — not `FLAGS`, not `BODYSTRUCTURE`, not `RFC822.SIZE`,
    /// and not `BODY` without a section, which returns structure rather than
    /// content.
    #[must_use]
    pub const fn sets_seen(&self) -> bool {
        matches!(self, Self::Section { peek: false, .. })
    }

    /// How this item is spelled in the response.
    #[must_use]
    pub fn render_name(&self) -> String {
        match self {
            Self::Uid => String::from("UID"),
            Self::Flags => String::from("FLAGS"),
            Self::InternalDate => String::from("INTERNALDATE"),
            Self::Rfc822Size => String::from("RFC822.SIZE"),
            Self::Envelope => String::from("ENVELOPE"),
            Self::BodyStructure => String::from("BODYSTRUCTURE"),
            Self::Body => String::from("BODY"),
            Self::ModSeq => String::from("MODSEQ"),
            Self::Section {
                section,
                partial,
                alias,
                ..
            } => {
                // An alias keeps its own name and takes no section brackets.
                if let Some(name) = alias {
                    return (*name).to_owned();
                }
                // `.PEEK` is deliberately not echoed: RFC 9051 §7.4.2 spells
                // the response item `BODY[...]` whether or not the request
                // peeked.
                let mut rendered = format!("BODY[{}]", section.render());
                // A partial response carries only the offset, never the count
                // (RFC 9051 §7.4.2) — the literal's own length states how many
                // octets actually came back.
                if let Some(partial) = partial {
                    let _ = write!(rendered, "<{}>", partial.offset);
                }
                rendered
            }
        }
    }

    /// Parses one item from its atom, and an optional following list.
    ///
    /// `BODY`/`BODY.PEEK` carry their section in the same atom, because the
    /// parser tokenises `BODY[HEADER.FIELDS (SUBJECT)]` as an atom, a list and
    /// a trailing atom; [`FetchRequest::parse`] reassembles those before
    /// calling this.
    fn parse(atom: &str) -> Result<Self, FetchParseError> {
        let upper = atom.to_ascii_uppercase();

        match upper.as_str() {
            "UID" => return Ok(Self::Uid),
            "FLAGS" => return Ok(Self::Flags),
            "INTERNALDATE" => return Ok(Self::InternalDate),
            "RFC822.SIZE" => return Ok(Self::Rfc822Size),
            "ENVELOPE" => return Ok(Self::Envelope),
            "BODYSTRUCTURE" => return Ok(Self::BodyStructure),
            "BODY" => return Ok(Self::Body),
            "MODSEQ" => return Ok(Self::ModSeq),

            // The RFC822 family are aliases for BODY sections (RFC 9051
            // §6.4.5 keeps them for compatibility). They are *not* peeks:
            // RFC822 and RFC822.TEXT set \Seen exactly as BODY[] does.
            "RFC822" => {
                return Ok(Self::Section {
                    section: Section {
                        path: Vec::new(),
                        kind: SectionKind::Full,
                    },
                    peek: false,
                    partial: None,
                    alias: Some("RFC822"),
                });
            }
            "RFC822.TEXT" => {
                return Ok(Self::Section {
                    section: Section {
                        path: Vec::new(),
                        kind: SectionKind::Text,
                    },
                    peek: false,
                    partial: None,
                    alias: Some("RFC822.TEXT"),
                });
            }
            // RFC822.HEADER is the one alias that does not set \Seen: it is
            // defined as equivalent to BODY.PEEK[HEADER].
            "RFC822.HEADER" => {
                return Ok(Self::Section {
                    section: Section {
                        path: Vec::new(),
                        kind: SectionKind::Header,
                    },
                    peek: true,
                    partial: None,
                    alias: Some("RFC822.HEADER"),
                });
            }
            _ => {}
        }

        // `BODY[...]` / `BODY.PEEK[...]`, with an optional `<offset.count>`.
        let (prefix, peek) = if let Some(rest) = upper.strip_prefix("BODY.PEEK") {
            (rest, true)
        } else if let Some(rest) = upper.strip_prefix("BODY") {
            (rest, false)
        } else {
            return Err(FetchParseError::UnknownItem(atom.to_owned()));
        };

        // Case matters inside the brackets only for header field names, which
        // are compared case-insensitively anyway, so the uppercased form is
        // used throughout.
        let Some(after_open) = prefix.strip_prefix('[') else {
            return Err(FetchParseError::UnknownItem(atom.to_owned()));
        };
        let Some((inside, after_close)) = after_open.rsplit_once(']') else {
            return Err(FetchParseError::InvalidSection(atom.to_owned()));
        };

        let section = Section::parse(inside)?;

        let partial = if after_close.is_empty() {
            None
        } else {
            let trimmed = after_close
                .strip_prefix('<')
                .and_then(|rest| rest.strip_suffix('>'))
                .ok_or_else(|| FetchParseError::InvalidPartial(after_close.to_owned()))?;
            Some(Partial::parse(trimmed)?)
        };

        Ok(Self::Section {
            section,
            peek,
            partial,
            alias: None,
        })
    }
}

/// A parsed `FETCH` item list, with any CONDSTORE modifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    /// The items, in the order the client asked for them.
    items: Vec<FetchItem>,
    /// `CHANGEDSINCE <n>` (RFC 7162 §3.1.4), when the client sent one.
    ///
    /// Restricts the response to placements whose modification sequence is
    /// greater than `n`. Held separately from [`Self::items`] because it is a
    /// *filter*, not a data item: it changes which messages are reported, not
    /// what is reported about them.
    changed_since: Option<sandesh_core::ModSeq>,
}

impl FetchRequest {
    /// The items, in request order.
    ///
    /// Order is preserved because RFC 9051 §7.4.2 lets the server return items
    /// in any order, and echoing the request order is what clients find least
    /// surprising — and what makes a response diffable in a test.
    #[must_use]
    pub fn items(&self) -> &[FetchItem] {
        &self.items
    }

    /// Whether any item requires the raw message bytes.
    #[must_use]
    pub fn needs_raw(&self) -> bool {
        self.items.iter().any(FetchItem::needs_raw)
    }

    /// Whether any item implicitly sets `\Seen`.
    #[must_use]
    pub fn sets_seen(&self) -> bool {
        self.items.iter().any(FetchItem::sets_seen)
    }

    /// The `CHANGEDSINCE` bound, when the client sent one.
    #[must_use]
    pub const fn changed_since(&self) -> Option<sandesh_core::ModSeq> {
        self.changed_since
    }

    /// Whether the response must carry `MODSEQ` for every message.
    ///
    /// RFC 7162 §3.1.4.1 requires `CHANGEDSINCE` to imply `MODSEQ`: a client
    /// filtering by modification sequence needs the new values back, or it
    /// cannot advance the value it filters on next time and would loop on the
    /// same messages forever.
    #[must_use]
    pub fn implies_mod_seq(&self) -> bool {
        self.changed_since.is_some() && !self.items.contains(&FetchItem::ModSeq)
    }

    /// Parses the data-item argument of a `FETCH` command.
    ///
    /// Accepts the three shapes RFC 9051 §6.4.5 allows:
    ///
    /// ```text
    /// FETCH 1 FLAGS                    → a bare item
    /// FETCH 1 (FLAGS UID)              → a parenthesised list
    /// FETCH 1 FAST                     → a macro
    /// ```
    ///
    /// # Macros
    ///
    /// `ALL`, `FAST` and `FULL` expand to fixed item lists. They may not be
    /// combined with other items — RFC 9051 §6.4.5 defines them as standing
    /// for the whole list, and `(FAST UID)` has no defined meaning.
    ///
    /// # Errors
    ///
    /// Returns [`FetchParseError`] when the list is empty, names an item this
    /// server does not implement, or contains a malformed section or partial
    /// specifier. The command is then answered `BAD` and nothing is executed.
    pub fn parse(tokens: &[Token]) -> Result<Self, FetchParseError> {
        // Reassemble the tokens into item atoms first. The generic parser
        // tokenises `BODY[HEADER.FIELDS (SUBJECT)]` into three tokens, so the
        // bracketed form has to be stitched back together before it can be
        // understood as one item.
        // `CHANGEDSINCE n` arrives as a trailing parenthesised modifier —
        // `FETCH 1:* (FLAGS) (CHANGEDSINCE 12)` — so it is removed before the
        // remaining tokens are read as data items. Doing this first keeps
        // `flatten_items` unaware of CONDSTORE entirely.
        let (tokens, changed_since) = split_changed_since(tokens)?;

        let atoms = flatten_items(&tokens);
        if atoms.is_empty() {
            return Err(FetchParseError::NoItems);
        }

        // A macro must stand alone.
        if let [single] = atoms.as_slice()
            && let Some(expanded) = expand_macro(single)
        {
            return Ok(Self {
                items: expanded,
                changed_since,
            });
        }
        for atom in &atoms {
            if expand_macro(atom).is_some() {
                return Err(FetchParseError::MacroNotAlone);
            }
        }

        let items = atoms
            .iter()
            .map(|atom| FetchItem::parse(atom.as_str()))
            .collect::<Result<Vec<_>, _>>()?;

        if items.is_empty() {
            return Err(FetchParseError::NoItems);
        }
        Ok(Self {
            items,
            changed_since,
        })
    }
}

/// Removes a trailing `(CHANGEDSINCE n)` modifier, returning it separately.
///
/// RFC 7162 §3.1.4 defines the modifier as a parenthesised list after the data
/// items. `VANISHED` — the other modifier the grammar allows there — belongs to
/// QRESYNC, which this server does not implement, so it is rejected rather than
/// silently ignored: accepting it would promise a `VANISHED` response the
/// client would then wait for.
fn split_changed_since(
    tokens: &[Token],
) -> Result<(Vec<Token>, Option<sandesh_core::ModSeq>), FetchParseError> {
    let trimmed: Vec<Token> = tokens
        .iter()
        .map(trim_token)
        .filter(|token| !matches!(token, Token::Atom(text) if text.is_empty()))
        .collect();

    let Some((Token::List(inner), rest)) = trimmed.split_last() else {
        return Ok((trimmed, None));
    };

    // Only a list whose first atom is a known modifier name is a modifier; any
    // other trailing list is a data-item list and must be left alone.
    let Some(name) = inner.first().and_then(Token::as_text) else {
        return Ok((trimmed, None));
    };
    if name.eq_ignore_ascii_case("VANISHED") {
        return Err(FetchParseError::UnknownItem(String::from("VANISHED")));
    }
    if !name.eq_ignore_ascii_case("CHANGEDSINCE") {
        return Ok((trimmed, None));
    }
    // A modifier with no items before it would leave nothing to fetch.
    if rest.is_empty() {
        return Err(FetchParseError::NoItems);
    }

    let value = inner
        .get(1)
        .and_then(Token::as_text)
        .ok_or_else(|| FetchParseError::UnknownItem(String::from("CHANGEDSINCE")))?;
    if inner.len() > 2 {
        return Err(FetchParseError::UnknownItem(String::from("CHANGEDSINCE")));
    }

    // RFC 7162 §3.1.4: the argument is a mod-sequence value. 0 is reserved and
    // is refused by the domain type rather than silently clamped.
    let parsed = value
        .parse::<u64>()
        .ok()
        .and_then(sandesh_core::ModSeq::new)
        .ok_or_else(|| FetchParseError::UnknownItem(format!("CHANGEDSINCE {value}")))?;

    Ok((rest.to_vec(), Some(parsed)))
}

/// Expands `ALL`, `FAST` and `FULL` (RFC 9051 §6.4.5).
///
/// The three differ only in how much they include, and all of them are
/// `.PEEK`-equivalent in the sense that none contains a `BODY[...]` section —
/// `FULL` includes `BODY`, the structure, not `BODY[]`, the content. So no
/// macro sets `\Seen`, which is what makes them safe for a client to issue
/// while listing a mailbox.
fn expand_macro(atom: &str) -> Option<Vec<FetchItem>> {
    let fast = || {
        vec![
            FetchItem::Flags,
            FetchItem::InternalDate,
            FetchItem::Rfc822Size,
        ]
    };
    match atom.to_ascii_uppercase().as_str() {
        "FAST" => Some(fast()),
        "ALL" => {
            let mut items = fast();
            items.push(FetchItem::Envelope);
            Some(items)
        }
        "FULL" => {
            let mut items = fast();
            items.push(FetchItem::Envelope);
            items.push(FetchItem::Body);
            Some(items)
        }
        _ => None,
    }
}

/// Flattens the token stream into one string per data item.
///
/// The generic parser knows nothing of `FETCH`, so a bracketed section arrives
/// split across tokens. This walks the tokens and rejoins anything belonging to
/// an unclosed `[`:
///
/// ```text
/// tokens:  Atom("BODY[HEADER.FIELDS") List([Atom("SUBJECT")]) Atom("]")
/// atoms:   ["BODY[HEADER.FIELDS (SUBJECT)]"]
/// ```
fn flatten_items(tokens: &[Token]) -> Vec<String> {
    // The line parser leaves the trailing CRLF attached to the final atom and
    // does not strip it, so every token is trimmed before it is interpreted.
    // Tokens that were nothing but the terminator disappear entirely.
    let trimmed: Vec<Token> = tokens
        .iter()
        .map(trim_token)
        .filter(|token| !matches!(token, Token::Atom(text) if text.is_empty()))
        .collect();

    // A bare item, or a parenthesised list of them. The list form is unwrapped
    // only when it is the *whole* argument, which is what distinguishes
    // `(UID FLAGS)` from the field-name list inside `BODY[HEADER.FIELDS (…)]`.
    let sequence: &[Token] = match trimmed.as_slice() {
        [Token::List(inner)] => inner,
        other => other,
    };

    let mut atoms: Vec<String> = Vec::new();
    // Whether the atom being accumulated has an unclosed `[`.
    let mut open = false;

    for token in sequence {
        let rendered = match token {
            Token::Atom(text) | Token::Quoted(text) => text.clone(),
            Token::List(inner) => {
                // Only meaningful inside a section, as a field-name list.
                let names: Vec<String> = inner
                    .iter()
                    .map(|item| item.as_text().unwrap_or_default().to_owned())
                    .collect();
                format!("({})", names.join(" "))
            }
            // A literal or NIL is not a data item; render it in a way that
            // cannot be mistaken for a valid one so it is reported as unknown.
            Token::Literal(_) => String::from("\u{0}literal"),
            Token::Nil => String::from("NIL"),
        };

        if open {
            // Continue the item under construction. A `(` list follows the
            // keyword directly, with the space the grammar requires.
            let separator = if rendered.starts_with('(') { " " } else { "" };
            if let Some(last) = atoms.last_mut() {
                last.push_str(separator);
                last.push_str(&rendered);
            }
        } else {
            atoms.push(rendered);
        }

        // Recompute whether the accumulated atom is still open. Counting
        // brackets rather than tracking a flag means a malformed item cannot
        // leave the walk permanently in the wrong mode.
        if let Some(last) = atoms.last() {
            let opens = last.bytes().filter(|b| *b == b'[').count();
            let closes = last.bytes().filter(|b| *b == b']').count();
            open = opens > closes;
        }
    }

    atoms
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Parses a `FETCH` item list from its wire text.
    ///
    /// Goes through the real command parser so the tests exercise the same
    /// tokenisation the session sees, not a convenient shortcut.
    fn parse(text: &str) -> Result<FetchRequest, FetchParseError> {
        let line = format!("A1 FETCH 1 {text}\r\n");
        let parsed = crate::parser::parse_line(line.as_bytes(), 65536)
            .expect("the harness line must tokenise");
        let crate::parser::Parsed::Complete(command) = parsed else {
            panic!("no literal in these tests");
        };
        // args[0] is the sequence set; the items follow.
        FetchRequest::parse(&command.args[1..])
    }

    /// The rendered item names of a parsed request.
    fn names(text: &str) -> Vec<String> {
        parse(text)
            .expect("valid item list")
            .items()
            .iter()
            .map(FetchItem::render_name)
            .collect()
    }

    // --- simple items -----------------------------------------------------

    #[test]
    fn the_metadata_items_parse() {
        assert_eq!(
            names("(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE)"),
            vec![
                "UID",
                "FLAGS",
                "INTERNALDATE",
                "RFC822.SIZE",
                "ENVELOPE",
                "BODYSTRUCTURE"
            ]
        );
    }

    #[test]
    fn a_bare_item_needs_no_parentheses() {
        assert_eq!(names("FLAGS"), vec!["FLAGS"]);
        assert_eq!(names("UID"), vec!["UID"]);
    }

    #[test]
    fn item_names_are_case_insensitive() {
        assert_eq!(names("(uid flags)"), vec!["UID", "FLAGS"]);
        assert_eq!(names("(Rfc822.Size)"), vec!["RFC822.SIZE"]);
    }

    #[test]
    fn request_order_is_preserved() {
        assert_eq!(names("(FLAGS UID)"), vec!["FLAGS", "UID"]);
        assert_eq!(names("(UID FLAGS)"), vec!["UID", "FLAGS"]);
    }

    #[test]
    fn an_unknown_item_is_rejected() {
        // Not silently dropped: a client that asked for something must be told
        // it cannot have it, or it will read a missing item as a missing value.
        assert_eq!(
            parse("(NOTATHING)"),
            Err(FetchParseError::UnknownItem(String::from("NOTATHING")))
        );
        assert!(parse("(FLAGS NOTATHING)").is_err());
        // Unimplemented rev2 items are rejected rather than faked. `MODSEQ`
        // was in this list until CONDSTORE was implemented; it now has its own
        // tests below. `EMAILID`/`THREADID` (OBJECTID) and `SAVEDATE` remain
        // genuinely unimplemented, so they stay here.
        assert!(parse("(BINARY[1])").is_err());
        assert!(parse("(EMAILID)").is_err());
        assert!(parse("(THREADID)").is_err());
        assert!(parse("(SAVEDATE)").is_err());
    }

    // --- CONDSTORE (RFC 7162) ---------------------------------------------

    #[test]
    fn modseq_is_a_recognised_data_item() {
        assert_eq!(names("(MODSEQ)"), vec!["MODSEQ"]);
        assert_eq!(names("MODSEQ"), vec!["MODSEQ"]);
        assert_eq!(names("(FLAGS MODSEQ UID)"), vec!["FLAGS", "MODSEQ", "UID"]);
    }

    #[test]
    fn modseq_is_matched_case_insensitively() {
        // Data-item names are atoms, so case must not matter (RFC 9051 §9).
        assert_eq!(names("(modseq)"), vec!["MODSEQ"]);
        assert_eq!(names("(ModSeq)"), vec!["MODSEQ"]);
    }

    #[test]
    fn modseq_needs_neither_raw_bytes_nor_sets_seen() {
        // The sequence lives on the placement, so answering it must not load
        // the message — and reading a modseq is not reading the message, so it
        // must not mark it \Seen.
        let request = parse("(MODSEQ)").expect("valid");
        assert!(!request.needs_raw(), "MODSEQ must not force a message load");
        assert!(!request.sets_seen(), "MODSEQ must not set \\Seen");
    }

    #[test]
    fn changed_since_is_parsed_as_a_modifier_not_an_item() {
        // RFC 7162 §3.1.4. The modifier filters which messages are reported;
        // treating it as a data item would put `CHANGEDSINCE` in the response.
        let request = parse("(FLAGS) (CHANGEDSINCE 12)").expect("valid");
        assert_eq!(
            request.changed_since().map(sandesh_core::ModSeq::get),
            Some(12)
        );
        assert_eq!(
            request
                .items()
                .iter()
                .map(FetchItem::render_name)
                .collect::<Vec<_>>(),
            vec!["FLAGS"],
            "the modifier must not appear among the data items"
        );
    }

    #[test]
    fn changed_since_implies_modseq_in_the_response() {
        // RFC 7162 §3.1.4.1. Without the implied MODSEQ a client cannot learn
        // the new values and would re-request the same messages forever.
        let implied = parse("(FLAGS) (CHANGEDSINCE 5)").expect("valid");
        assert!(implied.implies_mod_seq());

        // Already asked for explicitly: nothing to add, or it would be sent
        // twice.
        let explicit = parse("(FLAGS MODSEQ) (CHANGEDSINCE 5)").expect("valid");
        assert!(!explicit.implies_mod_seq());

        // No modifier, no implication.
        let plain = parse("(FLAGS)").expect("valid");
        assert!(!plain.implies_mod_seq());
    }

    #[test]
    fn a_request_without_the_modifier_has_no_bound() {
        assert!(parse("(FLAGS)").expect("valid").changed_since().is_none());
        assert!(parse("(MODSEQ)").expect("valid").changed_since().is_none());
    }

    #[test]
    fn a_malformed_changed_since_is_rejected() {
        // Not silently ignored: a client that asked to filter and was given
        // everything would treat unchanged messages as changed.
        assert!(parse("(FLAGS) (CHANGEDSINCE)").is_err());
        assert!(parse("(FLAGS) (CHANGEDSINCE abc)").is_err());
        assert!(parse("(FLAGS) (CHANGEDSINCE 1 2)").is_err());
        // 0 is reserved by RFC 7162 §3 and is not a usable bound.
        assert!(parse("(FLAGS) (CHANGEDSINCE 0)").is_err());
        // A modifier with nothing to fetch.
        assert!(parse("(CHANGEDSINCE 5)").is_err());
    }

    #[test]
    fn vanished_is_refused_because_qresync_is_not_implemented() {
        // `VANISHED` shares the modifier slot with `CHANGEDSINCE` but belongs
        // to QRESYNC. Accepting it would promise a `VANISHED` response this
        // server never sends, leaving the client waiting for data.
        assert!(parse("(FLAGS) (VANISHED)").is_err());
        assert!(parse("(FLAGS) (CHANGEDSINCE 1 VANISHED)").is_err());
    }

    #[test]
    fn a_trailing_field_list_is_not_mistaken_for_a_modifier() {
        // The modifier and a `BODY[HEADER.FIELDS (…)]` field list are both
        // trailing parenthesised lists. Only the modifier names may be
        // stripped, or this section would lose its fields.
        let request = parse("(BODY[HEADER.FIELDS (SUBJECT)])").expect("valid");
        assert!(request.changed_since().is_none());
        assert_eq!(
            request
                .items()
                .iter()
                .map(FetchItem::render_name)
                .collect::<Vec<_>>(),
            vec!["BODY[HEADER.FIELDS (SUBJECT)]"]
        );
    }

    #[test]
    fn an_empty_item_list_is_rejected() {
        assert_eq!(parse("()"), Err(FetchParseError::NoItems));
    }

    // --- macros -----------------------------------------------------------

    #[test]
    fn macros_expand_to_their_defined_items() {
        assert_eq!(names("FAST"), vec!["FLAGS", "INTERNALDATE", "RFC822.SIZE"]);
        assert_eq!(
            names("ALL"),
            vec!["FLAGS", "INTERNALDATE", "RFC822.SIZE", "ENVELOPE"]
        );
        assert_eq!(
            names("FULL"),
            vec!["FLAGS", "INTERNALDATE", "RFC822.SIZE", "ENVELOPE", "BODY"]
        );
    }

    #[test]
    fn no_macro_sets_seen() {
        // A client listing a mailbox with FULL must not mark everything read.
        for macro_name in ["FAST", "ALL", "FULL"] {
            assert!(
                !parse(macro_name).expect("valid").sets_seen(),
                "{macro_name} must not set \\Seen"
            );
        }
    }

    #[test]
    fn a_macro_may_not_be_combined() {
        assert_eq!(parse("(FAST UID)"), Err(FetchParseError::MacroNotAlone));
        assert_eq!(parse("(UID FULL)"), Err(FetchParseError::MacroNotAlone));
    }

    // --- sections ---------------------------------------------------------

    #[test]
    fn the_empty_section_is_the_whole_message() {
        assert_eq!(names("BODY[]"), vec!["BODY[]"]);
        let request = parse("BODY[]").expect("valid");
        let [FetchItem::Section { section, .. }] = request.items() else {
            panic!("expected one section item");
        };
        assert!(section.path.is_empty());
        assert_eq!(section.kind, SectionKind::Full);
    }

    #[test]
    fn message_level_sections_parse() {
        assert_eq!(names("BODY[HEADER]"), vec!["BODY[HEADER]"]);
        assert_eq!(names("BODY[TEXT]"), vec!["BODY[TEXT]"]);
    }

    #[test]
    fn part_paths_parse_at_any_depth() {
        for section in ["BODY[1]", "BODY[2.1]", "BODY[2.1.3]"] {
            assert_eq!(names(section), vec![section]);
        }
        let request = parse("BODY[2.1.3]").expect("valid");
        let [FetchItem::Section { section, .. }] = request.items() else {
            panic!("expected one section item");
        };
        assert_eq!(section.path, vec![2, 1, 3]);
        assert_eq!(section.kind, SectionKind::Full);
    }

    #[test]
    fn part_sections_combine_path_and_keyword() {
        for section in ["BODY[1.TEXT]", "BODY[1.HEADER]", "BODY[1.MIME]"] {
            assert_eq!(names(section), vec![section]);
        }
        let request = parse("BODY[2.1.TEXT]").expect("valid");
        let [FetchItem::Section { section, .. }] = request.items() else {
            panic!("expected one section item");
        };
        assert_eq!(section.path, vec![2, 1]);
        assert_eq!(section.kind, SectionKind::Text);
    }

    #[test]
    fn header_field_lists_parse_and_render() {
        let request = parse("BODY[HEADER.FIELDS (SUBJECT FROM)]").expect("valid");
        let [FetchItem::Section { section, .. }] = request.items() else {
            panic!("expected one section item");
        };
        assert_eq!(
            section.kind,
            SectionKind::HeaderFields(vec![String::from("SUBJECT"), String::from("FROM")])
        );
        assert_eq!(
            request.items()[0].render_name(),
            "BODY[HEADER.FIELDS (SUBJECT FROM)]"
        );
    }

    #[test]
    fn header_field_exclusion_lists_parse() {
        let request = parse("BODY[HEADER.FIELDS.NOT (RECEIVED)]").expect("valid");
        let [FetchItem::Section { section, .. }] = request.items() else {
            panic!("expected one section item");
        };
        assert_eq!(
            section.kind,
            SectionKind::HeaderFieldsNot(vec![String::from("RECEIVED")])
        );
    }

    #[test]
    fn header_field_names_are_uppercased_for_matching() {
        let request = parse("BODY[HEADER.FIELDS (subject)]").expect("valid");
        assert_eq!(
            request.items()[0].render_name(),
            "BODY[HEADER.FIELDS (SUBJECT)]"
        );
    }

    #[test]
    fn invalid_sections_are_rejected() {
        // Part numbering is 1-based, so `0` addresses nothing.
        assert!(parse("BODY[0]").is_err());
        assert!(parse("BODY[1.0]").is_err());
        // `MIME` needs a part: the message's own headers are `HEADER`.
        assert!(parse("BODY[MIME]").is_err());
        // Unknown keywords, unbalanced brackets, empty field lists.
        assert!(parse("BODY[NOPE]").is_err());
        assert!(parse("BODY[1.NOPE]").is_err());
        assert!(parse("BODY[HEADER.FIELDS ()]").is_err());
        assert!(parse("BODY[").is_err());
    }

    #[test]
    fn a_section_cannot_reach_outside_the_message() {
        // No section syntax may denote anything but a part path plus a
        // keyword — no traversal, no absolute reference (phase brief §22).
        for hostile in [
            "BODY[../1]",
            "BODY[/etc/passwd]",
            "BODY[1/../2]",
            "BODY[-1]",
            "BODY[1..2]",
        ] {
            assert!(parse(hostile).is_err(), "{hostile} must be rejected");
        }
    }

    // --- peek -------------------------------------------------------------

    #[test]
    fn peek_is_distinct_from_a_plain_body_fetch() {
        assert!(parse("BODY[]").expect("valid").sets_seen());
        assert!(!parse("BODY.PEEK[]").expect("valid").sets_seen());
        assert!(parse("BODY[TEXT]").expect("valid").sets_seen());
        assert!(!parse("BODY.PEEK[TEXT]").expect("valid").sets_seen());
    }

    #[test]
    fn peek_is_not_echoed_in_the_response_item_name() {
        // RFC 9051 §7.4.2: the response says BODY[...] either way.
        assert_eq!(names("BODY.PEEK[HEADER]"), vec!["BODY[HEADER]"]);
        assert_eq!(names("BODY.PEEK[]"), vec!["BODY[]"]);
    }

    #[test]
    fn structure_items_never_set_seen() {
        // BODY without a section is structure, not content.
        for item in ["BODY", "BODYSTRUCTURE", "FLAGS", "UID", "RFC822.SIZE"] {
            assert!(
                !parse(item).expect("valid").sets_seen(),
                "{item} must not set \\Seen"
            );
        }
    }

    // --- RFC822 aliases --------------------------------------------------

    #[test]
    fn the_rfc822_aliases_keep_their_own_names() {
        assert_eq!(names("RFC822"), vec!["RFC822"]);
        assert_eq!(names("RFC822.HEADER"), vec!["RFC822.HEADER"]);
        assert_eq!(names("RFC822.TEXT"), vec!["RFC822.TEXT"]);
    }

    #[test]
    fn rfc822_header_is_a_peek_but_the_others_are_not() {
        // RFC 9051 §6.4.5 defines RFC822.HEADER as BODY.PEEK[HEADER].
        assert!(!parse("RFC822.HEADER").expect("valid").sets_seen());
        assert!(parse("RFC822").expect("valid").sets_seen());
        assert!(parse("RFC822.TEXT").expect("valid").sets_seen());
    }

    // --- partial ---------------------------------------------------------

    #[test]
    fn partial_ranges_parse() {
        let request = parse("BODY[]<0.512>").expect("valid");
        let [
            FetchItem::Section {
                partial: Some(partial),
                ..
            },
        ] = request.items()
        else {
            panic!("expected a partial section");
        };
        assert_eq!(partial.offset, 0);
        assert_eq!(partial.count, 512);
    }

    #[test]
    fn a_partial_response_echoes_only_the_offset() {
        // RFC 9051 §7.4.2: the count is implied by the literal's own length.
        assert_eq!(names("BODY[]<100.50>"), vec!["BODY[]<100>"]);
        assert_eq!(names("BODY[TEXT]<7.3>"), vec!["BODY[TEXT]<7>"]);
    }

    #[test]
    fn invalid_partials_are_rejected() {
        for hostile in [
            "BODY[]<>",
            "BODY[]<1>",
            "BODY[]<1.>",
            "BODY[]<.1>",
            "BODY[]<a.b>",
            "BODY[]<-1.5>",
            "BODY[]<1.2.3>",
            "BODY[]<99999999999999999999.1>",
        ] {
            assert!(parse(hostile).is_err(), "{hostile} must be rejected");
        }
    }

    #[test]
    fn a_zero_count_is_legal_and_yields_nothing() {
        let request = parse("BODY[]<0.0>").expect("a zero count is legal");
        let [
            FetchItem::Section {
                partial: Some(partial),
                ..
            },
        ] = request.items()
        else {
            panic!("expected a partial section");
        };
        assert_eq!(partial.apply(b"hello"), b"");
    }

    // --- partial application ---------------------------------------------

    #[test]
    fn a_partial_window_slices_exactly() {
        let data = b"0123456789";
        assert_eq!(
            Partial {
                offset: 0,
                count: 4
            }
            .apply(data),
            b"0123"
        );
        assert_eq!(
            Partial {
                offset: 3,
                count: 4
            }
            .apply(data),
            b"3456"
        );
        assert_eq!(
            Partial {
                offset: 6,
                count: 4
            }
            .apply(data),
            b"6789"
        );
    }

    #[test]
    fn a_count_past_the_end_returns_only_what_remains() {
        let data = b"0123456789";
        assert_eq!(
            Partial {
                offset: 7,
                count: 100
            }
            .apply(data),
            b"789"
        );
        assert_eq!(
            Partial {
                offset: 0,
                count: u32::MAX,
            }
            .apply(data),
            data
        );
    }

    #[test]
    fn an_offset_past_the_end_returns_nothing() {
        let data = b"0123456789";
        assert_eq!(
            Partial {
                offset: 10,
                count: 5
            }
            .apply(data),
            b""
        );
        assert_eq!(
            Partial {
                offset: 11,
                count: 5
            }
            .apply(data),
            b""
        );
        assert_eq!(
            Partial {
                offset: u32::MAX,
                count: u32::MAX,
            }
            .apply(data),
            b"",
            "the extreme case must not panic or overflow"
        );
    }

    #[test]
    fn a_partial_window_never_panics_on_any_input() {
        // Exhaustive-ish: every offset around the boundary, with extreme
        // counts, against empty and non-empty data.
        for data in [&b""[..], &b"a"[..], &b"0123456789"[..]] {
            for offset in [0u32, 1, 5, 9, 10, 11, 1000, u32::MAX - 1, u32::MAX] {
                for count in [0u32, 1, 5, 1000, u32::MAX - 1, u32::MAX] {
                    let out = Partial { offset, count }.apply(data);
                    assert!(out.len() <= data.len(), "a window cannot grow the data");
                    assert!(
                        u64::from(count) >= out.len() as u64,
                        "a window cannot exceed its count"
                    );
                }
            }
        }
    }

    #[test]
    fn binary_data_survives_a_partial_window() {
        let data: Vec<u8> = (0u8..=255).collect();
        let window = Partial {
            offset: 100,
            count: 56,
        }
        .apply(&data);
        assert_eq!(window, &data[100..156]);
        assert!(window.contains(&0xFF) || window.iter().all(|b| *b >= 100));
    }

    // --- raw-byte requirement --------------------------------------------

    #[test]
    fn only_content_items_need_the_raw_bytes() {
        assert!(
            !parse("(UID FLAGS INTERNALDATE)")
                .expect("valid")
                .needs_raw()
        );
        assert!(parse("RFC822.SIZE").expect("valid").needs_raw());
        assert!(parse("BODY[]").expect("valid").needs_raw());
        assert!(parse("BODYSTRUCTURE").expect("valid").needs_raw());
        assert!(parse("ENVELOPE").expect("valid").needs_raw());
    }
}

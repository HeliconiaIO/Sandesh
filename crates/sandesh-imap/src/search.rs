//! Compiling RFC 9051 `SEARCH` keys into a [`SearchQuery`] (§6.4.4).
//!
//! This module is pure: tokens in, a domain query out. It performs no I/O,
//! evaluates nothing, and holds no opinion about which messages exist — the
//! session supplies the candidate set from its `SequenceMap`, and
//! `sandesh_core::SearchQuery::matches` does the deciding.
//!
//! ```text
//! SEARCH UNSEEN FROM alice
//!        │      │
//!        └──────┴──▶ [HasFlag(Seen) negated, Contains{From,"alice"}]
//!                              │
//!                              ▼
//!                    SearchQuery::And([...])     ← implicit AND (§6.4.4)
//! ```
//!
//! # Why the grammar needs a real parser
//!
//! Search keys are not a flat list. `NOT` takes one key, `OR` takes two, both
//! may nest, a parenthesised list is itself a key, and several keys consume a
//! following argument. A loop over tokens cannot express that; this is a
//! recursive-descent parser over the token stream, with an explicit depth cap
//! so a hostile `NOT NOT NOT …` cannot exhaust the stack.

use sandesh_core::{DateComparison, DateField, FlagKey, SearchQuery, TextField, parse_date_text};

use crate::command::Token;
use crate::sequence::SequenceSet;

/// How deeply search keys may nest.
///
/// RFC 9051 sets no limit. Real clients nest two or three deep; this bounds
/// recursion so a pathological query is a clean `BAD` rather than a crash
/// (the same discipline `MAX_NESTING_DEPTH` applies to the line parser).
pub const MAX_SEARCH_DEPTH: usize = 16;

/// Why a `SEARCH` criteria list could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchParseError {
    /// No search key was given.
    NoCriteria,
    /// A key name this server does not implement.
    UnknownKey(String),
    /// A key was missing its argument.
    MissingArgument(String),
    /// A numeric argument was not a number.
    InvalidNumber(String),
    /// A date argument was not a valid `date-text`.
    InvalidDate(String),
    /// A sequence set or UID set argument was malformed.
    InvalidSet(String),
    /// A `CHARSET` this server cannot search in.
    UnsupportedCharset(String),
    /// Keys nested deeper than [`MAX_SEARCH_DEPTH`].
    TooDeep,
    /// A `RETURN` option name this server does not implement.
    UnknownReturnOption(String),
    /// `RETURN` was given without the parenthesised option list.
    MalformedReturn,
}

impl SearchParseError {
    /// The text accompanying the refusal.
    ///
    /// Names the offending key so a client author can fix it, and nothing
    /// else: no storage detail, no message content, no mailbox contents.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NoCriteria => String::from("SEARCH requires at least one criterion"),
            Self::UnknownKey(key) => format!("unsupported SEARCH key: {key}"),
            Self::UnknownReturnOption(option) => {
                format!("unsupported SEARCH RETURN option: {option}")
            }
            Self::MalformedReturn => {
                String::from("SEARCH RETURN requires a parenthesised option list")
            }
            Self::MissingArgument(key) => format!("SEARCH key {key} requires an argument"),
            Self::InvalidNumber(value) => format!("invalid number in SEARCH: {value}"),
            Self::InvalidDate(value) => format!("invalid date in SEARCH: {value}"),
            Self::InvalidSet(value) => format!("invalid message set in SEARCH: {value}"),
            Self::UnsupportedCharset(charset) => {
                format!("unsupported SEARCH charset: {charset}")
            }
            Self::TooDeep => String::from("SEARCH criteria nested too deeply"),
        }
    }

    /// Whether RFC 9051 requires this refusal to carry `[BADCHARSET]`.
    ///
    /// §6.4.4 is specific: a charset the server cannot handle is answered `NO`
    /// with a `BADCHARSET` response code, not `BAD`, because the command was
    /// well-formed — the server simply cannot honour it. Clients use the code
    /// to retry in US-ASCII, so getting it wrong turns a recoverable
    /// situation into a dead end.
    #[must_use]
    pub const fn is_bad_charset(&self) -> bool {
        matches!(self, Self::UnsupportedCharset(_))
    }
}

impl core::fmt::Display for SearchParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message())
    }
}

/// Compiles a `SEARCH` criteria list into a domain query.
///
/// `uid_mode` decides how a bare message set is read: in `UID SEARCH` it names
/// UIDs, and in `SEARCH` it names sequence numbers. Sequence-number sets cannot
/// be expressed as a [`SearchQuery`] — the domain knows nothing of positions —
/// so they are returned separately for the session to resolve through its
/// `SequenceMap`.
///
/// # Errors
///
/// Returns [`SearchParseError`] for an empty list, an unimplemented key, a
/// missing or malformed argument, an unsupported charset, or excessive nesting.
/// Nothing is evaluated and no mailbox is consulted when compilation fails.
pub fn compile(tokens: &[Token], uid_mode: bool) -> Result<CompiledSearch, SearchParseError> {
    // The line parser leaves the trailing CRLF attached to the final atom, so
    // tokens are trimmed before interpretation and pure-terminator tokens are
    // dropped.
    let mut flattened: Vec<Atom> = flatten(tokens);
    if flattened.is_empty() {
        return Err(SearchParseError::NoCriteria);
    }

    // An optional leading `RETURN (...)` (RFC 9051 §6.4.4) selects the
    // `ESEARCH` response form. It is stripped here, before the criteria are
    // read, so everything downstream — the cursor, the keys, the sequence
    // sets — is exactly the parser that `SEARCH` already used. That is what
    // keeps this one search engine rather than two.
    let mut returns: Option<ReturnOptions> = None;
    if flattened
        .first()
        .and_then(Atom::text)
        .is_some_and(|text| text.eq_ignore_ascii_case("RETURN"))
    {
        let Some(Atom::List(options)) = flattened.get(1) else {
            // `RETURN` with no list, or with a non-list, is malformed. It is
            // never treated as the search key `RETURN`, because no such key
            // exists — reporting it as an unknown key would misdirect the
            // client author.
            return Err(SearchParseError::MalformedReturn);
        };
        let mut names: Vec<&str> = Vec::with_capacity(options.len());
        for option in options {
            let Some(name) = option.text() else {
                // A nested list where an option name belongs.
                return Err(SearchParseError::MalformedReturn);
            };
            names.push(name);
        }
        returns = Some(ReturnOptions::parse(&names)?);
        flattened.drain(0..2);

        // `SEARCH RETURN (...)` with no criteria at all is malformed: RFC 9051
        // §6.4.4 requires at least one key after the option list.
        if flattened.is_empty() {
            return Err(SearchParseError::NoCriteria);
        }
    }

    let mut cursor = Cursor {
        atoms: &flattened,
        position: 0,
        uid_mode,
        sequence_sets: Vec::new(),
    };

    // An optional leading `CHARSET <name>` (RFC 9051 §6.4.4).
    cursor.consume_charset()?;

    let mut keys = Vec::new();
    while !cursor.is_done() {
        keys.push(cursor.parse_key(0)?);
    }

    if keys.is_empty() {
        return Err(SearchParseError::NoCriteria);
    }

    // Adjacent keys are ANDed (§6.4.4). One key stays as itself rather than
    // becoming a single-element And, so a compiled query is comparable in
    // tests without unwrapping.
    let query = if keys.len() == 1 {
        keys.into_iter().next().unwrap_or(SearchQuery::All)
    } else {
        SearchQuery::And(keys)
    };

    Ok(CompiledSearch {
        query,
        sequence_sets: cursor.sequence_sets,
        returns,
    })
}

/// One `ESEARCH` return option (RFC 9051 §6.4.4).
///
/// Only the four options this server can answer correctly from the existing
/// search results are represented. `SAVE` is deliberately absent — see
/// [`ReturnOptions::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReturnOption {
    /// The lowest matching number.
    Min,
    /// The highest matching number.
    Max,
    /// Every match, as a sequence set.
    All,
    /// How many matched.
    Count,
}

impl ReturnOption {
    /// Every option this server implements, in the order RFC 9051 §7.3.4
    /// lists them in an `ESEARCH` response.
    ///
    /// The order is fixed rather than echoing the client's, so the response is
    /// deterministic. RFC 9051 §7.3.4 does not require a particular order, and
    /// a stable one is what makes the response testable.
    pub const ALL_OPTIONS: [Self; 4] = [Self::Min, Self::Max, Self::All, Self::Count];

    /// Recognises an option name, case-insensitively (RFC 9051 §9 atoms).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "MIN" => Some(Self::Min),
            "MAX" => Some(Self::Max),
            "ALL" => Some(Self::All),
            "COUNT" => Some(Self::Count),
            _ => None,
        }
    }

    /// The name as it appears in an `ESEARCH` response.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Min => "MIN",
            Self::Max => "MAX",
            Self::All => "ALL",
            Self::Count => "COUNT",
        }
    }
}

/// The `RETURN (...)` options a client asked for.
///
/// An *absent* `RETURN` and an empty `RETURN ()` are different requests, and
/// this type keeps them apart. Absent means "answer with the classic
/// `* SEARCH` line"; `RETURN ()` means "answer with `ESEARCH`, and RFC 9051
/// §6.4.4 says to treat it as `ALL`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnOptions {
    requested: Vec<ReturnOption>,
}

impl ReturnOptions {
    /// Parses a `RETURN (...)` option list.
    ///
    /// # Unsupported options are refused, not ignored
    ///
    /// An unrecognised option is [`SearchParseError::UnknownReturnOption`] and
    /// the whole command is `BAD`. That is the opposite of the `ENABLE` rule
    /// (RFC 5161 §3.1, ignore-and-continue) and deliberately so: `ENABLE`
    /// negotiates *future* syntax, so ignoring a name costs the client
    /// nothing, whereas a `RETURN` option names the answer the client is
    /// waiting for. Silently dropping `RETURN (SAVE)` would return a response
    /// that parses but answers a different question, and the client cannot
    /// tell.
    ///
    /// # `SAVE` is not implemented
    ///
    /// RFC 9051 §6.4.4 `SAVE` stores the result in the session's search-result
    /// variable for a later `$` reference. That requires the `$` marker to be
    /// resolvable everywhere a sequence set is accepted — `FETCH`, `STORE`,
    /// `COPY`, `MOVE` — and this server has no such variable. Accepting `SAVE`
    /// while ignoring it would leave a subsequent `FETCH $` addressing nothing,
    /// so it is refused as unsupported. Recorded in ADR-057.
    ///
    /// # Errors
    ///
    /// [`SearchParseError::UnknownReturnOption`] for a name this server does
    /// not implement, including `SAVE`.
    pub fn parse(names: &[&str]) -> Result<Self, SearchParseError> {
        let mut requested = Vec::new();
        for name in names {
            let option = ReturnOption::parse(name)
                .ok_or_else(|| SearchParseError::UnknownReturnOption((*name).to_owned()))?;
            if !requested.contains(&option) {
                requested.push(option);
            }
        }

        // RFC 9051 §6.4.4: an empty list is equivalent to `ALL`.
        if requested.is_empty() {
            requested.push(ReturnOption::All);
        }
        Ok(Self { requested })
    }

    /// Whether `option` was requested.
    #[must_use]
    pub fn wants(&self, option: ReturnOption) -> bool {
        self.requested.contains(&option)
    }

    /// The requested options in canonical response order.
    ///
    /// Not the client's order: see [`ReturnOption::ALL_OPTIONS`].
    #[must_use]
    pub fn in_response_order(&self) -> Vec<ReturnOption> {
        ReturnOption::ALL_OPTIONS
            .into_iter()
            .filter(|option| self.wants(*option))
            .collect()
    }
}

/// A compiled search: a domain query plus any sequence-number sets.
///
/// The two are separate because a sequence number is a *position in this
/// session's snapshot*, which is not a property of a message and so cannot live
/// in a domain query. The session intersects them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledSearch {
    /// The domain query, evaluated per message.
    query: SearchQuery,
    /// Sequence-number sets that must all also match, resolved by the caller.
    sequence_sets: Vec<SequenceSet>,
    /// `RETURN (...)` options, absent for a classic `SEARCH`.
    returns: Option<ReturnOptions>,
}

impl CompiledSearch {
    /// The domain query.
    #[must_use]
    pub const fn query(&self) -> &SearchQuery {
        &self.query
    }

    /// Sequence-number sets the caller must additionally satisfy.
    ///
    /// Every set must match (they are `AND`ed with the query), and each is
    /// resolved against the session's `SequenceMap` — never against UIDs.
    #[must_use]
    pub fn sequence_sets(&self) -> &[SequenceSet] {
        &self.sequence_sets
    }

    /// The `RETURN (...)` options, or `None` for a classic `SEARCH`.
    ///
    /// `None` and `Some(...)` select different *response forms*, not different
    /// searches: the matching is identical either way, and only the formatting
    /// of the result differs (RFC 9051 §7.3.4).
    #[must_use]
    pub const fn returns(&self) -> Option<&ReturnOptions> {
        self.returns.as_ref()
    }
}

/// One flattened token: text, or a nested list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    Text(String),
    List(Vec<Atom>),
}

impl Atom {
    /// The atom's text, if it is not a list.
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::List(_) => None,
        }
    }
}

/// Flattens tokens into atoms, trimming the line terminator.
fn flatten(tokens: &[Token]) -> Vec<Atom> {
    tokens
        .iter()
        .filter_map(|token| match token {
            Token::Atom(text) | Token::Quoted(text) => {
                let trimmed = text.trim_end_matches(['\r', '\n']);
                // A token that was nothing but the terminator carries no key.
                (!trimmed.is_empty()).then(|| Atom::Text(trimmed.to_owned()))
            }
            Token::Literal(bytes) => Some(Atom::Text(
                String::from_utf8_lossy(bytes)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned(),
            )),
            Token::List(inner) => Some(Atom::List(flatten(inner))),
            // `NIL` is not a search key or a valid argument.
            Token::Nil => Some(Atom::Text(String::from("NIL"))),
        })
        .collect()
}

/// A position in the atom stream, plus the sets collected so far.
struct Cursor<'atoms> {
    atoms: &'atoms [Atom],
    position: usize,
    uid_mode: bool,
    sequence_sets: Vec<SequenceSet>,
}

impl Cursor<'_> {
    /// Whether every atom has been consumed.
    fn is_done(&self) -> bool {
        self.position >= self.atoms.len()
    }

    /// The next atom, without consuming it.
    fn peek(&self) -> Option<&Atom> {
        self.atoms.get(self.position)
    }

    /// Consumes and returns the next atom.
    fn next_atom(&mut self) -> Option<&Atom> {
        let atom = self.atoms.get(self.position);
        if atom.is_some() {
            self.position += 1;
        }
        atom
    }

    /// Consumes the next atom as text, or reports it missing.
    fn next_text(&mut self, key: &str) -> Result<String, SearchParseError> {
        match self.next_atom() {
            Some(Atom::Text(text)) => Ok(text.clone()),
            // A list where an argument belongs is malformed, not an argument.
            Some(Atom::List(_)) | None => Err(SearchParseError::MissingArgument(key.to_owned())),
        }
    }

    /// Consumes an optional leading `CHARSET <name>`.
    ///
    /// Only US-ASCII and UTF-8 are accepted. Both are searched identically:
    /// matching is over Rust `String`s, which are UTF-8, and US-ASCII is a
    /// subset. Any other charset would need transcoding that is not
    /// implemented, so it is refused with `BADCHARSET` rather than silently
    /// searched as if it were UTF-8 — a wrong answer is worse than a refusal
    /// the client knows how to retry.
    fn consume_charset(&mut self) -> Result<(), SearchParseError> {
        let is_charset = self
            .peek()
            .and_then(Atom::text)
            .is_some_and(|text| text.eq_ignore_ascii_case("CHARSET"));
        if !is_charset {
            return Ok(());
        }
        self.position += 1;
        let name = self.next_text("CHARSET")?;
        if name.eq_ignore_ascii_case("UTF-8")
            || name.eq_ignore_ascii_case("US-ASCII")
            || name.eq_ignore_ascii_case("ASCII")
        {
            Ok(())
        } else {
            Err(SearchParseError::UnsupportedCharset(name))
        }
    }

    /// Parses one search key, recursing for `NOT`, `OR` and lists.
    fn parse_key(&mut self, depth: usize) -> Result<SearchQuery, SearchParseError> {
        if depth > MAX_SEARCH_DEPTH {
            return Err(SearchParseError::TooDeep);
        }

        let Some(atom) = self.next_atom().cloned() else {
            return Err(SearchParseError::NoCriteria);
        };

        // A parenthesised list is a key whose children are ANDed.
        let text = match atom {
            Atom::List(inner) => {
                let mut nested = Cursor {
                    atoms: &inner,
                    position: 0,
                    uid_mode: self.uid_mode,
                    sequence_sets: Vec::new(),
                };
                let mut keys = Vec::new();
                while !nested.is_done() {
                    keys.push(nested.parse_key(depth + 1)?);
                }
                // Sets found inside the list still apply to the whole search.
                self.sequence_sets.append(&mut nested.sequence_sets);
                if keys.is_empty() {
                    return Err(SearchParseError::NoCriteria);
                }
                return Ok(if keys.len() == 1 {
                    keys.into_iter().next().unwrap_or(SearchQuery::All)
                } else {
                    SearchQuery::And(keys)
                });
            }
            Atom::Text(text) => text,
        };

        let upper = text.to_ascii_uppercase();

        // Keys taking no argument are a pure name lookup, kept separate so the
        // argument-consuming cases below stay readable.
        if let Some(query) = flagless_key(&upper) {
            return query.map_err(|()| SearchParseError::UnknownKey(text.clone()));
        }

        match upper.as_str() {
            // --- one text argument -------------------------------------
            "FROM" => self.text_key(TextField::From, &upper),
            "TO" => self.text_key(TextField::To, &upper),
            "CC" => self.text_key(TextField::Cc, &upper),
            "BCC" => self.text_key(TextField::Bcc, &upper),
            "SUBJECT" => self.text_key(TextField::Subject, &upper),
            "BODY" => self.text_key(TextField::Body, &upper),
            "TEXT" => self.text_key(TextField::Text, &upper),

            "KEYWORD" => {
                let keyword = self.next_text(&upper)?;
                Ok(SearchQuery::HasKeyword(keyword))
            }
            "UNKEYWORD" => {
                let keyword = self.next_text(&upper)?;
                Ok(SearchQuery::LacksKeyword(keyword))
            }

            // --- header name plus value --------------------------------
            "HEADER" => {
                let name = self.next_text(&upper)?;
                let value = self.next_text(&upper)?;
                Ok(SearchQuery::Contains {
                    field: TextField::Header(name),
                    value,
                })
            }

            // --- numeric -----------------------------------------------
            "LARGER" => {
                let value = self.next_text(&upper)?;
                Ok(SearchQuery::Larger(parse_number(&value)?))
            }
            "SMALLER" => {
                let value = self.next_text(&upper)?;
                Ok(SearchQuery::Smaller(parse_number(&value)?))
            }

            "MODSEQ" => self.mod_seq_key(&upper),

            // --- dates -------------------------------------------------
            "BEFORE" => self.date_key(DateField::Internal, DateComparison::Before, &upper),
            "ON" => self.date_key(DateField::Internal, DateComparison::On, &upper),
            "SINCE" => self.date_key(DateField::Internal, DateComparison::Since, &upper),
            "SENTBEFORE" => self.date_key(DateField::Sent, DateComparison::Before, &upper),
            "SENTON" => self.date_key(DateField::Sent, DateComparison::On, &upper),
            "SENTSINCE" => self.date_key(DateField::Sent, DateComparison::Since, &upper),

            // --- explicit UID set --------------------------------------
            "UID" => {
                let set = self.next_text(&upper)?;
                let parsed = SequenceSet::parse(&set)
                    .map_err(|_| SearchParseError::InvalidSet(set.clone()))?;
                Ok(uid_set_query(&parsed))
            }

            // --- logic -------------------------------------------------
            "NOT" => {
                let inner = self.parse_key(depth + 1)?;
                Ok(SearchQuery::Not(Box::new(inner)))
            }
            "OR" => {
                // RFC 9051 §6.4.4: `OR` takes exactly two keys.
                let left = self.parse_key(depth + 1)?;
                let right = self.parse_key(depth + 1)?;
                Ok(SearchQuery::Or(vec![left, right]))
            }

            // --- a bare message set ------------------------------------
            _ => {
                // Anything else must be a message set, or it is not a key at
                // all. `UID SEARCH 1:5` names UIDs; `SEARCH 1:5` names
                // sequence numbers, which the session resolves.
                let Ok(set) = SequenceSet::parse(&text) else {
                    return Err(SearchParseError::UnknownKey(text));
                };
                if self.uid_mode {
                    Ok(uid_set_query(&set))
                } else {
                    self.sequence_sets.push(set);
                    // The set is enforced by the caller; as a query it
                    // constrains nothing further.
                    Ok(SearchQuery::All)
                }
            }
        }
    }

    /// Parses a key taking one text argument.
    fn text_key(&mut self, field: TextField, key: &str) -> Result<SearchQuery, SearchParseError> {
        let value = self.next_text(key)?;
        Ok(SearchQuery::Contains { field, value })
    }

    /// Parses `MODSEQ <n>` (RFC 7162 §3.1.5).
    ///
    /// The comparison is inclusive — "at or above" — because the client saved
    /// the highest value it had *seen*, so that revision itself is part of
    /// what it is asking about.
    ///
    /// RFC 7162 §3.1.5 also allows a longer form naming the entry-type and
    /// flag whose metadata is being asked about, as in
    /// `MODSEQ "/flags/\Seen" all 7`. This server stores one modification
    /// sequence per placement rather than per flag, so there is nothing to
    /// distinguish those variants by: accepting the form would mean ignoring
    /// two of its three arguments and answering a different question than the
    /// client asked. The short form is implemented; the long form fails here
    /// because its first argument is not a number, which is the honest
    /// refusal.
    fn mod_seq_key(&mut self, key: &str) -> Result<SearchQuery, SearchParseError> {
        let value = self.next_text(key)?;
        let parsed = value
            .parse::<u64>()
            .ok()
            .and_then(sandesh_core::ModSeq::new)
            .ok_or_else(|| SearchParseError::InvalidNumber(value.clone()))?;
        Ok(SearchQuery::ModSeqAtLeast(parsed))
    }

    /// Parses a key taking one date argument.
    fn date_key(
        &mut self,
        field: DateField,
        comparison: DateComparison,
        key: &str,
    ) -> Result<SearchQuery, SearchParseError> {
        let value = self.next_text(key)?;
        let timestamp =
            parse_date_text(&value).ok_or_else(|| SearchParseError::InvalidDate(value.clone()))?;
        Ok(SearchQuery::Date {
            field,
            comparison,
            value: timestamp,
        })
    }
}

/// Resolves a search key that takes no argument.
///
/// Returns `None` when the name is not such a key (the caller then tries the
/// argument-consuming forms), `Some(Err(()))` for a key this server refuses to
/// answer, and `Some(Ok(query))` otherwise.
///
/// `NEW`, `OLD` and `RECENT` are refused rather than answered: RFC 9051 §6.4.4
/// carries them over from rev1, but this server keeps no `\Recent` state
/// (ADR-045), so any answer would be fabricated. `SEEN`/`UNSEEN` cover what
/// clients actually want.
fn flagless_key(upper: &str) -> Option<Result<SearchQuery, ()>> {
    let query = match upper {
        "ALL" => SearchQuery::All,

        "SEEN" => SearchQuery::HasFlag(FlagKey::Seen),
        "UNSEEN" => SearchQuery::LacksFlag(FlagKey::Seen),
        "ANSWERED" => SearchQuery::HasFlag(FlagKey::Answered),
        "UNANSWERED" => SearchQuery::LacksFlag(FlagKey::Answered),
        "FLAGGED" => SearchQuery::HasFlag(FlagKey::Flagged),
        "UNFLAGGED" => SearchQuery::LacksFlag(FlagKey::Flagged),
        "DELETED" => SearchQuery::HasFlag(FlagKey::Deleted),
        "UNDELETED" => SearchQuery::LacksFlag(FlagKey::Deleted),
        "DRAFT" => SearchQuery::HasFlag(FlagKey::Draft),
        "UNDRAFT" => SearchQuery::LacksFlag(FlagKey::Draft),

        "NEW" | "OLD" | "RECENT" => return Some(Err(())),

        _ => return None,
    };
    Some(Ok(query))
}

/// A `SearchQuery` matching any UID in a parsed set.
///
/// A set is a union of ranges, so it becomes an `Or` of [`SearchQuery::UidRange`].
/// `*` cannot be resolved here — it means "the largest UID in use", which
/// depends on the mailbox — so a set containing it is expressed as the open
/// range up to `u32::MAX`, which selects the same messages for any mailbox.
fn uid_set_query(set: &SequenceSet) -> SearchQuery {
    let ranges = set.uid_ranges();
    if ranges.is_empty() {
        return SearchQuery::None;
    }
    let terms: Vec<SearchQuery> = ranges
        .into_iter()
        .map(|(low, high)| SearchQuery::UidRange { low, high })
        .collect();
    if terms.len() == 1 {
        terms.into_iter().next().unwrap_or(SearchQuery::None)
    } else {
        SearchQuery::Or(terms)
    }
}

/// Parses a numeric search argument.
fn parse_number(value: &str) -> Result<u64, SearchParseError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SearchParseError::InvalidNumber(value.to_owned()));
    }
    value
        .parse()
        .map_err(|_| SearchParseError::InvalidNumber(value.to_owned()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Compiles a `SEARCH` criteria list from its wire text.
    fn compile_text(text: &str, uid_mode: bool) -> Result<CompiledSearch, SearchParseError> {
        let prefix = if uid_mode { "UID SEARCH" } else { "SEARCH" };
        let line = format!("A1 {prefix} {text}\r\n");
        let Ok(parsed) = crate::parser::parse_line(line.as_bytes(), 65536) else {
            // A line the tokeniser rejects is refused before this module runs.
            return Err(SearchParseError::NoCriteria);
        };
        let crate::parser::Parsed::Complete(command) = parsed else {
            panic!("no literal in these tests");
        };
        compile(&command.args, uid_mode)
    }

    /// Compiles in sequence mode, expecting success.
    fn query(text: &str) -> SearchQuery {
        compile_text(text, false)
            .unwrap_or_else(|e| panic!("{text} should compile: {e}"))
            .query()
            .clone()
    }

    /// Compiles in UID mode, expecting success.
    fn uid_query(text: &str) -> SearchQuery {
        compile_text(text, true)
            .unwrap_or_else(|e| panic!("{text} should compile: {e}"))
            .query()
            .clone()
    }

    // --- flag keys --------------------------------------------------------

    #[test]
    fn all_compiles_to_all() {
        assert_eq!(query("ALL"), SearchQuery::All);
        assert_eq!(query("all"), SearchQuery::All, "case-insensitive");
    }

    #[test]
    fn every_flag_key_and_its_negation_compile() {
        for (text, expected) in [
            ("SEEN", SearchQuery::HasFlag(FlagKey::Seen)),
            ("UNSEEN", SearchQuery::LacksFlag(FlagKey::Seen)),
            ("ANSWERED", SearchQuery::HasFlag(FlagKey::Answered)),
            ("UNANSWERED", SearchQuery::LacksFlag(FlagKey::Answered)),
            ("FLAGGED", SearchQuery::HasFlag(FlagKey::Flagged)),
            ("UNFLAGGED", SearchQuery::LacksFlag(FlagKey::Flagged)),
            ("DELETED", SearchQuery::HasFlag(FlagKey::Deleted)),
            ("UNDELETED", SearchQuery::LacksFlag(FlagKey::Deleted)),
            ("DRAFT", SearchQuery::HasFlag(FlagKey::Draft)),
            ("UNDRAFT", SearchQuery::LacksFlag(FlagKey::Draft)),
        ] {
            assert_eq!(query(text), expected, "{text}");
        }
    }

    #[test]
    fn a_flag_key_and_its_negation_are_never_the_same() {
        // The mutation guard against SEEN/UNSEEN being wired identically.
        for (positive, negative) in [
            ("SEEN", "UNSEEN"),
            ("ANSWERED", "UNANSWERED"),
            ("FLAGGED", "UNFLAGGED"),
            ("DELETED", "UNDELETED"),
            ("DRAFT", "UNDRAFT"),
        ] {
            assert_ne!(
                query(positive),
                query(negative),
                "{positive} must differ from {negative}"
            );
        }
    }

    #[test]
    fn keyword_keys_compile() {
        assert_eq!(
            query("KEYWORD $Junk"),
            SearchQuery::HasKeyword(String::from("$Junk"))
        );
        assert_eq!(
            query("UNKEYWORD $Junk"),
            SearchQuery::LacksKeyword(String::from("$Junk"))
        );
    }

    #[test]
    fn the_recent_family_is_refused_not_faked() {
        // ADR-045: no `\Recent` state, so these cannot be answered honestly.
        for text in ["NEW", "OLD", "RECENT"] {
            assert!(
                matches!(
                    compile_text(text, false),
                    Err(SearchParseError::UnknownKey(_))
                ),
                "{text} must be refused"
            );
        }
    }

    // --- text keys --------------------------------------------------------

    #[test]
    fn text_keys_compile_with_their_field() {
        for (text, field) in [
            ("FROM alice", TextField::From),
            ("TO bob", TextField::To),
            ("CC carol", TextField::Cc),
            ("BCC dave", TextField::Bcc),
            ("SUBJECT report", TextField::Subject),
            ("BODY pineapple", TextField::Body),
            ("TEXT anything", TextField::Text),
        ] {
            let compiled = query(text);
            match compiled {
                SearchQuery::Contains { field: actual, .. } => {
                    assert_eq!(actual, field, "{text}");
                }
                other => panic!("{text} compiled to {other:?}"),
            }
        }
    }

    #[test]
    fn no_two_text_keys_share_a_field() {
        // The mutation guard against FROM/TO being wired to the same field.
        let fields: Vec<TextField> = [
            "FROM x",
            "TO x",
            "CC x",
            "BCC x",
            "SUBJECT x",
            "BODY x",
            "TEXT x",
        ]
        .iter()
        .map(|text| match query(text) {
            SearchQuery::Contains { field, .. } => field,
            other => panic!("{text} compiled to {other:?}"),
        })
        .collect();

        for (index, field) in fields.iter().enumerate() {
            for (other_index, other) in fields.iter().enumerate() {
                if index != other_index {
                    assert_ne!(field, other, "two keys share a field");
                }
            }
        }
    }

    #[test]
    fn a_quoted_argument_keeps_its_spaces() {
        assert_eq!(
            query(r#"SUBJECT "quarterly report""#),
            SearchQuery::Contains {
                field: TextField::Subject,
                value: String::from("quarterly report"),
            }
        );
    }

    #[test]
    fn the_argument_case_is_preserved() {
        // The key is case-insensitive; the *value* is not folded at compile
        // time, because matching lowercases both sides itself.
        assert_eq!(
            query("FROM Alice"),
            SearchQuery::Contains {
                field: TextField::From,
                value: String::from("Alice"),
            }
        );
    }

    #[test]
    fn header_takes_a_name_and_a_value() {
        assert_eq!(
            query("HEADER X-Custom marker"),
            SearchQuery::Contains {
                field: TextField::Header(String::from("X-Custom")),
                value: String::from("marker"),
            }
        );
    }

    #[test]
    fn a_missing_argument_is_rejected() {
        for text in [
            "FROM",
            "SUBJECT",
            "BODY",
            "HEADER",
            "HEADER X-Only",
            "KEYWORD",
        ] {
            assert!(
                matches!(
                    compile_text(text, false),
                    Err(SearchParseError::MissingArgument(_) | SearchParseError::NoCriteria)
                ),
                "{text} must be refused"
            );
        }
    }

    // --- numeric ----------------------------------------------------------

    #[test]
    fn size_keys_compile() {
        assert_eq!(query("LARGER 1000"), SearchQuery::Larger(1000));
        assert_eq!(query("SMALLER 500"), SearchQuery::Smaller(500));
    }

    #[test]
    fn larger_and_smaller_are_not_swapped() {
        assert!(matches!(query("LARGER 1"), SearchQuery::Larger(1)));
        assert!(matches!(query("SMALLER 1"), SearchQuery::Smaller(1)));
        assert_ne!(query("LARGER 1"), query("SMALLER 1"));
    }

    #[test]
    fn a_non_numeric_size_is_rejected() {
        for text in ["LARGER abc", "SMALLER -1", "LARGER 1.5", "LARGER "] {
            assert!(compile_text(text, false).is_err(), "{text}");
        }
    }

    // --- dates ------------------------------------------------------------

    #[test]
    fn date_keys_compile_with_their_field_and_comparison() {
        for (text, field, comparison) in [
            (
                "BEFORE 5-Sep-2026",
                DateField::Internal,
                DateComparison::Before,
            ),
            ("ON 5-Sep-2026", DateField::Internal, DateComparison::On),
            (
                "SINCE 5-Sep-2026",
                DateField::Internal,
                DateComparison::Since,
            ),
            (
                "SENTBEFORE 5-Sep-2026",
                DateField::Sent,
                DateComparison::Before,
            ),
            ("SENTON 5-Sep-2026", DateField::Sent, DateComparison::On),
            (
                "SENTSINCE 5-Sep-2026",
                DateField::Sent,
                DateComparison::Since,
            ),
        ] {
            match query(text) {
                SearchQuery::Date {
                    field: actual_field,
                    comparison: actual_comparison,
                    ..
                } => {
                    assert_eq!(actual_field, field, "{text} field");
                    assert_eq!(actual_comparison, comparison, "{text} comparison");
                }
                other => panic!("{text} compiled to {other:?}"),
            }
        }
    }

    #[test]
    fn date_text_parses_the_rfc_form() {
        for good in ["1-Jan-1970", "5-Sep-2026", "05-sep-2026", "31-DEC-1999"] {
            assert!(parse_date_text(good).is_some(), "{good} should parse");
        }
        for bad in [
            "",
            "2026-09-05",
            "5/Sep/2026",
            "32-Sep-2026",
            "5-Xxx-2026",
            "5-Sep",
        ] {
            assert!(parse_date_text(bad).is_none(), "{bad} should not parse");
        }
    }

    #[test]
    fn an_invalid_date_is_rejected() {
        for text in ["BEFORE 2026-09-05", "ON notadate", "SINCE 32-Sep-2026"] {
            assert!(
                matches!(
                    compile_text(text, false),
                    Err(SearchParseError::InvalidDate(_))
                ),
                "{text} must be refused"
            );
        }
    }

    // --- logic ------------------------------------------------------------

    #[test]
    fn not_wraps_its_key() {
        assert_eq!(
            query("NOT SEEN"),
            SearchQuery::Not(Box::new(SearchQuery::HasFlag(FlagKey::Seen)))
        );
    }

    #[test]
    fn or_takes_exactly_two_keys() {
        assert_eq!(
            query("OR SEEN FLAGGED"),
            SearchQuery::Or(vec![
                SearchQuery::HasFlag(FlagKey::Seen),
                SearchQuery::HasFlag(FlagKey::Flagged),
            ])
        );
        // A single key after OR is a missing argument.
        assert!(compile_text("OR SEEN", false).is_err());
    }

    #[test]
    fn adjacent_keys_are_anded() {
        assert_eq!(
            query("UNSEEN FLAGGED"),
            SearchQuery::And(vec![
                SearchQuery::LacksFlag(FlagKey::Seen),
                SearchQuery::HasFlag(FlagKey::Flagged),
            ])
        );
    }

    #[test]
    fn and_is_not_or() {
        // The mutation guard for the implicit-AND semantics.
        assert_ne!(query("SEEN FLAGGED"), query("OR SEEN FLAGGED"));
        assert!(matches!(query("SEEN FLAGGED"), SearchQuery::And(_)));
        assert!(matches!(query("OR SEEN FLAGGED"), SearchQuery::Or(_)));
    }

    #[test]
    fn a_parenthesised_list_is_a_key() {
        assert_eq!(
            query("(SEEN FLAGGED)"),
            SearchQuery::And(vec![
                SearchQuery::HasFlag(FlagKey::Seen),
                SearchQuery::HasFlag(FlagKey::Flagged),
            ])
        );
    }

    #[test]
    fn nesting_composes() {
        // NOT (OR SEEN FLAGGED)
        assert_eq!(
            query("NOT (OR SEEN FLAGGED)"),
            SearchQuery::Not(Box::new(SearchQuery::Or(vec![
                SearchQuery::HasFlag(FlagKey::Seen),
                SearchQuery::HasFlag(FlagKey::Flagged),
            ])))
        );

        // OR of two composite keys.
        assert_eq!(
            query("OR (SEEN FLAGGED) (UNSEEN DRAFT)"),
            SearchQuery::Or(vec![
                SearchQuery::And(vec![
                    SearchQuery::HasFlag(FlagKey::Seen),
                    SearchQuery::HasFlag(FlagKey::Flagged),
                ]),
                SearchQuery::And(vec![
                    SearchQuery::LacksFlag(FlagKey::Seen),
                    SearchQuery::HasFlag(FlagKey::Draft),
                ]),
            ])
        );
    }

    #[test]
    fn excessive_nesting_is_refused() {
        let deep = format!("{}SEEN", "NOT ".repeat(MAX_SEARCH_DEPTH + 5));
        assert_eq!(compile_text(&deep, false), Err(SearchParseError::TooDeep));
    }

    #[test]
    fn duplicate_criteria_are_harmless() {
        // Stating the same key twice is legal and means the same thing.
        assert_eq!(
            query("SEEN SEEN"),
            SearchQuery::And(vec![
                SearchQuery::HasFlag(FlagKey::Seen),
                SearchQuery::HasFlag(FlagKey::Seen),
            ])
        );
    }

    // --- UID and message sets --------------------------------------------

    #[test]
    fn an_explicit_uid_key_compiles_to_a_range() {
        assert_eq!(query("UID 4:9"), SearchQuery::UidRange { low: 4, high: 9 });
        assert_eq!(query("UID 7"), SearchQuery::UidRange { low: 7, high: 7 });
    }

    #[test]
    fn a_uid_set_becomes_a_union_of_ranges() {
        assert_eq!(
            query("UID 1,5:7"),
            SearchQuery::Or(vec![
                SearchQuery::UidRange { low: 1, high: 1 },
                SearchQuery::UidRange { low: 5, high: 7 },
            ])
        );
    }

    #[test]
    fn a_bare_set_in_uid_mode_names_uids() {
        assert_eq!(
            uid_query("4:9"),
            SearchQuery::UidRange { low: 4, high: 9 },
            "UID SEARCH 4:9 must constrain UIDs"
        );
    }

    #[test]
    fn a_bare_set_in_sequence_mode_is_deferred_to_the_caller() {
        // A sequence number is a position in the session's snapshot, which the
        // domain cannot express — it is returned separately.
        let compiled = compile_text("2:4", false).expect("compiles");
        assert_eq!(compiled.query(), &SearchQuery::All);
        assert_eq!(compiled.sequence_sets().len(), 1);
        assert_eq!(compiled.sequence_sets()[0].to_string(), "2:4");
    }

    #[test]
    fn a_sequence_set_combines_with_other_keys() {
        let compiled = compile_text("2:4 UNSEEN", false).expect("compiles");
        assert_eq!(compiled.sequence_sets().len(), 1);
        // The flag key still constrains the query.
        let rendered = format!("{:?}", compiled.query());
        assert!(rendered.contains("LacksFlag"), "{rendered}");
    }

    #[test]
    fn star_in_a_uid_set_becomes_an_open_range() {
        // `*` means the largest UID in use; an open range selects the same
        // messages without needing the mailbox.
        assert_eq!(
            query("UID 5:*"),
            SearchQuery::UidRange {
                low: 5,
                high: u32::MAX
            }
        );
    }

    #[test]
    fn an_invalid_uid_set_is_rejected() {
        for text in ["UID 0", "UID abc", "UID 1:", "UID -1"] {
            assert!(compile_text(text, false).is_err(), "{text} must be refused");
        }
    }

    // --- charset ----------------------------------------------------------

    #[test]
    fn the_supported_charsets_are_accepted() {
        for charset in ["UTF-8", "utf-8", "US-ASCII", "us-ascii"] {
            let text = format!("CHARSET {charset} UNSEEN");
            assert_eq!(
                query(&text),
                SearchQuery::LacksFlag(FlagKey::Seen),
                "{charset} should be accepted and not affect the query"
            );
        }
    }

    #[test]
    fn an_unsupported_charset_is_reported_as_badcharset() {
        // RFC 9051 §6.4.4: NO [BADCHARSET], not BAD — the client can retry.
        let error = compile_text("CHARSET ISO-8859-1 UNSEEN", false).expect_err("must be refused");
        assert_eq!(
            error,
            SearchParseError::UnsupportedCharset(String::from("ISO-8859-1"))
        );
        assert!(error.is_bad_charset(), "must carry BADCHARSET");
    }

    #[test]
    fn other_errors_are_not_badcharset() {
        for error in [
            SearchParseError::NoCriteria,
            SearchParseError::UnknownKey(String::from("X")),
            SearchParseError::TooDeep,
        ] {
            assert!(!error.is_bad_charset(), "{error:?}");
        }
    }

    #[test]
    fn charset_without_a_name_is_rejected() {
        assert!(compile_text("CHARSET", false).is_err());
    }

    // --- malformed --------------------------------------------------------

    #[test]
    fn an_empty_criteria_list_is_rejected() {
        assert_eq!(compile(&[], false), Err(SearchParseError::NoCriteria));
        assert_eq!(compile_text("", false), Err(SearchParseError::NoCriteria));
    }

    #[test]
    fn an_unknown_key_is_rejected_not_ignored() {
        for text in ["NOTAKEY", "SEEN NOTAKEY", "FUZZ 3"] {
            assert!(compile_text(text, false).is_err(), "{text} must be refused");
        }
    }

    #[test]
    fn unimplemented_rev2_keys_are_refused() {
        // Rejected cleanly rather than pretended: a client must not believe a
        // filter was applied when it was not. `MODSEQ` left this list when
        // CONDSTORE was implemented; it has its own tests below.
        for text in ["EMAILID x", "THREADID x", "SAVEDATESUPPORTED"] {
            assert!(compile_text(text, false).is_err(), "{text} must be refused");
        }
    }

    #[test]
    fn modseq_compiles_to_an_inclusive_comparison() {
        // RFC 7162 §3.1.5 defines `MODSEQ n` as "at or above n". Compiling it
        // to a strict `>` would hide the very revision the client is asking
        // about.
        let query = compile_text("MODSEQ 7", false).expect("MODSEQ must compile");
        assert_eq!(
            query.query(),
            &sandesh_core::SearchQuery::ModSeqAtLeast(
                sandesh_core::ModSeq::new(7).expect("non-zero")
            )
        );
    }

    #[test]
    fn modseq_combines_with_other_keys() {
        // The point of reusing one engine: MODSEQ must compose with everything
        // else rather than being a special top-level mode.
        assert!(compile_text("SEEN MODSEQ 3", false).is_ok());
        assert!(compile_text("OR MODSEQ 3 UNSEEN", false).is_ok());
        assert!(compile_text("NOT MODSEQ 3", false).is_ok());
    }

    #[test]
    fn a_malformed_modseq_is_refused() {
        // 0 is reserved by RFC 7162 §3, so it is not a usable bound.
        for text in ["MODSEQ", "MODSEQ abc", "MODSEQ 0", "MODSEQ -1"] {
            assert!(compile_text(text, false).is_err(), "{text} must be refused");
        }
    }

    #[test]
    fn the_per_flag_modseq_form_is_refused() {
        // RFC 7162 §3.1.5's `MODSEQ "/flags/\\Seen" all 7` names a specific
        // flag's metadata. This server keeps one sequence per placement, not
        // per flag, so accepting the form would mean ignoring the entry-name
        // and entry-type arguments and answering a different question.
        for text in [
            r#"MODSEQ "/flags/\Seen" all 7"#,
            r#"MODSEQ "/flags/\Draft" shared 1"#,
        ] {
            assert!(compile_text(text, false).is_err(), "{text} must be refused");
        }
    }

    #[test]
    fn compilation_never_panics_on_any_input() {
        for hostile in [
            "",
            "(",
            "()",
            "NOT",
            "OR",
            "OR OR OR",
            "NOT NOT NOT",
            "UID",
            "UID *",
            "LARGER",
            "HEADER",
            "CHARSET UTF-8",
            "4294967295",
            "1:*",
            "(((SEEN)))",
            "SEEN OR",
        ] {
            let _ = compile_text(hostile, false);
            let _ = compile_text(hostile, true);
        }
    }

    #[test]
    fn an_empty_parenthesised_list_is_rejected() {
        assert!(compile_text("()", false).is_err());
    }

    // --- realistic client queries ----------------------------------------

    #[test]
    fn the_odoo_query_compiles() {
        // Odoo `fetchmail` issues `SEARCH UNSEEN`, sometimes parenthesised.
        assert_eq!(query("UNSEEN"), SearchQuery::LacksFlag(FlagKey::Seen));
        assert_eq!(query("(UNSEEN)"), SearchQuery::LacksFlag(FlagKey::Seen));
    }

    #[test]
    fn typical_client_queries_compile() {
        for text in [
            "ALL",
            "UNSEEN",
            "UNDELETED",
            "NOT DELETED",
            "UNSEEN UNDELETED",
            "FROM alice UNSEEN",
            r#"SUBJECT "quarterly report" SINCE 1-Sep-2026"#,
            "OR UNSEEN FLAGGED",
            "OR (UNSEEN) (FLAGGED)",
            "LARGER 1000 SMALLER 100000",
            "HEADER Message-ID <abc@example.test>",
            "TEXT invoice",
            "UID 1:*",
            "CHARSET UTF-8 UNSEEN NOT DELETED",
        ] {
            assert!(
                compile_text(text, false).is_ok(),
                "{text} should compile: {:?}",
                compile_text(text, false)
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod esearch_tests {
    use super::*;

    /// Compiles through the real tokeniser, as the session does.
    fn compile_text(text: &str, uid_mode: bool) -> Result<CompiledSearch, SearchParseError> {
        let prefix = if uid_mode { "UID SEARCH" } else { "SEARCH" };
        let line = format!("A1 {prefix} {text}\r\n");
        let parsed = crate::parser::parse_line(line.as_bytes(), 65536).expect("tokenises");
        match parsed {
            crate::parser::Parsed::Complete(command) => compile(&command.args, uid_mode),
            crate::parser::Parsed::NeedLiteral(_) => panic!("expected a complete command"),
        }
    }

    #[test]
    fn a_search_without_return_has_no_options() {
        // The classic form must be untouched: `None` selects `* SEARCH`.
        let compiled = compile_text("ALL", false).expect("compiles");
        assert!(compiled.returns().is_none());
    }

    #[test]
    fn each_option_is_recognised() {
        for (text, option) in [
            ("MIN", ReturnOption::Min),
            ("MAX", ReturnOption::Max),
            ("ALL", ReturnOption::All),
            ("COUNT", ReturnOption::Count),
        ] {
            let compiled = compile_text(&format!("RETURN ({text}) ALL"), false).expect("compiles");
            let returns = compiled.returns().expect("has options");
            assert!(returns.wants(option), "{text} not recognised");
        }
    }

    #[test]
    fn option_names_are_case_insensitive() {
        let compiled = compile_text("RETURN (min Count) ALL", false).expect("compiles");
        let returns = compiled.returns().expect("has options");
        assert!(returns.wants(ReturnOption::Min));
        assert!(returns.wants(ReturnOption::Count));
    }

    #[test]
    fn several_options_are_all_recognised() {
        let compiled = compile_text("RETURN (MIN MAX ALL COUNT) ALL", false).expect("compiles");
        let returns = compiled.returns().expect("has options");
        for option in ReturnOption::ALL_OPTIONS {
            assert!(returns.wants(option), "{} missing", option.as_str());
        }
    }

    #[test]
    fn response_order_is_fixed_not_the_clients() {
        // Determinism: whatever order the client asked in, the response is
        // MIN MAX ALL COUNT. A test asserting on response bytes would
        // otherwise be order-dependent on client input.
        let scrambled = compile_text("RETURN (COUNT ALL MAX MIN) ALL", false).expect("compiles");
        let ordered = compile_text("RETURN (MIN MAX ALL COUNT) ALL", false).expect("compiles");
        assert_eq!(
            scrambled.returns().expect("options").in_response_order(),
            ordered.returns().expect("options").in_response_order()
        );
        assert_eq!(
            scrambled.returns().expect("options").in_response_order(),
            ReturnOption::ALL_OPTIONS.to_vec()
        );
    }

    #[test]
    fn a_duplicated_option_is_requested_once() {
        let compiled = compile_text("RETURN (COUNT COUNT count) ALL", false).expect("compiles");
        let ordered = compiled.returns().expect("options").in_response_order();
        assert_eq!(ordered, vec![ReturnOption::Count]);
    }

    #[test]
    fn an_empty_option_list_means_all() {
        // RFC 9051 §6.4.4. Distinct from an absent RETURN, which selects the
        // classic `* SEARCH` line instead.
        let compiled = compile_text("RETURN () ALL", false).expect("compiles");
        let returns = compiled.returns().expect("has options");
        assert!(returns.wants(ReturnOption::All));
        assert_eq!(returns.in_response_order(), vec![ReturnOption::All]);
    }

    #[test]
    fn an_unsupported_option_is_refused_not_ignored() {
        // The opposite of the ENABLE rule, deliberately: a RETURN option names
        // the answer the client is waiting for, so dropping it would answer a
        // different question in a response that still parses.
        let error = compile_text("RETURN (RELEVANCY) ALL", false).expect_err("must refuse");
        assert!(
            matches!(error, SearchParseError::UnknownReturnOption(ref name) if name == "RELEVANCY"),
            "{error:?}"
        );
        assert!(error.message().contains("RELEVANCY"));
    }

    #[test]
    fn save_is_refused_because_the_result_variable_does_not_exist() {
        // ADR-057. Accepting `SAVE` while ignoring it would leave a later
        // `FETCH $` addressing nothing.
        let error = compile_text("RETURN (SAVE) ALL", false).expect_err("must refuse");
        assert!(
            matches!(error, SearchParseError::UnknownReturnOption(ref name) if name == "SAVE"),
            "{error:?}"
        );
    }

    #[test]
    fn a_mix_of_supported_and_unsupported_options_is_refused_entirely() {
        // Partial acceptance would be the worst outcome: a response that looks
        // complete but silently omits what the client asked for.
        let error = compile_text("RETURN (MIN SAVE) ALL", false).expect_err("must refuse");
        assert!(
            matches!(error, SearchParseError::UnknownReturnOption(_)),
            "{error:?}"
        );
    }

    #[test]
    fn return_without_a_list_is_malformed() {
        for text in ["RETURN ALL", "RETURN"] {
            let error = compile_text(text, false).expect_err("must refuse");
            assert!(
                matches!(
                    error,
                    SearchParseError::MalformedReturn | SearchParseError::NoCriteria
                ),
                "{text}: {error:?}"
            );
        }
    }

    #[test]
    fn return_with_no_criteria_is_refused() {
        // RFC 9051 §6.4.4 requires at least one search key after the options.
        let error = compile_text("RETURN (COUNT)", false).expect_err("must refuse");
        assert!(matches!(error, SearchParseError::NoCriteria), "{error:?}");
    }

    #[test]
    fn criteria_after_return_compile_exactly_as_without_it() {
        // The point of stripping RETURN before parsing: one search engine, not
        // two. The compiled query must be identical either way.
        let plain = compile_text("SEEN FROM alice", false).expect("compiles");
        let extended = compile_text("RETURN (COUNT) SEEN FROM alice", false).expect("compiles");
        assert_eq!(plain.query(), extended.query());
        assert_eq!(plain.sequence_sets(), extended.sequence_sets());
    }

    #[test]
    fn sequence_sets_still_reach_the_caller_through_return() {
        // A sequence set in the criteria must not be swallowed by the RETURN
        // handling, or the search would silently widen to the whole mailbox.
        let extended = compile_text("RETURN (ALL) 2:4", false).expect("compiles");
        assert_eq!(extended.sequence_sets().len(), 1);
    }

    #[test]
    fn uid_mode_is_independent_of_return_options() {
        // `UID SEARCH RETURN (...)` must compile its sets as UIDs exactly as
        // `UID SEARCH` does.
        let plain = compile_text("2:4", true).expect("compiles");
        let extended = compile_text("RETURN (ALL) 2:4", true).expect("compiles");
        assert_eq!(plain.sequence_sets(), extended.sequence_sets());
    }
}

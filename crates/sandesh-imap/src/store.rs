//! `STORE` flag mutations: what a client may change, and how (RFC 9051 §6.4.6).
//!
//! This module is pure. It turns tokens into a [`StoreRequest`]; the session
//! applies it through [`sandesh_core::MessageStore::update_flags`], which is
//! the same port `FETCH` uses to set `\Seen`. There is deliberately no second
//! flag-mutation path.
//!
//! ```text
//! STORE <set> <±FLAGS[.SILENT]> (<flags>)
//!        │      │        │        │
//!        │      │        │        └─ the flags to set / add / remove
//!        │      │        └────────── whether to suppress the untagged FETCH
//!        │      └─────────────────── replace, add, or remove
//!        └────────────────────────── resolved through SequenceMap, never re-derived
//! ```
//!
//! # Why `.SILENT` is a property of the request, not of the response
//!
//! RFC 9051 §6.4.6 defines `.SILENT` as suppressing the untagged `FETCH` that
//! would otherwise report the new flags. The mutation happens either way.
//! Modelling it as a flag on the parsed request — rather than deciding at the
//! point of writing the response — keeps "did we change the flags" and "did we
//! tell the client" from ever being conflated, which is the mistake that makes
//! a silent store silently *not* store.

use core::fmt;
use std::collections::BTreeSet;

use sandesh_core::{FlagUpdate, Keyword, SystemFlags};

use crate::command::Token;

/// How many flags one `STORE` may name.
///
/// RFC 9051 sets no limit. The command line is already length-bounded, so this
/// is a statement of intent rather than the only guard: a single command may
/// not make the server build an unbounded keyword set.
pub const MAX_FLAGS: usize = 128;

/// Why a `STORE` argument could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreParseError {
    /// The `±FLAGS[.SILENT]` item was missing.
    MissingOperation,
    /// The flag list was missing.
    MissingFlags,
    /// The item was not a recognised `STORE` operation.
    UnknownOperation(String),
    /// A flag name was not a valid system flag or keyword.
    InvalidFlag(String),
    /// A system flag this server does not implement.
    UnsupportedFlag(String),
    /// More than [`MAX_FLAGS`] flags.
    TooManyFlags,
    /// The `UNCHANGEDSINCE` modifier was malformed.
    InvalidUnchangedSince(String),
}

impl StoreParseError {
    /// The text accompanying the `BAD` response.
    ///
    /// Names the offending token so a client author can fix it, and nothing
    /// else — no storage detail, no message content.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MissingOperation => String::from("STORE requires FLAGS, +FLAGS or -FLAGS"),
            Self::MissingFlags => String::from("STORE requires a flag list"),
            Self::UnknownOperation(item) => format!("unsupported STORE item: {item}"),
            Self::InvalidFlag(flag) => format!("invalid flag: {flag}"),
            Self::UnsupportedFlag(flag) => format!("unsupported flag: {flag}"),
            Self::TooManyFlags => String::from("STORE names too many flags"),
            Self::InvalidUnchangedSince(value) => {
                format!("invalid UNCHANGEDSINCE value: {value}")
            }
        }
    }
}

impl fmt::Display for StoreParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// Which of the three `STORE` operations was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOperation {
    /// `FLAGS` — the named flags become the entire set.
    Replace,
    /// `+FLAGS` — the named flags are added, others untouched.
    Add,
    /// `-FLAGS` — the named flags are removed, others untouched.
    Remove,
}

impl StoreOperation {
    /// The wire name, without the `.SILENT` suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "FLAGS",
            Self::Add => "+FLAGS",
            Self::Remove => "-FLAGS",
        }
    }
}

/// A parsed `STORE` argument: the operation, the flags, and whether to reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRequest {
    operation: StoreOperation,
    /// Whether the untagged `FETCH` is suppressed (`.SILENT`).
    silent: bool,
    /// The system flags named.
    system: SystemFlags,
    /// The keywords named.
    keywords: BTreeSet<Keyword>,
    /// `UNCHANGEDSINCE <n>` (RFC 7162 §3.2), when the client sent one.
    ///
    /// A precondition, not a filter: a placement whose modification sequence
    /// exceeds `n` is *refused* and named in a `MODIFIED` response code, rather
    /// than silently skipped. The distinction matters because the client must
    /// learn that its change did not happen.
    unchanged_since: Option<sandesh_core::ModSeq>,
}

impl StoreRequest {
    /// The `UNCHANGEDSINCE` precondition, when the client sent one.
    #[must_use]
    pub const fn unchanged_since(&self) -> Option<sandesh_core::ModSeq> {
        self.unchanged_since
    }

    /// Which operation this request performs.
    #[must_use]
    pub const fn operation(&self) -> StoreOperation {
        self.operation
    }

    /// Whether the untagged `FETCH` response is suppressed.
    ///
    /// The flags change regardless; only the report is suppressed
    /// (RFC 9051 §6.4.6).
    #[must_use]
    pub const fn is_silent(&self) -> bool {
        self.silent
    }

    /// The system flags named by the request.
    #[must_use]
    pub const fn system(&self) -> SystemFlags {
        self.system
    }

    /// The keywords named by the request.
    #[must_use]
    pub const fn keywords(&self) -> &BTreeSet<Keyword> {
        &self.keywords
    }

    /// The domain-level flag update this request performs.
    ///
    /// The mapping is one-to-one with [`FlagUpdate`], which exists precisely so
    /// that `STORE` does not need its own mutation logic: the storage layer
    /// applies add/remove/replace inside one transaction, and this adapter only
    /// decides *which*.
    #[must_use]
    pub fn to_update(&self) -> FlagUpdate {
        match self.operation {
            StoreOperation::Replace => FlagUpdate::Replace {
                system: self.system,
                keywords: self.keywords.clone(),
            },
            StoreOperation::Add => FlagUpdate::Add {
                system: self.system,
                keywords: self.keywords.clone(),
            },
            StoreOperation::Remove => FlagUpdate::Remove {
                system: self.system,
                keywords: self.keywords.clone(),
            },
        }
    }

    /// Parses the arguments following the sequence set.
    ///
    /// Accepts both shapes RFC 9051 §6.4.6 allows for the flag list:
    ///
    /// ```text
    /// STORE 1 +FLAGS (\Seen \Flagged)   → a parenthesised list
    /// STORE 1 +FLAGS \Seen              → a single bare flag
    /// STORE 1 FLAGS ()                  → the empty list, which clears everything
    /// ```
    ///
    /// # `FLAGS ()` is meaningful, not empty input
    ///
    /// An empty list with `FLAGS` means "the resulting flag set is empty" — it
    /// is how a client clears every flag, and it must not be confused with a
    /// missing argument. `+FLAGS ()` and `-FLAGS ()` are legal no-ops.
    ///
    /// # Errors
    ///
    /// Returns [`StoreParseError`] when the operation item is missing or
    /// unrecognised, when the flag list is absent, when a flag is not a valid
    /// keyword or known system flag, or when more than [`MAX_FLAGS`] are named.
    /// The command is then answered `BAD` and **nothing is mutated**.
    pub fn parse(tokens: &[Token]) -> Result<Self, StoreParseError> {
        // The line parser leaves the trailing CRLF attached to the final atom,
        // so every token is trimmed before it is interpreted, and tokens that
        // were nothing but the terminator disappear.
        let tokens: Vec<Token> = tokens
            .iter()
            .map(trim_token)
            .filter(|token| !matches!(token, Token::Atom(text) if text.is_empty()))
            .collect();

        // RFC 7162 §3.2 puts `(UNCHANGEDSINCE n)` *before* the operation:
        // `STORE 1 (UNCHANGEDSINCE 12) +FLAGS (\Seen)`. It is removed first so
        // the rest of the grammar is unchanged from the non-CONDSTORE form.
        let (tokens, unchanged_since) = split_unchanged_since(&tokens)?;

        let Some(item) = tokens.first().and_then(Token::as_text) else {
            return Err(StoreParseError::MissingOperation);
        };

        let (operation, silent) = parse_operation(item)?;

        // The flag list: either a parenthesised list or one bare flag.
        let names: Vec<String> = match tokens.get(1) {
            Some(Token::List(items)) => items
                .iter()
                .map(|token| token.as_text().unwrap_or_default().to_owned())
                .collect(),
            Some(token) => match token.as_text() {
                Some(text) if !text.is_empty() => vec![text.to_owned()],
                _ => return Err(StoreParseError::MissingFlags),
            },
            None => return Err(StoreParseError::MissingFlags),
        };

        // Anything after the flag list is not part of this command's grammar.
        if tokens.len() > 2 {
            let extra = tokens
                .get(2)
                .and_then(Token::as_text)
                .unwrap_or_default()
                .to_owned();
            return Err(StoreParseError::UnknownOperation(extra));
        }

        if names.len() > MAX_FLAGS {
            return Err(StoreParseError::TooManyFlags);
        }

        let mut system = SystemFlags::NONE;
        let mut keywords = BTreeSet::new();

        for name in &names {
            if name.is_empty() {
                return Err(StoreParseError::InvalidFlag(name.clone()));
            }
            if name.starts_with('\\') {
                // A system flag. `\*` is the "permit any keyword" marker that
                // appears in PERMANENTFLAGS; it is not storable, so a client
                // trying to *set* it is refused rather than silently ignored.
                match SystemFlags::from_name(name) {
                    Some(flag) => system = system.union(flag),
                    None => return Err(StoreParseError::UnsupportedFlag(name.clone())),
                }
            } else {
                match Keyword::new(name.clone()) {
                    Some(keyword) => {
                        keywords.insert(keyword);
                    }
                    None => return Err(StoreParseError::InvalidFlag(name.clone())),
                }
            }
        }

        Ok(Self {
            operation,
            silent,
            system,
            keywords,
            unchanged_since,
        })
    }
}

/// Removes a leading `(UNCHANGEDSINCE n)` modifier, returning it separately.
///
/// RFC 7162 §3.2 defines exactly one store modifier, so a leading
/// parenthesised list that is not `UNCHANGEDSINCE` is a syntax error rather
/// than something to pass through: the flag list never comes first, so a list
/// in this position can only have been intended as a modifier.
fn split_unchanged_since(
    tokens: &[Token],
) -> Result<(Vec<Token>, Option<sandesh_core::ModSeq>), StoreParseError> {
    let Some(Token::List(inner)) = tokens.first() else {
        return Ok((tokens.to_vec(), None));
    };

    let name = inner.first().and_then(Token::as_text).unwrap_or_default();
    if !name.eq_ignore_ascii_case("UNCHANGEDSINCE") {
        return Err(StoreParseError::UnknownOperation(name.to_owned()));
    }

    let value = inner
        .get(1)
        .and_then(Token::as_text)
        .ok_or_else(|| StoreParseError::InvalidUnchangedSince(String::new()))?;
    if inner.len() > 2 {
        return Err(StoreParseError::InvalidUnchangedSince(value.to_owned()));
    }

    // RFC 7162 §3 reserves 0, so it is not a usable precondition; the domain
    // type refuses it rather than this parser clamping it to 1.
    let parsed = value
        .parse::<u64>()
        .ok()
        .and_then(sandesh_core::ModSeq::new)
        .ok_or_else(|| StoreParseError::InvalidUnchangedSince(value.to_owned()))?;

    Ok((tokens.get(1..).unwrap_or_default().to_vec(), Some(parsed)))
}

/// Parses `FLAGS`, `+FLAGS`, `-FLAGS` and their `.SILENT` forms.
///
/// Case-insensitive: RFC 9051 §9 makes these atoms, and a client sending
/// `+flags.silent` means the same thing.
fn parse_operation(item: &str) -> Result<(StoreOperation, bool), StoreParseError> {
    let upper = item.to_ascii_uppercase();
    let (base, silent) = match upper.strip_suffix(".SILENT") {
        Some(base) => (base, true),
        None => (upper.as_str(), false),
    };

    let operation = match base {
        "FLAGS" => StoreOperation::Replace,
        "+FLAGS" => StoreOperation::Add,
        "-FLAGS" => StoreOperation::Remove,
        _ => return Err(StoreParseError::UnknownOperation(item.to_owned())),
    };
    Ok((operation, silent))
}

/// Strips the line terminator from a token, recursing into lists.
///
/// Trailing CRLF is a framing artefact, not part of a flag name: an atom of
/// `\Seen\r\n` names the same flag as `\Seen`, and leaving the terminator
/// attached would make the final flag of every bare-form `STORE` unparseable.
fn trim_token(token: &Token) -> Token {
    match token {
        Token::Atom(text) => Token::Atom(text.trim_end_matches(['\r', '\n']).to_owned()),
        Token::Quoted(text) => Token::Quoted(text.clone()),
        Token::List(inner) => Token::List(inner.iter().map(trim_token).collect()),
        Token::Literal(bytes) => Token::Literal(bytes.clone()),
        Token::Nil => Token::Nil,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Parses a `STORE` argument from its wire text.
    ///
    /// Goes through the real command parser, so these tests see the same
    /// tokenisation the session does — including the trailing CRLF.
    fn parse(text: &str) -> Result<StoreRequest, StoreParseError> {
        let line = format!("A1 STORE 1 {text}\r\n");
        // A line the *tokeniser* rejects — an unterminated list, say — is
        // already refused before this module is reached, and the session
        // answers `BAD` for it. It is reported here as a rejection so that
        // "the client cannot do this" tests do not have to care which of the
        // two layers refused.
        let Ok(parsed) = crate::parser::parse_line(line.as_bytes(), 65536) else {
            return Err(StoreParseError::MissingOperation);
        };
        let crate::parser::Parsed::Complete(command) = parsed else {
            panic!("no literal in these tests");
        };
        // args[0] is the sequence set; the operation and flags follow.
        StoreRequest::parse(&command.args[1..])
    }

    // --- operations -------------------------------------------------------

    #[test]
    fn the_three_operations_parse() {
        assert_eq!(
            parse(r"FLAGS (\Seen)").expect("valid").operation(),
            StoreOperation::Replace
        );
        assert_eq!(
            parse(r"+FLAGS (\Seen)").expect("valid").operation(),
            StoreOperation::Add
        );
        assert_eq!(
            parse(r"-FLAGS (\Seen)").expect("valid").operation(),
            StoreOperation::Remove
        );
    }

    #[test]
    fn operations_are_case_insensitive() {
        assert_eq!(
            parse(r"flags (\seen)").expect("valid").operation(),
            StoreOperation::Replace
        );
        assert_eq!(
            parse(r"+Flags (\Seen)").expect("valid").operation(),
            StoreOperation::Add
        );
    }

    #[test]
    fn the_three_operations_are_distinct_updates() {
        // The mutation this catches: add/remove/replace conflated. Each must
        // map to its own FlagUpdate variant.
        assert!(matches!(
            parse(r"FLAGS (\Seen)").expect("valid").to_update(),
            FlagUpdate::Replace { .. }
        ));
        assert!(matches!(
            parse(r"+FLAGS (\Seen)").expect("valid").to_update(),
            FlagUpdate::Add { .. }
        ));
        assert!(matches!(
            parse(r"-FLAGS (\Seen)").expect("valid").to_update(),
            FlagUpdate::Remove { .. }
        ));
    }

    #[test]
    fn an_unknown_operation_is_rejected() {
        for item in ["NOTFLAGS (\\Seen)", "*FLAGS (\\Seen)", "FLAG (\\Seen)"] {
            assert!(parse(item).is_err(), "{item} must be rejected");
        }
    }

    #[test]
    fn a_missing_operation_or_flag_list_is_rejected() {
        // `STORE 1` with nothing after it.
        let line = "A1 STORE 1\r\n";
        let parsed = crate::parser::parse_line(line.as_bytes(), 65536).expect("tokenises");
        let crate::parser::Parsed::Complete(command) = parsed else {
            panic!("no literal");
        };
        assert_eq!(
            StoreRequest::parse(&command.args[1..]),
            Err(StoreParseError::MissingOperation)
        );

        // An operation with no flags.
        assert_eq!(parse("+FLAGS"), Err(StoreParseError::MissingFlags));
    }

    // --- SILENT -----------------------------------------------------------

    #[test]
    fn silent_is_recognised_on_every_operation() {
        for text in [
            r"FLAGS.SILENT (\Seen)",
            r"+FLAGS.SILENT (\Seen)",
            r"-FLAGS.SILENT (\Seen)",
        ] {
            assert!(parse(text).expect(text).is_silent(), "{text}");
        }
    }

    #[test]
    fn the_non_silent_forms_are_not_silent() {
        for text in [r"FLAGS (\Seen)", r"+FLAGS (\Seen)", r"-FLAGS (\Seen)"] {
            assert!(!parse(text).expect(text).is_silent(), "{text}");
        }
    }

    #[test]
    fn silent_does_not_change_which_operation_runs() {
        // The mutation this catches: `.SILENT` parsed as a *different*
        // operation, or as a no-op. It affects only the response.
        for (silent, plain) in [
            (r"FLAGS.SILENT (\Seen)", r"FLAGS (\Seen)"),
            (r"+FLAGS.SILENT (\Seen)", r"+FLAGS (\Seen)"),
            (r"-FLAGS.SILENT (\Seen)", r"-FLAGS (\Seen)"),
        ] {
            let a = parse(silent).expect(silent);
            let b = parse(plain).expect(plain);
            assert_eq!(a.operation(), b.operation(), "{silent} vs {plain}");
            assert_eq!(a.system(), b.system());
            assert_eq!(a.keywords(), b.keywords());
            assert_ne!(a.is_silent(), b.is_silent(), "only silence differs");
        }
    }

    #[test]
    fn silent_is_case_insensitive() {
        assert!(parse(r"+flags.silent (\Seen)").expect("valid").is_silent());
        assert!(parse(r"+FLAGS.Silent (\Seen)").expect("valid").is_silent());
    }

    // --- flags ------------------------------------------------------------

    #[test]
    fn every_system_flag_parses() {
        let request = parse(r"FLAGS (\Seen \Answered \Flagged \Deleted \Draft)").expect("valid");
        assert_eq!(request.system(), SystemFlags::ALL);
        assert!(request.keywords().is_empty());
    }

    #[test]
    fn system_flag_names_are_case_insensitive() {
        let request = parse(r"FLAGS (\seen \FLAGGED)").expect("valid");
        assert!(request.system().contains(SystemFlags::SEEN));
        assert!(request.system().contains(SystemFlags::FLAGGED));
    }

    #[test]
    fn keywords_are_preserved() {
        let request = parse(r"+FLAGS ($Junk Urgent)").expect("valid");
        assert_eq!(request.system(), SystemFlags::NONE);
        let names: Vec<&str> = request.keywords().iter().map(Keyword::as_str).collect();
        assert_eq!(names, vec!["$Junk", "Urgent"]);
    }

    #[test]
    fn system_flags_and_keywords_mix() {
        let request = parse(r"FLAGS (\Seen $Junk)").expect("valid");
        assert!(request.system().contains(SystemFlags::SEEN));
        assert_eq!(request.keywords().len(), 1);
    }

    #[test]
    fn a_bare_flag_needs_no_parentheses() {
        let request = parse(r"+FLAGS \Seen").expect("valid");
        assert!(request.system().contains(SystemFlags::SEEN));
        // Including the trailing-CRLF case: the flag must not be `\Seen\r\n`.
        assert_eq!(
            request.keywords().len(),
            0,
            "no stray keyword from the CRLF"
        );
    }

    #[test]
    fn the_empty_list_clears_every_flag() {
        // `FLAGS ()` is how a client clears everything. It must parse, and it
        // must be a Replace with nothing in it — not a missing argument.
        let request = parse("FLAGS ()").expect("the empty list is legal");
        assert_eq!(request.operation(), StoreOperation::Replace);
        assert_eq!(request.system(), SystemFlags::NONE);
        assert!(request.keywords().is_empty());
        assert!(matches!(
            request.to_update(),
            FlagUpdate::Replace { system, ref keywords }
                if system == SystemFlags::NONE && keywords.is_empty()
        ));
    }

    #[test]
    fn an_unsupported_system_flag_is_rejected_not_ignored() {
        // `\Recent` was removed by RFC 9051 and this server keeps no such
        // state (ADR-045); `\*` is a PERMANENTFLAGS marker, not a storable
        // flag. Both must be refused rather than silently dropped, or a client
        // would believe a flag it cannot have was set.
        for flag in [r"\Recent", r"\*", r"\Nonsense"] {
            let text = format!("+FLAGS ({flag})");
            assert_eq!(
                parse(&text),
                Err(StoreParseError::UnsupportedFlag(flag.to_owned())),
                "{flag} must be refused"
            );
        }
    }

    #[test]
    fn an_invalid_keyword_is_rejected() {
        // Atom-specials cannot appear in a keyword (RFC 9051 §9).
        for flag in ["bad(keyword", "bad]keyword", "bad\"keyword"] {
            let text = format!("+FLAGS ({flag})");
            assert!(parse(&text).is_err(), "{flag} must be rejected");
        }
    }

    #[test]
    fn too_many_flags_is_rejected() {
        let many: Vec<String> = (0..MAX_FLAGS + 10).map(|n| format!("kw{n}")).collect();
        let text = format!("+FLAGS ({})", many.join(" "));
        assert_eq!(parse(&text), Err(StoreParseError::TooManyFlags));
    }

    #[test]
    fn trailing_junk_after_the_flag_list_is_rejected() {
        // A malformed command must not be partially honoured.
        assert!(parse(r"+FLAGS (\Seen) extra").is_err());
    }

    // --- update mapping ---------------------------------------------------

    #[test]
    fn the_update_carries_exactly_the_named_flags() {
        let request = parse(r"+FLAGS (\Seen $Junk)").expect("valid");
        match request.to_update() {
            FlagUpdate::Add { system, keywords } => {
                assert_eq!(system, SystemFlags::SEEN);
                assert_eq!(keywords.len(), 1);
                assert_eq!(keywords.iter().next().map(Keyword::as_str), Some("$Junk"));
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn parsing_never_panics_on_any_input() {
        for hostile in [
            "",
            "FLAGS",
            "+FLAGS ()",
            "-FLAGS ()",
            r"FLAGS (\)",
            r"FLAGS (\\)",
            "FLAGS (((",
            r"+FLAGS (\Seen \Seen \Seen)",
            "FLAGS (a b c d e f g)",
            r"FLAGS.SILENT.SILENT (\Seen)",
            r"++FLAGS (\Seen)",
        ] {
            let _ = parse(hostile);
        }
    }

    #[test]
    fn a_repeated_flag_is_idempotent() {
        let request = parse(r"+FLAGS (\Seen \Seen)").expect("valid");
        assert_eq!(request.system(), SystemFlags::SEEN);

        let request = parse("+FLAGS (dup dup)").expect("valid");
        assert_eq!(request.keywords().len(), 1, "a set, not a list");
    }
}

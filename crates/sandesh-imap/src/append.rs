//! `APPEND`: reading the optional flags and date-time that precede the literal
//! (RFC 9051 §6.3.12).
//!
//! This module is pure. It turns the tokens after the mailbox name into an
//! [`AppendRequest`]; the session then places the message through
//! [`sandesh_core::MessageStore::insert`].
//!
//! ```text
//! APPEND <mailbox> [(flags)] [date-time] {literal}
//!            │         │          │           │
//!            │         │          │           └─ raw octets, byte-authoritative
//!            │         │          └───────────── optional, quoted date-time
//!            │         └──────────────────────── optional, parenthesised
//!            └────────────────────────────────── the session resolves this
//! ```
//!
//! # Why both optional arguments are positional and untagged
//!
//! RFC 9051 §9 writes the grammar as
//! `append = "APPEND" SP mailbox [SP flag-list] [SP date-time] SP literal`.
//! Neither optional argument is introduced by a keyword, so what a token *is*
//! must be decided from its shape: a parenthesised list is the flag list, and a
//! quoted string is the date-time. Guessing by position alone would read
//! `APPEND INBOX "17-Jul-2026 09:04:11 +0000" {n}` — flags omitted, date given —
//! as a flag list, which is why [`AppendRequest::parse`] dispatches on the token
//! form instead.
//!
//! # Why an unparsable date-time is an error rather than "now"
//!
//! `INTERNALDATE` is what a client sorts and searches by, so silently
//! substituting the current time for a date the client actually sent would
//! store a value the client never asked for and cannot detect. RFC 9051 §6.3.12
//! makes a malformed date-time a syntax error; this returns one, and the
//! session answers `BAD` without storing anything.
//!
//! An *absent* date-time is different, and is not an error: the RFC leaves the
//! value to the server, and the session uses the time of receipt.

use core::fmt;
use std::collections::BTreeSet;

use sandesh_core::{Keyword, MessageFlags, SystemFlags, Timestamp};

use crate::command::Token;
use crate::store::MAX_FLAGS;

/// Why an `APPEND` could not be understood.
///
/// Every variant means nothing was stored: the request is parsed before the
/// session touches storage, so a malformed `APPEND` mutates no mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendParseError {
    /// No literal followed the arguments.
    MissingMessage,
    /// A flag was not a valid keyword or known system flag.
    InvalidFlag(String),
    /// A system flag this server does not store, such as `\*`.
    UnsupportedFlag(String),
    /// More than [`MAX_FLAGS`] flags were named.
    TooManyFlags,
    /// The date-time did not match RFC 9051 §9 `date-time`.
    InvalidDateTime(String),
    /// A token appeared where the grammar allows none.
    UnexpectedArgument(String),
}

impl AppendParseError {
    /// The text accompanying the `BAD` response.
    ///
    /// Describes the grammar, never the mailbox or the message: an error a
    /// client sees must not become a way to probe what exists.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MissingMessage => String::from("APPEND requires a message literal"),
            Self::InvalidFlag(flag) => format!("not a valid flag: {flag}"),
            Self::UnsupportedFlag(flag) => format!("flag cannot be stored: {flag}"),
            Self::TooManyFlags => format!("APPEND accepts at most {MAX_FLAGS} flags"),
            Self::InvalidDateTime(value) => format!("not a valid date-time: {value}"),
            Self::UnexpectedArgument(token) => format!("unexpected argument: {token}"),
        }
    }
}

impl fmt::Display for AppendParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// One `APPEND`, ready for the session to place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendRequest {
    /// The flags the new placement starts with.
    flags: MessageFlags,
    /// The `INTERNALDATE` the client asked for, if it named one.
    internal_date: Option<Timestamp>,
    /// The message, exactly as the client sent it.
    message: Vec<u8>,
}

impl AppendRequest {
    /// Parses the tokens that follow the mailbox name.
    ///
    /// # Errors
    ///
    /// Returns [`AppendParseError`] when the literal is missing, a flag is not
    /// storable, the date-time is malformed, or a token appears that the
    /// grammar does not allow. The session answers `BAD` and stores nothing.
    pub fn parse(tokens: &[Token]) -> Result<Self, AppendParseError> {
        let mut flags = MessageFlags::empty();
        let mut internal_date = None;
        let mut message = None;

        for token in tokens {
            match token {
                // A parenthesised list is the flag list. It may legitimately be
                // empty — `()` means "no flags", which is also the default.
                Token::List(items) if message.is_none() && internal_date.is_none() => {
                    flags = parse_flags(items)?;
                }
                // The literal is the message. It is taken verbatim: these are
                // the octets the product guarantees to preserve, so nothing
                // here trims, re-encodes or normalises them.
                Token::Literal(bytes) => message = Some(bytes.clone()),
                // A quoted string in this position is the date-time. Only
                // quoted: RFC 9051 §9 writes `date-time` as a quoted string,
                // and an unquoted one would be ambiguous with the flag list's
                // absence.
                Token::Quoted(text) if message.is_none() => {
                    internal_date = Some(parse_date_time(text)?);
                }
                // Trailing CRLF arrives as an empty atom; it is framing, not an
                // argument.
                Token::Atom(text) if text.trim_end_matches(['\r', '\n']).is_empty() => {}
                other => {
                    return Err(AppendParseError::UnexpectedArgument(
                        other.as_text().unwrap_or("(list)").to_owned(),
                    ));
                }
            }
        }

        Ok(Self {
            flags,
            internal_date,
            message: message.ok_or(AppendParseError::MissingMessage)?,
        })
    }

    /// The flags the new placement starts with.
    #[must_use]
    pub const fn flags(&self) -> &MessageFlags {
        &self.flags
    }

    /// The `INTERNALDATE` to store.
    ///
    /// The client's value when it sent one, otherwise `fallback` — the time of
    /// receipt, which RFC 9051 §6.3.12 leaves to the server.
    #[must_use]
    pub fn internal_date(&self, fallback: Timestamp) -> Timestamp {
        self.internal_date.unwrap_or(fallback)
    }

    /// Whether the client named an explicit `INTERNALDATE`.
    #[must_use]
    pub const fn has_explicit_date(&self) -> bool {
        self.internal_date.is_some()
    }

    /// The message octets, exactly as submitted.
    #[must_use]
    pub fn message(&self) -> &[u8] {
        &self.message
    }

    /// How many octets the message is.
    #[must_use]
    pub fn len(&self) -> usize {
        self.message.len()
    }

    /// Whether the submitted message is empty.
    ///
    /// An empty literal is syntactically legal — `{0}` — but is not a message.
    /// The session refuses it rather than storing an unparsable placement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.message.is_empty()
    }
}

/// Parses a flag list into a [`MessageFlags`].
///
/// The same rules `STORE` applies (`crate::store`): `\*` is refused because it
/// is the "any keyword permitted" marker from `PERMANENTFLAGS` and is not a
/// storable flag, and an unknown backslash name is refused rather than being
/// quietly dropped.
fn parse_flags(items: &[Token]) -> Result<MessageFlags, AppendParseError> {
    let names: Vec<String> = items
        .iter()
        .filter_map(|token| token.as_text())
        .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        .filter(|text| !text.is_empty())
        .collect();

    if names.len() > MAX_FLAGS {
        return Err(AppendParseError::TooManyFlags);
    }

    let mut system = SystemFlags::NONE;
    let mut keywords = BTreeSet::new();

    for name in &names {
        if name.starts_with('\\') {
            match SystemFlags::from_name(name) {
                Some(flag) => system = system.union(flag),
                None => return Err(AppendParseError::UnsupportedFlag(name.clone())),
            }
        } else {
            match Keyword::new(name.clone()) {
                Some(keyword) => {
                    keywords.insert(keyword);
                }
                None => return Err(AppendParseError::InvalidFlag(name.clone())),
            }
        }
    }

    Ok(MessageFlags::new(system, keywords))
}

/// Parses RFC 9051 §9 `date-time`: `"17-Jul-2026 09:04:11 +0000"`.
///
/// ```text
/// 17-Jul-2026 09:04:11 +0530
/// │  │   │    │        │
/// │  │   │    │        └─ zone; the result is normalised to UTC
/// │  │   │    └────────── time of day
/// │  │   └─────────────── four-digit year
/// │  └─────────────────── three-letter month, case-insensitive
/// └────────────────────── day, space- or zero-padded
/// ```
///
/// The zone is *applied*, not discarded: a client in `+0530` that appends a
/// message at 09:04 local means 03:34 UTC, and storing 09:04 would place the
/// message five and a half hours in the future for every other client. The
/// stored [`Timestamp`] is UTC by construction (`STORAGE_DESIGN.md` §2.1).
fn parse_date_time(value: &str) -> Result<Timestamp, AppendParseError> {
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];

    let invalid = || AppendParseError::InvalidDateTime(value.to_owned());
    let text = value.trim();

    // `date-day-fixed` may be space-padded, so split on whitespace runs.
    let mut parts = text.split_whitespace();
    let (Some(date), Some(time), Some(zone), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };

    let mut date_parts = date.split('-');
    let (Some(day), Some(month), Some(year), None) = (
        date_parts.next(),
        date_parts.next(),
        date_parts.next(),
        date_parts.next(),
    ) else {
        return Err(invalid());
    };

    let day: u8 = day.parse().map_err(|_| invalid())?;
    let year: i32 = year.parse().map_err(|_| invalid())?;
    if year.to_string().len() != 4 {
        // RFC 9051 §9 `date-year = 4DIGIT`. A two-digit year is ambiguous.
        return Err(invalid());
    }
    let month_index = MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(month))
        .ok_or_else(invalid)?;

    let mut time_parts = time.split(':');
    let (Some(hour), Some(minute), Some(second), None) = (
        time_parts.next(),
        time_parts.next(),
        time_parts.next(),
        time_parts.next(),
    ) else {
        return Err(invalid());
    };
    let hour: u8 = hour.parse().map_err(|_| invalid())?;
    let minute: u8 = minute.parse().map_err(|_| invalid())?;
    let second: u8 = second.parse().map_err(|_| invalid())?;

    // `zone = ("+" / "-") 4DIGIT`
    let (sign, digits) = match zone.split_at_checked(1) {
        Some(("+", digits)) => (1_i64, digits),
        Some(("-", digits)) => (-1_i64, digits),
        _ => return Err(invalid()),
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let zone_hours: i64 = digits
        .get(..2)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let zone_minutes: i64 = digits
        .get(2..)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;

    // Validate the civil date before converting: `days_from_civil` is a pure
    // arithmetic mapping and would happily accept 31-Feb, storing a date the
    // client never named.
    if month_index >= 12 || day == 0 || day > days_in_month(year, month_index + 1) {
        return Err(invalid());
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(invalid());
    }

    let days = days_from_civil(year, month_index + 1, day);
    let seconds = days
        .checked_mul(86_400)
        .and_then(|d| d.checked_add(i64::from(hour) * 3600))
        .and_then(|d| d.checked_add(i64::from(minute) * 60))
        .and_then(|d| d.checked_add(i64::from(second)))
        .ok_or_else(invalid)?;

    // Subtract the offset to reach UTC: 09:04 at +0530 is 03:34 UTC.
    let offset_seconds = sign * (zone_hours * 3600 + zone_minutes * 60);
    let micros = seconds
        .checked_sub(offset_seconds)
        .and_then(|seconds| seconds.checked_mul(1_000_000))
        .ok_or_else(invalid)?;

    Ok(Timestamp::from_micros(micros))
}

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// How many days the given 1-based month has.
const fn days_in_month(year: i32, month: usize) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from the Unix epoch to a proleptic Gregorian civil date.
///
/// Howard Hinnant's `days_from_civil`, which is exact for every date in range
/// and needs no calendar table. Used instead of pulling a date library into
/// this crate for one parse: the arithmetic is the definition, and
/// [`days_in_month`] has already rejected dates that do not exist.
fn days_from_civil(year: i32, month: usize, day: u8) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    // 1..=12 by construction: `days_in_month` rejected anything else.
    let month = i64::try_from(month).unwrap_or(1);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let year_of_era = i64::from(year_of_era);
    let doe = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + doy;
    i64::from(era) * 146_097 + doe - 719_468
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn literal(bytes: &[u8]) -> Token {
        Token::Literal(bytes.to_vec())
    }

    fn atom(text: &str) -> Token {
        Token::Atom(text.to_owned())
    }

    // --- the message ------------------------------------------------------

    #[test]
    fn a_bare_literal_is_a_complete_append() {
        let request = AppendRequest::parse(&[literal(b"From: a@b\r\n\r\nhi\r\n")]).expect("parses");
        assert_eq!(request.message(), b"From: a@b\r\n\r\nhi\r\n");
        assert_eq!(request.flags(), &MessageFlags::empty());
        assert!(!request.has_explicit_date());
    }

    #[test]
    fn an_append_without_a_literal_is_an_error() {
        // Nothing to store. Reported rather than treated as an empty message.
        assert_eq!(
            AppendRequest::parse(&[]),
            Err(AppendParseError::MissingMessage)
        );
    }

    #[test]
    fn the_message_bytes_are_preserved_exactly() {
        // The product guarantee: what the client sent is what is stored. CRLF
        // is not normalised, and a bare LF inside the body is not "fixed".
        let raw = b"From: a@b\r\nSubject: x\r\n\r\nline1\r\nline2\n\r\n";
        let request = AppendRequest::parse(&[literal(raw)]).expect("parses");
        assert_eq!(request.message(), raw);
    }

    #[test]
    fn binary_octets_survive_parsing() {
        // A literal is arbitrary octets (RFC 9051 §4.3), including bytes that
        // are not valid UTF-8. Decoding them as text would corrupt them.
        let raw: Vec<u8> = vec![0xFF, 0x00, 0xC3, 0x28, b'\r', b'\n'];
        let request = AppendRequest::parse(&[Token::Literal(raw.clone())]).expect("parses");
        assert_eq!(request.message(), raw.as_slice());
    }

    #[test]
    fn an_empty_literal_parses_but_is_reported_empty() {
        // `{0}` is syntactically legal; the session decides what to do with it.
        let request = AppendRequest::parse(&[literal(b"")]).expect("parses");
        assert!(request.is_empty());
        assert_eq!(request.len(), 0);
    }

    // --- flags ------------------------------------------------------------

    #[test]
    fn a_flag_list_sets_the_starting_flags() {
        let request = AppendRequest::parse(&[
            Token::List(vec![atom(r"\Seen"), atom(r"\Draft")]),
            literal(b"x"),
        ])
        .expect("parses");
        assert!(request.flags().has(SystemFlags::SEEN));
        assert!(request.flags().has(SystemFlags::DRAFT));
    }

    #[test]
    fn an_empty_flag_list_is_legal_and_means_no_flags() {
        let request = AppendRequest::parse(&[Token::List(vec![]), literal(b"x")]).expect("parses");
        assert_eq!(request.flags(), &MessageFlags::empty());
    }

    #[test]
    fn a_keyword_is_accepted_alongside_system_flags() {
        let request = AppendRequest::parse(&[
            Token::List(vec![atom(r"\Seen"), atom("$label1")]),
            literal(b"x"),
        ])
        .expect("parses");
        assert!(request.flags().has(SystemFlags::SEEN));
        assert!(
            request.flags().all_names().iter().any(|n| *n == "$label1"),
            "the keyword is kept: {:?}",
            request.flags().all_names()
        );
    }

    #[test]
    fn the_permanentflags_marker_cannot_be_stored() {
        // `\*` says "any keyword is permitted"; it is not itself a flag.
        assert_eq!(
            AppendRequest::parse(&[Token::List(vec![atom(r"\*")]), literal(b"x")]),
            Err(AppendParseError::UnsupportedFlag(String::from(r"\*")))
        );
    }

    #[test]
    fn an_unknown_system_flag_is_refused_not_dropped() {
        assert_eq!(
            AppendRequest::parse(&[Token::List(vec![atom(r"\Nonsense")]), literal(b"x")]),
            Err(AppendParseError::UnsupportedFlag(String::from(
                r"\Nonsense"
            )))
        );
    }

    #[test]
    fn too_many_flags_are_refused() {
        let many: Vec<Token> = (0..=MAX_FLAGS).map(|i| atom(&format!("k{i}"))).collect();
        assert_eq!(
            AppendRequest::parse(&[Token::List(many), literal(b"x")]),
            Err(AppendParseError::TooManyFlags)
        );
    }

    // --- date-time --------------------------------------------------------

    /// Parses a date-time argument and returns the resulting timestamp.
    fn date_of(value: &str) -> Result<Timestamp, AppendParseError> {
        AppendRequest::parse(&[Token::Quoted(value.to_owned()), literal(b"x")])
            .map(|request| request.internal_date(Timestamp::from_micros(0)))
    }

    #[test]
    fn a_utc_date_time_is_read_exactly() {
        // 1-Jan-2020 00:00:00 UTC is 1577836800 seconds after the epoch.
        let stamp = date_of("\u{31}-Jan-2020 00:00:00 +0000").expect("parses");
        assert_eq!(stamp.as_micros(), 1_577_836_800_000_000);
    }

    #[test]
    fn a_space_padded_day_is_accepted() {
        // RFC 9051 §9 `date-day-fixed = (SP DIGIT) / 2DIGIT`.
        let padded = date_of(" 1-Jan-2020 00:00:00 +0000").expect("parses");
        let plain = date_of("1-Jan-2020 00:00:00 +0000").expect("parses");
        assert_eq!(padded, plain);
    }

    #[test]
    fn a_positive_zone_is_subtracted_to_reach_utc() {
        // 05:30 at +0530 is midnight UTC. Ignoring the zone would store 05:30.
        let zoned = date_of("1-Jan-2020 05:30:00 +0530").expect("parses");
        assert_eq!(
            zoned.as_micros(),
            1_577_836_800_000_000,
            "the offset must be applied, not discarded"
        );
    }

    #[test]
    fn a_negative_zone_is_added_to_reach_utc() {
        // 19:00 on 31-Dec at -0500 is midnight UTC on 1-Jan.
        let zoned = date_of("31-Dec-2019 19:00:00 -0500").expect("parses");
        assert_eq!(zoned.as_micros(), 1_577_836_800_000_000);
    }

    #[test]
    fn the_month_is_case_insensitive() {
        assert_eq!(
            date_of("1-JAN-2020 00:00:00 +0000").expect("parses"),
            date_of("1-jan-2020 00:00:00 +0000").expect("parses")
        );
    }

    #[test]
    fn an_absent_date_time_falls_back_to_the_supplied_default() {
        let request = AppendRequest::parse(&[literal(b"x")]).expect("parses");
        let fallback = Timestamp::from_micros(1_234_567_890);
        assert_eq!(request.internal_date(fallback), fallback);
        assert!(!request.has_explicit_date());
    }

    #[test]
    fn a_supplied_date_time_overrides_the_default() {
        let request = AppendRequest::parse(&[
            Token::Quoted(String::from("1-Jan-2020 00:00:00 +0000")),
            literal(b"x"),
        ])
        .expect("parses");
        assert!(request.has_explicit_date());
        assert_eq!(
            request.internal_date(Timestamp::from_micros(0)).as_micros(),
            1_577_836_800_000_000
        );
    }

    #[test]
    fn a_malformed_date_time_is_an_error_not_a_silent_now() {
        // The important one: substituting the current time would store a value
        // the client never sent and cannot detect.
        for bad in [
            "not a date",
            "1-Jan-2020",
            "1-Foo-2020 00:00:00 +0000",
            "1-Jan-20 00:00:00 +0000",
            "32-Jan-2020 00:00:00 +0000",
            "1-Jan-2020 25:00:00 +0000",
            "1-Jan-2020 00:00:00",
            "1-Jan-2020 00:00:00 0000",
            "1-Jan-2020 00:00:00 +00",
            "1-Jan-2020 00:00 +0000",
        ] {
            assert!(
                matches!(date_of(bad), Err(AppendParseError::InvalidDateTime(_))),
                "{bad} must be refused"
            );
        }
    }

    // --- argument order ---------------------------------------------------

    #[test]
    fn flags_and_date_time_may_both_be_present() {
        let request = AppendRequest::parse(&[
            Token::List(vec![atom(r"\Seen")]),
            Token::Quoted(String::from("1-Jan-2020 00:00:00 +0000")),
            literal(b"body"),
        ])
        .expect("parses");
        assert!(request.flags().has(SystemFlags::SEEN));
        assert!(request.has_explicit_date());
        assert_eq!(request.message(), b"body");
    }

    #[test]
    fn a_date_time_without_flags_is_not_read_as_a_flag_list() {
        // The case that motivates dispatching on token shape: the date is the
        // *first* optional argument here, and it is still a date.
        let request = AppendRequest::parse(&[
            Token::Quoted(String::from("1-Jan-2020 00:00:00 +0000")),
            literal(b"body"),
        ])
        .expect("parses");
        assert!(request.has_explicit_date());
        assert_eq!(request.flags(), &MessageFlags::empty());
    }

    #[test]
    fn a_trailing_line_terminator_is_not_an_argument() {
        let request = AppendRequest::parse(&[literal(b"body"), atom("\r\n")]).expect("parses");
        assert_eq!(request.message(), b"body");
    }

    #[test]
    fn an_unexpected_argument_is_refused() {
        assert!(matches!(
            AppendRequest::parse(&[literal(b"body"), atom("EXTRA")]),
            Err(AppendParseError::UnexpectedArgument(_))
        ));
    }
}

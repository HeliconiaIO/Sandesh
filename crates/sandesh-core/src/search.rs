//! `SearchQuery` — the one search plan every surface compiles into.
//!
//! `STORAGE_DESIGN.md` §7 specifies two search surfaces sharing one plan: the
//! REST/UI filter syntax, and IMAP `SEARCH`. This is that plan. It lives in the
//! domain, not in a protocol adapter, so the two surfaces cannot answer the
//! same question differently.
//!
//! ```text
//! IMAP SEARCH keys ──┐
//!                    ├──▶ SearchQuery ──▶ matches(&Message, &MessageEntry)
//! REST filter syntax ┘      (domain)            (pure, no I/O)
//! ```
//!
//! # Why matching is a pure function, not a query
//!
//! A [`SearchQuery`] describes *what* to match; it does not know how candidates
//! are found. That keeps the domain free of SQL and lets the caller choose the
//! candidate set — for IMAP that is the selected mailbox's placements, which is
//! what makes mailbox scoping structural rather than a filter someone could
//! forget to apply.
//!
//! Text keys are evaluated against the message's already-parsed headers and
//! MIME parts. `STORAGE_DESIGN.md` §7 anticipates an FTS5 index behind a
//! `SearchIndex` trait for this; that is a scaling optimisation, and this type
//! is the plan such an index would execute, not a replacement for it.

use crate::{Message, MessageEntry, MessageFlags, SystemFlags, Timestamp};

/// Which flag a flag-key matches on.
///
/// Only the five RFC 9051 system flags. `\Recent` is deliberately absent:
/// `IMAP4rev2` removed it and this server keeps no such state (ADR-045), so a
/// query cannot ask about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKey {
    /// `\Seen`.
    Seen,
    /// `\Answered`.
    Answered,
    /// `\Flagged`.
    Flagged,
    /// `\Deleted`.
    Deleted,
    /// `\Draft`.
    Draft,
}

impl FlagKey {
    /// The system flag this key tests.
    #[must_use]
    pub const fn flag(self) -> SystemFlags {
        match self {
            Self::Seen => SystemFlags::SEEN,
            Self::Answered => SystemFlags::ANSWERED,
            Self::Flagged => SystemFlags::FLAGGED,
            Self::Deleted => SystemFlags::DELETED,
            Self::Draft => SystemFlags::DRAFT,
        }
    }
}

/// Which header-bearing field a text key searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextField {
    /// The `From` header.
    From,
    /// The `To` header.
    To,
    /// The `Cc` header.
    Cc,
    /// The `Bcc` header.
    Bcc,
    /// The `Subject` header.
    Subject,
    /// A named header, matched case-insensitively by name.
    Header(String),
    /// The message body — every non-multipart part's decoded text.
    Body,
    /// Headers and body together (`TEXT`).
    Text,
}

/// How a date key compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateComparison {
    /// Strictly earlier than the given day.
    Before,
    /// Within the given day.
    On,
    /// On or after the given day.
    Since,
}

/// Which date a date key compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    /// The placement's `INTERNALDATE`.
    Internal,
    /// The message's `Date` header.
    Sent,
}

/// One node of a search plan.
///
/// The tree is the plan: `And`/`Or`/`Not` compose, and every leaf is a
/// self-contained predicate. IMAP's implicit AND between adjacent keys becomes
/// an [`Self::And`]; `OR a b` becomes [`Self::Or`]; `NOT k` becomes
/// [`Self::Not`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchQuery {
    /// Matches every message (`ALL`).
    All,
    /// Matches nothing. Not an IMAP key — the identity for an impossible
    /// condition, so a caller never has to represent "no results" specially.
    None,

    /// The placement has this flag set.
    HasFlag(FlagKey),
    /// The placement does not have this flag set.
    ///
    /// A distinct variant rather than `Not(HasFlag(..))` because RFC 9051
    /// defines `UNSEEN` as its own key, and keeping the pairing explicit means
    /// a mutation that inverts one cannot silently pass as the other.
    LacksFlag(FlagKey),
    /// The placement carries this keyword.
    HasKeyword(String),
    /// The placement does not carry this keyword.
    LacksKeyword(String),

    /// A substring match, case-insensitive, against a field.
    ///
    /// RFC 9051 §6.4.4 defines these keys as substring matches, not word or
    /// prefix matches, and case-insensitively.
    Contains {
        /// Where to look.
        field: TextField,
        /// What to look for. An empty string matches any message that has the
        /// field at all, per RFC 9051 §6.4.4.
        value: String,
    },

    /// The raw message is strictly larger than this many octets.
    Larger(u64),
    /// The raw message is strictly smaller than this many octets.
    Smaller(u64),

    /// The placement's UID is in this inclusive range.
    ///
    /// A range rather than a set so the domain stays free of IMAP's
    /// sequence-set grammar; the adapter expands a set into a union of ranges.
    UidRange {
        /// Lowest UID accepted.
        low: u32,
        /// Highest UID accepted.
        high: u32,
    },

    /// A date comparison.
    Date {
        /// Which date.
        field: DateField,
        /// How to compare.
        comparison: DateComparison,
        /// The day being compared against, as a UTC timestamp; only the
        /// calendar day is significant (RFC 9051 §6.4.4).
        value: Timestamp,
    },

    /// The message has at least one attachment.
    HasAttachment,

    /// The placement's modification sequence is at or above this value.
    ///
    /// IMAP `MODSEQ` (RFC 7162 §3.1.5), which is defined as `>=` rather than
    /// `>`: a client asking `SEARCH MODSEQ n` is asking "what do I not yet
    /// know about", and it saved `n` as the highest value it had *seen*, so the
    /// boundary itself must be included.
    ///
    /// Answerable from the placement alone, like the flag and UID keys, so it
    /// costs no message load.
    ModSeqAtLeast(crate::ModSeq),
    /// Every child must match. An empty list matches everything.
    And(Vec<SearchQuery>),
    /// At least one child must match. An empty list matches nothing.
    Or(Vec<SearchQuery>),
    /// The child must not match.
    Not(Box<SearchQuery>),
}

impl SearchQuery {
    /// Whether `message`, placed as `entry`, satisfies this query.
    ///
    /// Pure and total: every input produces an answer, and nothing here can
    /// fail, block or allocate unboundedly. `message` may be `None` when the
    /// caller could not load it — a query needing message content then does
    /// not match, rather than matching vacuously.
    #[must_use]
    pub fn matches(&self, message: Option<&Message>, entry: &MessageEntry) -> bool {
        match self {
            Self::All => true,
            Self::None => false,

            Self::HasFlag(key) => entry.flags.has(key.flag()),
            Self::LacksFlag(key) => !entry.flags.has(key.flag()),
            Self::HasKeyword(keyword) => has_keyword(&entry.flags, keyword),
            Self::LacksKeyword(keyword) => !has_keyword(&entry.flags, keyword),

            Self::Contains { field, value } => {
                message.is_some_and(|message| field_contains(message, field, value))
            }

            // Size comes from the placement's message. RFC 9051 §6.4.4 defines
            // both as strict comparisons.
            Self::Larger(threshold) => message.is_some_and(|message| message.size() > *threshold),
            Self::Smaller(threshold) => message.is_some_and(|message| message.size() < *threshold),

            Self::UidRange { low, high } => {
                let uid = entry.uid.get();
                uid >= *low && uid <= *high
            }

            Self::Date {
                field,
                comparison,
                value,
            } => matches_date(message, entry, *field, *comparison, *value),

            Self::HasAttachment => {
                message.is_some_and(|message| !message.parsed.attachments.is_empty())
            }

            // RFC 7162 §3.1.5 defines the comparison as inclusive.
            Self::ModSeqAtLeast(threshold) => entry.mod_seq >= *threshold,

            Self::And(children) => children.iter().all(|child| child.matches(message, entry)),
            Self::Or(children) => children.iter().any(|child| child.matches(message, entry)),
            Self::Not(child) => !child.matches(message, entry),
        }
    }

    /// Whether evaluating this query requires the message, not just the
    /// placement.
    ///
    /// Flag, keyword and UID keys are answerable from a [`MessageEntry`]
    /// alone. Letting a caller ask means a `SEARCH UNSEEN` over a large
    /// mailbox loads no message bodies at all — the difference between a
    /// constant-cost search and one proportional to the mailbox's total size.
    #[must_use]
    pub fn needs_message(&self) -> bool {
        match self {
            Self::All
            | Self::None
            | Self::HasFlag(_)
            | Self::LacksFlag(_)
            | Self::HasKeyword(_)
            | Self::LacksKeyword(_)
            | Self::UidRange { .. }
            | Self::ModSeqAtLeast(_) => false,

            Self::Contains { .. } | Self::Larger(_) | Self::Smaller(_) | Self::HasAttachment => {
                true
            }

            // A date key needs the message only when it reads the `Date`
            // header; INTERNALDATE lives on the placement.
            Self::Date { field, .. } => matches!(field, DateField::Sent),

            Self::And(children) | Self::Or(children) => children.iter().any(Self::needs_message),
            Self::Not(child) => child.needs_message(),
        }
    }
}

/// Whether a flag set carries a keyword, compared case-insensitively.
///
/// RFC 9051 §9 makes `flag_keyword` an atom, and clients are inconsistent
/// about the case of conventional keywords such as `$Junk`.
fn has_keyword(flags: &MessageFlags, keyword: &str) -> bool {
    flags
        .keywords()
        .iter()
        .any(|existing| existing.as_str().eq_ignore_ascii_case(keyword))
}

/// Whether a message's field contains `value`, case-insensitively.
fn field_contains(message: &Message, field: &TextField, value: &str) -> bool {
    // RFC 9051 §6.4.4: an empty search string matches any message that has the
    // field. For BODY/TEXT it therefore matches everything.
    if value.is_empty() {
        return match field {
            TextField::Body | TextField::Text => true,
            TextField::From => header_present(message, "from"),
            TextField::To => header_present(message, "to"),
            TextField::Cc => header_present(message, "cc"),
            TextField::Bcc => header_present(message, "bcc"),
            TextField::Subject => header_present(message, "subject"),
            TextField::Header(name) => header_present(message, name),
        };
    }

    let needle = value.to_lowercase();

    match field {
        TextField::From => header_contains(message, "from", &needle),
        TextField::To => header_contains(message, "to", &needle),
        TextField::Cc => header_contains(message, "cc", &needle),
        TextField::Bcc => header_contains(message, "bcc", &needle),
        TextField::Subject => header_contains(message, "subject", &needle),
        TextField::Header(name) => header_contains(message, name, &needle),
        TextField::Body => body_contains(message, &needle),
        // RFC 9051 §6.4.4: TEXT is the header *and* the body.
        TextField::Text => any_header_contains(message, &needle) || body_contains(message, &needle),
    }
}

/// Whether any header with this name exists.
fn header_present(message: &Message, name: &str) -> bool {
    message
        .parsed
        .headers
        .iter()
        .any(|header| header.name_matches(name))
}

/// Whether any header with this name contains `needle` (already lowercased).
///
/// Every header of that name is checked, not just the first: a message with two
/// `Received` lines must match on either. Both the decoded value and the raw
/// bytes are considered, so a client can find either the human-readable text of
/// an encoded-word header or its literal on-the-wire form.
fn header_contains(message: &Message, name: &str, needle: &str) -> bool {
    message
        .parsed
        .headers
        .iter()
        .filter(|header| header.name_matches(name))
        .any(|header| value_contains(header, needle))
}

/// Whether any header at all contains `needle`.
fn any_header_contains(message: &Message, needle: &str) -> bool {
    message
        .parsed
        .headers
        .iter()
        .any(|header| value_contains(header, needle))
}

/// Whether a header's decoded or raw value contains `needle`.
fn value_contains(header: &crate::Header, needle: &str) -> bool {
    if header.decoded_value.to_lowercase().contains(needle) {
        return true;
    }
    // The raw bytes may not be UTF-8; a lossy read is right here because the
    // comparison is a convenience match, never a byte-exactness guarantee.
    String::from_utf8_lossy(&header.raw_value)
        .to_lowercase()
        .contains(needle)
}

/// Whether the message body contains `needle` (already lowercased).
///
/// Searches each non-multipart part's *decoded* text, so a match is found in a
/// quoted-printable or base64 text part as a reader would see it rather than as
/// it appears on the wire. A part that does not decode as text is skipped: a
/// substring match against binary is not meaningful.
fn body_contains(message: &Message, needle: &str) -> bool {
    message
        .parsed
        .parts
        .iter()
        .filter(|part| !part.is_multipart())
        .any(|part| {
            let decoded = message.part_body_decoded(&part.path);
            decoded.is_some_and(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .to_lowercase()
                    .contains(needle)
            })
        })
}

/// Number of microseconds in one day.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Whether a date key matches.
///
/// RFC 9051 §6.4.4 compares *dates*, not instants: `ON 5-Sep-2026` matches
/// anything that day, and `BEFORE` excludes the day itself. Both sides are
/// therefore truncated to a UTC day boundary before comparison, which is what
/// keeps a message received at 23:59 from falling into the next day.
fn matches_date(
    message: Option<&Message>,
    entry: &MessageEntry,
    field: DateField,
    comparison: DateComparison,
    value: Timestamp,
) -> bool {
    let subject = match field {
        DateField::Internal => Some(entry.internal_date),
        DateField::Sent => match message.and_then(sent_date) {
            Some(sent) => Some(sent),
            // No parseable `Date` header: the key cannot be satisfied, and a
            // vacuous match would put unrelated mail in the result.
            None => return false,
        },
    };
    let Some(subject) = subject else {
        return false;
    };

    let subject_day = day_of(subject);
    let target_day = day_of(value);

    match comparison {
        DateComparison::Before => subject_day < target_day,
        DateComparison::On => subject_day == target_day,
        DateComparison::Since => subject_day >= target_day,
    }
}

/// The UTC day a timestamp falls in, as a day number.
///
/// Uses floor division so pre-epoch timestamps land in the right day rather
/// than rounding toward zero.
fn day_of(timestamp: Timestamp) -> i64 {
    timestamp.as_micros().div_euclid(MICROS_PER_DAY)
}

/// The message's `Date` header as a timestamp, if it parses.
fn sent_date(message: &Message) -> Option<Timestamp> {
    let header = message
        .parsed
        .headers
        .iter()
        .find(|header| header.name_matches("date"))?;
    parse_rfc5322_date(&header.decoded_value)
}

/// Parses RFC 9051 §9 `date-text`: `d-Mon-yyyy`, e.g. `5-Sep-2026`.
///
/// Lives here rather than in the IMAP adapter because it produces a
/// [`Timestamp`], and the calendar arithmetic belongs beside the date matching
/// it feeds. The day may be one or two digits and the month is the three-letter
/// English abbreviation, case-insensitively. The result is midnight UTC on that
/// day; only the calendar day is significant to a date key.
#[must_use]
pub fn parse_date_text(value: &str) -> Option<Timestamp> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];

    let mut parts = value.split('-');
    let day: u8 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?.to_ascii_lowercase();
    let year: i32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    let month_index = MONTHS.iter().position(|m| *m == month_name)?;
    let month = time::Month::try_from(u8::try_from(month_index + 1).ok()?).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    let micros = date.midnight().assume_utc().unix_timestamp_nanos() / 1_000;
    Some(Timestamp::from_micros(i64::try_from(micros).ok()?))
}

/// Parses an RFC 5322 `Date` header into a [`Timestamp`].
///
/// Deliberately narrow: it reads the numeric day, the month name, the year and
/// the optional time, which is what every real `Date` header carries. Anything
/// it cannot read yields `None`, and the date key then does not match — better
/// than guessing a date and returning the wrong messages.
fn parse_rfc5322_date(value: &str) -> Option<Timestamp> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];

    // Strip an optional leading day-of-week: `Tue, 01 Sep 2026 ...`.
    let rest = value.split_once(',').map_or(value, |(_, rest)| rest);
    let mut fields = rest.split_whitespace();

    let day: u8 = fields.next()?.parse().ok()?;
    let month_name = fields.next()?.to_ascii_lowercase();
    let month = MONTHS.iter().position(|m| month_name.starts_with(m))? + 1;
    let year: i32 = fields.next()?.parse().ok()?;

    // The time is optional; a date-only header is treated as midnight UTC.
    let (hour, minute, second) = match fields.next() {
        Some(time) => {
            let mut parts = time.split(':');
            let hour: u8 = parts.next()?.parse().ok()?;
            let minute: u8 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            let second: u8 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            (hour, minute, second)
        }
        None => (0, 0, 0),
    };

    // The zone offset is deliberately ignored. Date keys compare calendar
    // days, and a header's own offset shifts the instant by at most a day's
    // fraction; honouring it would need full zone parsing for a distinction
    // RFC 9051 §6.4.4 does not draw.
    let month = u8::try_from(month).ok()?;
    let date =
        time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    let time_of_day = time::Time::from_hms(hour, minute, second).ok()?;
    let datetime = date.with_time(time_of_day).assume_utc();

    let micros = datetime.unix_timestamp_nanos() / 1_000;
    Some(Timestamp::from_micros(i64::try_from(micros).ok()?))
}

/// Parses a query string in developer filter syntax into a [`SearchQuery`].
///
/// Supported tokens:
/// - `from:<val>` -> `Contains { field: TextField::From, value }`
/// - `to:<val>` -> `Contains { field: TextField::To, value }`
/// - `cc:<val>` -> `Contains { field: TextField::Cc, value }`
/// - `bcc:<val>` -> `Contains { field: TextField::Bcc, value }`
/// - `subject:<val>` -> `Contains { field: TextField::Subject, value }`
/// - `body:<val>` -> `Contains { field: TextField::Body, value }`
/// - `text:<val>` -> `Contains { field: TextField::Text, value }`
/// - `is:read` / `is:seen` -> `HasFlag(FlagKey::Seen)`
/// - `is:unread` / `is:unseen` -> `LacksFlag(FlagKey::Seen)`
/// - `is:flagged` -> `HasFlag(FlagKey::Flagged)`
/// - `is:unflagged` -> `LacksFlag(FlagKey::Flagged)`
/// - `is:draft` -> `HasFlag(FlagKey::Draft)`
/// - `is:undraft` -> `LacksFlag(FlagKey::Draft)`
/// - `is:answered` -> `HasFlag(FlagKey::Answered)`
/// - `is:unanswered` -> `LacksFlag(FlagKey::Answered)`
/// - `is:deleted` -> `HasFlag(FlagKey::Deleted)`
/// - `is:undeleted` -> `LacksFlag(FlagKey::Deleted)`
/// - `has:attachment` / `has:attachments` -> `HasAttachment`
/// - `larger:<bytes|k|m|g>` -> `Larger(bytes)`
/// - `smaller:<bytes|k|m|g>` -> `Smaller(bytes)`
/// - Plain words / quoted phrases -> `Contains { field: TextField::Text, value }`
///
/// Multiple tokens are combined with [`SearchQuery::And`].
#[must_use]
pub fn parse_filter_query(input: &str) -> SearchQuery {
    let tokens = tokenize_filter(input);
    if tokens.is_empty() {
        return SearchQuery::All;
    }

    let mut terms = Vec::new();
    for token in tokens {
        if let Some((prefix, val)) = token.split_once(':') {
            let key = prefix.to_ascii_lowercase();
            let clean_val = val.trim_matches('"');
            match key.as_str() {
                "from" => terms.push(SearchQuery::Contains {
                    field: TextField::From,
                    value: clean_val.to_string(),
                }),
                "to" => terms.push(SearchQuery::Contains {
                    field: TextField::To,
                    value: clean_val.to_string(),
                }),
                "cc" => terms.push(SearchQuery::Contains {
                    field: TextField::Cc,
                    value: clean_val.to_string(),
                }),
                "bcc" => terms.push(SearchQuery::Contains {
                    field: TextField::Bcc,
                    value: clean_val.to_string(),
                }),
                "subject" => terms.push(SearchQuery::Contains {
                    field: TextField::Subject,
                    value: clean_val.to_string(),
                }),
                "body" => terms.push(SearchQuery::Contains {
                    field: TextField::Body,
                    value: clean_val.to_string(),
                }),
                "text" => terms.push(SearchQuery::Contains {
                    field: TextField::Text,
                    value: clean_val.to_string(),
                }),
                "is" => match clean_val.to_ascii_lowercase().as_str() {
                    "read" | "seen" => terms.push(SearchQuery::HasFlag(FlagKey::Seen)),
                    "unread" | "unseen" => terms.push(SearchQuery::LacksFlag(FlagKey::Seen)),
                    "flagged" => terms.push(SearchQuery::HasFlag(FlagKey::Flagged)),
                    "unflagged" => terms.push(SearchQuery::LacksFlag(FlagKey::Flagged)),
                    "draft" => terms.push(SearchQuery::HasFlag(FlagKey::Draft)),
                    "undraft" => terms.push(SearchQuery::LacksFlag(FlagKey::Draft)),
                    "answered" => terms.push(SearchQuery::HasFlag(FlagKey::Answered)),
                    "unanswered" => terms.push(SearchQuery::LacksFlag(FlagKey::Answered)),
                    "deleted" => terms.push(SearchQuery::HasFlag(FlagKey::Deleted)),
                    "undeleted" => terms.push(SearchQuery::LacksFlag(FlagKey::Deleted)),
                    _ => terms.push(SearchQuery::HasKeyword(clean_val.to_string())),
                },
                "has" => match clean_val.to_ascii_lowercase().as_str() {
                    "attachment" | "attachments" => terms.push(SearchQuery::HasAttachment),
                    _ => terms.push(SearchQuery::Contains {
                        field: TextField::Text,
                        value: token,
                    }),
                },
                "larger" => {
                    if let Some(bytes) = parse_size_spec(clean_val) {
                        terms.push(SearchQuery::Larger(bytes));
                    }
                }
                "smaller" => {
                    if let Some(bytes) = parse_size_spec(clean_val) {
                        terms.push(SearchQuery::Smaller(bytes));
                    }
                }
                _ => {
                    terms.push(SearchQuery::Contains {
                        field: TextField::Text,
                        value: token,
                    });
                }
            }
        } else {
            let clean = token.trim_matches('"');
            if !clean.is_empty() {
                terms.push(SearchQuery::Contains {
                    field: TextField::Text,
                    value: clean.to_string(),
                });
            }
        }
    }

    if terms.is_empty() {
        SearchQuery::All
    } else if terms.len() == 1 {
        terms.remove(0)
    } else {
        SearchQuery::And(terms)
    }
}

fn tokenize_filter(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars = input.chars();
    let mut cur = String::new();
    let mut in_quotes = false;

    for c in chars {
        if c == '"' {
            in_quotes = !in_quotes;
            cur.push(c);
        } else if c.is_whitespace() && !in_quotes {
            if !cur.is_empty() {
                tokens.push(cur.trim().to_string());
                cur.clear();
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur.trim().to_string());
    }

    tokens
}

fn parse_size_spec(val: &str) -> Option<u64> {
    let lower = val.to_ascii_lowercase();
    let (num_part, multiplier) = if let Some(stripped) =
        lower.strip_suffix("gb").or_else(|| lower.strip_suffix('g'))
    {
        (stripped, 1024 * 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix("mb").or_else(|| lower.strip_suffix('m')) {
        (stripped, 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix("kb").or_else(|| lower.strip_suffix('k')) {
        (stripped, 1024)
    } else if let Some(stripped) = lower.strip_suffix('b') {
        (stripped, 1)
    } else {
        (lower.as_str(), 1)
    };

    num_part
        .trim()
        .parse::<u64>()
        .ok()
        .map(|n| n.saturating_mul(multiplier))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{EntryId, MailboxId, MessageId, RawMessage, Uid, parser};
    use std::collections::BTreeSet;

    /// A message built from raw bytes, parsed by the production parser.
    fn message(raw: &[u8]) -> Message {
        Message {
            id: MessageId::new(),
            raw: RawMessage::new(raw.to_vec()),
            parsed: parser::parse(raw),
            envelope: crate::Envelope::default(),
            source: crate::DeliverySource::Smtp,
            received_at: Timestamp::now(),
        }
    }

    /// A placement with the given flags.
    fn entry_with(uid: u32, flags: MessageFlags) -> MessageEntry {
        MessageEntry {
            id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: MailboxId::new(),
            uid: Uid::new(uid).expect("non-zero UID"),
            flags,
            internal_date: Timestamp::from_micros(1_788_000_000_000_000),
            added_at: Timestamp::now(),
            mod_seq: crate::ModSeq::MIN,
        }
    }

    /// A plain placement with no flags.
    fn entry(uid: u32) -> MessageEntry {
        entry_with(uid, MessageFlags::empty())
    }

    /// A placement stamped at an exact modification sequence.
    fn entry_at_mod_seq(uid: u32, mod_seq: u64) -> MessageEntry {
        MessageEntry {
            mod_seq: crate::ModSeq::new(mod_seq).expect("non-zero modseq"),
            ..entry(uid)
        }
    }

    /// A representative message with headers and a text body.
    fn sample() -> Message {
        message(
            b"From: Alice <alice@example.test>\r\n\
To: Bob <bob@example.test>\r\n\
Cc: carol@example.test\r\n\
Subject: Quarterly report\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
X-Custom: marker-value\r\n\
\r\n\
The body mentions pineapple and nothing else.\r\n",
        )
    }

    // --- trivial ----------------------------------------------------------

    #[test]
    fn all_matches_everything_and_none_matches_nothing() {
        let message = sample();
        let entry = entry(1);
        assert!(SearchQuery::All.matches(Some(&message), &entry));
        assert!(!SearchQuery::None.matches(Some(&message), &entry));
        // ALL needs no message at all.
        assert!(SearchQuery::All.matches(None, &entry));
    }

    // --- flags ------------------------------------------------------------

    #[test]
    fn flag_keys_read_the_placements_flags() {
        let seen = entry_with(1, MessageFlags::from_system(SystemFlags::SEEN));
        let unseen = entry(2);

        assert!(SearchQuery::HasFlag(FlagKey::Seen).matches(None, &seen));
        assert!(!SearchQuery::HasFlag(FlagKey::Seen).matches(None, &unseen));
        assert!(SearchQuery::LacksFlag(FlagKey::Seen).matches(None, &unseen));
        assert!(!SearchQuery::LacksFlag(FlagKey::Seen).matches(None, &seen));
    }

    #[test]
    fn every_flag_key_maps_to_its_own_flag() {
        // The mutation guard: no two keys may test the same flag.
        let keys = [
            (FlagKey::Seen, SystemFlags::SEEN),
            (FlagKey::Answered, SystemFlags::ANSWERED),
            (FlagKey::Flagged, SystemFlags::FLAGGED),
            (FlagKey::Deleted, SystemFlags::DELETED),
            (FlagKey::Draft, SystemFlags::DRAFT),
        ];
        for (key, expected) in keys {
            assert_eq!(key.flag(), expected, "{key:?}");
            // Set only that flag, and only that key must match.
            let placement = entry_with(1, MessageFlags::from_system(expected));
            for (other, _) in keys {
                let matched = SearchQuery::HasFlag(other).matches(None, &placement);
                assert_eq!(matched, other == key, "{other:?} against {key:?}");
            }
        }
    }

    #[test]
    fn has_and_lacks_are_exact_complements() {
        for flags in [
            MessageFlags::empty(),
            MessageFlags::from_system(SystemFlags::SEEN),
            MessageFlags::from_system(SystemFlags::ALL),
        ] {
            let placement = entry_with(1, flags);
            for key in [FlagKey::Seen, FlagKey::Deleted, FlagKey::Draft] {
                let has = SearchQuery::HasFlag(key).matches(None, &placement);
                let lacks = SearchQuery::LacksFlag(key).matches(None, &placement);
                assert_ne!(has, lacks, "{key:?} must be one or the other");
            }
        }
    }

    #[test]
    fn keyword_keys_match_case_insensitively() {
        let mut keywords = BTreeSet::new();
        keywords.insert(crate::Keyword::new("$Junk").expect("valid"));
        let placement = entry_with(1, MessageFlags::new(SystemFlags::NONE, keywords));

        assert!(SearchQuery::HasKeyword(String::from("$Junk")).matches(None, &placement));
        assert!(SearchQuery::HasKeyword(String::from("$junk")).matches(None, &placement));
        assert!(!SearchQuery::HasKeyword(String::from("$Other")).matches(None, &placement));
        assert!(SearchQuery::LacksKeyword(String::from("$Other")).matches(None, &placement));
    }

    // --- headers ----------------------------------------------------------

    #[test]
    fn header_keys_match_substrings_case_insensitively() {
        let message = sample();
        let placement = entry(1);

        for (field, needle) in [
            (TextField::From, "alice"),
            (TextField::From, "ALICE"),
            (TextField::To, "bob@example.test"),
            (TextField::Cc, "carol"),
            (TextField::Subject, "quarterly"),
            (TextField::Subject, "REPORT"),
        ] {
            assert!(
                SearchQuery::Contains {
                    field: field.clone(),
                    value: String::from(needle),
                }
                .matches(Some(&message), &placement),
                "{field:?} should contain {needle}"
            );
        }
    }

    #[test]
    fn a_header_key_does_not_match_another_header() {
        let message = sample();
        let placement = entry(1);
        // "alice" is in From, not in To.
        assert!(
            !SearchQuery::Contains {
                field: TextField::To,
                value: String::from("alice"),
            }
            .matches(Some(&message), &placement)
        );
    }

    #[test]
    fn a_named_header_key_finds_arbitrary_headers() {
        let message = sample();
        let placement = entry(1);
        assert!(
            SearchQuery::Contains {
                field: TextField::Header(String::from("X-Custom")),
                value: String::from("marker"),
            }
            .matches(Some(&message), &placement)
        );
        // Name matching is case-insensitive.
        assert!(
            SearchQuery::Contains {
                field: TextField::Header(String::from("x-custom")),
                value: String::from("marker-value"),
            }
            .matches(Some(&message), &placement)
        );
        // A header that is not present matches nothing.
        assert!(
            !SearchQuery::Contains {
                field: TextField::Header(String::from("X-Absent")),
                value: String::from("anything"),
            }
            .matches(Some(&message), &placement)
        );
    }

    #[test]
    fn an_empty_search_string_matches_any_message_with_the_field() {
        // RFC 9051 §6.4.4.
        let message = sample();
        let placement = entry(1);
        assert!(
            SearchQuery::Contains {
                field: TextField::Subject,
                value: String::new(),
            }
            .matches(Some(&message), &placement)
        );
        assert!(
            !SearchQuery::Contains {
                field: TextField::Header(String::from("X-Absent")),
                value: String::new(),
            }
            .matches(Some(&message), &placement)
        );
    }

    #[test]
    fn duplicate_headers_are_all_searched() {
        let message = message(
            b"Received: from first.example\r\n\
Received: from second.example\r\n\
Subject: dup\r\n\
\r\n\
body\r\n",
        );
        let placement = entry(1);
        for needle in ["first.example", "second.example"] {
            assert!(
                SearchQuery::Contains {
                    field: TextField::Header(String::from("Received")),
                    value: String::from(needle),
                }
                .matches(Some(&message), &placement),
                "{needle} must be found"
            );
        }
    }

    // --- body and text ----------------------------------------------------

    #[test]
    fn body_searches_the_message_body_only() {
        let message = sample();
        let placement = entry(1);

        assert!(
            SearchQuery::Contains {
                field: TextField::Body,
                value: String::from("pineapple"),
            }
            .matches(Some(&message), &placement)
        );

        // A subject word is not in the body.
        assert!(
            !SearchQuery::Contains {
                field: TextField::Body,
                value: String::from("Quarterly"),
            }
            .matches(Some(&message), &placement)
        );
    }

    #[test]
    fn text_searches_headers_and_body() {
        let message = sample();
        let placement = entry(1);

        for needle in ["pineapple", "Quarterly", "alice"] {
            assert!(
                SearchQuery::Contains {
                    field: TextField::Text,
                    value: String::from(needle),
                }
                .matches(Some(&message), &placement),
                "TEXT must find {needle}"
            );
        }
        assert!(
            !SearchQuery::Contains {
                field: TextField::Text,
                value: String::from("absent-token"),
            }
            .matches(Some(&message), &placement)
        );
    }

    #[test]
    fn body_search_sees_through_transfer_encoding() {
        // "hello encoded world" as quoted-printable, so the wire bytes differ
        // from what a reader sees.
        let raw = b"Subject: qp\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
hello=20encoded=20world\r\n";
        let message = message(raw);
        let placement = entry(1);

        assert!(
            SearchQuery::Contains {
                field: TextField::Body,
                value: String::from("encoded world"),
            }
            .matches(Some(&message), &placement),
            "the decoded text must be searched"
        );
    }

    // --- size -------------------------------------------------------------

    #[test]
    fn size_keys_are_strict_comparisons() {
        let message = sample();
        let placement = entry(1);
        let size = message.size();

        assert!(SearchQuery::Larger(size - 1).matches(Some(&message), &placement));
        assert!(!SearchQuery::Larger(size).matches(Some(&message), &placement));
        assert!(SearchQuery::Smaller(size + 1).matches(Some(&message), &placement));
        assert!(!SearchQuery::Smaller(size).matches(Some(&message), &placement));
    }

    #[test]
    fn size_keys_measure_octets() {
        let message = message("Subject: s\r\n\r\néé\r\n".as_bytes());
        let placement = entry(1);
        let size = message.size();
        assert_eq!(size, message.raw.bytes().len() as u64);
        assert!(SearchQuery::Smaller(size + 1).matches(Some(&message), &placement));
    }

    // --- UID --------------------------------------------------------------

    #[test]
    fn uid_ranges_are_inclusive() {
        let query = SearchQuery::UidRange { low: 4, high: 9 };
        for (uid, expected) in [(3, false), (4, true), (6, true), (9, true), (10, false)] {
            assert_eq!(query.matches(None, &entry(uid)), expected, "UID {uid}");
        }
    }

    #[test]
    fn a_single_uid_is_a_degenerate_range() {
        let query = SearchQuery::UidRange { low: 7, high: 7 };
        assert!(query.matches(None, &entry(7)));
        assert!(!query.matches(None, &entry(6)));
        assert!(!query.matches(None, &entry(8)));
    }

    // --- dates ------------------------------------------------------------

    #[test]
    fn internal_date_keys_compare_calendar_days() {
        // The placement's internal date is 1_788_000_000_000_000 µs.
        let placement = entry(1);
        let same_day = placement.internal_date;
        let next_day = Timestamp::from_micros(same_day.as_micros() + MICROS_PER_DAY);
        let previous_day = Timestamp::from_micros(same_day.as_micros() - MICROS_PER_DAY);

        let on = |value| SearchQuery::Date {
            field: DateField::Internal,
            comparison: DateComparison::On,
            value,
        };
        assert!(on(same_day).matches(None, &placement));
        assert!(!on(next_day).matches(None, &placement));

        let before = |value| SearchQuery::Date {
            field: DateField::Internal,
            comparison: DateComparison::Before,
            value,
        };
        assert!(before(next_day).matches(None, &placement));
        assert!(
            !before(same_day).matches(None, &placement),
            "BEFORE excludes the day"
        );

        let since = |value| SearchQuery::Date {
            field: DateField::Internal,
            comparison: DateComparison::Since,
            value,
        };
        assert!(
            since(same_day).matches(None, &placement),
            "SINCE includes the day"
        );
        assert!(since(previous_day).matches(None, &placement));
        assert!(!since(next_day).matches(None, &placement));
    }

    #[test]
    fn a_time_within_the_day_does_not_shift_it() {
        // 23:59 must stay in its own day, not roll into the next.
        let placement = entry(1);
        let day_start = Timestamp::from_micros(
            placement
                .internal_date
                .as_micros()
                .div_euclid(MICROS_PER_DAY)
                * MICROS_PER_DAY,
        );
        let late = Timestamp::from_micros(day_start.as_micros() + MICROS_PER_DAY - 1);
        let query = SearchQuery::Date {
            field: DateField::Internal,
            comparison: DateComparison::On,
            value: late,
        };
        assert!(query.matches(None, &placement));
    }

    #[test]
    fn sent_date_keys_read_the_date_header() {
        let message = sample(); // Date: Tue, 01 Sep 2026 12:00:00 +0000
        let placement = entry(1);

        let sent = parse_rfc5322_date("Tue, 01 Sep 2026 12:00:00 +0000").expect("parses");
        let query = SearchQuery::Date {
            field: DateField::Sent,
            comparison: DateComparison::On,
            value: sent,
        };
        assert!(query.matches(Some(&message), &placement));
    }

    #[test]
    fn a_message_without_a_date_header_matches_no_sent_key() {
        let message = message(b"Subject: no date\r\n\r\nbody\r\n");
        let placement = entry(1);
        for comparison in [
            DateComparison::Before,
            DateComparison::On,
            DateComparison::Since,
        ] {
            let query = SearchQuery::Date {
                field: DateField::Sent,
                comparison,
                value: Timestamp::now(),
            };
            assert!(
                !query.matches(Some(&message), &placement),
                "{comparison:?} must not match vacuously"
            );
        }
    }

    #[test]
    fn date_text_parses_the_imap_form() {
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
            "5-Sep-2026-extra",
        ] {
            assert!(parse_date_text(bad).is_none(), "{bad} should not parse");
        }
    }

    #[test]
    fn date_text_yields_midnight_utc() {
        let day = parse_date_text("5-Sep-2026").expect("parses");
        assert_eq!(
            day.as_micros() % MICROS_PER_DAY,
            0,
            "must be a day boundary"
        );
    }

    #[test]
    fn date_parsing_handles_real_headers_and_rejects_junk() {
        for good in [
            "Tue, 01 Sep 2026 12:00:00 +0000",
            "1 Sep 2026 12:00:00 +0000",
            "Wed, 15 Jun 2020 08:30:00 GMT",
            "01 Jan 1970 00:00:00 +0000",
            "Mon, 5 Sep 2026",
        ] {
            assert!(parse_rfc5322_date(good).is_some(), "{good} should parse");
        }
        for bad in ["", "not a date", "Tue,", "32 Xxx 2026", "Sep 2026"] {
            assert!(parse_rfc5322_date(bad).is_none(), "{bad} should not parse");
        }
    }

    // --- composition ------------------------------------------------------

    #[test]
    fn and_requires_every_child() {
        let message = sample();
        let placement = entry_with(1, MessageFlags::from_system(SystemFlags::SEEN));

        let both = SearchQuery::And(vec![
            SearchQuery::HasFlag(FlagKey::Seen),
            SearchQuery::Contains {
                field: TextField::Subject,
                value: String::from("Quarterly"),
            },
        ]);
        assert!(both.matches(Some(&message), &placement));

        let one_fails = SearchQuery::And(vec![
            SearchQuery::HasFlag(FlagKey::Seen),
            SearchQuery::Contains {
                field: TextField::Subject,
                value: String::from("absent"),
            },
        ]);
        assert!(!one_fails.matches(Some(&message), &placement));
    }

    #[test]
    fn or_requires_at_least_one_child() {
        let message = sample();
        let placement = entry(1); // unseen

        let query = SearchQuery::Or(vec![
            SearchQuery::HasFlag(FlagKey::Seen), // false
            SearchQuery::Contains {
                field: TextField::Subject,
                value: String::from("Quarterly"),
            }, // true
        ]);
        assert!(query.matches(Some(&message), &placement));

        let neither = SearchQuery::Or(vec![
            SearchQuery::HasFlag(FlagKey::Seen),
            SearchQuery::HasFlag(FlagKey::Draft),
        ]);
        assert!(!neither.matches(Some(&message), &placement));
    }

    #[test]
    fn empty_and_matches_everything_empty_or_matches_nothing() {
        // The identities. An empty AND is the neutral element (everything
        // matches); an empty OR has no satisfying child.
        let placement = entry(1);
        assert!(SearchQuery::And(Vec::new()).matches(None, &placement));
        assert!(!SearchQuery::Or(Vec::new()).matches(None, &placement));
    }

    #[test]
    fn not_inverts_exactly() {
        let placement = entry_with(1, MessageFlags::from_system(SystemFlags::SEEN));
        let inner = SearchQuery::HasFlag(FlagKey::Seen);
        assert!(inner.matches(None, &placement));
        assert!(!SearchQuery::Not(Box::new(inner.clone())).matches(None, &placement));
        // Double negation is the identity.
        assert!(
            SearchQuery::Not(Box::new(SearchQuery::Not(Box::new(inner)))).matches(None, &placement)
        );
    }

    #[test]
    fn nested_composition_evaluates_correctly() {
        // NOT (SEEN OR (DRAFT AND FLAGGED)) against an unseen, flagged message.
        let placement = entry_with(1, MessageFlags::from_system(SystemFlags::FLAGGED));
        let query = SearchQuery::Not(Box::new(SearchQuery::Or(vec![
            SearchQuery::HasFlag(FlagKey::Seen),
            SearchQuery::And(vec![
                SearchQuery::HasFlag(FlagKey::Draft),
                SearchQuery::HasFlag(FlagKey::Flagged),
            ]),
        ])));
        assert!(query.matches(None, &placement), "neither disjunct holds");

        // Now make the inner AND true; the NOT must flip.
        let both = entry_with(
            1,
            MessageFlags::from_system(SystemFlags::DRAFT.union(SystemFlags::FLAGGED)),
        );
        assert!(!query.matches(None, &both));
    }

    // --- message requirement ----------------------------------------------

    #[test]
    fn only_content_keys_need_the_message() {
        assert!(!SearchQuery::All.needs_message());
        assert!(!SearchQuery::HasFlag(FlagKey::Seen).needs_message());
        assert!(!SearchQuery::LacksFlag(FlagKey::Seen).needs_message());
        assert!(!SearchQuery::HasKeyword(String::from("x")).needs_message());
        assert!(!SearchQuery::UidRange { low: 1, high: 2 }.needs_message());
        assert!(
            !SearchQuery::Date {
                field: DateField::Internal,
                comparison: DateComparison::On,
                value: Timestamp::now(),
            }
            .needs_message()
        );

        assert!(SearchQuery::Larger(1).needs_message());
        assert!(SearchQuery::Smaller(1).needs_message());
        assert!(
            SearchQuery::Contains {
                field: TextField::Subject,
                value: String::from("x"),
            }
            .needs_message()
        );
        assert!(
            SearchQuery::Date {
                field: DateField::Sent,
                comparison: DateComparison::On,
                value: Timestamp::now(),
            }
            .needs_message()
        );
    }

    #[test]
    fn the_requirement_propagates_through_composition() {
        let cheap = SearchQuery::HasFlag(FlagKey::Seen);
        let costly = SearchQuery::Contains {
            field: TextField::Body,
            value: String::from("x"),
        };

        assert!(!SearchQuery::And(vec![cheap.clone(), cheap.clone()]).needs_message());
        assert!(SearchQuery::And(vec![cheap.clone(), costly.clone()]).needs_message());
        assert!(SearchQuery::Or(vec![cheap.clone(), costly.clone()]).needs_message());
        assert!(SearchQuery::Not(Box::new(costly)).needs_message());
        assert!(!SearchQuery::Not(Box::new(cheap)).needs_message());
    }

    #[test]
    fn a_content_key_does_not_match_when_the_message_is_absent() {
        // A search that could not load the message must not match vacuously.
        let placement = entry(1);
        assert!(
            !SearchQuery::Contains {
                field: TextField::Subject,
                value: String::from("anything"),
            }
            .matches(None, &placement)
        );
        assert!(!SearchQuery::Larger(0).matches(None, &placement));
        assert!(!SearchQuery::Smaller(u64::MAX).matches(None, &placement));
    }

    #[test]
    fn matching_never_panics_on_a_degenerate_message() {
        // An empty message, and one that is all headers with no body.
        for raw in [&b""[..], &b"Subject: only\r\n"[..], &b"\r\n\r\n"[..]] {
            let message = message(raw);
            let placement = entry(1);
            for query in [
                SearchQuery::All,
                SearchQuery::Contains {
                    field: TextField::Body,
                    value: String::from("x"),
                },
                SearchQuery::Contains {
                    field: TextField::Text,
                    value: String::new(),
                },
                SearchQuery::Larger(0),
                SearchQuery::Smaller(u64::MAX),
            ] {
                let _ = query.matches(Some(&message), &placement);
            }
        }
    }

    #[test]
    fn a_deeply_nested_query_terminates() {
        // Depth is bounded by the caller (the IMAP parser caps nesting), but
        // evaluation must not blow up for a legal tree.
        let mut query = SearchQuery::All;
        for _ in 0..64 {
            query = SearchQuery::Not(Box::new(query));
        }
        let _ = query.matches(None, &entry(1));
    }

    #[test]
    fn unparsed_messages_are_still_searchable_by_flag() {
        // A message whose MIME could not be parsed must still be findable by
        // its placement's flags — the search must not depend on parse success.
        let message = message(b"\x00\x01 not a message at all");
        let placement = entry_with(1, MessageFlags::from_system(SystemFlags::SEEN));
        assert!(SearchQuery::HasFlag(FlagKey::Seen).matches(Some(&message), &placement));
        assert!(SearchQuery::All.matches(Some(&message), &placement));
    }

    #[test]
    fn parse_filter_query_handles_keys_and_flags() {
        let q =
            parse_filter_query("from:alice to:bob is:unread has:attachment larger:10k pineapple");
        match q {
            SearchQuery::And(terms) => {
                assert_eq!(terms.len(), 6);
                assert!(terms.contains(&SearchQuery::Contains {
                    field: TextField::From,
                    value: "alice".to_string(),
                }));
                assert!(terms.contains(&SearchQuery::Contains {
                    field: TextField::To,
                    value: "bob".to_string(),
                }));
                assert!(terms.contains(&SearchQuery::LacksFlag(FlagKey::Seen)));
                assert!(terms.contains(&SearchQuery::HasAttachment));
                assert!(terms.contains(&SearchQuery::Larger(10 * 1024)));
                assert!(terms.contains(&SearchQuery::Contains {
                    field: TextField::Text,
                    value: "pineapple".to_string(),
                }));
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn parse_filter_query_handles_quotes_and_empty() {
        assert_eq!(parse_filter_query(""), SearchQuery::All);
        assert_eq!(parse_filter_query("   "), SearchQuery::All);
        let q = parse_filter_query("subject:\"Quarterly Report\"");
        assert_eq!(
            q,
            SearchQuery::Contains {
                field: TextField::Subject,
                value: "Quarterly Report".to_string(),
            }
        );
    }
    #[test]
    fn modseq_search_includes_the_threshold_itself() {
        // RFC 7162 §3.1.5 defines `MODSEQ n` as "at or above n". The boundary
        // is the whole content of the rule: a client saved `n` as the highest
        // value it had *seen*, so an exclusive comparison would silently hide
        // the revision it is actually asking about.
        //
        // Constructed at an exact value rather than driven through storage,
        // because the allocator never hands out the floor — so an end-to-end
        // test cannot place an entry *on* the boundary and cannot distinguish
        // `>=` from `>` there.
        let query = SearchQuery::ModSeqAtLeast(crate::ModSeq::new(5).expect("non-zero"));

        assert!(
            query.matches(None, &entry_at_mod_seq(1, 5)),
            "an entry exactly at the threshold must match"
        );
        assert!(
            query.matches(None, &entry_at_mod_seq(2, 6)),
            "an entry above the threshold must match"
        );
        assert!(
            !query.matches(None, &entry_at_mod_seq(3, 4)),
            "an entry below the threshold must not match"
        );
    }

    #[test]
    fn modseq_search_needs_no_message() {
        // The sequence lives on the placement, so a MODSEQ search must not
        // force a message load — and must still answer when none is available.
        let query = SearchQuery::ModSeqAtLeast(crate::ModSeq::MIN);
        assert!(!query.needs_message());
        assert!(
            query.matches(None, &entry_at_mod_seq(1, 1)),
            "the answer must not depend on having the message"
        );
    }
}

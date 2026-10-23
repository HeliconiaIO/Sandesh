//! Sequence sets: the `1,3:5,7:*` identifier lists every message command takes.
//!
//! RFC 9051 §9 defines the grammar:
//!
//! ```text
//! sequence-set = (seq-number / seq-range) *("," sequence-set)
//! seq-range    = seq-number ":" seq-number
//! seq-number   = nz-number / "*"
//! ```
//!
//! # What this type is, and is not
//!
//! A [`SequenceSet`] is a parsed *request*: a list of numbers and ranges, with
//! `*` still symbolic. It deliberately does **not** know which messages exist.
//! Resolving it against a mailbox is [`SequenceSet::resolve`], which takes the
//! [`SequenceMap`] — the canonical ordering (ADR-044) — and never re-derives it.
//!
//! Keeping the two apart matters because the same syntax means different things
//! in the two modes RFC 9051 §6.4.8 defines:
//!
//! ```text
//! FETCH 2:4      → sequence numbers → positions in the mailbox
//! UID FETCH 2:4  → UIDs            → stable identities, sparse
//! ```
//!
//! `*` differs accordingly: it is the largest sequence number in sequence mode
//! and the largest *UID in use* in UID mode.
//!
//! # Why `nz-number` matters
//!
//! Zero is not a sequence number and not a UID — [`sandesh_core::Uid`] cannot
//! even represent it. A set containing `0` is a syntax error, not an empty
//! result, and conflating the two is how a client's off-by-one becomes a
//! server's wrong answer.

use core::fmt;

use sandesh_core::{MessageEntry, Uid};

use crate::mailbox::SequenceMap;

/// The largest number accepted in a sequence set.
///
/// RFC 9051 §9 `number` is a 32-bit value, and UIDs are `u32`. Parsing into
/// `u32` and rejecting overflow means a hostile `99999999999999999999` is a
/// clean syntax error rather than a wrapped, plausible-looking number — the
/// bound is enforced by the parse itself, and named here so it is documented.
pub const MAX_NUMBER: u32 = u32::MAX;

/// How many comma-separated items one set may contain.
///
/// RFC 9051 sets no limit. This bounds the work a single command can request,
/// so a client cannot make the server allocate without limit from one line —
/// the command line itself is already length-bounded, but a bound stated here
/// keeps the intent explicit (phase brief §21).
pub const MAX_SET_ITEMS: usize = 1024;

/// Why a sequence set could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceError {
    /// The set was empty.
    Empty,
    /// A number was `0`, which RFC 9051 §9 `nz-number` forbids.
    ZeroNotAllowed,
    /// A number was not a number, or overflowed 32 bits.
    NotANumber,
    /// A range had something other than two endpoints.
    MalformedRange,
    /// More than [`MAX_SET_ITEMS`] items.
    TooManyItems,
}

impl SequenceError {
    /// The text accompanying the `BAD` response.
    ///
    /// Describes the syntax problem only. Nothing here reveals how many
    /// messages exist or what UIDs are in use — an unparseable set is rejected
    /// before the mailbox is consulted at all.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Empty => "sequence set is empty",
            Self::ZeroNotAllowed => "0 is not a valid message number",
            Self::NotANumber => "sequence set contains a malformed number",
            Self::MalformedRange => "sequence set contains a malformed range",
            Self::TooManyItems => "sequence set has too many items",
        }
    }
}

impl fmt::Display for SequenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// One endpoint in a sequence set: a number, or `*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    /// An explicit `nz-number`.
    Number(u32),
    /// `*` — the largest number in use.
    Star,
}

impl Endpoint {
    /// Parses `nz-number / "*"`.
    fn parse(text: &str) -> Result<Self, SequenceError> {
        if text == "*" {
            return Ok(Self::Star);
        }
        if text.is_empty() {
            return Err(SequenceError::NotANumber);
        }
        // Reject a leading sign or whitespace explicitly: `u32::from_str`
        // accepts neither, but being explicit documents that `+3` and ` 3`
        // are syntax errors rather than accidental successes.
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SequenceError::NotANumber);
        }
        // Parsing into `u32` is itself the bound (see `MAX_NUMBER`): a number
        // too large for 32 bits is a syntax error, never a wrapped small one.
        let value: u32 = text.parse().map_err(|_| SequenceError::NotANumber)?;
        if value == 0 {
            return Err(SequenceError::ZeroNotAllowed);
        }
        Ok(Self::Number(value))
    }

    /// The concrete value, given what `*` currently means.
    ///
    /// `largest` is the largest sequence number, or the largest UID in use,
    /// depending on the mode. When the mailbox is empty there is no such value
    /// and `*` resolves to nothing.
    const fn value(self, largest: Option<u32>) -> Option<u32> {
        match self {
            Self::Number(n) => Some(n),
            Self::Star => largest,
        }
    }
}

/// One item: a single number, or an inclusive range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Single(Endpoint),
    Range(Endpoint, Endpoint),
}

/// A parsed sequence set, with `*` still symbolic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceSet {
    items: Vec<Item>,
}

impl SequenceSet {
    /// Builds the smallest set that names exactly `numbers`.
    ///
    /// This is the `ALL` return option of `ESEARCH` (RFC 9051 §7.3.4): a
    /// result list rendered as a sequence set rather than as individual
    /// numbers, so `1 2 3 4 9` goes out as `1:4,9`.
    ///
    /// It lives here, beside [`Self::parse`] and [`fmt::Display`], because
    /// this module owns what a sequence set *is*. Coalescing runs elsewhere
    /// would be a second, subtly different notion of the same wire form —
    /// and the two would eventually disagree about an edge like a singleton
    /// run, which must render `5` and never `5:5`.
    ///
    /// Input is sorted and deduplicated first, so a caller may pass results in
    /// any order without changing the output. An empty input gives an empty
    /// set, which [`Self::is_empty`] reports and which a caller must not send:
    /// RFC 9051 §7.3.4 omits the `ALL` key entirely when nothing matched,
    /// rather than sending an empty one.
    #[must_use]
    pub fn from_numbers(numbers: &[u32]) -> Self {
        let mut sorted: Vec<u32> = numbers.iter().copied().filter(|n| *n > 0).collect();
        sorted.sort_unstable();
        sorted.dedup();

        let mut items: Vec<Item> = Vec::new();
        let mut index = 0;
        while index < sorted.len() {
            let start = sorted[index];
            let mut end = start;
            // Extend while the run stays contiguous. `checked_add` rather than
            // `+ 1` so a run ending at `u32::MAX` terminates instead of
            // wrapping to 0 and swallowing the rest of the list.
            while let Some(next) = sorted.get(index + 1)
                && end.checked_add(1) == Some(*next)
            {
                end = *next;
                index += 1;
            }
            index += 1;

            // A single number renders as itself. `5:5` denotes the same set but
            // is not what any client sends, and RFC 9051 §9 prefers the
            // shorter form.
            items.push(if start == end {
                Item::Single(Endpoint::Number(start))
            } else {
                Item::Range(Endpoint::Number(start), Endpoint::Number(end))
            });
        }

        Self { items }
    }

    /// Whether the set names nothing.
    ///
    /// Only a set built by [`Self::from_numbers`] from an empty list can be
    /// empty; [`Self::parse`] rejects an empty input rather than producing one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Parses RFC 9051 §9 `sequence-set`.
    ///
    /// # Ranges are unordered
    ///
    /// RFC 9051 §9 is explicit that `2:4` and `4:2` denote the same set. A
    /// descending range is therefore *legal*, not an error, and is normalised
    /// at resolution time rather than rejected — a client that sorts its own
    /// input differently must still get the right messages.
    ///
    /// # Errors
    ///
    /// Returns [`SequenceError`] for an empty set, a `0` where RFC 9051 §9
    /// requires `nz-number`, a malformed number or range, or more than
    /// [`MAX_SET_ITEMS`] items. The command is answered `BAD` and no message
    /// is touched.
    pub fn parse(text: &str) -> Result<Self, SequenceError> {
        if text.is_empty() {
            return Err(SequenceError::Empty);
        }

        let mut items = Vec::new();
        for piece in text.split(',') {
            if items.len() >= MAX_SET_ITEMS {
                return Err(SequenceError::TooManyItems);
            }
            if piece.is_empty() {
                // A trailing or doubled comma: `1,,3` or `1,`.
                return Err(SequenceError::Empty);
            }

            let mut endpoints = piece.split(':');
            let first = endpoints.next().unwrap_or_default();
            match endpoints.next() {
                None => items.push(Item::Single(Endpoint::parse(first)?)),
                Some(second) => {
                    if endpoints.next().is_some() {
                        // `1:2:3` — three endpoints is not a range.
                        return Err(SequenceError::MalformedRange);
                    }
                    items.push(Item::Range(
                        Endpoint::parse(first)?,
                        Endpoint::parse(second)?,
                    ));
                }
            }
        }

        if items.is_empty() {
            return Err(SequenceError::Empty);
        }
        Ok(Self { items })
    }

    /// Resolves the set against a mailbox, in sequence-number mode.
    ///
    /// Returns `(sequence number, entry)` pairs in ascending sequence order,
    /// each appearing at most once however many times the set names it.
    ///
    /// # Why duplicates collapse and order is imposed
    ///
    /// `FETCH 3,3,1:2` names message 3 twice and out of order. RFC 9051 §6.4.8
    /// does not require the server to echo that shape, and emitting a message
    /// twice would make a client's own bookkeeping wrong. Iterating the mailbox
    /// once, in order, and keeping what the set matches gives a deterministic
    /// answer whose cost does not depend on how redundant the request was.
    ///
    /// # Out-of-range members are silently ignored
    ///
    /// Also RFC 9051 §6.4.8: a sequence number with no message is not an
    /// error. `FETCH 1:*` on an empty mailbox succeeds with no data, and
    /// `FETCH 99` on a three-message mailbox likewise. This is the one place
    /// where "no such message" must *not* become a failure.
    #[must_use]
    pub fn resolve<'map>(&self, map: &'map SequenceMap) -> Vec<(usize, &'map MessageEntry)> {
        if map.is_empty() {
            return Vec::new();
        }
        // In sequence mode `*` is the largest sequence number, which is the
        // message count.
        let largest = u32::try_from(map.len()).ok();

        map.enumerated()
            .filter(|(sequence, _)| {
                u32::try_from(*sequence).is_ok_and(|number| self.contains(number, largest))
            })
            .collect()
    }

    /// Resolves the set against a mailbox, in UID mode.
    ///
    /// Returns `(sequence number, entry)` pairs in ascending sequence order —
    /// the sequence number is still needed, because RFC 9051 §7.4.2 requires
    /// every `FETCH` response to be identified by sequence number even when the
    /// command addressed messages by UID.
    ///
    /// # `*` is the largest UID in use, not `UIDNEXT`
    ///
    /// RFC 9051 §6.4.8: in UID mode `*` means the highest UID currently in the
    /// mailbox. Using `UIDNEXT - 1` instead would name a UID that may never
    /// have existed, and `UID FETCH 1:*` would then behave differently from
    /// `UID FETCH <largest>` on a mailbox whose last message was expunged.
    #[must_use]
    pub fn resolve_uids<'map>(&self, map: &'map SequenceMap) -> Vec<(usize, &'map MessageEntry)> {
        if map.is_empty() {
            return Vec::new();
        }
        let largest = map.iter().map(|entry| entry.uid.get()).max();

        map.enumerated()
            .filter(|(_, entry)| self.contains(entry.uid.get(), largest))
            .collect()
    }

    /// Whether `number` is named by this set, given what `*` means.
    ///
    /// Ranges are normalised so a descending range matches the same numbers as
    /// its ascending twin (RFC 9051 §9).
    fn contains(&self, number: u32, largest: Option<u32>) -> bool {
        self.items.iter().any(|item| match *item {
            Item::Single(endpoint) => endpoint.value(largest) == Some(number),
            Item::Range(from, to) => {
                let (Some(a), Some(b)) = (from.value(largest), to.value(largest)) else {
                    return false;
                };
                let (low, high) = if a <= b { (a, b) } else { (b, a) };
                (low..=high).contains(&number)
            }
        })
    }

    /// The set as a list of inclusive `(low, high)` UID ranges.
    ///
    /// Used to compile a UID set into a [`sandesh_core::SearchQuery`], which
    /// knows nothing of IMAP's sequence-set grammar. Ranges are normalised so a
    /// descending range comes back ascending (RFC 9051 §9 makes `4:2` and `2:4`
    /// the same set).
    ///
    /// `*` becomes `u32::MAX`. In UID terms that is exact rather than a
    /// shortcut: `*` means the largest UID in use, and no UID can exceed
    /// `u32::MAX`, so an open range selects precisely the same messages without
    /// needing to consult the mailbox.
    #[must_use]
    pub fn uid_ranges(&self) -> Vec<(u32, u32)> {
        self.items
            .iter()
            .map(|item| match *item {
                Item::Single(endpoint) => {
                    let value = endpoint.value(Some(u32::MAX)).unwrap_or(u32::MAX);
                    (value, value)
                }
                Item::Range(from, to) => {
                    let a = from.value(Some(u32::MAX)).unwrap_or(u32::MAX);
                    let b = to.value(Some(u32::MAX)).unwrap_or(u32::MAX);
                    if a <= b { (a, b) } else { (b, a) }
                }
            })
            .collect()
    }

    /// Whether the set names `uid` directly, ignoring `*`.
    ///
    /// Used only where a UID must be checked without a mailbox to give `*` a
    /// meaning; `*` never matches here, because its value is unknown.
    #[must_use]
    pub fn contains_uid(&self, uid: Uid) -> bool {
        self.contains(uid.get(), None)
    }
}

impl fmt::Display for SequenceSet {
    /// Renders the set back to its wire form.
    ///
    /// Round-trips the parsed shape rather than the resolved one, so a
    /// descending range stays descending. Used in tests and diagnostics, never
    /// to echo client input back into a response.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<String> = self
            .items
            .iter()
            .map(|item| match *item {
                Item::Single(endpoint) => render_endpoint(endpoint),
                Item::Range(from, to) => {
                    format!("{}:{}", render_endpoint(from), render_endpoint(to))
                }
            })
            .collect();
        f.write_str(&rendered.join(","))
    }
}

/// Renders one endpoint back to its wire form.
fn render_endpoint(endpoint: Endpoint) -> String {
    match endpoint {
        Endpoint::Number(n) => n.to_string(),
        Endpoint::Star => String::from("*"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::SequenceSet as Set;

    /// `from_numbers` renders through `Display`, which is the wire form.
    fn coalesce(numbers: &[u32]) -> String {
        Set::from_numbers(numbers).to_string()
    }

    #[test]
    fn contiguous_runs_become_ranges() {
        assert_eq!(coalesce(&[1, 2, 3, 4]), "1:4");
    }

    #[test]
    fn a_lone_number_is_not_a_range() {
        // `5:5` names the same set, but RFC 9051 §9 prefers the short form and
        // no client sends the long one.
        assert_eq!(coalesce(&[5]), "5");
    }

    #[test]
    fn runs_and_singletons_mix() {
        assert_eq!(coalesce(&[1, 2, 3, 4, 9]), "1:4,9");
        assert_eq!(coalesce(&[2, 5, 6, 7, 11, 12]), "2,5:7,11:12");
    }

    #[test]
    fn gaps_split_runs() {
        // The UID-gap case: an expunged middle must not be swallowed into a
        // range, or the client is told a message exists that does not.
        assert_eq!(coalesce(&[1, 2, 4, 5]), "1:2,4:5");
        assert_eq!(coalesce(&[1, 3, 5, 7]), "1,3,5,7");
    }

    #[test]
    fn input_order_does_not_matter() {
        assert_eq!(coalesce(&[9, 3, 1, 2, 4]), "1:4,9");
        assert_eq!(coalesce(&[4, 3, 2, 1]), "1:4");
    }

    #[test]
    fn duplicates_are_collapsed() {
        assert_eq!(coalesce(&[3, 3, 3]), "3");
        assert_eq!(coalesce(&[1, 1, 2, 2, 3]), "1:3");
    }

    #[test]
    fn an_empty_input_gives_an_empty_set() {
        // The caller must then omit `ALL` entirely (RFC 9051 §7.3.4) rather
        // than send an empty one.
        let set = Set::from_numbers(&[]);
        assert!(set.is_empty());
        assert_eq!(set.to_string(), "");
    }

    #[test]
    fn zero_is_not_a_valid_member() {
        // RFC 9051 §9 sequence numbers and UIDs are `nz-number`. A 0 arriving
        // here would be a caller bug; dropping it keeps a malformed number out
        // of a response rather than propagating it.
        assert_eq!(coalesce(&[0]), "");
        assert_eq!(coalesce(&[0, 1, 2]), "1:2");
    }

    #[test]
    fn a_run_ending_at_the_maximum_terminates() {
        // `end + 1` would wrap to 0 here and, comparing equal to no successor,
        // simply stop — but `checked_add` makes that explicit rather than
        // accidental.
        assert_eq!(coalesce(&[u32::MAX - 1, u32::MAX]), "4294967294:4294967295");
        assert_eq!(coalesce(&[u32::MAX]), "4294967295");
    }

    #[test]
    fn a_coalesced_set_parses_back_to_the_same_members() {
        // The round trip that matters: what the client receives must resolve to
        // the numbers the server meant.
        for numbers in [
            vec![1u32, 2, 3, 4, 9],
            vec![5],
            vec![2, 5, 6, 7],
            vec![1, 3, 5],
        ] {
            let rendered = coalesce(&numbers);
            let reparsed = Set::parse(&rendered).expect("a coalesced set must reparse");
            assert_eq!(
                reparsed,
                Set::from_numbers(&numbers),
                "round trip failed for {rendered}"
            );
        }
    }

    use super::*;
    use sandesh_core::{
        EntryId, MailboxId, MessageFlags, MessageId, SystemFlags, Timestamp, Uid as CoreUid,
    };

    /// A placement with the given UID.
    fn entry(uid: u32) -> MessageEntry {
        MessageEntry {
            id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: MailboxId::new(),
            uid: CoreUid::new(uid).expect("non-zero UID"),
            flags: MessageFlags::from_system(SystemFlags::NONE),
            internal_date: Timestamp::now(),
            added_at: Timestamp::now(),
            mod_seq: sandesh_core::ModSeq::MIN,
        }
    }

    /// A mailbox holding the given UIDs.
    fn map_of(uids: &[u32]) -> SequenceMap {
        SequenceMap::new(uids.iter().copied().map(entry).collect())
    }

    /// The sequence numbers a set resolves to.
    fn sequences(set: &str, uids: &[u32]) -> Vec<usize> {
        let map = map_of(uids);
        SequenceSet::parse(set)
            .expect("valid set")
            .resolve(&map)
            .into_iter()
            .map(|(sequence, _)| sequence)
            .collect()
    }

    /// The UIDs a set resolves to in UID mode.
    fn uids_resolved(set: &str, uids: &[u32]) -> Vec<u32> {
        let map = map_of(uids);
        SequenceSet::parse(set)
            .expect("valid set")
            .resolve_uids(&map)
            .into_iter()
            .map(|(_, entry)| entry.uid.get())
            .collect()
    }

    // --- parsing ----------------------------------------------------------

    #[test]
    fn a_single_number_parses() {
        assert_eq!(SequenceSet::parse("1").expect("valid").to_string(), "1");
        assert_eq!(SequenceSet::parse("42").expect("valid").to_string(), "42");
    }

    #[test]
    fn ranges_and_stars_parse() {
        for text in ["1:5", "*", "2:*", "*:4", "1,3,5", "1:3,7:9", "*:*"] {
            let parsed = SequenceSet::parse(text).expect(text);
            assert_eq!(parsed.to_string(), text, "round trip of {text}");
        }
    }

    #[test]
    fn zero_is_never_a_valid_number() {
        // RFC 9051 §9 `nz-number`. Zero is a syntax error, not an empty set:
        // the distinction is the whole point of `nz-`.
        for text in ["0", "0:5", "1:0", "1,0", "0,1"] {
            assert_eq!(
                SequenceSet::parse(text),
                Err(SequenceError::ZeroNotAllowed),
                "{text} must be rejected"
            );
        }
    }

    #[test]
    fn malformed_sets_are_rejected() {
        assert_eq!(SequenceSet::parse(""), Err(SequenceError::Empty));
        assert_eq!(SequenceSet::parse("1,"), Err(SequenceError::Empty));
        assert_eq!(SequenceSet::parse(",1"), Err(SequenceError::Empty));
        assert_eq!(SequenceSet::parse("1,,2"), Err(SequenceError::Empty));
        assert_eq!(
            SequenceSet::parse("1:2:3"),
            Err(SequenceError::MalformedRange)
        );
        for text in ["abc", "1:abc", "-1", "+3", "1 2", "1.5", "1:"] {
            assert!(SequenceSet::parse(text).is_err(), "{text} must be rejected");
        }
    }

    #[test]
    fn an_overflowing_number_is_a_syntax_error_not_a_wrap() {
        // The hostile case: a number that does not fit in 32 bits must not
        // wrap into a plausible-looking small one.
        let huge = format!("{}", u64::from(MAX_NUMBER) + 1);
        assert_eq!(
            SequenceSet::parse(&huge),
            Err(SequenceError::NotANumber),
            "overflow must not wrap"
        );
        assert_eq!(
            SequenceSet::parse("99999999999999999999"),
            Err(SequenceError::NotANumber)
        );
        // u32::MAX itself is legal.
        assert!(SequenceSet::parse(&MAX_NUMBER.to_string()).is_ok());
    }

    #[test]
    fn too_many_items_is_rejected() {
        let hostile = (1..=MAX_SET_ITEMS + 10)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            SequenceSet::parse(&hostile),
            Err(SequenceError::TooManyItems)
        );
    }

    // --- sequence-mode resolution ----------------------------------------

    #[test]
    fn a_single_sequence_number_selects_one_message() {
        assert_eq!(sequences("2", &[4, 9, 17]), vec![2]);
    }

    #[test]
    fn a_range_selects_inclusively() {
        assert_eq!(sequences("1:3", &[4, 9, 17]), vec![1, 2, 3]);
        assert_eq!(sequences("2:3", &[4, 9, 17]), vec![2, 3]);
    }

    #[test]
    fn star_is_the_last_message_in_sequence_mode() {
        assert_eq!(sequences("*", &[4, 9, 17]), vec![3]);
        assert_eq!(sequences("2:*", &[4, 9, 17]), vec![2, 3]);
        assert_eq!(sequences("*:2", &[4, 9, 17]), vec![2, 3]);
    }

    #[test]
    fn a_descending_range_matches_the_same_messages() {
        // RFC 9051 §9: `3:1` and `1:3` denote the same set.
        assert_eq!(sequences("3:1", &[4, 9, 17]), sequences("1:3", &[4, 9, 17]));
        assert_eq!(sequences("3:1", &[4, 9, 17]), vec![1, 2, 3]);
    }

    #[test]
    fn duplicates_and_overlaps_collapse() {
        // Each message appears once, in ascending order, however the set names
        // it. A client that receives message 3 twice miscounts.
        assert_eq!(sequences("3,3,3", &[4, 9, 17]), vec![3]);
        assert_eq!(sequences("1:3,2:3", &[4, 9, 17]), vec![1, 2, 3]);
        assert_eq!(
            sequences("3,1", &[4, 9, 17]),
            vec![1, 3],
            "order is imposed"
        );
    }

    #[test]
    fn out_of_range_members_are_ignored_not_errors() {
        // RFC 9051 §6.4.8: a number with no message is not an error.
        assert_eq!(sequences("99", &[4, 9, 17]), Vec::<usize>::new());
        assert_eq!(sequences("2:99", &[4, 9, 17]), vec![2, 3]);
        assert_eq!(sequences("1:*", &[]), Vec::<usize>::new());
        assert_eq!(sequences("*", &[]), Vec::<usize>::new());
    }

    #[test]
    fn sequence_numbers_are_positions_not_uids() {
        // The mutation this test exists to catch: resolving `2` as UID 2
        // rather than as the second message (UID 9).
        let map = map_of(&[4, 9, 17]);
        let resolved = SequenceSet::parse("2").expect("valid").resolve(&map);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, 2, "sequence number");
        assert_eq!(resolved[0].1.uid.get(), 9, "the second message is UID 9");
    }

    // --- UID-mode resolution ---------------------------------------------

    #[test]
    fn uid_mode_resolves_by_uid_not_position() {
        // The inverse mutation: `UID FETCH 9` must find UID 9, which is the
        // *second* message, not the ninth.
        assert_eq!(uids_resolved("9", &[4, 9, 17]), vec![9]);
        assert_eq!(uids_resolved("2", &[4, 9, 17]), Vec::<u32>::new());
    }

    #[test]
    fn uid_ranges_span_gaps() {
        // UIDs 4, 9, 17: the range 4:9 covers two of them and the absent
        // UIDs in between are simply not there.
        assert_eq!(uids_resolved("4:9", &[4, 9, 17]), vec![4, 9]);
        assert_eq!(uids_resolved("1:100", &[4, 9, 17]), vec![4, 9, 17]);
        assert_eq!(uids_resolved("5:8", &[4, 9, 17]), Vec::<u32>::new());
    }

    #[test]
    fn star_is_the_largest_uid_in_use() {
        // Not UIDNEXT, and not the message count.
        assert_eq!(uids_resolved("*", &[4, 9, 17]), vec![17]);
        assert_eq!(uids_resolved("9:*", &[4, 9, 17]), vec![9, 17]);
        assert_eq!(uids_resolved("*:9", &[4, 9, 17]), vec![9, 17]);
    }

    #[test]
    fn uid_mode_still_reports_sequence_numbers() {
        // RFC 9051 §7.4.2: the untagged FETCH is identified by sequence
        // number even when the command addressed a UID.
        let map = map_of(&[4, 9, 17]);
        let resolved = SequenceSet::parse("17").expect("valid").resolve_uids(&map);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, 3, "UID 17 is the third message");
        assert_eq!(resolved[0].1.uid.get(), 17);
    }

    #[test]
    fn uid_mode_on_an_empty_mailbox_yields_nothing() {
        assert_eq!(uids_resolved("1:*", &[]), Vec::<u32>::new());
        assert_eq!(uids_resolved("*", &[]), Vec::<u32>::new());
    }

    #[test]
    fn contains_uid_ignores_star() {
        // Without a mailbox `*` has no value, so it matches nothing rather
        // than everything.
        let set = SequenceSet::parse("*").expect("valid");
        assert!(!set.contains_uid(CoreUid::new(5).expect("non-zero")));

        let explicit = SequenceSet::parse("3:7").expect("valid");
        assert!(explicit.contains_uid(CoreUid::new(5).expect("non-zero")));
        assert!(!explicit.contains_uid(CoreUid::new(9).expect("non-zero")));
    }

    #[test]
    fn resolution_never_panics_on_any_parseable_set() {
        // Breadth check: no combination of legal syntax may panic or hang
        // against any mailbox shape.
        let shapes: [&[u32]; 4] = [&[], &[1], &[4, 9, 17], &[1, 2, 3, 4, 5]];
        for text in [
            "1",
            "*",
            "1:*",
            "*:1",
            "1:1",
            "4294967295",
            "1:4294967295",
            "1,2,3",
            "3:1",
            "*,*",
            "1:*,*:1",
        ] {
            let set = SequenceSet::parse(text).expect(text);
            for shape in shapes {
                let map = map_of(shape);
                let _ = set.resolve(&map);
                let _ = set.resolve_uids(&map);
            }
        }
    }
}

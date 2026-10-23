//! Mailbox-name wire concerns and the sequence-number mapping.
//!
//! Three things live here, all of them pure and all of them shared by every
//! mailbox command so that no handler grows its own variant:
//!
//! * [`matches_pattern`] — the RFC 9051 §6.3.9 `%`/`*` wildcard rules.
//! * [`quote_mailbox`] — rendering a name as an `astring` on the wire.
//! * [`SequenceMap`] — the canonical sequence-number ↔ UID mapping.
//!
//! # Why these are not in `sandesh-core`
//!
//! They are protocol encoding, not domain truth. A [`sandesh_core::MailboxName`]
//! is a validated hierarchical name; how it is quoted, and how a client's `%`
//! pattern selects from a list of them, is IMAP's business alone. Putting them
//! in core would push protocol grammar into the domain the architecture test
//! keeps protocol-free.

use std::collections::BTreeSet;

use sandesh_core::{HIERARCHY_DELIMITER, MessageEntry, MessageFlags, Uid};

/// Whether `name` matches an RFC 9051 §6.3.9 list pattern.
///
/// The two wildcards differ in exactly one respect, and it is the one clients
/// depend on to draw a collapsed folder tree:
///
/// ```text
/// pattern   matches                       does not match
/// ───────   ───────────────────────────   ──────────────────
/// Work      Work                          Work/Projects
/// Work/%    Work/Projects                 Work/Projects/Deep
/// Work/*    Work/Projects, Work/…/Deep    —
/// %         INBOX, Work                   Work/Projects
/// *         everything                    —
/// ```
///
/// `*` matches any sequence including the hierarchy delimiter; `%` matches any
/// sequence *except* the delimiter, so it stops at one level.
///
/// Matching is performed over characters rather than bytes so that a
/// multi-byte name cannot be split mid-character by a wildcard boundary.
#[must_use]
pub fn matches_pattern(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_from(&pattern, 0, &name, 0)
}

/// Backtracking matcher over `pattern[p..]` against `name[n..]`.
///
/// Recursion depth is bounded by `name.len()`: every recursive call either
/// advances `n`, or advances `p` past a literal while also advancing `n`. A
/// pattern of any length therefore cannot recurse more than once per remaining
/// character, which is what keeps a hostile pattern such as `****…` from
/// exhausting the stack (phase brief §18).
fn matches_from(pattern: &[char], p: usize, name: &[char], n: usize) -> bool {
    if p == pattern.len() {
        return n == name.len();
    }

    match pattern[p] {
        // `*` crosses the hierarchy delimiter, `%` stops at it. Otherwise the
        // two behave identically, so they share one loop.
        wildcard @ ('*' | '%') => {
            let crosses_delimiter = wildcard == '*';
            let mut consumed = n;
            loop {
                if matches_from(pattern, p + 1, name, consumed) {
                    return true;
                }
                if consumed == name.len() {
                    return false;
                }
                if !crosses_delimiter && name[consumed] == HIERARCHY_DELIMITER {
                    // `%` may not consume the delimiter, so this is as far as
                    // it reaches.
                    return false;
                }
                consumed += 1;
            }
        }
        literal => {
            n < name.len() && name[n] == literal && matches_from(pattern, p + 1, name, n + 1)
        }
    }
}

/// Applies a `LIST` reference to a pattern (RFC 9051 §6.3.9).
///
/// The reference is a prefix the pattern is interpreted relative to. Real
/// clients almost always send an empty reference and an absolute pattern, so
/// the common path is a straight passthrough; a non-empty reference is joined
/// with the hierarchy delimiter unless one side already supplies it.
#[must_use]
pub fn resolve_reference(reference: &str, pattern: &str) -> String {
    if reference.is_empty() {
        return pattern.to_owned();
    }
    if pattern.starts_with(HIERARCHY_DELIMITER) || reference.ends_with(HIERARCHY_DELIMITER) {
        format!("{reference}{pattern}")
    } else {
        format!("{reference}{HIERARCHY_DELIMITER}{pattern}")
    }
}

/// Renders a mailbox name as an `astring` (RFC 9051 §9).
///
/// A name that is a bare `atom` may go out unquoted, but anything containing a
/// space, a quote, a backslash or a non-ASCII character must be quoted, with
/// `\` and `"` escaped. Quoting more than strictly necessary is always legal
/// and never ambiguous, so this quotes whenever there is any doubt.
///
/// CR and LF are stripped rather than escaped: they cannot appear in a
/// validated [`sandesh_core::MailboxName`], and if one ever did, emitting it
/// would split the response line.
#[must_use]
pub fn quote_mailbox(name: &str) -> String {
    // Unicode in, modified UTF-7 out (RFC 3501 §5.1.3) — the mirror of the
    // decode in `Session::mailbox_argument`, so a name a client sent comes
    // back in the form it sent it. Pure ASCII is unchanged, which is why rev2
    // clients and every existing test see identical output.
    let encoded = crate::utf7::encode(name);
    let cleaned: String = encoded
        .chars()
        .filter(|c| *c != '\r' && *c != '\n')
        .collect();

    let is_bare_atom = !cleaned.is_empty()
        && cleaned.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '+' | '&' | '#' | '$')
        });

    if is_bare_atom {
        return cleaned;
    }

    let mut out = String::with_capacity(cleaned.len() + 2);
    out.push('"');
    for c in cleaned.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The canonical sequence-number ↔ UID mapping for a selected mailbox.
///
/// # The invariant this type exists to hold
///
/// RFC 9051 §2.3.1.2 defines message sequence numbers as the position of a
/// message in the mailbox, counting from **1**, in ascending UID order. Both
/// halves matter and both are easy to get wrong:
///
/// ```text
///   position:   1     2     3      ← sequence numbers, contiguous from 1
///   entries:  [ e ]-[ e ]-[ e ]    ← ordered by UID, ascending
///   UID:        4     9    17      ← sparse; gaps are expunged messages
/// ```
///
/// A UID is the message's stable identity and survives gaps; a sequence number
/// is a *position* and is renumbered whenever the mailbox changes. Deriving one
/// from a database row id, or letting a gap in UIDs create a gap in sequence
/// numbers, breaks every client.
///
/// Later increments build `FETCH`, `SEARCH`, `STORE` and `COPY` on this type
/// rather than re-deriving the ordering, which is what keeps them consistent
/// with each other (phase brief §12).
#[derive(Debug, Clone, Default)]
pub struct SequenceMap {
    /// Entries in ascending UID order. The index is `sequence number - 1`.
    entries: Vec<MessageEntry>,
}

impl SequenceMap {
    /// Builds the mapping from a mailbox's placements.
    ///
    /// Sorts by UID rather than trusting the caller. The storage port already
    /// returns `ORDER BY uid`, but this type *is* the definition of the
    /// ordering and must not depend on a detail of one adapter's SQL to be
    /// correct.
    #[must_use]
    pub fn new(mut entries: Vec<MessageEntry>) -> Self {
        entries.sort_by_key(|entry| entry.uid.get());
        Self { entries }
    }

    /// How many messages the mailbox holds — the `EXISTS` count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the mailbox is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry at a 1-based sequence number.
    ///
    /// Returns `None` for 0, which is never a valid sequence number, and for
    /// anything past the end.
    #[must_use]
    pub fn by_sequence(&self, sequence: usize) -> Option<&MessageEntry> {
        if sequence == 0 {
            return None;
        }
        self.entries.get(sequence - 1)
    }

    /// The entry with a given UID.
    #[must_use]
    pub fn by_uid(&self, uid: Uid) -> Option<&MessageEntry> {
        let index = self
            .entries
            .binary_search_by_key(&uid.get(), |entry| entry.uid.get())
            .ok()?;
        self.entries.get(index)
    }

    /// The 1-based sequence number of a UID, if it is present.
    #[must_use]
    pub fn sequence_of(&self, uid: Uid) -> Option<usize> {
        self.entries
            .binary_search_by_key(&uid.get(), |entry| entry.uid.get())
            .ok()
            .map(|index| index + 1)
    }

    /// The entries, in sequence order.
    pub fn iter(&self) -> impl Iterator<Item = &MessageEntry> {
        self.entries.iter()
    }

    /// The entries paired with their 1-based sequence numbers.
    pub fn enumerated(&self) -> impl Iterator<Item = (usize, &MessageEntry)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (index + 1, entry))
    }

    /// Replaces one placement's flags and modification sequence, leaving
    /// membership and order alone.
    ///
    /// `FETCH BODY[...]` sets `\Seen` through the storage port; this reflects
    /// that back into the snapshot so the session does not keep reporting the
    /// flags it captured at `SELECT`. Only flags and the sequence change: a
    /// snapshot's set of messages and their sequence numbers are fixed at
    /// selection time (ADR-046), and altering them here would renumber
    /// messages under a client that is mid-conversation.
    ///
    /// # Why the modseq travels with the flags
    ///
    /// A flag change *is* a modseq change — storage allocates one for exactly
    /// the mutations that alter flags. Recording the flags without the
    /// sequence leaves the snapshot internally inconsistent, and a following
    /// `FETCH MODSEQ` in the same session then reports the value from before
    /// the change: the client is told its own store did not happen. `None`
    /// leaves the stored sequence alone, for the callers that genuinely have
    /// no new value (an event carrying only flag names).
    pub fn set_flags(
        &mut self,
        uid: Uid,
        flags: MessageFlags,
        mod_seq: Option<sandesh_core::ModSeq>,
    ) {
        if let Ok(index) = self
            .entries
            .binary_search_by_key(&uid.get(), |entry| entry.uid.get())
            && let Some(entry) = self.entries.get_mut(index)
        {
            entry.flags = flags;
            if let Some(mod_seq) = mod_seq {
                entry.mod_seq = mod_seq;
            }
        }
    }

    /// Removes the named placements and renumbers what remains.
    ///
    /// This is the one operation that changes a snapshot's *membership*, and it
    /// exists for exactly one caller: `EXPUNGE` (ADR-049). Every other command
    /// leaves membership alone — [`set_flags`](Self::set_flags) deliberately
    /// does not — because renumbering under a client that is mid-conversation
    /// is how sequence numbers and UIDs get confused.
    ///
    /// After an `EXPUNGE` the acting session's own view *must* change, though:
    /// RFC 9051 §6.4.3 has the server report each removal as
    /// `* <seq> EXPUNGE`, and a client that has seen those reports expects its
    /// next `FETCH 1:*` to address the mailbox as it now is.
    ///
    /// ```text
    ///   before   seq: 1    2    3    4        removing UIDs 9 and 17
    ///            uid: 4    9   17   23
    ///
    ///   after    seq: 1    2                  UIDs are unchanged;
    ///            uid: 4   23                  positions close the gap
    /// ```
    ///
    /// UIDs are never rewritten — they are the messages' stable identities, and
    /// the entries kept here are the same entries, only at new positions.
    /// Returns how many placements were actually removed, so a caller cannot
    /// mistake "named" for "removed".
    pub fn remove_uids(&mut self, removed: &BTreeSet<u32>) -> usize {
        let before = self.entries.len();
        // Retain preserves the ascending-UID order, so the surviving entries
        // are already correctly positioned: the renumbering *is* the removal.
        self.entries
            .retain(|entry| !removed.contains(&entry.uid.get()));
        before - self.entries.len()
    }

    /// Adds a placement, keeping the ascending-UID order.
    ///
    /// The counterpart to [`remove_uids`](Self::remove_uids), and the second of
    /// the only two operations that change a snapshot's *membership*. It exists
    /// for `IDLE`: a message that arrives while a session is idling must become
    /// addressable in that session, because the client has just been told
    /// `* <n> EXISTS` and its next `FETCH n` refers to it.
    ///
    /// ```text
    ///   before   seq: 1    2    3         inserting UID 12
    ///            uid: 4    9   17
    ///
    ///   after    seq: 1    2    3    4    12 takes position 3;
    ///            uid: 4    9   12   17    17 shifts down one
    /// ```
    ///
    /// The entry is *placed*, not appended: the map is UID-ordered by
    /// definition, and appending blindly would break every binary search on it.
    /// A UID already present is not inserted twice — a duplicate would make the
    /// mailbox appear to hold a message it does not.
    pub fn insert(&mut self, entry: MessageEntry) {
        match self
            .entries
            .binary_search_by_key(&entry.uid.get(), |existing| existing.uid.get())
        {
            // Already present: nothing to do. Replacing it would discard flags
            // this session may have recorded since.
            Ok(_) => {}
            Err(index) => self.entries.insert(index, entry),
        }
    }

    /// How many placements lack `\Seen`.
    #[must_use]
    pub fn unseen_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_seen()).count()
    }

    /// The sequence number of the first unseen message (RFC 9051 §7.3.1).
    #[must_use]
    pub fn first_unseen(&self) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| !entry.is_seen())
            .map(|index| index + 1)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // --- pattern matching -------------------------------------------------

    #[test]
    fn a_literal_pattern_matches_only_itself() {
        assert!(matches_pattern("INBOX", "INBOX"));
        assert!(!matches_pattern("INBOX", "INBOX/Sub"));
        assert!(!matches_pattern("INBOX", "Work"));
    }

    #[test]
    fn star_crosses_the_hierarchy_delimiter() {
        assert!(matches_pattern("*", "INBOX"));
        assert!(matches_pattern("*", "Work/Projects/Deep"));
        assert!(matches_pattern("Work/*", "Work/Projects"));
        assert!(matches_pattern("Work/*", "Work/Projects/Deep"));
    }

    #[test]
    fn percent_stops_at_the_hierarchy_delimiter() {
        // The distinction a collapsed folder tree depends on.
        assert!(matches_pattern("%", "INBOX"));
        assert!(!matches_pattern("%", "Work/Projects"));
        assert!(matches_pattern("Work/%", "Work/Projects"));
        assert!(!matches_pattern("Work/%", "Work/Projects/Deep"));
    }

    #[test]
    fn wildcards_match_the_empty_string() {
        assert!(matches_pattern("*", ""));
        assert!(matches_pattern("Work*", "Work"));
        assert!(matches_pattern("Work%", "Work"));
    }

    #[test]
    fn a_hostile_pattern_terminates() {
        // Repeated wildcards must not blow the stack or hang (phase brief §18).
        let pattern = "*".repeat(64);
        assert!(matches_pattern(&pattern, "Work/Projects/Deep"));

        let mixed = "%*".repeat(32);
        let _ = matches_pattern(&mixed, "a/b/c/d/e");
    }

    #[test]
    fn matching_is_character_wise_not_byte_wise() {
        assert!(matches_pattern("Ärchiv/%", "Ärchiv/2026"));
        assert!(matches_pattern("受%", "受信箱"));
    }

    #[test]
    fn a_reference_is_prefixed_onto_the_pattern() {
        assert_eq!(resolve_reference("", "Work/%"), "Work/%");
        assert_eq!(resolve_reference("Work", "%"), "Work/%");
        assert_eq!(resolve_reference("Work/", "%"), "Work/%");
    }

    // --- quoting ----------------------------------------------------------

    #[test]
    fn a_plain_name_needs_no_quotes() {
        assert_eq!(quote_mailbox("INBOX"), "INBOX");
        assert_eq!(quote_mailbox("Work/Projects"), "Work/Projects");
    }

    #[test]
    fn a_name_with_a_space_is_quoted() {
        assert_eq!(quote_mailbox("Sent Items"), "\"Sent Items\"");
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(quote_mailbox(r#"od"d"#), r#""od\"d""#);
        assert_eq!(quote_mailbox(r"back\slash"), r#""back\\slash""#);
    }

    #[test]
    fn a_unicode_name_goes_out_as_modified_utf7() {
        // Superseded the earlier expectation of a quoted raw-UTF-8 name. While
        // the rev1 capability name is advertised (ADR-042) a rev1 client may be
        // reading, and it decodes modified UTF-7 unconditionally — a raw UTF-8
        // name would display as mojibake and, worse, would not match the name
        // it sent us. Encoded output is pure ASCII, so it needs no quoting.
        assert_eq!(quote_mailbox("Ärchiv"), "&AMQ-rchiv");
        assert_eq!(crate::utf7::decode(&quote_mailbox("Ärchiv")), "Ärchiv");
    }

    #[test]
    fn a_name_survives_the_wire_round_trip() {
        // What the duplicate-folder bug actually broke: whatever a client
        // sends must come back as the same name, in the same encoding.
        for name in ["INBOX", "Work/Projects", "Wichtig-über", "受信箱", "R&D"] {
            let on_the_wire = quote_mailbox(name);
            let unquoted = on_the_wire.trim_matches('"');
            assert_eq!(crate::utf7::decode(unquoted), name, "{on_the_wire}");
        }
    }

    #[test]
    fn newlines_cannot_escape_the_quoted_string() {
        // The response-splitting guard, restated at the mailbox level.
        let quoted = quote_mailbox("a\r\n* BYE forged");
        assert!(!quoted.contains('\r'), "{quoted}");
        assert!(!quoted.contains('\n'), "{quoted}");
    }

    // --- sequence mapping -------------------------------------------------

    use sandesh_core::{EntryId, MailboxId, MessageFlags, MessageId, SystemFlags, Timestamp, Uid};

    /// A placement with the given UID, seen or not.
    fn entry(uid: u32, seen: bool) -> MessageEntry {
        let flags = if seen {
            MessageFlags::from_system(SystemFlags::SEEN)
        } else {
            MessageFlags::empty()
        };
        MessageEntry {
            id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: MailboxId::new(),
            uid: Uid::new(uid).unwrap_or_else(|| panic!("UID {uid} must be non-zero")),
            flags,
            internal_date: Timestamp::now(),
            added_at: Timestamp::now(),
            mod_seq: sandesh_core::ModSeq::MIN,
        }
    }

    fn uid(value: u32) -> Uid {
        Uid::new(value).unwrap_or_else(|| panic!("UID {value} must be non-zero"))
    }

    #[test]
    fn the_first_message_is_sequence_one() {
        // Not zero. The single most consequential off-by-one in IMAP.
        let map = SequenceMap::new(vec![entry(4, false)]);
        assert!(map.by_sequence(0).is_none(), "0 is never a sequence number");
        assert_eq!(map.by_sequence(1).map(|e| e.uid.get()), Some(4));
    }

    #[test]
    fn sequence_numbers_are_contiguous_despite_uid_gaps() {
        // UIDs 4, 9, 17 — gaps where messages were expunged. Sequence numbers
        // must still run 1, 2, 3 with no holes.
        let map = SequenceMap::new(vec![entry(4, false), entry(9, false), entry(17, false)]);

        assert_eq!(map.len(), 3);
        let uids: Vec<u32> = map.enumerated().map(|(_, e)| e.uid.get()).collect();
        assert_eq!(uids, vec![4, 9, 17]);

        let seqs: Vec<usize> = map.enumerated().map(|(seq, _)| seq).collect();
        assert_eq!(seqs, vec![1, 2, 3], "sequence numbers must be contiguous");
    }

    #[test]
    fn ordering_is_by_uid_and_deterministic() {
        // Built out of order; the map must still order by UID ascending.
        let map = SequenceMap::new(vec![entry(17, false), entry(4, false), entry(9, false)]);
        let uids: Vec<u32> = map.iter().map(|e| e.uid.get()).collect();
        assert_eq!(uids, vec![4, 9, 17]);

        // Deterministic: the same input always yields the same order.
        let again = SequenceMap::new(vec![entry(9, false), entry(17, false), entry(4, false)]);
        let again_uids: Vec<u32> = again.iter().map(|e| e.uid.get()).collect();
        assert_eq!(uids, again_uids);
    }

    #[test]
    fn uid_is_the_authoritative_identity() {
        let map = SequenceMap::new(vec![entry(4, false), entry(9, false), entry(17, false)]);

        assert_eq!(map.sequence_of(uid(9)), Some(2));
        assert_eq!(map.by_uid(uid(17)).map(|e| e.uid.get()), Some(17));

        // A UID that is not present maps to nothing rather than to a position.
        assert_eq!(map.sequence_of(uid(5)), None);
        assert!(map.by_uid(uid(5)).is_none());
    }

    #[test]
    fn a_sequence_number_past_the_end_is_not_a_message() {
        let map = SequenceMap::new(vec![entry(4, false)]);
        assert!(map.by_sequence(2).is_none());
        assert!(map.by_sequence(usize::MAX).is_none());
    }

    #[test]
    fn an_empty_mailbox_has_no_sequence_numbers() {
        let map = SequenceMap::new(Vec::new());
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
        assert!(map.by_sequence(1).is_none());
        assert_eq!(map.first_unseen(), None);
    }

    #[test]
    fn a_new_arrival_extends_the_mapping() {
        let mut entries = vec![entry(4, false), entry(9, false)];
        let before = SequenceMap::new(entries.clone());
        assert_eq!(before.len(), 2);

        entries.push(entry(12, false));
        let after = SequenceMap::new(entries);

        assert_eq!(after.len(), 3);
        assert_eq!(after.sequence_of(uid(12)), Some(3), "the arrival is last");
        // Existing messages keep their sequence numbers.
        assert_eq!(after.sequence_of(uid(4)), Some(1));
        assert_eq!(after.sequence_of(uid(9)), Some(2));
    }

    #[test]
    fn unseen_counting_matches_the_flags() {
        let map = SequenceMap::new(vec![entry(1, true), entry(2, false), entry(3, false)]);
        assert_eq!(map.unseen_count(), 2);
        assert_eq!(map.first_unseen(), Some(2), "the first unseen is at 2");

        let all_seen = SequenceMap::new(vec![entry(1, true), entry(2, true)]);
        assert_eq!(all_seen.unseen_count(), 0);
        assert_eq!(all_seen.first_unseen(), None);
    }

    // --- membership mutation (EXPUNGE, ADR-049) ---------------------------

    #[test]
    fn removing_uids_renumbers_the_survivors_contiguously() {
        // UIDs 4, 9, 17, 23 at sequence 1..4. Removing 9 and 17 must leave
        // UIDs 4 and 23 at sequence 1 and 2 — no gap where they were.
        let mut map = SequenceMap::new(vec![
            entry(4, false),
            entry(9, false),
            entry(17, false),
            entry(23, false),
        ]);
        assert_eq!(map.remove_uids(&BTreeSet::from([9, 17])), 2);
        assert_eq!(map.len(), 2);
        assert_eq!(map.by_sequence(1).map(|e| e.uid.get()), Some(4));
        assert_eq!(map.by_sequence(2).map(|e| e.uid.get()), Some(23));
        assert!(map.by_sequence(3).is_none());
    }

    #[test]
    fn removing_uids_leaves_the_surviving_uids_unchanged() {
        // The whole point of a UID: it is not a position and does not shift.
        let mut map = SequenceMap::new(vec![entry(4, false), entry(9, false), entry(17, false)]);
        map.remove_uids(&BTreeSet::from([4]));
        assert_eq!(map.by_uid(uid(9)).map(|e| e.uid.get()), Some(9));
        assert_eq!(map.by_uid(uid(17)).map(|e| e.uid.get()), Some(17));
        assert_eq!(map.sequence_of(uid(9)), Some(1));
        assert_eq!(map.sequence_of(uid(17)), Some(2));
    }

    #[test]
    fn a_removed_uid_is_no_longer_resolvable() {
        let mut map = SequenceMap::new(vec![entry(4, false), entry(9, false)]);
        map.remove_uids(&BTreeSet::from([4]));
        assert!(map.by_uid(uid(4)).is_none());
        assert!(map.sequence_of(uid(4)).is_none());
    }

    #[test]
    fn removing_a_uid_the_mailbox_does_not_hold_changes_nothing() {
        let mut map = SequenceMap::new(vec![entry(4, false), entry(9, false)]);
        assert_eq!(map.remove_uids(&BTreeSet::from([99])), 0);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn removing_every_uid_empties_the_mailbox() {
        let mut map = SequenceMap::new(vec![entry(4, false), entry(9, false)]);
        assert_eq!(map.remove_uids(&BTreeSet::from([4, 9])), 2);
        assert!(map.is_empty());
        assert!(map.first_unseen().is_none());
    }

    #[test]
    fn removing_the_first_message_shifts_the_first_unseen() {
        // seq 1 unseen, seq 2 seen, seq 3 unseen. Remove seq 1 and the first
        // unseen becomes the message that was at seq 3, now at seq 2.
        let mut map = SequenceMap::new(vec![entry(4, false), entry(9, true), entry(17, false)]);
        assert_eq!(map.first_unseen(), Some(1));
        map.remove_uids(&BTreeSet::from([4]));
        assert_eq!(map.first_unseen(), Some(2));
        assert_eq!(map.unseen_count(), 1);
    }

    #[test]
    fn an_empty_removal_set_is_a_no_op() {
        let mut map = SequenceMap::new(vec![entry(4, false)]);
        assert_eq!(map.remove_uids(&BTreeSet::new()), 0);
        assert_eq!(map.len(), 1);
    }
}

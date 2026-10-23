//! `COPY` and `UID COPY`: pairing source UIDs with the destination UIDs
//! storage assigned, and rendering `[COPYUID]` (RFC 9051 §6.4.7, RFC 4315 §3).
//!
//! This module is pure. The session resolves the client's set through the
//! selected mailbox's [`SequenceMap`], calls
//! [`sandesh_core::MessageStore::copy_entry`] once per placement, and records
//! each `(source UID, destination UID)` pair here. This module then renders the
//! response code.
//!
//! ```text
//! SequenceMap ──▶ placements to copy
//!                        │
//!                        ▼
//!              copy_entry per placement          ← storage assigns the new UID
//!                        │
//!                (source uid, dest uid)
//!                        │
//!                        ▼
//!   [COPYUID <dest uidvalidity> <source set> <dest set>]
//! ```
//!
//! # Why the pairing is recorded rather than computed
//!
//! It is tempting to render the destination set as `uid_next .. uid_next + n`,
//! since `copy_entry` allocates consecutively today. That would be a *guess*
//! about a storage detail: a concurrent `APPEND` or `COPY` into the same
//! mailbox interleaves allocations, and the guess then names UIDs belonging to
//! someone else's messages. RFC 4315 §3 requires the two sets to correspond
//! message-for-message, so the only correct source for a destination UID is the
//! value `copy_entry` returned.
//!
//! # Why order matters
//!
//! RFC 4315 §3 pairs the sets **positionally**: the *n*th UID of the source set
//! names the same message as the *n*th UID of the destination set. Both sets are
//! therefore rendered in the order the copies were performed — ascending source
//! UID, because that is the order the `SequenceMap` walk produces — and neither
//! is sorted independently. Sorting the destination set alone would silently
//! re-pair the messages.

use core::fmt::Write as _;

use sandesh_core::{Uid, UidValidity};

/// The source and destination UIDs of one copied message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedUid {
    /// The UID the message holds in the source mailbox. Unchanged by the copy.
    pub source: Uid,
    /// The UID storage assigned in the destination mailbox.
    pub destination: Uid,
}

/// What one `COPY` actually copied, in the order it was performed.
///
/// Empty when the set named no message, which RFC 9051 §6.4.8 makes a
/// successful `COPY` that copies nothing — and which must therefore carry **no**
/// `[COPYUID]`, because there are no UIDs to report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyResult {
    copied: Vec<CopiedUid>,
}

impl CopyResult {
    /// An empty result.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one completed copy.
    ///
    /// Called only after `copy_entry` returned, with the UID it returned. A
    /// placement whose copy failed is never recorded, so it cannot appear in
    /// `[COPYUID]`.
    pub fn record(&mut self, source: Uid, destination: Uid) {
        self.copied.push(CopiedUid {
            source,
            destination,
        });
    }

    /// Whether nothing was copied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.copied.is_empty()
    }

    /// How many messages were copied.
    #[must_use]
    pub fn len(&self) -> usize {
        self.copied.len()
    }

    /// The pairs, in the order the copies happened.
    pub fn iter(&self) -> impl Iterator<Item = &CopiedUid> {
        self.copied.iter()
    }

    /// The `COPYUID` response code, or `None` when nothing was copied.
    ///
    /// ```text
    /// COPYUID <destination UIDVALIDITY> <source UID set> <destination UID set>
    /// ```
    ///
    /// The UIDVALIDITY is the **destination** mailbox's: the destination UIDs
    /// are meaningless without the epoch they belong to, and the source epoch
    /// is one the client already knows from its own `SELECT`.
    #[must_use]
    pub fn copyuid(&self, destination_validity: UidValidity) -> Option<String> {
        if self.copied.is_empty() {
            return None;
        }
        let sources: Vec<Uid> = self.copied.iter().map(|c| c.source).collect();
        let destinations: Vec<Uid> = self.copied.iter().map(|c| c.destination).collect();
        Some(format!(
            "COPYUID {} {} {}",
            destination_validity.get(),
            render_uid_set(&sources),
            render_uid_set(&destinations)
        ))
    }
}

/// Renders UIDs as a `uid-set` (RFC 9051 §9), coalescing runs into ranges.
///
/// ```text
/// [4]              → 4
/// [4, 5, 6]        → 4:6
/// [4, 9, 10, 17]   → 4,9:10,17
/// ```
///
/// Coalescing is only applied to *consecutive ascending* runs, so the rendered
/// set names exactly the UIDs given, in the order given. A descending or
/// interleaved sequence therefore renders as singletons rather than being
/// reordered into ranges — which keeps the positional pairing with the other
/// set intact even if a future storage change hands back non-monotonic UIDs.
#[must_use]
pub fn render_uid_set(uids: &[Uid]) -> String {
    let mut out = String::new();
    let mut index = 0;
    while index < uids.len() {
        let start = uids[index].get();
        let mut end = start;
        // Extend while the next UID continues the run by exactly one.
        while index + 1 < uids.len() && uids[index + 1].get() == end + 1 {
            index += 1;
            end = uids[index].get();
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            let _ = write!(out, "{start}");
        } else {
            let _ = write!(out, "{start}:{end}");
        }
        index += 1;
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn uid(value: u32) -> Uid {
        Uid::new(value).unwrap_or_else(|| panic!("UID {value} must be non-zero"))
    }

    fn validity(value: u32) -> UidValidity {
        UidValidity::new(value)
    }

    // --- uid-set rendering ------------------------------------------------

    #[test]
    fn a_single_uid_renders_alone() {
        assert_eq!(render_uid_set(&[uid(4)]), "4");
    }

    #[test]
    fn a_consecutive_run_becomes_a_range() {
        assert_eq!(render_uid_set(&[uid(4), uid(5), uid(6)]), "4:6");
    }

    #[test]
    fn gaps_split_the_set() {
        assert_eq!(
            render_uid_set(&[uid(4), uid(9), uid(10), uid(17)]),
            "4,9:10,17"
        );
    }

    #[test]
    fn a_pair_of_consecutive_uids_is_a_range_not_two_singletons() {
        // `4:5` and `4,5` are the same set; the range form is what clients see
        // from other servers, so it is what this renders.
        assert_eq!(render_uid_set(&[uid(4), uid(5)]), "4:5");
    }

    #[test]
    fn an_empty_set_renders_empty() {
        assert_eq!(render_uid_set(&[]), "");
    }

    #[test]
    fn a_non_ascending_sequence_is_not_reordered() {
        // Order is the positional pairing with the other set. Sorting here
        // would re-pair the messages.
        assert_eq!(render_uid_set(&[uid(9), uid(4)]), "9,4");
    }

    #[test]
    fn a_run_at_the_end_still_coalesces() {
        assert_eq!(render_uid_set(&[uid(1), uid(7), uid(8), uid(9)]), "1,7:9");
    }

    // --- COPYUID ----------------------------------------------------------

    #[test]
    fn copyuid_names_the_destination_uidvalidity() {
        let mut result = CopyResult::new();
        result.record(uid(4), uid(11));
        assert_eq!(
            result.copyuid(validity(99)),
            Some(String::from("COPYUID 99 4 11"))
        );
    }

    #[test]
    fn copyuid_pairs_the_sets_positionally() {
        // Source UIDs 4, 9, 17 landed as 11, 12, 13. The nth of each set must
        // name the same message.
        let mut result = CopyResult::new();
        result.record(uid(4), uid(11));
        result.record(uid(9), uid(12));
        result.record(uid(17), uid(13));
        assert_eq!(
            result.copyuid(validity(1)),
            Some(String::from("COPYUID 1 4,9,17 11:13"))
        );
    }

    #[test]
    fn copyuid_reports_non_consecutive_destination_uids_faithfully() {
        // A concurrent writer interleaved its own allocations, so the
        // destination UIDs are not a run. Reporting `11:13` here would name a
        // stranger's message.
        let mut result = CopyResult::new();
        result.record(uid(4), uid(11));
        result.record(uid(5), uid(13));
        assert_eq!(
            result.copyuid(validity(1)),
            Some(String::from("COPYUID 1 4:5 11,13"))
        );
    }

    #[test]
    fn an_empty_copy_carries_no_copyuid() {
        // RFC 9051 §6.4.8: copying nothing is success. There are no UIDs to
        // report, and a `COPYUID` with an empty set would be malformed.
        assert!(CopyResult::new().copyuid(validity(1)).is_none());
    }

    #[test]
    fn the_source_set_preserves_uid_gaps() {
        // Source UIDs 2 and 7 with a gap between them: the source set must
        // show the gap, not renumber it away.
        let mut result = CopyResult::new();
        result.record(uid(2), uid(1));
        result.record(uid(7), uid(2));
        assert_eq!(
            result.copyuid(validity(5)),
            Some(String::from("COPYUID 5 2,7 1:2"))
        );
    }

    #[test]
    fn the_two_sets_always_name_the_same_number_of_messages() {
        let mut result = CopyResult::new();
        for (source, destination) in [(1, 20), (2, 21), (5, 22)] {
            result.record(uid(source), uid(destination));
        }
        let code = result.copyuid(validity(1)).expect("a non-empty copy");
        let mut parts = code.split(' ').skip(2);
        let sources = parts.next().expect("a source set");
        let destinations = parts.next().expect("a destination set");
        assert_eq!(count_uids(sources), count_uids(destinations));
        assert_eq!(count_uids(sources), result.len());
    }

    /// Counts the UIDs a rendered `uid-set` names.
    fn count_uids(set: &str) -> usize {
        set.split(',')
            .map(|piece| match piece.split_once(':') {
                Some((low, high)) => {
                    let low: u32 = low.parse().expect("a number");
                    let high: u32 = high.parse().expect("a number");
                    (high - low + 1) as usize
                }
                None => 1,
            })
            .sum()
    }

    #[test]
    fn recording_tracks_the_count_and_emptiness() {
        let mut result = CopyResult::new();
        assert!(result.is_empty());
        result.record(uid(1), uid(2));
        assert!(!result.is_empty());
        assert_eq!(result.len(), 1);
        assert_eq!(result.iter().count(), 1);
    }
}

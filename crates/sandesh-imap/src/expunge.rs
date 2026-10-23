//! `EXPUNGE` and `UID EXPUNGE`: deciding *what* to remove, and *how to say so*.
//!
//! This module is pure. It takes a selected mailbox's [`SequenceMap`] and,
//! optionally, a client's UID set, and produces an [`ExpungePlan`] — the
//! placements to remove, each paired with the sequence number it held *before*
//! any removal. The session then applies the plan through
//! [`sandesh_core::MessageStore::delete_entry`] and reports it.
//!
//! ```text
//! SequenceMap ─┬─▶ every \Deleted placement          (EXPUNGE)
//!              └─▶ every \Deleted placement in <set> (UID EXPUNGE)
//!                                │
//!                                ▼
//!                          ExpungePlan
//!                     (entry_id, uid, seq-before)
//!                                │
//!               ┌────────────────┴──────────────────┐
//!               ▼                                   ▼
//!    delete_entry per placement            * <seq> EXPUNGE lines,
//!    (emits EntryDeleted)                  highest sequence first
//! ```
//!
//! # Why the plan is built before anything is removed
//!
//! A sequence number is a *position*, so it changes the moment a message ahead
//! of it disappears. Computing the whole plan first — against one unmutated
//! snapshot — is what makes the reported numbers the ones RFC 9051 §7.4.1
//! describes, and it is what lets a malformed command mutate nothing at all.
//!
//! # `\Deleted` is the only selector
//!
//! `EXPUNGE` removes messages *marked* `\Deleted`, never messages a client
//! merely named. `UID EXPUNGE <set>` (RFC 4315 §2.1) narrows that to the set:
//! it is an intersection, not an alternative. A UID in the set without
//! `\Deleted` is left alone, which is the property that makes `UID EXPUNGE`
//! safe for a client to issue against a set it did not just re-check.

use std::collections::BTreeSet;

use sandesh_core::{EntryId, Uid};

use crate::mailbox::SequenceMap;
use crate::sequence::SequenceSet;

/// One placement to remove, named three ways.
///
/// All three are needed and none is derivable from the others once removal
/// starts: the `entry_id` addresses storage, the `uid` updates the snapshot,
/// and the `sequence` is what the client is told — a number that stops being
/// true as soon as the first removal lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Doomed {
    /// The placement to delete. A *placement*, never a message: another
    /// mailbox's placement of the same message is a different `EntryId` and is
    /// untouched.
    pub entry_id: EntryId,
    /// The UID it holds in the selected mailbox.
    pub uid: Uid,
    /// The sequence number it held *before* any removal in this command.
    pub sequence: usize,
}

/// What one `EXPUNGE` will remove, decided against an unmutated snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpungePlan {
    doomed: Vec<Doomed>,
}

impl ExpungePlan {
    /// Plans an `EXPUNGE`: every `\Deleted` placement in the mailbox.
    #[must_use]
    pub fn all_deleted(map: &SequenceMap) -> Self {
        Self {
            doomed: map
                .enumerated()
                .filter(|(_, entry)| entry.is_deleted())
                .map(|(sequence, entry)| Doomed {
                    entry_id: entry.id,
                    uid: entry.uid,
                    sequence,
                })
                .collect(),
        }
    }

    /// Plans a `UID EXPUNGE`: the `\Deleted` placements whose UIDs the set
    /// names (RFC 4315 §2.1).
    ///
    /// The set is resolved through [`SequenceSet::resolve_uids`], so `*` means
    /// the largest UID in use, UID gaps are skipped rather than shifting the
    /// selection along, a repeated UID selects one message, and a UID naming no
    /// message in this mailbox selects nothing. None of those is an error.
    #[must_use]
    pub fn deleted_in_set(map: &SequenceMap, set: &SequenceSet) -> Self {
        Self {
            doomed: set
                .resolve_uids(map)
                .into_iter()
                .filter(|(_, entry)| entry.is_deleted())
                .map(|(sequence, entry)| Doomed {
                    entry_id: entry.id,
                    uid: entry.uid,
                    sequence,
                })
                .collect(),
        }
    }

    /// Whether the plan removes nothing.
    ///
    /// An empty plan is a successful `EXPUNGE` that reports no untagged lines —
    /// the ordinary outcome when nothing is marked `\Deleted`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.doomed.is_empty()
    }

    /// How many placements the plan names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.doomed.len()
    }

    /// The placements, in ascending sequence order.
    pub fn iter(&self) -> impl Iterator<Item = &Doomed> {
        self.doomed.iter()
    }

    /// The placements in the order they must be **reported**: highest sequence
    /// number first.
    ///
    /// RFC 9051 §7.4.1 has each `* <seq> EXPUNGE` decrement the sequence
    /// numbers of everything above it, so a client applies the reports one at a
    /// time to its own list. Reporting descending means every number is still
    /// valid when the client reaches it:
    ///
    /// ```text
    ///   removing seq 2 and 4 of 5
    ///
    ///   descending (correct)        ascending (wrong)
    ///   * 4 EXPUNGE  → 1,2,3,5      * 2 EXPUNGE  → 1,3,4,5
    ///   * 2 EXPUNGE  → 1,3,5        * 4 EXPUNGE  → 1,3,4   ← removed the wrong one
    /// ```
    ///
    /// The ascending column loses message 5 and keeps message 4: the second
    /// number was computed before the first removal but applied after it. That
    /// is the bug this ordering exists to prevent, and it is invisible unless a
    /// single command removes two messages.
    pub fn report_order(&self) -> impl Iterator<Item = &Doomed> {
        self.doomed.iter().rev()
    }

    /// The UIDs the plan names, for updating the snapshot's membership.
    #[must_use]
    pub fn uids(&self) -> BTreeSet<u32> {
        self.doomed.iter().map(|d| d.uid.get()).collect()
    }

    /// Keeps only the placements storage actually removed.
    ///
    /// A placement another session expunged first is dropped here, so the
    /// client is never told a message vanished when this command did not remove
    /// it — no false `EXPUNGE` line, and no phantom removal from the snapshot.
    pub fn retain_removed(&mut self, removed: &BTreeSet<EntryId>) {
        self.doomed.retain(|d| removed.contains(&d.entry_id));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use sandesh_core::{MailboxId, MessageEntry, MessageFlags, MessageId, SystemFlags, Timestamp};

    use super::*;

    /// A placement with the given UID, `\Deleted` or not.
    fn entry(uid: u32, deleted: bool) -> MessageEntry {
        let flags = if deleted {
            MessageFlags::from_system(SystemFlags::DELETED)
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

    fn map(entries: Vec<MessageEntry>) -> SequenceMap {
        SequenceMap::new(entries)
    }

    #[test]
    fn an_expunge_plans_only_deleted_placements() {
        let m = map(vec![entry(1, false), entry(2, true), entry(3, false)]);
        let plan = ExpungePlan::all_deleted(&m);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.iter().next().unwrap().sequence, 2);
    }

    #[test]
    fn sequence_numbers_are_those_held_before_removal() {
        // UIDs 10, 20, 30, 40 — the second and fourth are \Deleted, so their
        // pre-removal sequence numbers are 2 and 4, not 1 and 2.
        let m = map(vec![
            entry(10, false),
            entry(20, true),
            entry(30, false),
            entry(40, true),
        ]);
        let plan = ExpungePlan::all_deleted(&m);
        let sequences: Vec<usize> = plan.iter().map(|d| d.sequence).collect();
        assert_eq!(sequences, vec![2, 4]);
    }

    #[test]
    fn reports_run_highest_sequence_first() {
        let m = map(vec![
            entry(10, true),
            entry(20, false),
            entry(30, true),
            entry(40, true),
        ]);
        let plan = ExpungePlan::all_deleted(&m);
        let reported: Vec<usize> = plan.report_order().map(|d| d.sequence).collect();
        assert_eq!(
            reported,
            vec![4, 3, 1],
            "must be descending (RFC 9051 §7.4.1)"
        );
    }

    #[test]
    fn an_empty_mailbox_plans_nothing() {
        assert!(ExpungePlan::all_deleted(&map(Vec::new())).is_empty());
    }

    #[test]
    fn nothing_deleted_plans_nothing() {
        let m = map(vec![entry(1, false), entry(2, false)]);
        assert!(ExpungePlan::all_deleted(&m).is_empty());
    }

    // --- UID EXPUNGE ------------------------------------------------------

    #[test]
    fn uid_expunge_is_the_intersection_of_the_set_and_deleted() {
        // UID 1 is in the set but not \Deleted; UID 3 is \Deleted but not in
        // the set. Only UID 2 satisfies both.
        let m = map(vec![entry(1, false), entry(2, true), entry(3, true)]);
        let set = SequenceSet::parse("1,2").unwrap();
        let plan = ExpungePlan::deleted_in_set(&m, &set);
        assert_eq!(plan.uids(), BTreeSet::from([2]));
    }

    #[test]
    fn uid_expunge_uses_uids_not_sequence_numbers() {
        // Sparse UIDs: sequence 1,2,3 hold UIDs 5,6,7. A set naming "1:3"
        // would select all three if it were read as sequence numbers, and
        // nothing at all read correctly as UIDs.
        let m = map(vec![entry(5, true), entry(6, true), entry(7, true)]);
        let plan = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("1:3").unwrap());
        assert!(
            plan.is_empty(),
            "UIDs 1-3 name no message; sequence numbers must not leak in"
        );

        let plan = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("6").unwrap());
        assert_eq!(plan.uids(), BTreeSet::from([6]));
    }

    #[test]
    fn uid_expunge_skips_gaps_rather_than_shifting_along() {
        // UIDs 2 and 7 exist; 3..=6 were expunged earlier. `2:6` names the
        // gap, and must select only UID 2.
        let m = map(vec![entry(2, true), entry(7, true)]);
        let plan = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("2:6").unwrap());
        assert_eq!(plan.uids(), BTreeSet::from([2]));
    }

    #[test]
    fn a_star_means_the_largest_uid_in_use() {
        let m = map(vec![entry(4, true), entry(9, true), entry(17, true)]);
        let plan = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("9:*").unwrap());
        assert_eq!(plan.uids(), BTreeSet::from([9, 17]));

        let plan = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("*").unwrap());
        assert_eq!(plan.uids(), BTreeSet::from([17]));
    }

    #[test]
    fn a_duplicated_uid_selects_one_message() {
        let m = map(vec![entry(4, true), entry(9, true)]);
        let plan = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("4,4,4:4").unwrap());
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn a_uid_naming_no_message_selects_nothing_and_is_not_an_error() {
        let m = map(vec![entry(4, true)]);
        let plan = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("99").unwrap());
        assert!(plan.is_empty());
    }

    #[test]
    fn a_descending_range_names_the_same_uids() {
        let m = map(vec![entry(4, true), entry(9, true), entry(17, true)]);
        let up = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("4:9").unwrap());
        let down = ExpungePlan::deleted_in_set(&m, &SequenceSet::parse("9:4").unwrap());
        assert_eq!(up.uids(), down.uids());
    }

    // --- applying what storage actually did -------------------------------

    #[test]
    fn placements_storage_did_not_remove_are_dropped_from_the_plan() {
        let m = map(vec![entry(1, true), entry(2, true)]);
        let mut plan = ExpungePlan::all_deleted(&m);
        let kept = plan.iter().next().unwrap().entry_id;
        plan.retain_removed(&BTreeSet::from([kept]));
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.iter().next().unwrap().entry_id, kept);
    }

    #[test]
    fn a_plan_nothing_removed_from_reports_nothing() {
        let m = map(vec![entry(1, true)]);
        let mut plan = ExpungePlan::all_deleted(&m);
        plan.retain_removed(&BTreeSet::new());
        assert!(
            plan.is_empty(),
            "a failed removal must emit no EXPUNGE line"
        );
        assert!(plan.uids().is_empty());
    }
}

//! `MOVE` and `UID MOVE` (RFC 6851): planning the move, and reporting it
//! honestly when only part of it succeeds.
//!
//! This module is pure. The session resolves the client's set through the
//! selected mailbox's [`SequenceMap`], builds a [`MovePlan`] against that
//! unmutated snapshot, calls [`sandesh_core::MessageStore::move_entry`] once
//! per placement, and records each outcome here. This module then renders the
//! `[COPYUID]` code and the `* <seq> EXPUNGE` lines.
//!
//! ```text
//! SequenceMap ──▶ MovePlan (entry_id, uid, seq-before)
//!                        │
//!                        ▼
//!              move_entry per placement       ← storage assigns the new UID
//!                        │                      and removes the source
//!                 ┌──────┴───────┐
//!                 ▼              ▼
//!         (src uid, dst uid)   * <seq> EXPUNGE
//!                 │            highest sequence first
//!                 ▼
//!   [COPYUID <dst uidvalidity> <src set> <dst set>]
//! ```
//!
//! # Why the plan is built before anything moves
//!
//! Exactly the reason [`crate::expunge`] builds its plan first: a sequence
//! number is a *position*, and it stops being true the moment a message ahead
//! of it leaves the mailbox. `MOVE` both removes from the source and reports
//! those removals as `* <seq> EXPUNGE`, so every reported number must come from
//! one snapshot taken before the first removal (RFC 9051 §7.4.1).
//!
//! # Why `MOVE` is `COPY` plus `EXPUNGE`, and why that matters here
//!
//! RFC 6851 §3.3 defines `MOVE` as equivalent to `COPY` followed by marking
//! `\Deleted` and expunging — but performed as one operation, so a client never
//! observes the intermediate state where the message exists in both places or
//! in neither. That equivalence is why the `[COPYUID]` renderer is shared with
//! [`crate::copy`] verbatim rather than reimplemented: the pairing rule is
//! identical, and a second renderer would be a second chance to get it wrong.
//!
//! # Partial success is reported as failure
//!
//! Storage moves one placement per transaction (`MessageStore::move_entry`), so
//! a multi-message `MOVE` is a sequence of commits rather than a single one. If
//! an earlier placement moves and a later one does not, the command did *not*
//! do what the client asked. RFC 6851 §3.3 requires that a `MOVE` which fails
//! part-way still report the messages it did move — the moves are real and
//! already committed, and hiding them would leave the client's view wrong — but
//! the tagged response must be `NO`, because `OK` would assert that every named
//! message moved. [`MovePlan::is_partial`] is what the session tests to decide.
//! ADR-052 records the limitation and why it is not papered over.

use std::collections::BTreeSet;

use sandesh_core::{EntryId, Uid, UidValidity};

use crate::copy::CopyResult;
use crate::mailbox::SequenceMap;
use crate::sequence::SequenceSet;

/// One placement to move, named three ways.
///
/// All three are needed and none survives the move: the `entry_id` addresses
/// storage, the `uid` pairs into `[COPYUID]` and updates the snapshot, and the
/// `sequence` is what the client is told — a number that stops being true as
/// soon as the first removal lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moving {
    /// The placement to move. A *placement*, never a message: another
    /// mailbox's placement of the same message is a different `EntryId` and is
    /// untouched.
    pub entry_id: EntryId,
    /// The UID it holds in the source mailbox.
    pub uid: Uid,
    /// The sequence number it held *before* any removal in this command.
    pub sequence: usize,
}

/// What one `MOVE` will move, decided against an unmutated snapshot, and what
/// actually moved once storage has answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovePlan {
    /// Every placement the client's set named, in ascending UID order.
    planned: Vec<Moving>,
    /// The placements storage actually moved, in the order it moved them.
    moved: Vec<Moving>,
    /// Source UID paired with the destination UID storage assigned.
    copied: CopyResult,
}

impl MovePlan {
    /// Plans a `MOVE` from a client's set.
    ///
    /// `uid_mode` selects how the set is read: as UIDs for `UID MOVE`, as
    /// sequence numbers for `MOVE`. Both resolve through the same
    /// [`SequenceMap`], which is what keeps the two forms consistent with each
    /// other and with every other command (phase brief §6).
    ///
    /// Out-of-range members are not errors (RFC 9051 §6.4.8, RFC 6851 §3.3):
    /// they name nothing and contribute nothing, so a set that resolves to no
    /// message plans no move rather than failing.
    #[must_use]
    pub fn resolve(map: &SequenceMap, set: &SequenceSet, uid_mode: bool) -> Self {
        let resolved = if uid_mode {
            set.resolve_uids(map)
        } else {
            set.resolve(map)
        };

        Self {
            planned: resolved
                .into_iter()
                .map(|(sequence, entry)| Moving {
                    entry_id: entry.id,
                    uid: entry.uid,
                    sequence,
                })
                .collect(),
            moved: Vec::new(),
            copied: CopyResult::new(),
        }
    }

    /// The placements to attempt, in ascending UID order.
    pub fn iter(&self) -> impl Iterator<Item = &Moving> {
        self.planned.iter()
    }

    /// Whether the set named no message at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.planned.is_empty()
    }

    /// How many placements the set named.
    #[must_use]
    pub fn planned_len(&self) -> usize {
        self.planned.len()
    }

    /// How many placements actually moved.
    #[must_use]
    pub fn moved_len(&self) -> usize {
        self.moved.len()
    }

    /// Records one completed move.
    ///
    /// Called only after `move_entry` returned, with the UID it returned. A
    /// placement whose move failed is never recorded, so it can appear neither
    /// in `[COPYUID]` nor among the `EXPUNGE` lines — the client is never told
    /// a message left a mailbox it is still in.
    pub fn record(&mut self, moving: Moving, destination: Uid) {
        self.copied.record(moving.uid, destination);
        self.moved.push(moving);
    }

    /// Whether some placements moved and others did not.
    ///
    /// The condition that makes the tagged response `NO` rather than `OK`: the
    /// command did not do everything it was asked, and saying `OK` would claim
    /// otherwise. An empty plan is not partial — nothing was asked, so nothing
    /// was missed.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.moved.len() != self.planned.len()
    }

    /// The `COPYUID` response code, or `None` when nothing moved.
    ///
    /// RFC 6851 §4.3: `MOVE` reports `[COPYUID]` exactly as `COPY` does, naming
    /// the destination's UIDVALIDITY and pairing the sets positionally. The
    /// renderer is [`crate::copy`]'s, unmodified.
    #[must_use]
    pub fn copyuid(&self, destination_validity: UidValidity) -> Option<String> {
        self.copied.copyuid(destination_validity)
    }

    /// The moved placements, highest sequence number first.
    ///
    /// The order RFC 9051 §7.4.1 requires for `* <seq> EXPUNGE`: reporting the
    /// highest first means every number is still valid when the client applies
    /// it, because removing a message never renumbers one ahead of it.
    #[must_use]
    pub fn report_order(&self) -> Vec<Moving> {
        let mut ordered = self.moved.clone();
        ordered.sort_by_key(|moving| core::cmp::Reverse(moving.sequence));
        ordered
    }

    /// The UIDs that actually left the source mailbox.
    ///
    /// What the acting session removes from its own snapshot, and what a
    /// subscribed session is told about. Only *moved* placements appear: a
    /// placement storage refused to move is still in the mailbox.
    #[must_use]
    pub fn moved_uids(&self) -> BTreeSet<u32> {
        self.moved.iter().map(|m| m.uid.get()).collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use sandesh_core::{MailboxId, MessageFlags, MessageId, Timestamp};

    use super::*;
    use sandesh_core::MessageEntry;

    fn uid(value: u32) -> Uid {
        Uid::new(value).unwrap_or_else(|| panic!("UID {value} must be non-zero"))
    }

    /// A placement with the given UID, in one fixed mailbox.
    fn entry(value: u32) -> MessageEntry {
        MessageEntry {
            id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: MailboxId::new(),
            uid: uid(value),
            flags: MessageFlags::empty(),
            internal_date: Timestamp::now(),
            added_at: Timestamp::now(),
            mod_seq: sandesh_core::ModSeq::MIN,
        }
    }

    /// A mailbox holding UIDs 4, 9, 17, 23 — deliberately gapped, because a
    /// contiguous mailbox cannot tell sequence numbers and UIDs apart.
    fn gapped() -> SequenceMap {
        SequenceMap::new(vec![entry(4), entry(9), entry(17), entry(23)])
    }

    fn set(text: &str) -> SequenceSet {
        SequenceSet::parse(text).expect("test set must parse")
    }

    // --- planning ---------------------------------------------------------

    #[test]
    fn sequence_mode_reads_positions_not_uids() {
        // `MOVE 2` names the second message, UID 9 — not UID 2, which does not
        // exist. This is the confusion the gapped mailbox exists to catch.
        let plan = MovePlan::resolve(&gapped(), &set("2"), false);
        let planned: Vec<u32> = plan.iter().map(|m| m.uid.get()).collect();
        assert_eq!(planned, vec![9]);
    }

    #[test]
    fn uid_mode_reads_uids_not_positions() {
        // `UID MOVE 9` names UID 9, which is at position 2.
        let plan = MovePlan::resolve(&gapped(), &set("9"), true);
        let planned: Vec<u32> = plan.iter().map(|m| m.uid.get()).collect();
        assert_eq!(planned, vec![9]);
        assert_eq!(plan.iter().next().map(|m| m.sequence), Some(2));
    }

    #[test]
    fn a_uid_range_spans_gaps() {
        // UIDs 5..=17 names 9 and 17; 5 and the gap between are simply absent.
        let plan = MovePlan::resolve(&gapped(), &set("5:17"), true);
        let planned: Vec<u32> = plan.iter().map(|m| m.uid.get()).collect();
        assert_eq!(planned, vec![9, 17]);
    }

    #[test]
    fn a_descending_range_names_the_same_messages() {
        // RFC 9051 §9: `9:4` and `4:9` are the same set.
        let ascending = MovePlan::resolve(&gapped(), &set("4:9"), true);
        let descending = MovePlan::resolve(&gapped(), &set("9:4"), true);
        let a: Vec<u32> = ascending.iter().map(|m| m.uid.get()).collect();
        let d: Vec<u32> = descending.iter().map(|m| m.uid.get()).collect();
        assert_eq!(a, d);
    }

    #[test]
    fn duplicate_members_move_a_message_once() {
        // `4,4,4` names one message. Moving it three times would move it once
        // and then fail twice against a placement that no longer exists.
        let plan = MovePlan::resolve(&gapped(), &set("4,4,4"), true);
        assert_eq!(plan.planned_len(), 1);
    }

    #[test]
    fn nonexistent_members_are_skipped_not_refused() {
        // RFC 9051 §6.4.8: naming a message that is not there is not an error.
        let plan = MovePlan::resolve(&gapped(), &set("7"), true);
        assert!(plan.is_empty());
    }

    #[test]
    fn a_mixed_set_moves_the_members_that_exist() {
        // 4 and 17 exist; 7 does not. The command moves two messages.
        let plan = MovePlan::resolve(&gapped(), &set("4,7,17"), true);
        let planned: Vec<u32> = plan.iter().map(|m| m.uid.get()).collect();
        assert_eq!(planned, vec![4, 17]);
    }

    #[test]
    fn star_names_the_highest_uid() {
        // `*` is the largest UID in use, not the largest possible.
        let plan = MovePlan::resolve(&gapped(), &set("*"), true);
        let planned: Vec<u32> = plan.iter().map(|m| m.uid.get()).collect();
        assert_eq!(planned, vec![23]);
    }

    #[test]
    fn a_star_range_reaches_the_end() {
        let plan = MovePlan::resolve(&gapped(), &set("17:*"), true);
        let planned: Vec<u32> = plan.iter().map(|m| m.uid.get()).collect();
        assert_eq!(planned, vec![17, 23]);
    }

    #[test]
    fn planning_against_an_empty_mailbox_plans_nothing() {
        let plan = MovePlan::resolve(&SequenceMap::default(), &set("1:*"), false);
        assert!(plan.is_empty());
    }

    // --- recording and reporting ------------------------------------------

    /// A plan over UIDs 4, 9, 17 with every move recorded, destinations 100+.
    fn fully_moved() -> MovePlan {
        let map = gapped();
        let mut plan = MovePlan::resolve(&map, &set("4,9,17"), true);
        let planned: Vec<Moving> = plan.iter().copied().collect();
        for (offset, moving) in planned.into_iter().enumerate() {
            plan.record(moving, uid(100 + u32::try_from(offset).unwrap_or(0)));
        }
        plan
    }

    #[test]
    fn copyuid_pairs_sources_with_the_destinations_storage_returned() {
        // The load-bearing assertion: the nth source names the same message as
        // the nth destination. Sources 4,9,17 are not consecutive, so they
        // render as singletons; destinations 100,101,102 are, so they coalesce.
        let plan = fully_moved();
        let code = plan.copyuid(UidValidity::new(42)).expect("moves recorded");
        assert_eq!(code, "COPYUID 42 4,9,17 100:102");
    }

    #[test]
    fn copyuid_is_absent_when_nothing_moved() {
        // RFC 4315 §3: an empty set would be malformed, so the code is omitted
        // rather than rendered empty.
        let plan = MovePlan::resolve(&gapped(), &set("7"), true);
        assert_eq!(plan.copyuid(UidValidity::new(42)), None);
    }

    #[test]
    fn copyuid_names_only_the_placements_that_moved() {
        // One of three moves failed. The failed one must not appear in either
        // set, or the pairing would name a message that never arrived.
        let map = gapped();
        let mut plan = MovePlan::resolve(&map, &set("4,9,17"), true);
        let planned: Vec<Moving> = plan.iter().copied().collect();
        plan.record(planned[0], uid(100));
        plan.record(planned[2], uid(101));

        let code = plan.copyuid(UidValidity::new(42)).expect("two moved");
        assert_eq!(code, "COPYUID 42 4,17 100:101");
    }

    #[test]
    fn expunge_lines_run_highest_sequence_first() {
        // RFC 9051 §7.4.1. UIDs 4,9,17 sit at positions 1,2,3, so the client is
        // told 3, then 2, then 1 — each still valid when it is applied.
        let plan = fully_moved();
        let sequences: Vec<usize> = plan.report_order().iter().map(|m| m.sequence).collect();
        assert_eq!(sequences, vec![3, 2, 1]);
    }

    #[test]
    fn only_moved_placements_are_reported_as_expunged() {
        // A placement storage refused to move is still in the mailbox; telling
        // the client it left would corrupt the client's sequence numbering.
        let map = gapped();
        let mut plan = MovePlan::resolve(&map, &set("4,9,17"), true);
        let planned: Vec<Moving> = plan.iter().copied().collect();
        plan.record(planned[0], uid(100));

        let sequences: Vec<usize> = plan.report_order().iter().map(|m| m.sequence).collect();
        assert_eq!(sequences, vec![1]);
        assert_eq!(plan.moved_uids(), BTreeSet::from([4]));
    }

    #[test]
    fn the_recorded_order_matches_the_copyuid_order() {
        // `record` appends to two collections — the `[COPYUID]` pairs and the
        // moved placements — and nothing outside this type keeps them in step.
        // If one grew in a different order from the other, `report_order` would
        // still look right (it sorts), but the two would describe different
        // messages at the same index. Pinned here, because a mutation that
        // reversed one of them survived every other test in this file.
        let map = gapped();
        let mut plan = MovePlan::resolve(&map, &set("4,9,17"), true);
        let planned: Vec<Moving> = plan.iter().copied().collect();
        for (offset, moving) in planned.into_iter().enumerate() {
            plan.record(moving, uid(100 + u32::try_from(offset).unwrap_or(0)));
        }

        let recorded: Vec<u32> = plan.moved.iter().map(|m| m.uid.get()).collect();
        let paired: Vec<u32> = plan.copied.iter().map(|c| c.source.get()).collect();
        assert_eq!(
            recorded, paired,
            "the nth recorded move and the nth COPYUID pair must be the same message"
        );
    }

    #[test]
    fn a_fully_completed_move_is_not_partial() {
        assert!(!fully_moved().is_partial());
    }

    #[test]
    fn a_move_that_lost_one_placement_is_partial() {
        let map = gapped();
        let mut plan = MovePlan::resolve(&map, &set("4,9"), true);
        let planned: Vec<Moving> = plan.iter().copied().collect();
        plan.record(planned[0], uid(100));
        assert!(plan.is_partial());
    }

    #[test]
    fn an_empty_plan_is_not_partial() {
        // Nothing was asked, so nothing was missed: `OK` is the honest answer.
        let plan = MovePlan::resolve(&gapped(), &set("7"), true);
        assert!(!plan.is_partial());
    }

    #[test]
    fn moved_uids_reports_what_left_the_mailbox() {
        assert_eq!(fully_moved().moved_uids(), BTreeSet::from([4, 9, 17]));
    }
}

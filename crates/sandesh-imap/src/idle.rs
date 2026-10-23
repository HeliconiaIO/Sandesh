//! `IDLE` (RFC 2177): turning domain events into unsolicited mailbox updates.
//!
//! This module is pure. The session subscribes to the existing
//! [`sandesh_events::EventBus`], hands each [`DomainEvent`] to
//! [`Update::from_event`], and writes whatever comes back. Deciding *what a
//! change means to a client* is protocol work and lives here; deciding *what
//! changed* is storage's, and already happened.
//!
//! ```text
//!  storage mutation
//!        │  emits
//!        ▼
//!   DomainEvent ──▶ Update::from_event(selected mailbox, SequenceMap)
//!                          │
//!            ┌─────────────┼──────────────┐
//!            ▼             ▼              ▼
//!      * n EXISTS    * n EXPUNGE   * n FETCH (FLAGS (…))
//! ```
//!
//! # Mailbox scoping is the load-bearing rule
//!
//! Every event names the mailbox it happened in, and [`Update::from_event`]
//! returns `None` for any event naming a mailbox other than the selected one.
//! A session idling on `INBOX` must never learn that `Archive` changed — that
//! would be a cross-mailbox information leak as well as a protocol error, since
//! the sequence numbers in such a response would address the wrong mailbox
//! entirely (phase brief §4).
//!
//! # Why there is no second event system
//!
//! The bus already guarantees what `IDLE` needs: a total order, bounded
//! per-subscriber queues, and a producer that never blocks on a slow consumer
//! (`EVENT_MODEL.md`). A stalled IMAP client therefore cannot throttle SMTP
//! delivery, and an `IDLE` session cannot accumulate events without limit —
//! when it falls too far behind the bus drops the oldest and tells it how many
//! it missed. This module adds no queue of its own.
//!
//! # What an event is *not* allowed to do
//!
//! An event never renumbers a client's view on its own. `* n EXISTS` reports
//! the new message count and `* n EXPUNGE` reports a removal; both are applied
//! to the session's [`SequenceMap`] by the caller, in the same order the client
//! was told, so that the numbers the client holds and the numbers the session
//! holds cannot diverge (phase brief §6).

use std::collections::BTreeSet;

use sandesh_core::{DomainEvent, EventPayload, Keyword, MailboxId, MessageFlags, SystemFlags, Uid};

use crate::fetch_response;
use crate::mailbox::SequenceMap;
use crate::response;

/// One unsolicited response, and the snapshot change that must accompany it.
///
/// The two travel together deliberately. A caller that wrote the line but
/// forgot the snapshot change — or made the change but wrote the wrong line —
/// would leave the client and the session disagreeing about what sequence
/// number names what message, which is the failure this type exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// A message arrived: the mailbox now holds this many messages.
    ///
    /// RFC 9051 §7.4.1. The placement must be added to the session's snapshot
    /// *before* the count is rendered, because the count is the snapshot's
    /// length — which is what keeps `EXISTS` and a following `FETCH n` agreeing.
    Exists {
        /// The UID that arrived, for the snapshot.
        uid: Uid,
    },
    /// A message was removed from the mailbox.
    ///
    /// Carries the UID rather than a sequence number: the number depends on the
    /// snapshot, and resolving it here would use a view that may already have
    /// moved on. The caller resolves it against its own snapshot and then
    /// removes it.
    Expunge {
        /// The UID that left.
        uid: Uid,
    },
    /// A message's flags changed.
    ///
    /// RFC 9051 §7.5.2 reports this as an untagged `FETCH`, because there is no
    /// dedicated flag-change response: a flag update *is* a `FETCH` result the
    /// client did not ask for.
    Flags {
        /// The placement whose flags changed.
        uid: Uid,
        /// The flags as storage now holds them.
        flags: MessageFlags,
        /// The modification sequence the change was stamped with.
        ///
        /// Reported only to a session that enabled `CONDSTORE`; see
        /// [`flags_line`].
        mod_seq: sandesh_core::ModSeq,
    },
}

impl Update {
    /// Maps a domain event to the update a session selecting `mailbox` needs.
    ///
    /// Returns `None` when the event concerns a different mailbox, or when it
    /// describes something a selected session has no unsolicited response for
    /// (a mailbox rename, an account change). Silence is the correct answer for
    /// both: inventing a response for an event that has none would put bytes on
    /// the wire that no client asked for and no RFC defines.
    #[must_use]
    pub fn from_event(event: &DomainEvent, mailbox: MailboxId) -> Option<Self> {
        match &event.payload {
            // A new placement in this mailbox: delivery, COPY, MOVE or APPEND.
            // All four are the same fact to a client — a message arrived.
            EventPayload::EntryCreated {
                mailbox_id, uid, ..
            }
            | EventPayload::MessageReceived {
                mailbox_id, uid, ..
            } if *mailbox_id == mailbox => Some(Self::Exists { uid: *uid }),

            EventPayload::EntryDeleted {
                mailbox_id, uid, ..
            } if *mailbox_id == mailbox => Some(Self::Expunge { uid: *uid }),

            // The event carries the resulting flags as *names*, because the bus
            // is shared with consumers that have no `MessageFlags` type. They
            // are parsed back through the same vocabulary `STORE` uses, so a
            // name this server cannot store cannot arrive here either.
            EventPayload::EntryFlagsChanged {
                mailbox_id,
                uid,
                flags_now,
                mod_seq,
                ..
            } if *mailbox_id == mailbox => Some(Self::Flags {
                uid: *uid,
                flags: flags_from_names(flags_now),
                mod_seq: *mod_seq,
            }),

            // Everything else — another mailbox's change, a mailbox or account
            // event, a delivery failure — produces nothing.
            _ => None,
        }
    }

    /// The UID this update concerns.
    #[must_use]
    pub const fn uid(&self) -> Uid {
        match self {
            Self::Exists { uid } | Self::Expunge { uid } | Self::Flags { uid, .. } => *uid,
        }
    }
}

/// Rebuilds a [`MessageFlags`] from the flag names an event carries.
///
/// A name this server does not recognise is dropped rather than guessed at:
/// the alternative is reporting a flag the client cannot have set and this
/// server cannot store.
fn flags_from_names(names: &[String]) -> MessageFlags {
    let mut system = SystemFlags::NONE;
    let mut keywords = BTreeSet::new();
    for name in names {
        if name.starts_with('\\') {
            if let Some(flag) = SystemFlags::from_name(name) {
                system = system.union(flag);
            }
        } else if let Some(keyword) = Keyword::new(name.clone()) {
            keywords.insert(keyword);
        }
    }
    MessageFlags::new(system, keywords)
}

/// Renders `* <count> EXISTS`, given the snapshot *after* the arrival.
#[must_use]
pub fn exists_line(count: usize) -> String {
    response::count_line(count, "EXISTS")
}

/// Renders `* <sequence> EXPUNGE`, given the sequence number *before* removal.
#[must_use]
pub fn expunge_line(sequence: usize) -> String {
    response::count_line(sequence, "EXPUNGE")
}

/// Renders `* <sequence> FETCH (UID <uid> FLAGS (…))`.
///
/// The `UID` item is included alongside the flags even though the client did
/// not ask for it. RFC 9051 §7.5.2 permits it, and it is what makes the
/// response unambiguous to a client whose own numbering may be a step behind:
/// the UID identifies the message even if the sequence number does not.
#[must_use]
pub fn flags_line(
    sequence: usize,
    uid: Uid,
    flags: &MessageFlags,
    mod_seq: Option<sandesh_core::ModSeq>,
) -> String {
    // RFC 7162 §3.2: a CONDSTORE-enabled session's unsolicited flag report
    // carries `MODSEQ`, so a client learns the revision another session's
    // change produced and can keep its own high-water mark current without
    // re-issuing a FETCH.
    //
    // Gated on the session having enabled CONDSTORE, not merely on the server
    // implementing it: RFC 5161 §3 entitles a client that did not enable an
    // extension to a parser that does not know its syntax, and an unsolicited
    // response is exactly where an unexpected item would break one.
    match mod_seq {
        Some(mod_seq) => format!(
            "* {} FETCH (UID {} MODSEQ ({}) FLAGS {})\r\n",
            sequence,
            uid.get(),
            mod_seq.get(),
            fetch_response::flags(flags)
        ),
        None => format!(
            "* {} FETCH (UID {} FLAGS {})\r\n",
            sequence,
            uid.get(),
            fetch_response::flags(flags)
        ),
    }
}

/// Applies an update to a session's snapshot and renders its response line.
///
/// Returns `None` when the update names a message the snapshot cannot place —
/// an `EXPUNGE` for a UID already gone, or a flag change for one never seen.
/// Both are ordinary under concurrency, and both must produce **no** line: a
/// sequence number invented for a message this session does not have would
/// corrupt the client's view of every message after it.
///
/// The snapshot change and the rendering happen together here for the reason
/// [`Update`] documents — separating them is how the two views drift apart.
pub fn apply(
    update: &Update,
    sequences: &mut SequenceMap,
    entry: Option<&sandesh_core::MessageEntry>,
    condstore: bool,
) -> Option<String> {
    match update {
        Update::Exists { uid } => {
            // The placement itself is needed to extend the snapshot: a count
            // alone would leave a hole that a following `FETCH` could not fill.
            let entry = entry?;
            if sequences.by_uid(*uid).is_some() {
                // Already present. A duplicate arrival announcement would tell
                // the client the mailbox grew when it did not.
                return None;
            }
            sequences.insert(entry.clone());
            Some(exists_line(sequences.len()))
        }
        Update::Expunge { uid } => {
            // Resolved before removal: after it, the number is someone else's.
            let sequence = sequences.sequence_of(*uid)?;
            sequences.remove_uids(&core::iter::once(uid.get()).collect());
            Some(expunge_line(sequence))
        }
        Update::Flags {
            uid,
            flags,
            mod_seq,
        } => {
            let sequence = sequences.sequence_of(*uid)?;
            // The event carries the sequence storage allocated, so the
            // idling session's own snapshot advances with the change rather
            // than keeping the value it held at SELECT.
            sequences.set_flags(*uid, flags.clone(), Some(*mod_seq));
            Some(flags_line(
                sequence,
                *uid,
                flags,
                condstore.then_some(*mod_seq),
            ))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use sandesh_core::{
        AccountId, EntryId, EventId, MessageEntry, MessageId, SystemFlags, Timestamp,
    };

    use super::*;

    fn uid(value: u32) -> Uid {
        Uid::new(value).unwrap_or_else(|| panic!("UID {value} must be non-zero"))
    }

    /// A placement with the given UID in the given mailbox.
    fn entry_in(mailbox: MailboxId, value: u32) -> MessageEntry {
        MessageEntry {
            id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: mailbox,
            uid: uid(value),
            flags: MessageFlags::empty(),
            internal_date: Timestamp::now(),
            added_at: Timestamp::now(),
            mod_seq: sandesh_core::ModSeq::MIN,
        }
    }

    /// Wraps a payload as a delivered event.
    fn event(payload: EventPayload) -> DomainEvent {
        DomainEvent {
            id: EventId::new(),
            seq: 1,
            occurred_at: Timestamp::now(),
            payload,
        }
    }

    fn entry_created(mailbox: MailboxId, value: u32) -> DomainEvent {
        event(EventPayload::EntryCreated {
            entry_id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: mailbox,
            uid: uid(value),
        })
    }

    fn entry_deleted(mailbox: MailboxId, value: u32) -> DomainEvent {
        event(EventPayload::EntryDeleted {
            entry_id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: mailbox,
            uid: uid(value),
        })
    }

    fn flags_changed(mailbox: MailboxId, value: u32, flags: &MessageFlags) -> DomainEvent {
        event(EventPayload::EntryFlagsChanged {
            entry_id: EntryId::new(),
            mailbox_id: mailbox,
            uid: uid(value),
            added: Vec::new(),
            removed: Vec::new(),
            flags_now: flags.all_names(),
            mod_seq: sandesh_core::ModSeq::new(42).unwrap_or(sandesh_core::ModSeq::MIN),
        })
    }

    // --- mailbox scoping --------------------------------------------------

    #[test]
    fn an_event_in_the_selected_mailbox_produces_an_update() {
        let inbox = MailboxId::new();
        assert_eq!(
            Update::from_event(&entry_created(inbox, 4), inbox),
            Some(Update::Exists { uid: uid(4) })
        );
    }

    #[test]
    fn an_event_in_another_mailbox_produces_nothing() {
        // The isolation rule: a change in Archive must never reach a session
        // selecting INBOX.
        let inbox = MailboxId::new();
        let archive = MailboxId::new();
        assert_eq!(Update::from_event(&entry_created(archive, 4), inbox), None);
        assert_eq!(Update::from_event(&entry_deleted(archive, 4), inbox), None);
        assert_eq!(
            Update::from_event(&flags_changed(archive, 4, &MessageFlags::empty()), inbox),
            None
        );
    }

    #[test]
    fn a_mailbox_level_event_produces_nothing() {
        // There is no unsolicited response for a rename, so none is invented.
        let inbox = MailboxId::new();
        let renamed = event(EventPayload::MailboxRenamed {
            mailbox_id: inbox,
            old_name: String::from("INBOX"),
            new_name: String::from("Renamed"),
        });
        assert_eq!(Update::from_event(&renamed, inbox), None);
    }

    // --- event mapping ----------------------------------------------------

    #[test]
    fn a_delivery_is_an_arrival_like_any_other() {
        // `MessageReceived` and `EntryCreated` are the same fact to a client.
        let inbox = MailboxId::new();
        let received = event(EventPayload::MessageReceived {
            message_id: MessageId::new(),
            entry_id: EntryId::new(),
            mailbox_id: inbox,
            account_id: AccountId::new(),
            uid: uid(7),
            size: 42,
        });
        assert_eq!(
            Update::from_event(&received, inbox),
            Some(Update::Exists { uid: uid(7) })
        );
    }

    #[test]
    fn a_removal_maps_to_expunge() {
        let inbox = MailboxId::new();
        assert_eq!(
            Update::from_event(&entry_deleted(inbox, 9), inbox),
            Some(Update::Expunge { uid: uid(9) })
        );
    }

    #[test]
    fn a_flag_change_carries_the_new_flags_and_its_modseq() {
        // The modseq must come *from the event*, not be defaulted: the helper
        // emits 42, so a translation that substituted the floor would be
        // caught here. A wrong modseq is worse than none — it would tell the
        // client a revision it can then never reconcile.
        let inbox = MailboxId::new();
        let flags = MessageFlags::from_system(SystemFlags::SEEN);
        assert_eq!(
            Update::from_event(&flags_changed(inbox, 4, &flags), inbox),
            Some(Update::Flags {
                uid: uid(4),
                flags,
                mod_seq: sandesh_core::ModSeq::new(42).expect("non-zero"),
            })
        );
    }

    // --- applying to a snapshot -------------------------------------------

    /// A mailbox holding UIDs 4, 9, 17 — gapped, so positions and UIDs differ.
    fn gapped(mailbox: MailboxId) -> SequenceMap {
        SequenceMap::new(vec![
            entry_in(mailbox, 4),
            entry_in(mailbox, 9),
            entry_in(mailbox, 17),
        ])
    }

    #[test]
    fn an_arrival_extends_the_snapshot_and_reports_the_new_count() {
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);
        let arriving = entry_in(inbox, 23);

        let line = apply(
            &Update::Exists { uid: uid(23) },
            &mut map,
            Some(&arriving),
            false,
        )
        .expect("an arrival produces a line");

        assert_eq!(line, "* 4 EXISTS\r\n");
        assert_eq!(map.len(), 4, "the snapshot grew");
        assert_eq!(
            map.sequence_of(uid(23)),
            Some(4),
            "the new message is addressable at the reported position"
        );
    }

    #[test]
    fn an_arrival_out_of_uid_order_still_lands_in_uid_order() {
        // Storage assigns ascending UIDs, but an event may be applied to a
        // snapshot that already saw a later one. The map is UID-ordered by
        // definition, so the newcomer must be placed, not appended blindly.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);
        let arriving = entry_in(inbox, 12);

        apply(
            &Update::Exists { uid: uid(12) },
            &mut map,
            Some(&arriving),
            false,
        )
        .expect("an arrival produces a line");

        assert_eq!(
            map.sequence_of(uid(12)),
            Some(3),
            "UID 12 sits between 9 and 17"
        );
        assert_eq!(map.sequence_of(uid(17)), Some(4), "17 moved down one");
    }

    #[test]
    fn a_duplicate_arrival_is_ignored() {
        // Telling the client the mailbox grew when it did not is worse than
        // saying nothing: its next FETCH would address a message that is absent.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);
        let duplicate = entry_in(inbox, 9);

        assert_eq!(
            apply(
                &Update::Exists { uid: uid(9) },
                &mut map,
                Some(&duplicate),
                false
            ),
            None
        );
        assert_eq!(map.len(), 3, "the snapshot did not grow");
    }

    #[test]
    fn a_removal_reports_the_sequence_number_it_held_before_removal() {
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);

        let line = apply(&Update::Expunge { uid: uid(9) }, &mut map, None, false)
            .expect("a removal produces a line");

        assert_eq!(line, "* 2 EXPUNGE\r\n", "UID 9 was at position 2");
        assert_eq!(map.len(), 2);
        assert_eq!(map.sequence_of(uid(9)), None, "it is gone");
        assert_eq!(map.sequence_of(uid(17)), Some(2), "17 renumbered to 2");
    }

    #[test]
    fn removing_the_last_message_renumbers_nothing() {
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);

        let line = apply(&Update::Expunge { uid: uid(17) }, &mut map, None, false).expect("a line");

        assert_eq!(line, "* 3 EXPUNGE\r\n");
        assert_eq!(map.sequence_of(uid(4)), Some(1));
        assert_eq!(map.sequence_of(uid(9)), Some(2));
    }

    #[test]
    fn an_expunge_for_an_absent_uid_produces_no_line() {
        // Ordinary under concurrency, and inventing a number here would shift
        // every message after it in the client's view.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);

        assert_eq!(
            apply(&Update::Expunge { uid: uid(99) }, &mut map, None, false),
            None
        );
        assert_eq!(map.len(), 3, "the snapshot is untouched");
    }

    #[test]
    fn successive_expunges_renumber_between_each_other() {
        // The property that makes descending order matter. Applied in the order
        // the client is told, each number must be right at the moment it is used.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);

        let first =
            apply(&Update::Expunge { uid: uid(17) }, &mut map, None, false).expect("a line");
        let second =
            apply(&Update::Expunge { uid: uid(9) }, &mut map, None, false).expect("a line");

        assert_eq!(first, "* 3 EXPUNGE\r\n");
        assert_eq!(second, "* 2 EXPUNGE\r\n");
        assert_eq!(map.len(), 1);
        assert_eq!(map.sequence_of(uid(4)), Some(1));
    }

    #[test]
    fn a_flag_change_reports_the_sequence_number_and_the_uid() {
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);
        let flags = MessageFlags::from_system(SystemFlags::SEEN);

        let line = apply(
            &Update::Flags {
                uid: uid(9),
                flags: flags.clone(),
                mod_seq: sandesh_core::ModSeq::MIN,
            },
            &mut map,
            None,
            false,
        )
        .expect("a flag change produces a line");

        assert_eq!(line, "* 2 FETCH (UID 9 FLAGS (\\Seen))\r\n");
        assert!(
            map.by_uid(uid(9)).expect("still present").is_seen(),
            "the snapshot records the new flags"
        );
    }

    #[test]
    fn a_condstore_session_is_sent_modseq_in_the_unsolicited_fetch() {
        // RFC 7162 §3.2: a CONDSTORE-enabled session learns the revision that
        // another session's flag change produced, so it can keep its own
        // high-water mark current without polling.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);
        let flags = MessageFlags::from_system(SystemFlags::SEEN);

        let line = apply(
            &Update::Flags {
                uid: uid(9),
                flags: flags.clone(),
                mod_seq: sandesh_core::ModSeq::new(77).expect("non-zero"),
            },
            &mut map,
            None,
            true,
        )
        .expect("a flag change produces a line");

        assert_eq!(line, "* 2 FETCH (UID 9 MODSEQ (77) FLAGS (\\Seen))\r\n");
    }

    #[test]
    fn a_session_without_condstore_is_not_sent_modseq() {
        // RFC 5161 §3: a client that did not enable an extension is entitled
        // to a parser that does not know its syntax. An unsolicited response
        // is exactly where an unexpected item would break one, so the gate is
        // per-session rather than per-server.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);

        let line = apply(
            &Update::Flags {
                uid: uid(9),
                flags: MessageFlags::from_system(SystemFlags::SEEN),
                mod_seq: sandesh_core::ModSeq::new(77).expect("non-zero"),
            },
            &mut map,
            None,
            false,
        )
        .expect("a flag change produces a line");

        assert_eq!(line, "* 2 FETCH (UID 9 FLAGS (\\Seen))\r\n");
        assert!(
            !line.contains("MODSEQ"),
            "a session that did not enable CONDSTORE must not see MODSEQ"
        );
    }

    #[test]
    fn a_flag_change_does_not_renumber() {
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);

        apply(
            &Update::Flags {
                uid: uid(4),
                flags: MessageFlags::from_system(SystemFlags::SEEN),
                mod_seq: sandesh_core::ModSeq::MIN,
            },
            &mut map,
            None,
            false,
        );

        assert_eq!(map.len(), 3, "membership is unchanged");
        assert_eq!(map.sequence_of(uid(17)), Some(3), "positions are unchanged");
    }

    #[test]
    fn a_flag_change_for_an_absent_uid_produces_no_line() {
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);

        assert_eq!(
            apply(
                &Update::Flags {
                    uid: uid(99),
                    flags: MessageFlags::empty(),
                    mod_seq: sandesh_core::ModSeq::MIN,
                },
                &mut map,
                None,
                false,
            ),
            None
        );
    }

    #[test]
    fn an_arrival_without_its_placement_produces_no_line() {
        // The count cannot be reported without the entry to extend the snapshot
        // with: a bare count would leave a hole a following FETCH could not fill.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);
        assert_eq!(
            apply(&Update::Exists { uid: uid(23) }, &mut map, None, false),
            None
        );
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn uids_are_stable_across_every_update() {
        // The invariant that makes UID commands independent of sequence changes.
        let inbox = MailboxId::new();
        let mut map = gapped(inbox);
        let arriving = entry_in(inbox, 23);

        apply(
            &Update::Exists { uid: uid(23) },
            &mut map,
            Some(&arriving),
            false,
        );
        apply(&Update::Expunge { uid: uid(4) }, &mut map, None, false);
        apply(
            &Update::Flags {
                uid: uid(9),
                flags: MessageFlags::from_system(SystemFlags::SEEN),
                mod_seq: sandesh_core::ModSeq::MIN,
            },
            &mut map,
            None,
            false,
        );

        let uids: Vec<u32> = map.iter().map(|e| e.uid.get()).collect();
        assert_eq!(
            uids,
            vec![9, 17, 23],
            "UIDs never change, only positions do"
        );
    }
}

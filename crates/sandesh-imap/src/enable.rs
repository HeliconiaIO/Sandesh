//! `ENABLE` (RFC 5161): the session's set of enabled extensions.
//!
//! # What this module owns
//!
//! Which capability names this server will take on, and the per-session set of
//! the ones it has. It is pure: it parses names and answers questions about a
//! set. The command loop in [`crate::session`] owns the I/O and holds the set.
//!
//! ```text
//! ENABLE CONDSTORE FOO ──▶ Enabled::enable ──▶ ["CONDSTORE"]  ──▶ * ENABLED CONDSTORE
//!                              │                    ▲
//!                              └── FOO unknown ─────┘ silently absent (RFC 5161 §3.1)
//! ```
//!
//! # Why an unknown capability is not an error
//!
//! RFC 5161 §3.1 requires a server to *ignore* names it does not implement and
//! still answer `OK`. That is not laxness: `ENABLE` is how a client discovers
//! what it may use, so a client naming a capability this server lacks has done
//! nothing wrong and must not be broken by a `BAD`. The refusal is carried by
//! the capability's absence from the `* ENABLED` line, which is the signal the
//! client is required to read.
//!
//! # What is enableable
//!
//! [`Extension`] lists the names a client realistically sends, and each one
//! carries whether this server has actually implemented it — two separate
//! questions, kept apart deliberately. `CONDSTORE` (RFC 7162 §3) is
//! implemented and therefore enableable and advertised. `QRESYNC` is
//! recognised, refused, and absent from the capability list, because it needs
//! a durable record of expunged UIDs that this server does not keep.

use std::collections::BTreeSet;

/// A capability name a client may ask to enable.
///
/// Membership here means "this server recognises the name", not "this server
/// implements it" — [`Extension::is_implemented`] is the second, separate
/// question. Keeping the two apart is what lets the refusal be deliberate and
/// tested rather than an accident of a missing match arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Extension {
    /// `CONDSTORE` (RFC 7162 §3.1).
    CondStore,
    /// `QRESYNC` (RFC 7162 §3.2).
    QResync,
    /// A capability that exists only under `cfg(test)`, and is implemented.
    ///
    /// It was introduced when nothing shipping was implemented, so that the
    /// storage paths in [`Enabled`] had a real member to carry rather than
    /// being tested vacuously. `CONDSTORE` now covers those paths as a
    /// shipping extension, and this is deliberately kept as a *control*: it is
    /// an implemented capability that is not `CONDSTORE`, which is what lets
    /// the mixed-request test distinguish "enables what is implemented" from
    /// "enables `CONDSTORE` specifically".
    ///
    /// It is `cfg(test)`, so no client can reach it and no capability list can
    /// mention it.
    #[cfg(test)]
    Probe,
}

impl Extension {
    /// Every recognised name, in canonical order.
    #[cfg(not(test))]
    pub const ALL: [Self; 2] = [Self::CondStore, Self::QResync];
    /// Every recognised name, in canonical order.
    #[cfg(test)]
    pub const ALL: [Self; 3] = [Self::CondStore, Self::QResync, Self::Probe];

    /// The names a *client* can reach: everything shipping.
    ///
    /// Distinct from [`Self::ALL`] so the test-only probe cannot leak into an
    /// assertion about what this server advertises or implements.
    #[cfg(test)]
    pub const SHIPPING: [Self; 2] = [Self::CondStore, Self::QResync];

    /// Recognises a capability name, implemented or not.
    ///
    /// Comparison is ASCII-case-insensitive, as capability names are
    /// case-insensitive atoms (RFC 9051 §9).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|extension| name.eq_ignore_ascii_case(extension.as_str()))
    }

    /// Whether the behaviour behind this name actually exists.
    ///
    /// The single place that decides, and the one the capability list and
    /// `* ENABLED` both derive from, so the two can never disagree. Flipping a
    /// name to `true` without the behaviour behind it would make every other
    /// honesty check in this crate a lie, which is why the decision is a named
    /// function with its own test rather than an inline literal.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        match self {
            // `CONDSTORE` is implemented: migration 0002 gives every mailbox a
            // monotonic `highest_mod_seq`, flag changes and insertions are
            // stamped from it atomically, and `FETCH MODSEQ`,
            // `FETCH ... (CHANGEDSINCE n)`, `STORE ... (UNCHANGEDSINCE n)`,
            // `SEARCH MODSEQ` and `HIGHESTMODSEQ` all answer from it.
            Self::CondStore => true,
            // `QRESYNC` remains unimplemented. It is built on CONDSTORE, but
            // needs more than a modification sequence: a persistent record of
            // *expunged* UIDs, so `VANISHED (EARLIER)` can tell a returning
            // client which messages disappeared while it was away. This server
            // keeps no such record — an expunged placement is simply gone — so
            // the resynchronisation QRESYNC promises cannot be honoured, and
            // advertising it would strand a client waiting for `VANISHED`.
            Self::QResync => false,
            #[cfg(test)]
            Self::Probe => true,
        }
    }

    /// The canonical capability name, as it appears in `* ENABLED`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CondStore => "CONDSTORE",
            Self::QResync => "QRESYNC",
            #[cfg(test)]
            Self::Probe => "X-SANDESH-PROBE",
        }
    }
}

/// The extensions one session has enabled.
///
/// Per session, never global. RFC 5161 §3 makes enabled state a property of the
/// connection, and it must be: one client enabling an extension that changes
/// response syntax must not change what a *different* client is sent
/// mid-conversation. Holding this inside [`crate::session::Session`] rather
/// than in the server or the store is what makes that leak impossible rather
/// than merely unlikely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Enabled {
    active: BTreeSet<Extension>,
}

impl Enabled {
    /// A session with nothing enabled, which is how every session starts.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables every recognised name in `names`, returning what was enabled.
    ///
    /// The returned names are the `* ENABLED` payload: canonical spellings from
    /// [`Extension::as_str`] rather than the client's, deduplicated, in a
    /// deterministic order. Unrecognised names are dropped (RFC 5161 §3.1).
    ///
    /// Repeating a name — within one command or across several — is safe and
    /// idempotent. RFC 5161 §3.2 requires an already-enabled capability to
    /// still be reported as enabled, so a client that re-sends `ENABLE` because
    /// it lost track gets a consistent answer rather than an empty one.
    pub fn enable<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let requested: BTreeSet<Extension> = names
            .into_iter()
            .filter_map(Extension::parse)
            .filter(|extension| extension.is_implemented())
            .collect();
        self.active.extend(requested.iter().copied());
        requested
            .into_iter()
            .map(|extension| String::from(extension.as_str()))
            .collect()
    }

    /// Whether this session enabled `extension`.
    ///
    /// Handlers ask this before emitting extension-specific data. A client that
    /// did not enable an extension must not be sent its syntax, because it is
    /// entitled to a parser that does not know it (RFC 5161 §3).
    ///
    /// This was `cfg(test)` while nothing shipping was implemented, because an
    /// always-false predicate in the production build would have been dead
    /// code pretending to be a feature. CONDSTORE removed the attribute: the
    /// unsolicited `FETCH ... MODSEQ` in [`crate::idle`] is the caller that
    /// justifies it.
    #[must_use]
    pub fn has(&self, extension: Extension) -> bool {
        self.active.contains(&extension)
    }

    /// Whether nothing is enabled.
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognised_names_parse_case_insensitively() {
        // Capability names are case-insensitive atoms (RFC 9051 §9), so a
        // client shouting or whispering must reach the same decision.
        for spelling in ["CONDSTORE", "condstore", "CondStore"] {
            assert_eq!(Extension::parse(spelling), Some(Extension::CondStore));
        }
        assert_eq!(Extension::parse("qresync"), Some(Extension::QResync));
    }

    #[test]
    fn unknown_names_do_not_parse() {
        for name in ["", "X-MADE-UP", "CONDSTOR", "CONDSTORE2", "IMAP4rev2"] {
            assert_eq!(Extension::parse(name), None, "{name} should not parse");
        }
    }

    #[test]
    fn the_shipping_vocabulary_is_exactly_condstore_and_qresync() {
        // `SHIPPING` is what a client can actually reach, as distinct from
        // `ALL` which includes the test-only probe. Asserting its membership
        // keeps the probe from leaking into a claim about what this server
        // recognises, and pins the vocabulary so a new name must be added
        // deliberately.
        assert_eq!(
            Extension::SHIPPING,
            [Extension::CondStore, Extension::QResync]
        );
        for extension in Extension::SHIPPING {
            assert_ne!(
                extension.as_str(),
                Extension::Probe.as_str(),
                "the test-only probe must not appear among shipping names"
            );
        }
    }

    #[test]
    fn condstore_is_implemented_and_qresync_is_not() {
        // The honesty gate, asserted directly, and the successor to
        // `nothing_is_implemented_yet`: this increment implemented CONDSTORE,
        // so the gate had to be updated deliberately rather than drifting.
        //
        // `QRESYNC` stays false on purpose. It needs a durable record of
        // expunged UIDs for `VANISHED (EARLIER)`, which this server does not
        // keep, so flipping it would make every other honesty check here a
        // lie.
        assert!(Extension::CondStore.is_implemented());
        assert!(
            !Extension::QResync.is_implemented(),
            "QRESYNC must not claim to be implemented without VANISHED support"
        );
    }

    #[test]
    fn names_round_trip_through_their_canonical_spelling() {
        for extension in Extension::ALL {
            assert_eq!(Extension::parse(extension.as_str()), Some(extension));
        }
    }

    #[test]
    fn a_new_session_has_nothing_enabled() {
        let session = Enabled::new();
        assert!(session.is_empty());
        for extension in Extension::ALL {
            assert!(!session.has(extension));
        }
    }

    #[test]
    fn unimplemented_capabilities_are_not_enabled() {
        // The refusal. `QRESYNC` is a real extension and a recognised name,
        // and it is still not enabled, because it does not work. This is the
        // mutation to guard: dropping the `is_implemented` filter in
        // `enable` would make this server claim QRESYNC semantics it has not
        // got, and that claim would reach the client as `* ENABLED QRESYNC`.
        let mut session = Enabled::new();
        assert!(session.enable(["QRESYNC"]).is_empty());
        assert!(!session.has(Extension::QResync));
        assert!(session.is_empty());
    }

    #[test]
    fn condstore_is_enabled_and_remembered() {
        // The shipping counterpart to the `Probe` test: a real, advertised
        // extension that a real client will send.
        let mut session = Enabled::new();
        assert_eq!(
            session.enable(["CONDSTORE"]),
            vec![String::from("CONDSTORE")]
        );
        assert!(session.has(Extension::CondStore));
    }

    #[test]
    fn unknown_capabilities_are_ignored_not_reported() {
        // RFC 5161 §3.1: ignore, do not fail. The absence from the report is
        // the whole of the response.
        let mut session = Enabled::new();
        assert!(session.enable(["X-NOPE", "ALSO-NOPE"]).is_empty());
        assert!(session.is_empty());
    }

    #[test]
    fn an_implemented_capability_is_enabled_and_remembered() {
        // The persistence requirement, and the reason `Probe` exists. Dropping
        // the store in `enable` leaves `has` false while the report still looks
        // right — a mutation nothing could catch if no extension were ever
        // implemented.
        let mut session = Enabled::new();
        let reported = session.enable(["X-SANDESH-PROBE"]);

        assert_eq!(reported, vec![String::from("X-SANDESH-PROBE")]);
        assert!(session.has(Extension::Probe), "enabled state must persist");
        assert!(!session.is_empty());
    }

    #[test]
    fn enabled_state_persists_across_later_enable_calls() {
        // A second ENABLE naming something else must not forget the first.
        let mut session = Enabled::new();
        let _ = session.enable(["X-SANDESH-PROBE"]);
        let _ = session.enable(["QRESYNC"]);

        assert!(
            session.has(Extension::Probe),
            "an unrelated ENABLE must not clear what was already enabled"
        );
    }

    #[test]
    fn the_canonical_spelling_is_reported_not_the_clients() {
        // RFC 5161 §3.2: `* ENABLED` carries capability names. A client that
        // shouted must still be able to match the reply against its request.
        let mut session = Enabled::new();
        assert_eq!(
            session.enable(["x-sandesh-probe"]),
            vec![String::from("X-SANDESH-PROBE")]
        );
    }

    #[test]
    fn enabling_is_idempotent_and_deterministic() {
        // RFC 5161 §3.2: a repeated ENABLE must report the capability as
        // enabled again rather than reporting nothing the second time.
        let mut session = Enabled::new();
        let first = session.enable(["X-SANDESH-PROBE"]);
        let second = session.enable(["X-SANDESH-PROBE"]);
        assert_eq!(first, second, "repeated ENABLE must be stable");
        assert!(session.has(Extension::Probe));

        let duplicated = session.enable(["X-SANDESH-PROBE", "x-sandesh-probe"]);
        assert_eq!(
            duplicated,
            vec![String::from("X-SANDESH-PROBE")],
            "a capability must not be reported twice"
        );
    }

    #[test]
    fn a_mixed_request_enables_only_what_is_implemented() {
        // The realistic case: a client asks for several things at once and must
        // be told precisely which it got.
        let mut session = Enabled::new();
        let reported = session.enable(["CONDSTORE", "X-SANDESH-PROBE", "X-NOPE", "QRESYNC"]);

        assert_eq!(
            reported,
            vec![String::from("CONDSTORE"), String::from("X-SANDESH-PROBE")],
            "only implemented capabilities may be reported"
        );
        assert!(session.has(Extension::Probe));
        assert!(session.has(Extension::CondStore));
        assert!(
            !session.has(Extension::QResync),
            "QRESYNC must not be enabled alongside CONDSTORE"
        );
    }

    #[test]
    fn sessions_do_not_share_enabled_state() {
        // The isolation requirement, made concrete. Two `Enabled` values are
        // independent, so one session's ENABLE cannot be observed by another.
        let mut one = Enabled::new();
        let mut two = Enabled::new();
        let _ = one.enable(["X-SANDESH-PROBE"]);

        assert!(one.has(Extension::Probe));
        assert!(
            !two.has(Extension::Probe),
            "a second session must not inherit enabled state"
        );
        assert!(two.is_empty());
        assert_eq!(two, Enabled::new(), "a second session must be untouched");

        // And the reverse: the second session enabling nothing must not
        // disable what the first has.
        let _ = two.enable(["QRESYNC"]);
        assert!(one.has(Extension::Probe));
    }
}

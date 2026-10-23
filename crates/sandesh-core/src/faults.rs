//! Deliberate failures, configured at runtime, for testing application resilience.
//!
//! Phase 8's shift is from *observing* mail to *exercising* the application that
//! sends it. An application's retry and error handling is the part least likely
//! to be tested, because provoking a real mail server into failing on demand is
//! awkward. This module makes it a configuration call.
//!
//! # Why this is runtime state and the rest of the configuration is not
//!
//! ADR-031 fixed configuration as `SANDESH_*` environment variables read once in
//! the composition root, and everything else in `SmtpConfig` still works that
//! way. Faults are the exception, by ADR-084: a CI suite turns a rejection on
//! for one test and off for the next, in the same process, and a restart per
//! scenario would defeat the feature. The exception is deliberately narrow —
//! ports, paths and TLS stay immutable, because nothing asks to change them
//! mid-run.
//!
//! # Why the rules do not persist
//!
//! Nothing here is written to storage (ADR-084). The failure mode of fault
//! injection is a developer losing an afternoon to a "broken" server that is
//! doing exactly what they configured an hour ago, and an in-memory registry
//! makes **restart the escape hatch** from that state. A persisted fault would
//! survive the first thing anyone tries.

use crate::EmailAddress;
use std::sync::RwLock;
use std::time::Duration;

/// What a fault does to the conversation when it fires.
///
/// Each variant names a real SMTP failure an application must cope with, and
/// maps to one specific wire behaviour. The adapter owns the reply codes —
/// this enum names the *decision*, as [`UnknownRecipient`](crate::UnknownRecipient)
/// does — except where the distinction between a permanent and a temporary
/// refusal is the whole point of the mode, which is why
/// [`TemporaryFailure`](Self::TemporaryFailure) exists separately from
/// [`RejectAtRcpt`](Self::RejectAtRcpt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultMode {
    /// Refuse the recipient permanently — SMTP `550`.
    ///
    /// The transaction stands: RFC 5321 §3.3 lets the client offer another
    /// address, and a client that gives up here has a bug worth finding.
    RejectAtRcpt,
    /// Accept the recipients, then refuse the message — SMTP `554`.
    ///
    /// Distinct from [`RejectAtRcpt`](Self::RejectAtRcpt) because the client has
    /// already transmitted the body by this point. An application that assumes
    /// acceptance at `RCPT` means delivery fails here.
    RejectAtData,
    /// Refuse temporarily — SMTP `421`/`451`.
    ///
    /// The mode that exercises retry logic, which is the reason most
    /// applications adopt a queue at all.
    TemporaryFailure,
    /// Answer, but slowly.
    ///
    /// Exercises client timeouts. The delay is applied before the reply the
    /// command would otherwise have received, so the conversation stays valid —
    /// this tests patience, not protocol violation.
    SlowResponse(Duration),
    /// Close the connection with no reply at all.
    ///
    /// The harshest mode, and the one real servers produce under load. A client
    /// that cannot distinguish this from a clean `QUIT` will lose mail.
    ConnectionDrop,
}

/// Which point in the conversation a fault applies to.
///
/// Derived from the mode rather than configured beside it: a `RejectAtData`
/// that fired at `RCPT` would be a contradiction the caller could construct,
/// and making it underivable is cheaper than validating it away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// Applies when a recipient is offered.
    Rcpt,
    /// Applies when the message body has been received.
    Data,
}

impl FaultMode {
    /// Where in the conversation this mode takes effect.
    #[must_use]
    pub const fn point(self) -> FaultPoint {
        match self {
            Self::RejectAtRcpt => FaultPoint::Rcpt,
            // A body-refusal, a temporary failure, a stall and a drop are all
            // most useful once the client believes the transaction is under
            // way: that is where an application's error handling actually runs.
            Self::RejectAtData
            | Self::TemporaryFailure
            | Self::SlowResponse(_)
            | Self::ConnectionDrop => FaultPoint::Data,
        }
    }
}

/// Which conversations a fault applies to.
///
/// An empty match — every field `None` — matches everything. That is the
/// common case for a CI scenario ("make the next delivery fail") and needs no
/// ceremony to express.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaultMatch {
    /// Fires only for this recipient, compared on the normalised address.
    pub recipient: Option<String>,
    /// Fires only for this sender, compared on the normalised address.
    pub sender: Option<String>,
}

impl FaultMatch {
    /// Whether this fault applies to a transaction with these addresses.
    ///
    /// `sender` is `None` for the null reverse-path `<>`, which a bounce uses.
    /// A rule naming a sender does not match one, rather than matching every
    /// one: `<>` is an address the rule did not name.
    #[must_use]
    pub fn matches(&self, sender: Option<&EmailAddress>, recipient: Option<&EmailAddress>) -> bool {
        Self::field_matches(self.sender.as_deref(), sender)
            && Self::field_matches(self.recipient.as_deref(), recipient)
    }

    fn field_matches(wanted: Option<&str>, actual: Option<&EmailAddress>) -> bool {
        match wanted {
            None => true,
            Some(want) => actual.is_some_and(|a| a.normalized().eq_ignore_ascii_case(want)),
        }
    }
}

/// One configured fault: what to do, and to whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultRule {
    /// What this fault does.
    pub mode: FaultMode,
    /// Which conversations it applies to.
    pub selector: FaultMatch,
}

impl FaultRule {
    /// A fault applying to every conversation.
    #[must_use]
    pub fn always(mode: FaultMode) -> Self {
        Self {
            mode,
            selector: FaultMatch::default(),
        }
    }
}

/// The configured faults, shared between the API that sets them and the
/// protocol adapters that obey them.
///
/// Held as `Arc<FaultRegistry>` and cloned into both, matching the
/// `Arc<DeliveryService>` idiom the SMTP session already carries (ADR-084).
///
/// The lock is `std::sync::RwLock`, not `tokio`'s: `sandesh-core` has no async
/// runtime and `tests/architecture.rs` enforces that mechanically. It is also
/// the right choice on its own merits — every critical section here is an
/// in-memory scan of a short vector with no `.await` inside it, so the guard
/// cannot be held across a yield point.
#[derive(Debug, Default)]
pub struct FaultRegistry {
    rules: RwLock<Vec<FaultRule>>,
}

impl FaultRegistry {
    /// An empty registry: no faults, so the server behaves exactly as it did
    /// before this module existed (ADR-005, ADR-084).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a rule, returning how many are now configured.
    pub fn add(&self, rule: FaultRule) -> usize {
        let mut rules = self.write();
        rules.push(rule);
        rules.len()
    }

    /// Replaces every rule with `rules`.
    pub fn replace(&self, rules: Vec<FaultRule>) {
        *self.write() = rules;
    }

    /// Removes every rule, returning how many were cleared.
    pub fn clear(&self) -> usize {
        let mut rules = self.write();
        let count = rules.len();
        rules.clear();
        count
    }

    /// Every configured rule.
    #[must_use]
    pub fn rules(&self) -> Vec<FaultRule> {
        self.read().clone()
    }

    /// Whether any fault is configured.
    ///
    /// The check the adapters make first, and the one `/info` reports so an
    /// active fault is never invisible (ADR-084).
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.read().is_empty()
    }

    /// The first rule applying at `point` for these addresses, if any.
    ///
    /// First rather than most-specific: ordering is the caller's, stated by the
    /// order they added them, and a precedence system nobody asked for would be
    /// a second thing to explain when a fault does not fire.
    #[must_use]
    pub fn match_fault(
        &self,
        point: FaultPoint,
        sender: Option<&EmailAddress>,
        recipient: Option<&EmailAddress>,
    ) -> Option<FaultMode> {
        self.read()
            .iter()
            .find(|rule| rule.mode.point() == point && rule.selector.matches(sender, recipient))
            .map(|rule| rule.mode)
    }

    /// Reads the rules, recovering from a poisoned lock.
    ///
    /// A panic while holding the lock leaves the rules readable but suspect.
    /// Propagating the panic would take the whole mail server down over a
    /// diagnostic feature; the rules are plain data with no invariant a partial
    /// write could break, so recovering is both safe and the lesser failure.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<FaultRule>> {
        self.rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<FaultRule>> {
        self.rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> EmailAddress {
        EmailAddress::parse_lenient(s)
    }

    #[test]
    fn an_empty_registry_matches_nothing() {
        let registry = FaultRegistry::new();
        assert!(!registry.is_active());
        assert_eq!(
            registry.match_fault(FaultPoint::Rcpt, None, Some(&addr("a@b.test"))),
            None
        );
    }

    #[test]
    fn a_rule_with_no_selector_matches_every_conversation() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule::always(FaultMode::RejectAtRcpt));

        assert!(registry.is_active());
        assert_eq!(
            registry.match_fault(FaultPoint::Rcpt, None, Some(&addr("anyone@b.test"))),
            Some(FaultMode::RejectAtRcpt)
        );
    }

    #[test]
    fn a_rule_only_fires_at_its_own_point() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule::always(FaultMode::RejectAtRcpt));

        // Configured for RCPT, so DATA must not see it.
        assert_eq!(registry.match_fault(FaultPoint::Data, None, None), None);
    }

    #[test]
    fn a_recipient_selector_excludes_other_recipients() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule {
            mode: FaultMode::RejectAtRcpt,
            selector: FaultMatch {
                recipient: Some(String::from("wanted@b.test")),
                sender: None,
            },
        });

        assert_eq!(
            registry.match_fault(FaultPoint::Rcpt, None, Some(&addr("wanted@b.test"))),
            Some(FaultMode::RejectAtRcpt)
        );
        assert_eq!(
            registry.match_fault(FaultPoint::Rcpt, None, Some(&addr("other@b.test"))),
            None
        );
    }

    #[test]
    fn address_matching_ignores_case() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule {
            mode: FaultMode::RejectAtRcpt,
            selector: FaultMatch {
                recipient: Some(String::from("wanted@b.test")),
                sender: None,
            },
        });

        assert_eq!(
            registry.match_fault(FaultPoint::Rcpt, None, Some(&addr("WANTED@B.TEST"))),
            Some(FaultMode::RejectAtRcpt)
        );
    }

    #[test]
    fn a_sender_rule_does_not_match_the_null_reverse_path() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule {
            mode: FaultMode::RejectAtData,
            selector: FaultMatch {
                recipient: None,
                sender: Some(String::from("someone@b.test")),
            },
        });

        // `<>` is not the address the rule named, so a bounce passes through.
        assert_eq!(registry.match_fault(FaultPoint::Data, None, None), None);
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule::always(FaultMode::TemporaryFailure));
        registry.add(FaultRule::always(FaultMode::ConnectionDrop));

        assert_eq!(
            registry.match_fault(FaultPoint::Data, None, None),
            Some(FaultMode::TemporaryFailure)
        );
    }

    #[test]
    fn clearing_returns_the_count_and_deactivates() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule::always(FaultMode::RejectAtRcpt));
        registry.add(FaultRule::always(FaultMode::ConnectionDrop));

        assert_eq!(registry.clear(), 2);
        assert!(!registry.is_active());
    }

    #[test]
    fn replace_discards_the_previous_rules() {
        let registry = FaultRegistry::new();
        registry.add(FaultRule::always(FaultMode::RejectAtRcpt));
        registry.replace(vec![FaultRule::always(FaultMode::ConnectionDrop)]);

        let rules = registry.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].mode, FaultMode::ConnectionDrop);
    }

    #[test]
    fn every_mode_has_a_point() {
        // Guards the derivation in `FaultMode::point` against a new variant
        // being added and silently defaulting to the wrong half of the
        // conversation.
        assert_eq!(FaultMode::RejectAtRcpt.point(), FaultPoint::Rcpt);
        assert_eq!(FaultMode::RejectAtData.point(), FaultPoint::Data);
        assert_eq!(FaultMode::TemporaryFailure.point(), FaultPoint::Data);
        assert_eq!(
            FaultMode::SlowResponse(Duration::from_millis(1)).point(),
            FaultPoint::Data
        );
        assert_eq!(FaultMode::ConnectionDrop.point(), FaultPoint::Data);
    }
}

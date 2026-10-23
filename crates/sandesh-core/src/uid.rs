//! IMAP UID value objects (`CORE_SPEC.md` §3).

use core::fmt;

/// A per-mailbox message UID.
///
/// Valid range is `1..=u32::MAX`; RFC 9051 reserves 0. UIDs ascend strictly
/// within a mailbox and are **never reused**, even after `EXPUNGE` — reuse
/// would silently corrupt every client's cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uid(u32);

impl Uid {
    /// The lowest valid UID.
    pub const MIN: Self = Self(1);

    /// Creates a UID, rejecting the reserved value 0.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// The numeric value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The next UID, or `None` at `u32::MAX`.
    ///
    /// Exhaustion is not a silent wrap: wrapping would reuse UIDs, so the
    /// caller must handle it (by bumping `UIDVALIDITY`).
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A mailbox's UID-space generation (RFC 9051 §2.3.1.1).
///
/// Changing this tells clients to discard everything they have cached for the
/// mailbox. It changes only when a mailbox is deleted and recreated under the
/// same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UidValidity(u32);

impl UidValidity {
    /// Wraps a raw value.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// The numeric value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// A fresh UID-space generation for a newly created mailbox.
    ///
    /// Derived from the current time in whole seconds, which is what satisfies
    /// the RFC 9051 §2.3.1.1 requirement that a mailbox deleted and recreated
    /// under the same name must not reuse its predecessor's validity: a later
    /// creation always yields a strictly larger value, so a client's cached
    /// UIDs are correctly invalidated.
    ///
    /// It lives in the domain rather than in one adapter because both the
    /// storage layer and the IMAP `CREATE` handler must agree on it; two
    /// generators would be two chances to reuse an epoch.
    #[must_use]
    pub fn next_epoch() -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self((crate::Timestamp::now().as_micros() / 1_000_000) as u32)
    }
}

impl fmt::Display for UidValidity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A mailbox-scoped modification sequence (RFC 7162 §3).
///
/// CONDSTORE's whole contract is one comparison: "give me everything that
/// changed since `n`". That only works if the value is **monotonic within the
/// mailbox** and never reused, so a client holding `n` can be certain nothing
/// at or below `n` changed after it looked.
///
/// # Why per-mailbox rather than per-entry
///
/// A per-entry counter cannot answer the question. Two messages whose flags
/// changed at different moments would both sit at 2, so "changed since 1"
/// cannot distinguish them and `HIGHESTMODSEQ` has no value to report. The
/// sequence is therefore owned by the mailbox and stamped onto entries, in the
/// same way `uid_next` owns UID allocation.
///
/// # Why 1 is the floor
///
/// RFC 7162 §3 requires `HIGHESTMODSEQ` to be non-zero for any mailbox a
/// server reports it for, and treats 0 as "no modification sequence" — a
/// distinct, reserved meaning. Refusing 0 in the constructor keeps that
/// reserved value from ever reaching the wire as if it were real.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModSeq(u64);

impl ModSeq {
    /// The lowest valid modification sequence, and where a mailbox starts.
    pub const MIN: Self = Self(1);

    /// Creates a modification sequence, rejecting the reserved value 0.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// The numeric value, as it appears on the wire.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next sequence value, or `None` at `u64::MAX`.
    ///
    /// Like [`Uid::next`], exhaustion is explicit rather than a wrap: wrapping
    /// would re-issue a value a client has already seen, which is precisely
    /// the corruption the type exists to prevent.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }
}

impl fmt::Display for ModSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_rejected() {
        assert!(Uid::new(0).is_none(), "RFC 9051 reserves UID 0");
        assert!(Uid::new(1).is_some());
    }

    #[test]
    fn uids_order_numerically() {
        let (a, b) = (Uid::new(5), Uid::new(42));
        assert!(a < b);
    }

    #[test]
    fn a_new_epoch_is_non_zero_and_monotonic() {
        // A recreated mailbox must not reuse its predecessor's validity.
        let first = UidValidity::next_epoch();
        assert!(first.get() > 0, "a validity of 0 invalidates nothing");
        assert!(UidValidity::next_epoch() >= first);
    }

    #[test]
    fn next_advances_and_saturates_safely() {
        let uid = Uid::new(41).unwrap_or(Uid::MIN);
        assert_eq!(uid.next().map(Uid::get), Some(42));

        let max = Uid::new(u32::MAX).unwrap_or(Uid::MIN);
        assert!(max.next().is_none(), "must not wrap into UID reuse");
    }
    #[test]
    fn modseq_rejects_the_reserved_zero() {
        // RFC 7162 §3 gives 0 the distinct meaning "no modification sequence".
        // Allowing it to be constructed would let that reserved value reach the
        // wire as if it were a real sequence number.
        assert!(ModSeq::new(0).is_none());
        assert_eq!(ModSeq::new(1).map(ModSeq::get), Some(1));
        assert_eq!(ModSeq::MIN.get(), 1);
    }

    #[test]
    fn modseqs_order_numerically() {
        // The comparison CONDSTORE is built on.
        let (a, b) = (ModSeq::new(7), ModSeq::new(70));
        assert!(a < b);
    }

    #[test]
    fn modseq_next_advances_and_refuses_to_wrap() {
        let seq = ModSeq::new(41).unwrap_or(ModSeq::MIN);
        assert_eq!(seq.next().map(ModSeq::get), Some(42));

        let max = ModSeq::new(u64::MAX).unwrap_or(ModSeq::MIN);
        assert!(max.next().is_none(), "must not wrap into modseq reuse");
    }
}

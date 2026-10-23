//! Message flags, carried per *placement* rather than per message.
//!
//! Flags live on [`MessageEntry`](crate::MessageEntry), never on
//! [`Message`](crate::Message). One message in two mailboxes has two
//! independent flag sets — that is IMAP `COPY` semantics, and it is the reason
//! the entry entity exists at all.
//!
//! `\Recent` is deliberately **not** modelled (`CORE_SPEC.md` §3, §9). RFC 9051
//! deprecates it and its semantics are inherently session-scoped; the IMAP
//! adapter synthesises it from session state if a legacy client needs it.

use core::fmt;
use std::collections::BTreeSet;

/// The five RFC 9051 system flags, as a bitmask.
///
/// A bitmask (rather than a set) is right here because the flag set is closed
/// and fixed by the RFC, and it stores as a single `INTEGER` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SystemFlags(u8);

impl SystemFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);
    /// `\Seen` — the message has been read.
    pub const SEEN: Self = Self(1 << 0);
    /// `\Answered` — the message has been replied to.
    pub const ANSWERED: Self = Self(1 << 1);
    /// `\Flagged` — marked for urgent attention.
    pub const FLAGGED: Self = Self(1 << 2);
    /// `\Deleted` — marked for removal at the next `EXPUNGE`.
    pub const DELETED: Self = Self(1 << 3);
    /// `\Draft` — an incomplete composition.
    pub const DRAFT: Self = Self(1 << 4);

    /// All defined flags, for masking off unknown bits read from storage.
    pub const ALL: Self = Self(0b0001_1111);

    /// Wraps a raw bitmask, discarding undefined bits.
    #[must_use]
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// The raw bitmask, for persistence.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether every flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// The union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `self` with every flag in `other` removed.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether no flags are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The flag with this IMAP wire name, if it names a system flag.
    ///
    /// The inverse of [`names`](Self::names), and deliberately beside it so the
    /// two cannot drift. Matching is case-insensitive because RFC 9051 system
    /// flag names are case-insensitive, and a client sending `\\seen` means
    /// `\\Seen`. Returns `None` for a keyword or nonsense — an unknown name is
    /// the caller's to report, not a domain failure.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        for (flag, known) in [
            (Self::SEEN, r"\Seen"),
            (Self::ANSWERED, r"\Answered"),
            (Self::FLAGGED, r"\Flagged"),
            (Self::DELETED, r"\Deleted"),
            (Self::DRAFT, r"\Draft"),
        ] {
            if name.eq_ignore_ascii_case(known) {
                return Some(flag);
            }
        }
        None
    }

    /// The IMAP wire names of the flags that are set.
    #[must_use]
    pub fn names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (flag, name) in [
            (Self::SEEN, r"\Seen"),
            (Self::ANSWERED, r"\Answered"),
            (Self::FLAGGED, r"\Flagged"),
            (Self::DELETED, r"\Deleted"),
            (Self::DRAFT, r"\Draft"),
        ] {
            if self.contains(flag) {
                out.push(name);
            }
        }
        out
    }
}

impl fmt::Display for SystemFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.names().join(" "))
    }
}

/// A client-defined keyword — an IMAP atom without the leading backslash.
///
/// The keyword set is open (`$Forwarded`, `$Junk`, project-specific labels), so
/// keywords are a side table rather than bitmask bits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Keyword(String);

impl Keyword {
    /// Creates a keyword, rejecting empty input and IMAP atom-specials.
    ///
    /// Returns `None` rather than erroring: an invalid keyword is a client
    /// mistake to be reported by the adapter, not a domain failure.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Option<Self> {
        /// RFC 9051 atom-specials, which cannot appear in a `flag_keyword`.
        const FORBIDDEN: &[char] = &['(', ')', '{', ' ', '%', '*', '"', ']', '\\', '\r', '\n'];

        let value = value.into();
        if value.is_empty() || value.starts_with('\\') {
            return None;
        }
        if value
            .chars()
            .any(|c| FORBIDDEN.contains(&c) || c.is_control())
        {
            return None;
        }
        Some(Self(value))
    }

    /// The keyword text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The complete flag state of one message placement.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageFlags {
    system: SystemFlags,
    keywords: BTreeSet<Keyword>,
}

impl MessageFlags {
    /// An empty flag set.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Builds from system flags alone.
    #[must_use]
    pub fn from_system(system: SystemFlags) -> Self {
        Self {
            system,
            keywords: BTreeSet::new(),
        }
    }

    /// Builds from both system flags and keywords.
    #[must_use]
    pub fn new(system: SystemFlags, keywords: BTreeSet<Keyword>) -> Self {
        Self { system, keywords }
    }

    /// The system flags.
    #[must_use]
    pub const fn system(&self) -> SystemFlags {
        self.system
    }

    /// The keywords, in sorted order.
    #[must_use]
    pub const fn keywords(&self) -> &BTreeSet<Keyword> {
        &self.keywords
    }

    /// Whether every system flag in `flags` is set.
    #[must_use]
    pub const fn has(&self, flags: SystemFlags) -> bool {
        self.system.contains(flags)
    }

    /// Whether the keyword is present.
    #[must_use]
    pub fn has_keyword(&self, keyword: &Keyword) -> bool {
        self.keywords.contains(keyword)
    }

    /// Adds flags and keywords — IMAP `STORE +FLAGS`.
    pub fn add(&mut self, system: SystemFlags, keywords: impl IntoIterator<Item = Keyword>) {
        self.system = self.system.union(system);
        self.keywords.extend(keywords);
    }

    /// Removes flags and keywords — IMAP `STORE -FLAGS`.
    pub fn remove(&mut self, system: SystemFlags, keywords: &BTreeSet<Keyword>) {
        self.system = self.system.difference(system);
        self.keywords.retain(|k| !keywords.contains(k));
    }

    /// Replaces the entire flag set — IMAP `STORE FLAGS`.
    pub fn replace(&mut self, system: SystemFlags, keywords: BTreeSet<Keyword>) {
        self.system = system;
        self.keywords = keywords;
    }

    /// Every flag name, system flags first, for IMAP `FETCH FLAGS`.
    #[must_use]
    pub fn all_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .system
            .names()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        names.extend(self.keywords.iter().map(|k| k.as_str().to_owned()));
        names
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod from_name_tests {
    use super::*;

    #[test]
    fn every_rendered_name_parses_back() {
        // names() and from_name() must stay inverses of each other.
        let all = SystemFlags::ALL;
        for name in all.names() {
            assert_eq!(
                SystemFlags::from_name(name),
                Some(match name {
                    r"\Seen" => SystemFlags::SEEN,
                    r"\Answered" => SystemFlags::ANSWERED,
                    r"\Flagged" => SystemFlags::FLAGGED,
                    r"\Deleted" => SystemFlags::DELETED,
                    _ => SystemFlags::DRAFT,
                }),
                "{name} did not round-trip"
            );
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        // RFC 9051 system flag names are case-insensitive.
        assert_eq!(SystemFlags::from_name(r"\seen"), Some(SystemFlags::SEEN));
        assert_eq!(SystemFlags::from_name(r"\SEEN"), Some(SystemFlags::SEEN));
    }

    #[test]
    fn a_keyword_is_not_a_system_flag() {
        assert_eq!(SystemFlags::from_name("important"), None);
        assert_eq!(SystemFlags::from_name(r"\Nonsense"), None);
        assert_eq!(SystemFlags::from_name(""), None);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn kw(s: &str) -> Keyword {
        Keyword::new(s).unwrap_or_else(|| Keyword(String::from("invalid")))
    }

    #[test]
    fn bitmask_round_trips() {
        let f = SystemFlags::SEEN.union(SystemFlags::DELETED);
        assert_eq!(SystemFlags::from_bits_truncate(f.bits()), f);
    }

    #[test]
    fn unknown_bits_are_discarded() {
        // Guards against a corrupt or future-version row poisoning the domain.
        assert_eq!(
            SystemFlags::from_bits_truncate(0b1110_0000),
            SystemFlags::NONE
        );
    }

    #[test]
    fn add_is_idempotent() {
        let mut f = MessageFlags::empty();
        f.add(SystemFlags::SEEN, []);
        let once = f.clone();
        f.add(SystemFlags::SEEN, []);
        assert_eq!(f, once, "+FLAGS must be idempotent");
    }

    #[test]
    fn remove_is_idempotent() {
        let mut f = MessageFlags::from_system(SystemFlags::SEEN);
        f.remove(SystemFlags::SEEN, &BTreeSet::new());
        let once = f.clone();
        f.remove(SystemFlags::SEEN, &BTreeSet::new());
        assert_eq!(f, once, "-FLAGS must be idempotent");
    }

    #[test]
    fn replace_discards_previous_state() {
        let mut f = MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::DRAFT));
        f.add(SystemFlags::NONE, [kw("$Forwarded")]);
        f.replace(SystemFlags::FLAGGED, BTreeSet::new());

        assert!(f.has(SystemFlags::FLAGGED));
        assert!(!f.has(SystemFlags::SEEN));
        assert!(f.keywords().is_empty(), "FLAGS replaces keywords too");
    }

    #[test]
    fn keywords_reject_invalid_atoms() {
        assert!(Keyword::new("").is_none());
        assert!(
            Keyword::new(r"\Seen").is_none(),
            "system flags are not keywords"
        );
        assert!(Keyword::new("has space").is_none());
        assert!(Keyword::new("paren)").is_none());
        assert!(Keyword::new("$Forwarded").is_some());
        assert!(Keyword::new("Label1").is_some());
    }

    #[test]
    fn names_render_in_imap_form() {
        let mut f = MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::FLAGGED));
        f.add(SystemFlags::NONE, [kw("$Forwarded")]);
        assert_eq!(f.all_names(), vec![r"\Seen", r"\Flagged", "$Forwarded"]);
    }

    #[test]
    fn flag_sets_are_independent_values() {
        // The value-level guarantee behind per-entry flag independence; the
        // persisted equivalent is asserted in the storage tests.
        let base = MessageFlags::from_system(SystemFlags::SEEN);
        let mut copy = base.clone();
        copy.add(SystemFlags::DELETED, []);

        assert!(base.has(SystemFlags::SEEN));
        assert!(
            !base.has(SystemFlags::DELETED),
            "mutating a clone must not alias"
        );
        assert!(copy.has(SystemFlags::DELETED));
    }
}

//! Mailboxes: named, hierarchical containers owning a UID space.

use crate::{AccountId, MailboxId, Timestamp, Uid, UidValidity};
use core::fmt;
use std::collections::BTreeSet;

/// The IMAP hierarchy delimiter.
pub const HIERARCHY_DELIMITER: char = '/';

/// The reserved inbox name (RFC 9051 §5.1), matched case-insensitively.
pub const INBOX: &str = "INBOX";

/// A validated mailbox name.
///
/// Names are hierarchical with `/` as the delimiter and stored as Unicode —
/// `IMAP4rev2` mandates Net-Unicode, and modified-UTF-7 is a wire encoding the
/// IMAP adapter applies only for older clients. Encoding is not the domain's
/// concern.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MailboxName(String);

impl MailboxName {
    /// Validates and normalises a mailbox name.
    ///
    /// `INBOX` is case-insensitive and normalised to upper case; every other
    /// name keeps its case.
    ///
    /// # Errors
    /// Returns [`InvalidMailboxName`] for empty names, names with empty path
    /// components, or names containing control characters.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidMailboxName> {
        let name = name.into();

        if name.is_empty() {
            return Err(InvalidMailboxName::Empty);
        }
        if name.chars().any(char::is_control) {
            return Err(InvalidMailboxName::ControlCharacter);
        }
        if name.split(HIERARCHY_DELIMITER).any(str::is_empty) {
            return Err(InvalidMailboxName::EmptyComponent);
        }

        if name.eq_ignore_ascii_case(INBOX) {
            return Ok(Self(String::from(INBOX)));
        }
        Ok(Self(name))
    }

    /// The full path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the inbox.
    #[must_use]
    pub fn is_inbox(&self) -> bool {
        self.0 == INBOX
    }

    /// The path components.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split(HIERARCHY_DELIMITER)
    }

    /// The parent path, or `None` for a top-level mailbox.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.0
            .rsplit_once(HIERARCHY_DELIMITER)
            .map(|(parent, _)| Self(parent.to_owned()))
    }

    /// The final path component — the name shown in a mailbox list.
    #[must_use]
    pub fn leaf(&self) -> &str {
        self.0
            .rsplit_once(HIERARCHY_DELIMITER)
            .map_or(self.0.as_str(), |(_, leaf)| leaf)
    }

    /// Whether this mailbox is a descendant of `other`.
    #[must_use]
    pub fn is_descendant_of(&self, other: &Self) -> bool {
        self.0
            .strip_prefix(&other.0)
            .is_some_and(|rest| rest.starts_with(HIERARCHY_DELIMITER))
    }
}

impl fmt::Display for MailboxName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a mailbox name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidMailboxName {
    /// The name was empty.
    #[error("mailbox name must not be empty")]
    Empty,
    /// A path component between delimiters was empty.
    #[error("mailbox name must not contain an empty path component")]
    EmptyComponent,
    /// The name contained a control character.
    #[error("mailbox name must not contain control characters")]
    ControlCharacter,
}

/// RFC 6154 SPECIAL-USE and RFC 9051 name attributes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MailboxAttributes(BTreeSet<String>);

/// Every attribute RFC 6154 §2 defines as a special use.
///
/// Exhaustive and closed: RFC 6154 defines these six and no more, so an
/// attribute outside this list is not a special use no matter what it looks
/// like. Ordered as the RFC lists them, which makes every response that
/// derives from it deterministic.
///
/// `\All` is included for completeness of the vocabulary even though this
/// server provisions no all-mail mailbox — recognising a name a client may
/// send is separate from creating a mailbox that bears it.
pub const SPECIAL_USE_ATTRIBUTES: [&str; 6] = [
    r"\All",
    r"\Archive",
    r"\Drafts",
    r"\Junk",
    r"\Sent",
    r"\Trash",
];

impl MailboxAttributes {
    /// No attributes.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Builds from attribute names such as `\Sent`.
    ///
    /// Named `from_names` rather than `from_iter` so it cannot be confused with
    /// [`FromIterator::from_iter`], which has different semantics.
    #[must_use]
    pub fn from_names<S: Into<String>>(items: impl IntoIterator<Item = S>) -> Self {
        Self(
            items
                .into_iter()
                .map(Into::into)
                .filter(|s: &String| !s.is_empty())
                .collect(),
        )
    }

    /// Whether an attribute is present.
    #[must_use]
    pub fn contains(&self, attribute: &str) -> bool {
        self.0.contains(attribute)
    }

    /// The attributes, sorted.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    /// The RFC 6154 special-use attributes, in a deterministic order.
    ///
    /// A *filter*, not a rename: only the six names RFC 6154 §2 defines are
    /// returned, and any other stored attribute (`\Noselect`, a
    /// client-invented label) is left out. That matters because
    /// `LIST (SPECIAL-USE)` is a filter on exactly this set — matching on the
    /// full attribute list would let `\Noselect` masquerade as a special use.
    #[must_use]
    pub fn special_use(&self) -> Vec<&str> {
        SPECIAL_USE_ATTRIBUTES
            .iter()
            .filter(|attribute| self.contains(attribute))
            .copied()
            .collect()
    }

    /// Whether the mailbox carries any RFC 6154 special-use attribute.
    #[must_use]
    pub fn has_special_use(&self) -> bool {
        SPECIAL_USE_ATTRIBUTES
            .iter()
            .any(|attribute| self.contains(attribute))
    }

    /// Whether the mailbox cannot be selected.
    #[must_use]
    pub fn is_noselect(&self) -> bool {
        self.contains(r"\Noselect")
    }

    /// The comma-separated persisted form.
    #[must_use]
    pub fn to_csv(&self) -> String {
        self.0.iter().cloned().collect::<Vec<_>>().join(",")
    }

    /// Parses the comma-separated persisted form.
    #[must_use]
    pub fn from_csv(value: &str) -> Self {
        Self::from_names(value.split(',').filter(|s| !s.is_empty()))
    }
}

/// A mailbox: a named container owning its own UID space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    /// Identity.
    pub id: MailboxId,
    /// Owning account.
    pub account_id: AccountId,
    /// Full hierarchical name.
    pub name: MailboxName,
    /// Parent mailbox, when nested.
    pub parent_id: Option<MailboxId>,
    /// UID-space generation.
    pub uid_validity: UidValidity,
    /// The next UID to assign. Never decreases, even after `EXPUNGE`.
    pub uid_next: Uid,
    /// Name attributes.
    pub attributes: MailboxAttributes,
    /// Whether the mailbox is subscribed.
    pub subscribed: bool,
    /// Creation time.
    pub created_at: Timestamp,
    /// The highest modification sequence ever issued in this mailbox.
    ///
    /// IMAP `HIGHESTMODSEQ` (RFC 7162 §3.1.1) and the allocator every flag
    /// change draws from. Owned by the mailbox rather than by the entry so
    /// that "changed since `n`" is answerable by comparison alone; see
    /// [`ModSeq`](crate::ModSeq).
    pub highest_mod_seq: crate::ModSeq,
}

#[cfg(test)]
mod tests {

    #[test]
    fn special_use_returns_only_rfc_6154_attributes() {
        // The filter, not a rename. `\Noselect` is a name attribute and must
        // not be reported as a special use, or a `LIST (SPECIAL-USE)` filter
        // would match a mailbox that has none.
        let attrs = MailboxAttributes::from_names([r"\Sent", r"\Noselect", r"\HasChildren"]);
        assert_eq!(attrs.special_use(), vec![r"\Sent"]);
        assert!(attrs.has_special_use());
    }

    #[test]
    fn a_mailbox_without_a_special_use_reports_none() {
        let attrs = MailboxAttributes::from_names([r"\Noselect"]);
        assert!(attrs.special_use().is_empty());
        assert!(!attrs.has_special_use());
    }

    #[test]
    fn no_attributes_means_no_special_use() {
        assert!(!MailboxAttributes::none().has_special_use());
        assert!(MailboxAttributes::none().special_use().is_empty());
    }

    #[test]
    fn special_use_is_reported_in_a_deterministic_order() {
        // Order follows SPECIAL_USE_ATTRIBUTES, not insertion, so any response
        // built from it is reproducible.
        let attrs = MailboxAttributes::from_names([r"\Trash", r"\Archive", r"\Sent"]);
        assert_eq!(attrs.special_use(), vec![r"\Archive", r"\Sent", r"\Trash"]);
    }

    #[test]
    fn a_duplicated_attribute_is_held_once() {
        // The dedup guarantee, asserted where it actually lives. The set is the
        // mechanism — a caller pushing a name twice cannot produce a duplicate
        // attribute in a LIST response, so the equivalent check at the CREATE
        // parsing site is defence in depth rather than the thing being relied
        // on.
        let attrs = MailboxAttributes::from_names([r"\Archive", r"\Archive"]);
        assert_eq!(attrs.special_use(), vec![r"\Archive"]);
        assert_eq!(attrs.iter().count(), 1);
        assert_eq!(attrs.to_csv(), r"\Archive");
    }

    #[test]
    fn every_rfc_6154_attribute_is_recognised() {
        // The vocabulary is closed and exhaustive: each name RFC 6154 §2
        // defines must be recognised, and nothing else may be.
        for attribute in SPECIAL_USE_ATTRIBUTES {
            let attrs = MailboxAttributes::from_names([attribute]);
            assert_eq!(attrs.special_use(), vec![attribute]);
        }
        for invented in [r"\Inbox", r"\Spam", r"\Important", "Sent"] {
            let attrs = MailboxAttributes::from_names([invented]);
            assert!(
                attrs.special_use().is_empty(),
                "{invented} must not be a special use"
            );
        }
    }

    #[test]
    fn special_use_survives_the_persisted_round_trip() {
        // Attributes persist as CSV, so the round trip is what a restart
        // actually performs.
        let original = MailboxAttributes::from_names([r"\Sent", r"\Noselect"]);
        let restored = MailboxAttributes::from_csv(&original.to_csv());
        assert_eq!(restored.special_use(), vec![r"\Sent"]);
        assert!(restored.is_noselect());
        assert_eq!(original, restored);
    }
    use super::*;

    fn name(s: &str) -> MailboxName {
        MailboxName::new(s).unwrap_or_else(|_| MailboxName(String::from("INBOX")))
    }

    #[test]
    fn inbox_is_case_insensitive_and_normalised() {
        // RFC 9051 §5.1.
        for spelling in ["INBOX", "inbox", "InBoX"] {
            let mailbox = name(spelling);
            assert_eq!(mailbox.as_str(), "INBOX");
            assert!(mailbox.is_inbox());
        }
    }

    #[test]
    fn other_names_keep_their_case() {
        assert_eq!(name("Sent").as_str(), "Sent");
        assert_ne!(name("sent").as_str(), name("Sent").as_str());
    }

    #[test]
    fn rejects_malformed_names() {
        assert_eq!(MailboxName::new(""), Err(InvalidMailboxName::Empty));
        assert_eq!(
            MailboxName::new("a//b"),
            Err(InvalidMailboxName::EmptyComponent)
        );
        assert_eq!(
            MailboxName::new("/leading"),
            Err(InvalidMailboxName::EmptyComponent)
        );
        assert_eq!(
            MailboxName::new("bad\nname"),
            Err(InvalidMailboxName::ControlCharacter)
        );
    }

    #[test]
    fn accepts_unicode_names() {
        // IMAP4rev2 mandates Net-Unicode; encoding is the adapter's problem.
        assert_eq!(name("Ärchiv/2026").as_str(), "Ärchiv/2026");
        assert_eq!(name("受信箱").as_str(), "受信箱");
    }

    #[test]
    fn hierarchy_navigation_works() {
        let mailbox = name("Work/Projects/Sandesh");
        assert_eq!(mailbox.leaf(), "Sandesh");
        assert_eq!(
            mailbox.parent().map(|p| p.as_str().to_owned()),
            Some(String::from("Work/Projects"))
        );
        assert_eq!(mailbox.components().count(), 3);
        assert!(mailbox.is_descendant_of(&name("Work")));
        assert!(!mailbox.is_descendant_of(&name("Personal")));
    }

    #[test]
    fn descendant_check_requires_a_delimiter_boundary() {
        // "Workshop" must not count as a child of "Work".
        assert!(!name("Workshop").is_descendant_of(&name("Work")));
    }

    #[test]
    fn attributes_round_trip_through_csv() {
        let attrs = MailboxAttributes::from_names([r"\Sent", r"\Noselect"]);
        assert_eq!(MailboxAttributes::from_csv(&attrs.to_csv()), attrs);
        assert!(attrs.is_noselect());
        assert!(MailboxAttributes::from_csv("").iter().next().is_none());
    }
}

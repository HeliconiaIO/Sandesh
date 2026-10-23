//! Accounts and the addresses that route to them.

use crate::{AccountId, EmailAddress, IdentityId, Timestamp};

/// An address that delivers into an account.
///
/// Accounts and addresses are separate because a developer routinely wants
/// `dev@example.test`, `dev+tag@example.test` and `noreply@example.test` all
/// landing in one inspectable mailbox (`CORE_SPEC.md` §4.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailIdentity {
    /// Identity of this record.
    pub id: IdentityId,
    /// The account this address delivers to.
    pub account_id: AccountId,
    /// The address itself.
    pub address: EmailAddress,
    /// Whether this is the account's primary address.
    pub is_primary: bool,
}

/// A credential for authenticating as an account.
///
/// Phase 4.5 implements verification; Phase 1 only needs to persist the shape.
/// `plain` exists solely for CRAM-MD5, which requires a recoverable secret and
/// is therefore only offered for accounts explicitly configured for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    /// Argon2id hash of the password.
    pub hash: String,
    /// Plaintext-equivalent secret, present only when CRAM-MD5 is enabled.
    pub plain: Option<String>,
}

/// A mail account: a login identity owning mailboxes and addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Identity.
    pub id: AccountId,
    /// Login username.
    pub username: String,
    /// Credential, or `None` when no password is set.
    pub credential: Option<Credential>,
    /// Whether this account receives mail for unmatched recipients.
    pub is_catch_all: bool,
    /// Creation time.
    pub created_at: Timestamp,
}

/// The mailboxes created for every new account.
///
/// `INBOX` is required by both IMAP and POP3. The rest carry RFC 6154
/// SPECIAL-USE attributes because mail clients expect to find them and will
/// otherwise create their own.
pub const DEFAULT_MAILBOXES: &[(&str, &str)] = &[
    ("INBOX", ""),
    ("Sent", r"\Sent"),
    ("Drafts", r"\Drafts"),
    ("Trash", r"\Trash"),
    ("Junk", r"\Junk"),
    ("Archive", r"\Archive"),
];

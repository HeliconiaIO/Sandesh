//! Email address representation.
//!
//! Parsing is deliberately **lenient** (`CORE_SPEC.md` §3): Sandesh is a testing
//! tool, and a mail server that rejects a malformed address is useless for
//! testing malformed addresses. Every parse yields a best-effort value; failures
//! surface as a [`ParseDiagnostic`](crate::ParseDiagnostic)-style note rather
//! than dropping the address.

use core::fmt;

/// An `addr-spec` with its original text preserved.
///
/// Comparison follows RFC 5321 §2.3.11: the domain is case-insensitive, the
/// local part is **case-sensitive** — only the destination host may interpret
/// it. Sandesh is that host, but preserving the distinction keeps behaviour
/// correct for applications that rely on it.
#[derive(Debug, Clone, Eq)]
pub struct EmailAddress {
    /// Verbatim local part, including any quoted form.
    local_part: String,
    /// Domain as written; may be empty for an address with no `@`.
    domain: String,
    /// Lowercased domain, used for comparison and hashing.
    domain_folded: String,
    /// Optional display name from a `Name <addr>` form.
    display_name: Option<String>,
    /// The exact input text, for raw fidelity.
    original: String,
}

impl EmailAddress {
    /// Parses leniently, always succeeding.
    ///
    /// Recognises `local@domain`, `Display Name <local@domain>` and `<addr>`.
    /// Input that matches none of these is kept whole as the local part with an
    /// empty domain, so nothing is ever lost.
    #[must_use]
    pub fn parse_lenient(input: &str) -> Self {
        let original = input.to_owned();
        let trimmed = input.trim();

        // `Display Name <addr>` — take the last '<' so a '<' inside a quoted
        // display name does not confuse the split.
        let (display_name, addr) = match (trimmed.rfind('<'), trimmed.rfind('>')) {
            (Some(open), Some(close)) if close > open => {
                let name = trimmed[..open].trim().trim_matches('"').trim();
                let name = if name.is_empty() {
                    None
                } else {
                    Some(name.to_owned())
                };
                (name, &trimmed[open + 1..close])
            }
            _ => (None, trimmed),
        };

        // Split on the last '@': a quoted local part may legally contain one.
        let (local_part, domain) = match addr.rfind('@') {
            Some(at) => (addr[..at].to_owned(), addr[at + 1..].to_owned()),
            None => (addr.to_owned(), String::new()),
        };

        Self {
            domain_folded: domain.to_lowercase(),
            local_part,
            domain,
            display_name,
            original,
        }
    }

    /// Replaces the display name, for callers that parsed it separately.
    #[must_use]
    pub fn with_display_name(mut self, display_name: Option<String>) -> Self {
        self.display_name = display_name;
        self
    }

    /// The local part, verbatim.
    #[must_use]
    pub fn local_part(&self) -> &str {
        &self.local_part
    }

    /// The domain as written.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// The display name, if the input carried one.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// The exact input text.
    #[must_use]
    pub fn original(&self) -> &str {
        &self.original
    }

    /// `local@lowercased-domain`, the form stored in `email_identities.address`
    /// and used for delivery routing.
    #[must_use]
    pub fn normalized(&self) -> String {
        if self.domain_folded.is_empty() {
            self.local_part.clone()
        } else {
            format!("{}@{}", self.local_part, self.domain_folded)
        }
    }

    /// Whether the address has both a local part and a domain.
    ///
    /// A `false` result never blocks delivery; it drives diagnostics only.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.local_part.is_empty() && !self.domain.is_empty()
    }
}

impl PartialEq for EmailAddress {
    /// Case-sensitive on the local part, case-insensitive on the domain.
    /// The display name and original text are presentation, not identity.
    fn eq(&self, other: &Self) -> bool {
        self.local_part == other.local_part && self.domain_folded == other.domain_folded
    }
}

impl core::hash::Hash for EmailAddress {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.local_part.hash(state);
        self.domain_folded.hash(state);
    }
}

impl fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.normalized())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_address() {
        let a = EmailAddress::parse_lenient("dev@example.test");
        assert_eq!(a.local_part(), "dev");
        assert_eq!(a.domain(), "example.test");
        assert_eq!(a.display_name(), None);
        assert!(a.is_well_formed());
    }

    #[test]
    fn parses_display_name_form() {
        let a = EmailAddress::parse_lenient("Dev Team <dev@Example.TEST>");
        assert_eq!(a.local_part(), "dev");
        assert_eq!(a.display_name(), Some("Dev Team"));
        assert_eq!(a.normalized(), "dev@example.test", "domain folds to lower");
        assert_eq!(
            a.original(),
            "Dev Team <dev@Example.TEST>",
            "input preserved"
        );
    }

    #[test]
    fn domain_is_case_insensitive_local_part_is_not() {
        // RFC 5321 §2.3.11.
        let lower = EmailAddress::parse_lenient("dev@example.test");
        let upper_domain = EmailAddress::parse_lenient("dev@EXAMPLE.TEST");
        let upper_local = EmailAddress::parse_lenient("DEV@example.test");

        assert_eq!(lower, upper_domain);
        assert_ne!(lower, upper_local);
    }

    #[test]
    fn preserves_plus_addressing() {
        let a = EmailAddress::parse_lenient("dev+tag@example.test");
        assert_eq!(a.local_part(), "dev+tag", "sub-addressing must survive");
    }

    #[test]
    fn handles_quoted_local_part_with_at_sign() {
        let a = EmailAddress::parse_lenient(r#""odd@name"@example.test"#);
        assert_eq!(a.local_part(), r#""odd@name""#);
        assert_eq!(a.domain(), "example.test");
    }

    #[test]
    fn malformed_input_never_panics_and_loses_nothing() {
        for input in [
            "",
            "@",
            "no-at-sign",
            "@no-local.test",
            "trailing@",
            "a@b@c",
            "<>",
            "  ",
            "Name <unterminated@example.test",
            "💌@例え.テスト",
        ] {
            let a = EmailAddress::parse_lenient(input);
            assert_eq!(a.original(), input, "original text must be retained");
        }
    }

    #[test]
    fn null_reverse_path_is_representable() {
        // SMTP `MAIL FROM:<>` — bounce testing depends on this being storable.
        let a = EmailAddress::parse_lenient("<>");
        assert!(!a.is_well_formed());
        assert_eq!(a.normalized(), "");
    }
}

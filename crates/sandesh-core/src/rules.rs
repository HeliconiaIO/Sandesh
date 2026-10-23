//! Message rules: auto-tagging at delivery, with an auditable reason.
//!
//! # The criterion shapes the design
//!
//! A3's acceptance criterion is "rules apply at delivery **and are visible in
//! the delivery record**", and the second clause is the harder half. A rule
//! that silently adds a keyword is exactly the opacity Phase 7 was built to
//! remove: a developer who finds `$Invoice` on a message with no way to learn
//! which rule put it there is worse off than one who finds no tag at all. So
//! [`RuleSet::evaluate`] returns [`RuleMatch`] values carrying the rule's name
//! alongside the keyword, and the delivery record stores both (ADR-087).
//!
//! # Why the conditions are narrow
//!
//! A header substring, the envelope sender, the envelope recipient — and
//! deliberately no expression language. Sandesh already has one query grammar
//! in `SearchQuery` (ADR-048), and a second subtly different one would be a
//! parser to write, a grammar to document and a fuzz target to maintain, for a
//! criterion that asks only that rules apply and are explained. Body matching
//! is excluded for a different reason: it would make every delivery scan the
//! decoded body, real per-message cost on the hot path for a capability nobody
//! has asked for.
//!
//! # Evaluation is pure and total
//!
//! No I/O, no failure, no way to reject a delivery. A malformed message must
//! still be deliverable — storing malformed mail is the product's purpose — so
//! a rule that cannot be evaluated against one simply does not match.

use crate::{EmailAddress, Keyword, ParsedMessage, RuleId, Timestamp};

/// What a rule looks at.
///
/// Each variant is one narrow question with obvious semantics. Matching is
/// case-insensitive substring throughout, because the alternatives — anchoring,
/// globbing, regex — are the first step toward the expression language ADR-087
/// declines to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleCondition {
    /// A named header contains this text.
    ///
    /// Tests the *decoded* value, so `Subject: =?utf-8?B?SW52b2ljZQ==?=`
    /// matches `Invoice`. Testing raw bytes would make a rule's behaviour
    /// depend on how the sender happened to encode the header, which is not a
    /// distinction anyone writing a rule means to draw.
    HeaderContains {
        /// Header name, matched case-insensitively.
        name: String,
        /// Substring to look for in the decoded value.
        value: String,
    },
    /// The envelope sender (`MAIL FROM`) contains this text.
    SenderContains(String),
    /// Any envelope recipient (`RCPT TO`) contains this text.
    RecipientContains(String),
}

impl RuleCondition {
    /// Whether this condition holds for the message.
    ///
    /// Total by construction: every branch is a substring test over data that
    /// is always present or absent, never malformed.
    #[must_use]
    pub fn matches(
        &self,
        parsed: &ParsedMessage,
        sender: Option<&EmailAddress>,
        recipients: &[EmailAddress],
    ) -> bool {
        match self {
            Self::HeaderContains { name, value } => parsed
                .headers_named(name)
                .any(|header| contains_ignoring_case(&header.decoded_value, value)),
            // `normalized()` rather than `original()`: a rule naming an
            // address should mean the address the router actually used, not
            // whatever casing or display name the sender happened to write.
            Self::SenderContains(text) => {
                sender.is_some_and(|address| contains_ignoring_case(&address.normalized(), text))
            }
            Self::RecipientContains(text) => recipients
                .iter()
                .any(|address| contains_ignoring_case(&address.normalized(), text)),
        }
    }
}

/// Case-insensitive substring test.
///
/// An empty needle matches, which is what `str::contains` does and what a rule
/// author writing an empty condition has asked for. Rejecting it belongs at the
/// API boundary, where it can be reported, rather than here where it would have
/// to be silently ignored.
fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// What a rule does when it matches.
///
/// One variant in Pass A. An enum rather than a bare `Keyword` because the
/// plan's Pass B candidates — mailbox placement, forwarding — are actions, and
/// a struct field named `keyword` would have to be reshaped to admit them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleAction {
    /// Add this keyword to every placement the delivery creates.
    AddKeyword(Keyword),
}

/// One configured rule.
///
/// Conditions are `AND`ed. That is the useful default — "from billing@ *and*
/// subject contains invoice" — and OR is expressible by writing two rules,
/// which keeps the combinator out of the data model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Identity.
    pub id: RuleId,
    /// A human name, shown in the delivery record.
    ///
    /// Recorded alongside each match rather than referenced by id, so a rule
    /// edited or deleted later cannot rewrite history (ADR-087).
    pub name: String,
    /// Every condition must hold.
    pub conditions: Vec<RuleCondition>,
    /// What to do when they all do.
    pub action: RuleAction,
    /// Whether the rule is evaluated at all.
    pub enabled: bool,
    /// When it was created.
    pub created_at: Timestamp,
}

impl Rule {
    /// Whether every condition holds.
    ///
    /// A rule with no conditions matches **nothing**, not everything. The
    /// vacuous-truth reading of "all conditions hold" would make an
    /// incompletely-configured rule tag every message that arrives, which is
    /// the most surprising possible behaviour for a half-finished rule.
    #[must_use]
    pub fn matches(
        &self,
        parsed: &ParsedMessage,
        sender: Option<&EmailAddress>,
        recipients: &[EmailAddress],
    ) -> bool {
        if !self.enabled || self.conditions.is_empty() {
            return false;
        }
        self.conditions
            .iter()
            .all(|condition| condition.matches(parsed, sender, recipients))
    }
}

/// One rule firing, as recorded on the delivery.
///
/// Carries the name and the keyword rather than a foreign key: a delivery
/// record that renders as "rule 019a… matched" once its rule is gone explains
/// nothing, which is the failure this whole increment exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatch {
    /// Which rule fired.
    pub rule_id: RuleId,
    /// Its name at the time it fired.
    pub rule_name: String,
    /// The keyword it added.
    pub keyword: Keyword,
}

/// The rules in force, evaluated together.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    rules: Vec<Rule>,
}

impl RuleSet {
    /// Builds a set from configured rules.
    #[must_use]
    pub const fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// Whether any rule is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Every rule that fires for this message, in configuration order.
    ///
    /// Pure and total: no I/O, no failure. Rules are independent, so one that
    /// does not match cannot prevent another from matching, and evaluation
    /// never rejects a delivery.
    #[must_use]
    pub fn evaluate(
        &self,
        parsed: &ParsedMessage,
        sender: Option<&EmailAddress>,
        recipients: &[EmailAddress],
    ) -> Vec<RuleMatch> {
        self.rules
            .iter()
            .filter(|rule| rule.matches(parsed, sender, recipients))
            .map(|rule| {
                let RuleAction::AddKeyword(keyword) = &rule.action;
                RuleMatch {
                    rule_id: rule.id,
                    rule_name: rule.name.clone(),
                    keyword: keyword.clone(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> ParsedMessage {
        crate::parse(raw.as_bytes())
    }

    fn keyword(text: &str) -> Keyword {
        Keyword::new(text).expect("a valid keyword")
    }

    fn address(text: &str) -> EmailAddress {
        EmailAddress::parse_lenient(text)
    }

    fn rule(name: &str, conditions: Vec<RuleCondition>) -> Rule {
        Rule {
            id: RuleId::new(),
            name: name.to_owned(),
            conditions,
            action: RuleAction::AddKeyword(keyword("$Tagged")),
            enabled: true,
            created_at: Timestamp::now(),
        }
    }

    const INVOICE: &str = "From: billing@vendor.test\r\n\
                           To: dev@example.test\r\n\
                           Subject: Invoice 4021 attached\r\n\
                           \r\n\
                           body\r\n";

    #[test]
    fn a_header_substring_matches_case_insensitively() {
        let set = RuleSet::new(vec![rule(
            "invoices",
            vec![RuleCondition::HeaderContains {
                name: String::from("subject"),
                value: String::from("INVOICE"),
            }],
        )]);

        let matches = set.evaluate(&parse(INVOICE), None, &[]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].rule_name, "invoices");
        assert_eq!(matches[0].keyword.as_str(), "$Tagged");
    }

    #[test]
    fn a_match_carries_the_rule_name_not_only_its_id() {
        // The criterion is that a rule is *visible in the delivery record*. An
        // id alone would render as "rule 019a… matched" once the rule is gone.
        let set = RuleSet::new(vec![rule(
            "from billing",
            vec![RuleCondition::SenderContains(String::from("billing@"))],
        )]);

        let matches = set.evaluate(&parse(INVOICE), Some(&address("billing@vendor.test")), &[]);
        assert_eq!(matches[0].rule_name, "from billing");
    }

    #[test]
    fn conditions_are_anded() {
        let both = RuleSet::new(vec![rule(
            "billing invoices",
            vec![
                RuleCondition::SenderContains(String::from("billing@")),
                RuleCondition::HeaderContains {
                    name: String::from("subject"),
                    value: String::from("invoice"),
                },
            ],
        )]);

        let parsed = parse(INVOICE);
        assert_eq!(
            both.evaluate(&parsed, Some(&address("billing@vendor.test")), &[])
                .len(),
            1,
            "both conditions hold"
        );
        assert!(
            both.evaluate(&parsed, Some(&address("someone@else.test")), &[])
                .is_empty(),
            "one condition failing must fail the rule"
        );
    }

    #[test]
    fn a_rule_with_no_conditions_matches_nothing() {
        // Not vacuous truth. "All of zero conditions hold" would tag every
        // message that arrives, which is the worst possible reading of a
        // half-configured rule.
        let set = RuleSet::new(vec![rule("unfinished", vec![])]);
        assert!(set.evaluate(&parse(INVOICE), None, &[]).is_empty());
    }

    #[test]
    fn a_disabled_rule_does_not_fire() {
        let mut disabled = rule(
            "off",
            vec![RuleCondition::HeaderContains {
                name: String::from("subject"),
                value: String::from("invoice"),
            }],
        );
        disabled.enabled = false;

        let set = RuleSet::new(vec![disabled]);
        assert!(set.evaluate(&parse(INVOICE), None, &[]).is_empty());
    }

    #[test]
    fn rules_are_independent_of_each_other() {
        let set = RuleSet::new(vec![
            rule(
                "never",
                vec![RuleCondition::SenderContains(String::from("nobody@"))],
            ),
            rule(
                "always",
                vec![RuleCondition::HeaderContains {
                    name: String::from("subject"),
                    value: String::from("invoice"),
                }],
            ),
        ]);

        // A rule that does not match must not stop a later one that does.
        let matches = set.evaluate(&parse(INVOICE), Some(&address("billing@vendor.test")), &[]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].rule_name, "always");
    }

    #[test]
    fn an_encoded_header_matches_its_decoded_text() {
        // Testing raw bytes would make a rule depend on how the sender encoded
        // the header, which is not a distinction a rule author means to draw.
        let encoded = "From: a@b.test\r\nSubject: =?utf-8?B?SW52b2ljZQ==?=\r\n\r\nbody\r\n";
        let set = RuleSet::new(vec![rule(
            "decoded",
            vec![RuleCondition::HeaderContains {
                name: String::from("subject"),
                value: String::from("invoice"),
            }],
        )]);

        assert_eq!(set.evaluate(&parse(encoded), None, &[]).len(), 1);
    }

    #[test]
    fn a_recipient_condition_matches_any_recipient() {
        let set = RuleSet::new(vec![rule(
            "support",
            vec![RuleCondition::RecipientContains(String::from("support@"))],
        )]);

        let parsed = parse(INVOICE);
        let recipients = [address("dev@example.test"), address("support@example.test")];
        assert_eq!(set.evaluate(&parsed, None, &recipients).len(), 1);
        assert!(
            set.evaluate(&parsed, None, &[address("dev@example.test")])
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_message_is_still_evaluable() {
        // Storing malformed mail is the product's purpose, so evaluation must
        // be total over it rather than panicking or refusing.
        let set = RuleSet::new(vec![rule(
            "anything",
            vec![RuleCondition::HeaderContains {
                name: String::from("subject"),
                value: String::from("invoice"),
            }],
        )]);

        for raw in ["", "\r\n", "not a message at all", "Subject:\r\n\r\n"] {
            let _ = set.evaluate(&parse(raw), None, &[]);
        }
    }

    #[test]
    fn an_empty_rule_set_matches_nothing() {
        assert!(RuleSet::default().is_empty());
        assert!(
            RuleSet::default()
                .evaluate(&parse(INVOICE), None, &[])
                .is_empty()
        );
    }
}

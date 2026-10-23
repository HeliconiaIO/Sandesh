//! The delivery record — an auditable answer to "why did this land here?".

use crate::{
    AccountId, ConnectionId, DeliveryId, DeliverySource, EmailAddress, EntryId, Envelope,
    MessageId, Timestamp,
};

/// What happened to one envelope recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Routed to an account that owns the address.
    Delivered,
    /// The address matched nothing; routed to the catch-all account.
    CatchAll,
    /// Rejected, under the `strict` unknown-recipient policy.
    Rejected,
}

impl DeliveryOutcome {
    /// The persisted discriminant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::CatchAll => "catch_all",
            Self::Rejected => "rejected",
        }
    }

    /// Parses a persisted discriminant.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "delivered" => Some(Self::Delivered),
            "catch_all" => Some(Self::CatchAll),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// The fate of one recipient within a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientOutcome {
    /// The envelope recipient address.
    pub rcpt_to: EmailAddress,
    /// The account the address resolved to, when it resolved.
    pub resolved_account: Option<AccountId>,
    /// The placement created, when one was.
    pub entry_id: Option<EntryId>,
    /// What happened.
    pub outcome: DeliveryOutcome,
    /// Explanation, for rejections and catch-all routing.
    pub reason: Option<String>,
}

/// A record of one message being delivered.
///
/// Delivery is a first-class entity rather than a function call: "why did this
/// land in that mailbox?" and "what did the transaction look like?" are the
/// questions a mail debugging tool exists to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// Identity.
    pub id: DeliveryId,
    /// The message delivered, if one was stored.
    pub message_id: Option<MessageId>,
    /// How it arrived.
    pub source: DeliverySource,
    /// The envelope presented.
    pub envelope: Envelope,
    /// Per-recipient results.
    pub outcomes: Vec<RecipientOutcome>,
    /// When delivery completed.
    pub received_at: Timestamp,
    /// How long it took, in microseconds.
    pub duration_us: i64,
    /// The connection this delivery arrived on, when it arrived on one.
    ///
    /// The transcript itself lives on the connection record (ADR-066): one
    /// connection can produce many deliveries, so the transcript cannot belong
    /// to any single one of them. `None` for an API injection, which has no
    /// protocol conversation.
    pub connection_id: Option<ConnectionId>,
    /// The rules that fired, and what each one did (ADR-087).
    ///
    /// The A3 criterion is that rules are "visible in the delivery record", so
    /// this is that visibility. Each entry names the rule as it stood when it
    /// fired, rather than pointing at one that may since have been edited or
    /// deleted — a record that renders as "rule 019a… matched" once its rule
    /// is gone explains nothing.
    pub rules_applied: Vec<crate::RuleMatch>,
    /// The message this one was replayed from, when it was a replay (A5).
    ///
    /// The same link is written into the replayed message as an
    /// `X-Sandesh-Replay-Of` header, so it travels with the bytes; this field
    /// is what makes it queryable. `None` for every ordinary delivery.
    pub replay_of: Option<MessageId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_round_trip() {
        for outcome in [
            DeliveryOutcome::Delivered,
            DeliveryOutcome::CatchAll,
            DeliveryOutcome::Rejected,
        ] {
            assert_eq!(DeliveryOutcome::parse(outcome.as_str()), Some(outcome));
        }
        assert_eq!(DeliveryOutcome::parse("nonsense"), None);
    }
}

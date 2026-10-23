//! Strongly typed entity identifiers.
//!
//! Every entity has its own identifier type. They are all UUID v7 underneath —
//! time-ordered, so they cluster well in B-tree indexes (see `STORAGE_DESIGN.md`
//! §2.1) — but they are *distinct types*, so the compiler rejects passing a
//! [`MailboxId`] where a [`MessageId`] is expected.
//!
//! This matters more than it might appear. `MessageEntry` carries both a
//! `message_id` and a `mailbox_id`; with bare UUIDs, transposing them at a call
//! site would compile cleanly and corrupt mailbox contents at runtime.

use core::fmt;
use uuid::Uuid;

/// Declares a newtype over [`Uuid`] with a consistent surface.
macro_rules! entity_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a fresh time-ordered identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wraps an existing UUID, for reconstruction from storage.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID, for persistence.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// The identifier as raw bytes, for `BLOB` storage.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 16] {
                self.0.as_bytes()
            }

            /// Reconstructs from raw `BLOB` bytes.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(Uuid::from_bytes(bytes))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

entity_id!(
    /// Identifies an [`Account`](crate::Account).
    AccountId
);
entity_id!(
    /// Identifies an [`EmailIdentity`](crate::EmailIdentity).
    IdentityId
);
entity_id!(
    /// Identifies a [`Mailbox`](crate::Mailbox).
    MailboxId
);
entity_id!(
    /// Identifies a [`Message`](crate::Message) — immutable content.
    MessageId
);
entity_id!(
    /// Identifies a [`MessageEntry`](crate::MessageEntry) — one placement of a
    /// message in a mailbox.
    EntryId
);
entity_id!(
    /// Identifies an [`Attachment`](crate::Attachment).
    AttachmentId
);
entity_id!(
    /// Identifies a [`Delivery`](crate::Delivery) record.
    DeliveryId
);
entity_id!(
    /// Identifies a [`ConnectionRecord`](crate::ConnectionRecord) — one
    /// accepted socket and its protocol transcript.
    ConnectionId
);
entity_id!(
    /// Identifies a [`DomainEvent`](crate::DomainEvent).
    EventId
);
entity_id!(
    /// Identifies a [`WebhookEndpoint`](crate::WebhookEndpoint).
    EndpointId
);
entity_id!(
    /// Identifies an [`OutboxEntry`](crate::OutboxEntry) — one pending or
    /// finished webhook delivery attempt.
    OutboxId
);
entity_id!(
    /// Identifies a [`Rule`](crate::Rule) — one auto-tagging rule.
    RuleId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        assert_ne!(MessageId::new(), MessageId::new());
    }

    #[test]
    fn ids_round_trip_through_bytes() {
        let id = MailboxId::new();
        assert_eq!(MailboxId::from_bytes(*id.as_bytes()), id);
    }

    #[test]
    fn v7_ids_are_time_ordered() {
        // UUID v7 embeds a millisecond timestamp, which is what gives inserts
        // index locality. Generating in a loop keeps the ordering observable
        // even when several land in the same millisecond.
        let mut previous = MessageId::new();
        for _ in 0..100 {
            let next = MessageId::new();
            assert!(next.as_uuid() > previous.as_uuid(), "v7 ids must ascend");
            previous = next;
        }
    }
}
